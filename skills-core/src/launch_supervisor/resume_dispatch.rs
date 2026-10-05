//! Frozen preparation of the retained Session's operator-authorized Resume.

use super::{
    ActiveOperation, ErrorCode, LifecycleRequest, MechanicFailure, OwnerEvent, PendingPhase,
    ProtocolError, ReceiptIntent, SessionOwner, SessionState, SupervisorError,
};
use crate::launch_protocol::GuardResumeRequest;

impl SessionOwner {
    pub(super) fn begin_resume_activation(&mut self) {
        let Some(pending) = self.pending.as_mut() else {
            return;
        };
        pending.phase = PendingPhase::Activating;
        let operation_epoch = pending.epoch;
        self.resume_capability_result = Some(
            self.resources
                .capability
                .as_mut()
                .ok_or(SupervisorError::CapabilityUnavailable)
                .and_then(|capability| capability.enable_after_resume()),
        );
        if !matches!(self.resume_capability_result, Some(Ok(()))) {
            self.fail_receipt_operation(ErrorCode::LifecycleMechanicUnavailable);
            return;
        }
        let sender = self.sender.clone();
        let connection_epoch = self.connection_epoch;
        let result = self
            .resources
            .process
            .as_mut()
            .ok_or(SupervisorError::IsolationRejected)
            .and_then(|process| {
                process.activate_guard_resume(Box::new(move |result| {
                    let _ = sender.try_send(OwnerEvent::GuardResumeActivated {
                        connection_epoch,
                        operation_epoch,
                        result,
                    });
                }))
            });
        if let Err(error) = result {
            self.guard_resume_activated(connection_epoch, operation_epoch, &Err(error));
        }
    }

    pub(super) fn guard_resume_activated(
        &mut self,
        connection_epoch: u64,
        operation_epoch: u64,
        guard: &Result<(), SupervisorError>,
    ) {
        if connection_epoch != self.connection_epoch
            || !self.pending_matches(operation_epoch, PendingPhase::Activating)
            || self.state != SessionState::Running
        {
            return;
        }
        let capability = self.resume_capability_result.take();
        if guard.is_err() || !matches!(capability, Some(Ok(()))) {
            self.fail_receipt_operation(ErrorCode::LifecycleMechanicUnavailable);
            return;
        }
        self.channel_state = super::ChannelState::Enabled;
        self.last_failure = None;
        self.resumed_conformance();
        self.resume_command_reception();
        self.complete_pending_receipt(false);
    }

    pub(super) fn handle_guard_resume(&mut self, authority: GuardResumeRequest) {
        let valid = authority.validate().is_ok();
        // Replays and busy refusals never install preparation authority. In
        // particular, an old wrapper must not replace a newer operation's scope.
        let recorded_or_busy = self.pending.is_some()
            || self.resources.conformance.resume.is_some()
            || self
                .failed
                .iter()
                .chain(self.saturated_failed_park.iter())
                .any(|failed| failed.request_id == authority.request.request_id)
            || self
                .completed
                .iter()
                .any(|completed| completed.request_id == authority.request.request_id);
        if valid && recorded_or_busy {
            self.handle_lifecycle(authority.request);
            return;
        }
        if !valid || authority.parked_head != self.broker_head || self.guard_resume.is_some() {
            self.send_error(
                authority.request.request_id,
                ProtocolError::new(
                    ErrorCode::InvalidRequest,
                    Some(self.state),
                    Some(self.broker_head.sequence),
                ),
            );
            return;
        }
        let request = authority.request.clone();
        self.guard_resume = Some(authority);
        self.handle_lifecycle(request);
        if self.pending.is_none() && self.resources.conformance.resume.is_none() {
            self.guard_resume = None;
        }
    }

    pub(super) fn begin_resume(&mut self, request: LifecycleRequest, intent: ReceiptIntent) {
        let Some(operation_epoch) = self.operation_epoch.checked_add(1) else {
            self.send_error(
                request.request_id,
                ProtocolError::new(
                    ErrorCode::InvalidRequest,
                    Some(self.state),
                    Some(self.broker_head.sequence),
                ),
            );
            self.guard_resume = None;
            return;
        };
        self.operation_epoch = operation_epoch;
        self.pending = Some(ActiveOperation {
            epoch: operation_epoch,
            request,
            intent,
            phase: PendingPhase::Applying,
            receipt: None,
            payload_override: None,
            respond: true,
            finish: None,
        });
        if self.schedule_operation_deadline(operation_epoch).is_err() {
            self.fail_guard_resume_preparation();
            return;
        }
        let connection_epoch = self.connection_epoch;
        let sender = self.sender.clone();
        let result = self
            .resources
            .broker
            .clone()
            .ok_or(SupervisorError::BrokerUnavailable)
            .and_then(|broker| {
                self.resources
                    .process
                    .as_mut()
                    .ok_or(SupervisorError::IsolationRejected)?
                    .prepare_guard_resume(
                        self.guard_resume.take(),
                        broker,
                        Box::new(move |result| {
                            // A saturated mailbox cannot authorize a thaw; the armed deadline narrows it.
                            let _ = sender.try_send(OwnerEvent::GuardResumePrepared {
                                connection_epoch,
                                operation_epoch,
                                result,
                            });
                        }),
                    )
            });
        if result.is_err() {
            self.fail_guard_resume_preparation();
        }
    }

    pub(super) fn guard_resume_prepared(
        &mut self,
        connection_epoch: u64,
        operation_epoch: u64,
        result: &Result<(), SupervisorError>,
    ) {
        if connection_epoch != self.connection_epoch
            || !self.pending_matches(operation_epoch, PendingPhase::Applying)
            || self.state != SessionState::Parked
        {
            return;
        }
        if result.is_err() {
            self.fail_guard_resume_preparation();
            return;
        }
        let resume = self
            .resources
            .process
            .as_mut()
            .map_or(Err(MechanicFailure::Ambiguous), |process| process.resume());
        match resume {
            Ok(()) | Err(MechanicFailure::Running) => self.state = SessionState::Running,
            Err(MechanicFailure::Parked) => {
                self.fail_guard_resume_preparation();
                return;
            }
            Err(MechanicFailure::Terminal(classification)) => {
                self.fail_mechanic_in_state(Some(SessionState::Terminal));
                self.begin_process_exit(classification);
                return;
            }
            Err(MechanicFailure::Ambiguous) => {
                self.quarantine_pending_mechanic();
                return;
            }
        }
        if let Some(pending) = self.pending.as_mut() {
            pending.phase = PendingPhase::Signing;
        }
        self.start_signing(operation_epoch);
    }

    pub(super) fn fail_guard_resume_preparation(&mut self) {
        self.guard_resume = None;
        // Preparation never thaws. A known-Parked mechanic refusal likewise
        // proves the tree stayed frozen; close prepared effects without another Park.
        let capability = if self.channel_state == super::ChannelState::Enabled {
            self.resources
                .capability
                .as_mut()
                .ok_or(SupervisorError::CapabilityUnavailable)
                .and_then(|capability| capability.revoke())
        } else {
            Ok(())
        };
        let guard = self
            .resources
            .process
            .as_mut()
            .ok_or(SupervisorError::CleanupUnproven)
            .and_then(|process| {
                process.revoke_guard()?;
                process.close_guard_handoff()
            });
        self.channel_state = super::ChannelState::Revoked;
        if capability.is_err() || guard.is_err() {
            self.quarantine_pending_mechanic();
            return;
        }
        self.fail_mechanic();
        self.widening_blocked =
            self.controller_loss_unresolved || self.cleanup_unproven || self.has_receipt_backlog();
    }
}
