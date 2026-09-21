//! One owned validity worker; lifecycle deadlines never wait for its I/O.

use super::{DeferredReceipt, ParkResult, SessionOwner};
use crate::{
    conformance::admission::Condition,
    launch_protocol::{
        BrokerConnection, CONFORMANCE_UPDATE_SCHEMA, ChannelState, ConformanceCheck,
        ConformanceFailure, ConformanceUpdate, ErrorCode, LaunchAuthorization, LifecycleRequest,
        ProtocolError, ReceiptIntent,
    },
    launch_receipt::{ConformanceEvidence, ReceiptCause, SessionState},
    launch_supervisor::{LaunchPlatform, SupervisorError},
};
use std::{
    sync::{Arc, Mutex, mpsc},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const INTERVAL: Duration = Duration::from_secs(1);
const FRESHNESS: Duration = Duration::from_secs(5);

pub(in crate::launch_supervisor) struct Monitor {
    pub(super) enabled: bool,
    pub(super) suspended: bool,
    containment_failed: bool,
    invalidated_at: Option<Instant>,
    source: Option<(Arc<dyn LaunchPlatform>, LaunchAuthorization)>,
    waiver_revision: u64,
    origin: Instant,
    origin_ms: u64,
    last_success: Instant,
    next_check: Instant,
    pending: Option<(
        Instant,
        mpsc::Receiver<Result<ConformanceEvidence, SupervisorError>>,
    )>,
    worker: Option<JoinHandle<()>>,
    sequence: u64,
    pub(super) resume: Option<(LifecycleRequest, ReceiptIntent, u64)>,
    resume_deadline: Option<Instant>,
    latest: Option<ConformanceUpdate>,
    last_success_ms: Option<u64>,
    publication: Arc<Mutex<Option<Result<(), SupervisorError>>>>,
    publishing: Option<(u64, u64, Instant)>,
    published: u64,
}

impl Monitor {
    fn waiver_valid_at(&self, now: Instant) -> bool {
        self.source.as_ref().is_some_and(|(_, authorization)| {
            authorization
                .conformance
                .waiver
                .as_ref()
                .is_some_and(|waiver| self.at_ms(now) < waiver.expires_at_ms)
        })
    }

    fn waiver_expired(&self, now: Instant) -> bool {
        !self.waiver_valid_at(now)
            && self.latest.as_ref().is_some_and(|current| {
                matches!(
                    current.check,
                    ConformanceCheck::Current {
                        evidence: ConformanceEvidence::Waived { .. }
                    }
                )
            })
    }
    fn change_waiver(
        &mut self,
        change: &crate::launch_protocol::conformance::WaiverChange,
        now: Instant,
    ) -> Result<bool, ProtocolError> {
        let invalid = || ProtocolError::new(ErrorCode::InvalidRequest, None, None);
        change.validate()?;
        let now_ms = self.at_ms(now);
        let (_, authorization) = self.source.as_ref().ok_or_else(invalid)?;
        if !self.enabled
            || change.session_id != authorization.session_id
            || change.request_digest != authorization.request_digest
            || authorization.conformance.attendance
                != crate::conformance::admission::Attendance::Interactive
            || change.revision < self.waiver_revision
        {
            return Err(invalid());
        }
        if change.revision == self.waiver_revision {
            return if change.waiver == authorization.conformance.waiver {
                Ok(false)
            } else {
                Err(invalid())
            };
        }
        if let Some(waiver) = &change.waiver {
            crate::launch_protocol::ConformanceAuthorization {
                attendance: authorization.conformance.attendance,
                waiver: Some(waiver.clone()),
            }
            .validate_for(
                &authorization.session_id,
                &authorization.request_digest,
                authorization.controller_uid,
                now_ms,
            )?;
            let current = self.latest.as_ref().ok_or_else(invalid)?;
            let condition = match &current.check {
                ConformanceCheck::Invalid {
                    failure: ConformanceFailure::Condition(condition),
                }
                | ConformanceCheck::Current {
                    evidence: ConformanceEvidence::Waived { condition, .. },
                } => *condition,
                _ => return Err(invalid()),
            };
            if self.containment_failed
                || condition != waiver.condition
                || condition == Condition::ContainmentFailure
                || current.observed_at_ms > now_ms
                || now_ms.saturating_sub(current.observed_at_ms) >= 5_000
            {
                return Err(invalid());
            }
        }
        let (_, authorization) = self.source.as_mut().ok_or_else(invalid)?;
        authorization.conformance.waiver.clone_from(&change.waiver);
        self.waiver_revision = change.revision;
        // A check already in flight used the previous decision. Its callback may
        // finish, but cannot publish proof or renew authority under this revision.
        self.pending = None;
        self.invalidated_at = Some(now);
        self.next_check = now;
        Ok(true)
    }
    pub(in crate::launch_supervisor) fn new(
        source: Option<(Arc<dyn LaunchPlatform>, LaunchAuthorization)>,
        origin_ms: u64,
        origin: Instant,
    ) -> Self {
        Self {
            enabled: false,
            suspended: false,
            containment_failed: false,
            invalidated_at: None,
            source,
            waiver_revision: 0,
            origin,
            origin_ms,
            last_success: origin,
            next_check: origin,
            pending: None,
            worker: None,
            sequence: 0,
            resume: None,
            resume_deadline: None,
            latest: None,
            last_success_ms: None,
            publication: Arc::new(Mutex::new(None)),
            publishing: None,
            published: 0,
        }
    }

    fn start(&mut self, now: Instant) -> Result<(), SupervisorError> {
        if self.pending.is_some() || now < self.next_check {
            return Ok(());
        }
        if self
            .worker
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
        {
            return Ok(());
        }
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .map_err(|_| SupervisorError::WorkerUnavailable)?;
        }
        let (platform, authorization) = self
            .source
            .as_ref()
            .ok_or(SupervisorError::ConformanceUnavailable)?;
        let platform = Arc::clone(platform);
        let authorization = authorization.clone();
        let now_ms = self.origin_ms.saturating_add(
            u64::try_from(now.duration_since(self.origin).as_millis()).unwrap_or(u64::MAX),
        );
        let (sender, receiver) = mpsc::sync_channel(1);
        self.worker = Some(
            thread::Builder::new()
                .name("louiselm-conformance-check".into())
                .spawn(move || {
                    let (done, result) = mpsc::sync_channel(1);
                    let deadline = Instant::now() + INTERVAL;
                    let checked = platform
                        .revalidate_conformance(
                            &authorization,
                            now_ms,
                            deadline,
                            Box::new(move |result| {
                                let _ = done.try_send(result);
                            }),
                        )
                        .and_then(|()| {
                            result
                                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                                .map_err(|_| SupervisorError::ConformanceUnavailable)?
                        });
                    let _ = sender.try_send(checked);
                })
                .map_err(|_| SupervisorError::WorkerUnavailable)?,
        );
        self.pending = Some((now, receiver));
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or(SupervisorError::ConformanceUnavailable)?;
        self.next_check = now + INTERVAL;
        Ok(())
    }

    fn collect(
        &mut self,
        now: Instant,
    ) -> Option<(Instant, Result<ConformanceEvidence, SupervisorError>)> {
        let (started, receiver) = self.pending.as_ref()?;
        // The check retains its own input time. Queueing and late delivery never
        // manufacture a new successful check or extend its freshness.
        let result = match receiver.try_recv() {
            Ok(result)
                if now.duration_since(*started) < FRESHNESS
                    && (!matches!(result, Ok(ConformanceEvidence::Waived { .. }))
                        || self.waiver_valid_at(now))
                    && self
                        .invalidated_at
                        .is_none_or(|invalidated| *started >= invalidated) =>
            {
                result
            }
            Ok(_) | Err(mpsc::TryRecvError::Disconnected) => {
                Err(SupervisorError::ConformanceUnavailable)
            }
            Err(mpsc::TryRecvError::Empty) => return None,
        };
        if matches!(
            result,
            Ok(ConformanceEvidence::Certified { .. } | ConformanceEvidence::Waived { .. })
        ) {
            self.last_success = *started;
        }
        let started = *started;
        self.pending = None;
        Some((started, result))
    }

    fn at_ms(&self, at: Instant) -> u64 {
        self.origin_ms.saturating_add(
            u64::try_from(at.duration_since(self.origin).as_millis()).unwrap_or(u64::MAX),
        )
    }

    pub(super) fn finish(&mut self) -> Result<(), SupervisorError> {
        self.worker.take().map_or(Ok(()), |worker| {
            worker
                .join()
                .map_err(|_| SupervisorError::WorkerUnavailable)
        })
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        // The worker only reads protected conformance inputs. It never owns an
        // Agent, identity or capability; cleanup revokes those before this join.
        let _ = self.finish();
    }
}

impl SessionOwner {
    pub(super) fn handle_waiver_change(
        &mut self,
        change: crate::launch_protocol::conformance::WaiverChange,
    ) {
        let result = if matches!(self.state, SessionState::Running | SessionState::Parked)
            && self.pending.is_none()
            && !self.quarantined
            && !self.key_authority.withdrawn
        {
            self.resources
                .conformance
                .change_waiver(&change, self.timer.now())
        } else {
            Err(ProtocolError::new(
                ErrorCode::OperationPending,
                Some(self.state),
                Some(self.broker_head.sequence),
            ))
        };
        match result {
            Ok(changed) => {
                if changed {
                    self.cancel_conformance_resume();
                    self.suspend_conformance(false);
                    self.observe_conformance(
                        ConformanceCheck::Invalid {
                            failure: ConformanceFailure::Unavailable,
                        },
                        self.timer.now(),
                    );
                }
                self.send_response(crate::launch_protocol::ProtocolResponse {
                    schema: crate::launch_protocol::RESPONSE_SCHEMA.into(),
                    protocol_version: crate::launch::PROTOCOL_VERSION,
                    request_id: change.request_id.clone(),
                    result: crate::launch_protocol::ResponseResult::WaiverChanged {
                        change: Box::new(change),
                    },
                });
            }
            Err(error) => self.send_error(change.request_id, error),
        }
    }
    pub(super) fn maintain_conformance(&mut self) {
        if !self.resources.conformance.enabled
            || self.state == SessionState::Terminal
            || self.quarantined
            || self.key_authority.withdrawn
        {
            return;
        }
        let now = self.timer.now();
        if self.resources.conformance.waiver_expired(now) {
            self.suspend_conformance(false);
            self.observe_conformance(
                ConformanceCheck::Invalid {
                    failure: ConformanceFailure::Unavailable,
                },
                now,
            );
        }
        if self
            .resources
            .conformance
            .resume_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            self.cancel_conformance_resume();
        }
        if now.duration_since(self.resources.conformance.last_success) >= FRESHNESS {
            if !self.resources.conformance.suspended {
                self.observe_conformance(
                    ConformanceCheck::Invalid {
                        failure: ConformanceFailure::Deadline,
                    },
                    now,
                );
            }
            self.suspend_conformance(false);
        }
        if let Some((started, result)) = self.resources.conformance.collect(now) {
            let usable = matches!(
                &result,
                Ok(ConformanceEvidence::Certified { .. } | ConformanceEvidence::Waived { .. })
            );
            let check = match &result {
                Ok(evidence) if usable => ConformanceCheck::Current {
                    evidence: evidence.clone(),
                },
                Err(SupervisorError::ConformanceRefused(condition)) => ConformanceCheck::Invalid {
                    failure: ConformanceFailure::Condition(*condition),
                },
                _ => ConformanceCheck::Invalid {
                    failure: ConformanceFailure::Unavailable,
                },
            };
            self.observe_conformance(check, if usable { started } else { now });
            match &result {
                Ok(ConformanceEvidence::Certified { .. } | ConformanceEvidence::Waived { .. }) => {}
                Err(SupervisorError::ConformanceRefused(Condition::ContainmentFailure)) => {
                    self.suspend_conformance(true);
                }
                Ok(ConformanceEvidence::Unevaluated) | Err(_) => self.suspend_conformance(false),
            }
            if self
                .resources
                .conformance
                .resume
                .as_ref()
                .is_some_and(|(_, _, sequence)| *sequence <= self.resources.conformance.sequence)
            {
                if usable && !self.resources.conformance.containment_failed {
                    if let Some((request, intent, _)) = self.resources.conformance.resume.take() {
                        self.resources.conformance.resume_deadline = None;
                        self.resources.conformance.suspended = false;
                        self.begin_resume(request, intent);
                    }
                } else {
                    self.cancel_conformance_resume();
                }
            }
        }
        if self.state != SessionState::Terminal
            && !self.quarantined
            && self.resources.conformance.start(now).is_err()
        {
            self.observe_conformance(
                ConformanceCheck::Invalid {
                    failure: ConformanceFailure::Unavailable,
                },
                now,
            );
            self.suspend_conformance(false);
        }
        self.publish_conformance();
    }

    fn suspend_conformance(&mut self, containment_failed: bool) {
        self.resources.conformance.containment_failed |= containment_failed;
        if self.resources.conformance.suspended {
            return;
        }
        self.resources.conformance.suspended = true;
        self.resources.conformance.invalidated_at = Some(self.timer.now());
        self.restore_after_reconnect = false;
        let was_running = self.state == SessionState::Running;
        let revoked = self
            .resources
            .capability
            .as_mut()
            .is_some_and(|gate| gate.revoke().is_ok());
        self.channel_state = ChannelState::Revoked;
        if !matches!(self.attempt_park(), ParkResult::Parked) || !revoked {
            let pending = self.pending.take();
            self.quarantine_mechanic(pending);
            return;
        }
        self.last_failure = Some(ProtocolError::new(
            ErrorCode::ConformanceUnavailable,
            Some(self.state),
            Some(self.broker_head.sequence),
        ));
        if !was_running {
            return;
        }
        if self.pending.is_none()
            && !self.has_receipt_backlog()
            && self.broker_connection == BrokerConnection::Connected
        {
            self.begin_caused_park(ReceiptCause::ConformanceInvalid, "conformance");
        } else {
            let resuming = self.pending.as_ref().is_some_and(|pending| {
                pending.request.action == crate::launch_protocol::LifecycleAction::Resume
            });
            self.fail_receipt_operation(ErrorCode::ConformanceUnavailable);
            // Failed Resume already retains its Running receipt and a causal
            // Park. A second Park would contradict that immutable chain.
            if !resuming {
                self.defer(DeferredReceipt::CausalPark {
                    request_id: format!("conformance-{}", self.receipt().digest().hex()),
                    envelope_revision: self.binding.envelope_revision,
                    cause: ReceiptCause::ConformanceInvalid,
                });
            }
        }
    }

    pub(super) fn request_conformance_resume(
        &mut self,
        request: LifecycleRequest,
        intent: ReceiptIntent,
    ) {
        if self.resources.conformance.containment_failed {
            self.send_error(
                request.request_id,
                ProtocolError::new(
                    ErrorCode::ConformanceUnavailable,
                    Some(self.state),
                    Some(self.broker_head.sequence),
                ),
            );
            return;
        }
        self.resources.conformance.resume = Some((
            request,
            intent,
            self.resources.conformance.sequence.saturating_add(1),
        ));
        self.resources.conformance.resume_deadline =
            Some(self.timer.now() + FRESHNESS.min(self.timeout));
        self.resources.conformance.next_check = self.timer.now();
    }

    pub(super) fn cancel_conformance_resume(&mut self) {
        self.resources.conformance.resume_deadline = None;
        if let Some((request, _, _)) = self.resources.conformance.resume.take() {
            self.cache_and_send_error(
                request,
                ProtocolError::new(
                    ErrorCode::ConformanceUnavailable,
                    Some(self.state),
                    Some(self.broker_head.sequence),
                ),
            );
        }
    }

    fn observe_conformance(&mut self, mut check: ConformanceCheck, at: Instant) {
        if self.resources.conformance.containment_failed {
            check = ConformanceCheck::Invalid {
                failure: ConformanceFailure::Condition(Condition::ContainmentFailure),
            };
        }
        let observed_at_ms = self.resources.conformance.at_ms(at);
        if matches!(check, ConformanceCheck::Current { .. }) {
            self.resources.conformance.last_success_ms = Some(observed_at_ms);
        }
        let sequence = self
            .resources
            .conformance
            .latest
            .as_ref()
            .map_or(1, |old| old.sequence.saturating_add(1));
        if let Some((_, authorization)) = &self.resources.conformance.source {
            self.resources.conformance.latest = Some(ConformanceUpdate {
                waiver_revision: self.resources.conformance.waiver_revision,
                schema: CONFORMANCE_UPDATE_SCHEMA.into(),
                session_id: authorization.session_id.clone(),
                run_id: authorization.run_id.clone(),
                authorization_id: authorization.authorization_id.clone(),
                request_digest: authorization.request_digest.clone(),
                envelope_revision: authorization.envelope_revision,
                sequence,
                observed_at_ms,
                last_success_at_ms: self.resources.conformance.last_success_ms,
                suspended: self.resources.conformance.suspended
                    || self.state != SessionState::Running
                    || self.channel_state != ChannelState::Enabled
                    || matches!(check, ConformanceCheck::Invalid { .. }),
                check,
            });
        }
    }

    fn publish_conformance(&mut self) {
        if let Some((epoch, sequence, started)) = self.resources.conformance.publishing {
            let result = self
                .resources
                .conformance
                .publication
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            if epoch != self.connection_epoch {
                self.resources.conformance.publishing = None;
            } else if let Some(result) = result {
                self.resources.conformance.publishing = None;
                if let Err(error) = result {
                    self.lose_broker(error);
                    return;
                }
                self.resources.conformance.published = sequence;
            } else if self.timer.now().duration_since(started) >= FRESHNESS {
                self.resources.conformance.publishing = None;
                self.lose_broker(SupervisorError::BrokerTimeout);
                return;
            } else {
                return;
            }
        }
        if self.broker_connection != BrokerConnection::Connected {
            return;
        }
        let Some(update) = self
            .resources
            .conformance
            .latest
            .as_ref()
            .filter(|update| update.sequence > self.resources.conformance.published)
            .cloned()
        else {
            return;
        };
        // One mailbox per send keeps stale callbacks isolated across reconnects.
        let completion = Arc::new(Mutex::new(None));
        self.resources.conformance.publication = Arc::clone(&completion);
        self.resources.conformance.publishing =
            Some((self.connection_epoch, update.sequence, self.timer.now()));
        let result = self
            .resources
            .broker
            .as_ref()
            .ok_or(SupervisorError::BrokerUnavailable)
            .and_then(|broker| {
                broker.send_conformance(
                    update,
                    Box::new(move |result| {
                        *completion
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(result);
                    }),
                )
            });
        if let Err(error) = result {
            self.resources.conformance.publishing = None;
            self.lose_broker(error);
        }
    }

    pub(super) fn reconnect_conformance(&mut self) {
        self.resources.conformance.published = 0;
        self.resources.conformance.publishing = None;
    }

    pub(super) fn resumed_conformance(&mut self) {
        if let Some(latest) = self.resources.conformance.latest.as_mut() {
            latest.sequence = latest.sequence.saturating_add(1);
            latest.suspended = false;
        }
    }
}
