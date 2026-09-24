//! Serialized operator decisions and delivery to the existing enforcing owner.

use super::{Context, Outcome, Request, WaiverError};
use crate::{
    broker::{BrokerError, BrokerService, BrokerSession},
    conformance::admission::{Attendance, Condition},
    launch_protocol::{
        ConformanceCheck, ConformanceFailure, ResponseResult, conformance::WaiverChange,
    },
    launch_receipt::{ConformanceEvidence, SessionState},
    launch_transport::LauncherPacket,
};

impl BrokerService {
    /// Read a durable preview/approval after disconnection or Session termination.
    /// This is historical inspection; it does not contact the supervisor or renew policy.
    /// # Errors
    /// Refuses foreign operators, mutation requests and unavailable trusted history.
    pub fn waiver_history(
        &self,
        operator_uid: u32,
        session_id: &str,
        request: &Request,
        now_ms: u64,
    ) -> Result<Outcome, BrokerError> {
        if !matches!(request, Request::Inspect | Request::Result { .. }) {
            return Err(WaiverError::InvalidRequest.into());
        }
        let authorization = self
            .authorizations()
            .consumed_for_session(session_id)?
            .ok_or(WaiverError::Unknown)?;
        if authorization.controller_uid != operator_uid {
            return Err(WaiverError::WrongOperator.into());
        }
        if authorization.conformance.attendance != Attendance::Interactive {
            return Err(WaiverError::Unattended.into());
        }
        self.check_history(session_id)?;
        let context = Context {
            preparation: None,
            session_id: session_id.into(),
            run_id: authorization.run_id,
            authorization_id: authorization.authorization_id,
            request_digest: authorization.request_digest,
            envelope_revision: authorization.envelope_revision,
            operator_uid,
            attendance: authorization.conformance.attendance,
            condition: Condition::Missing,
            receipt_head: self
                .receipts()
                .head(session_id)?
                .ok_or(WaiverError::Unknown)?
                .digest,
        };
        let mut outcome = self.waivers.control(&context, request, now_ms)?;
        outcome.active &= self.receipts().state(session_id)? != Some(SessionState::Terminal);
        Ok(outcome)
    }
    /// Inspect, preview, approve or revoke an exact live conformance exception.
    /// Run on the Session's owning worker; the operator UID comes from transport.
    /// Approval never resumes processes or restores command grants.
    /// # Errors
    /// Refuses foreign/unattended callers, stale evidence, replay conflicts,
    /// non-waivable conditions and unavailable durable or supervisor state.
    pub fn waiver_control<F>(
        &self,
        session: &mut BrokerSession,
        operator_uid: u32,
        request: &Request,
        now_ms: u64,
        mut verify: F,
    ) -> Result<Outcome, BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        let authorization = session.authorization().clone();
        if operator_uid != authorization.controller_uid {
            return Err(WaiverError::WrongOperator.into());
        }
        if authorization.conformance.attendance != Attendance::Interactive {
            return Err(WaiverError::Unattended.into());
        }
        let started = std::time::Instant::now();
        let status = self.supervisor_status(session, &mut verify)?;
        let now =
            now_ms.saturating_add(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX));
        let current = session
            .posture_evidence
            .current_conformance
            .as_ref()
            .map(|value| &value.update);
        let condition = current.and_then(|current| match &current.check {
            ConformanceCheck::Invalid {
                failure: ConformanceFailure::Condition(condition),
            }
            | ConformanceCheck::Current {
                evidence: ConformanceEvidence::Waived { condition, .. },
            } => Some(*condition),
            _ => None,
        });
        let context = Context {
            preparation: None,
            session_id: authorization.session_id.clone(),
            run_id: authorization.run_id.clone(),
            authorization_id: authorization.authorization_id.clone(),
            request_digest: authorization.request_digest.clone(),
            envelope_revision: authorization.envelope_revision,
            operator_uid,
            attendance: authorization.conformance.attendance,
            condition: condition.unwrap_or(Condition::Missing),
            receipt_head: status
                .broker_head
                .as_ref()
                .ok_or(WaiverError::Unavailable)?
                .digest
                .clone(),
        };
        let already_approved = if let Request::Apply { plan_digest } = request {
            self.waivers
                .control(
                    &context,
                    &Request::Result {
                        plan_digest: plan_digest.clone(),
                    },
                    now,
                )?
                .receipt
                .is_some()
        } else {
            false
        };
        if matches!(request, Request::Plan { .. })
            || matches!(request, Request::Apply { .. }) && !already_approved
        {
            if !matches!(status.state, SessionState::Running | SessionState::Parked)
                || status.pending_operation.is_some()
                || self.lifecycle.is_quarantined(&authorization.session_id)?
            {
                return Err(WaiverError::StalePlan.into());
            }
            if condition.is_none()
                || condition == Some(Condition::ContainmentFailure)
                || current.is_none_or(|current| {
                    current.observed_at_ms > now
                        || now.saturating_sub(current.observed_at_ms) >= 5_000
                })
            {
                return Err(WaiverError::NotWaivable.into());
            }
        }
        let before_revision = self.waivers.decision(&authorization)?.0;
        let mut outcome = self.waivers.control(&context, request, now)?;
        outcome.active &= matches!(status.state, SessionState::Running | SessionState::Parked);
        if matches!(request, Request::Apply { .. } | Request::Revoke { .. }) {
            self.publish_waiver_decision(
                session,
                request,
                before_revision,
                outcome.active,
                &mut verify,
            )?;
        }
        Ok(outcome)
    }

    fn publish_waiver_decision<F>(
        &self,
        session: &mut BrokerSession,
        request: &Request,
        before_revision: u64,
        active: bool,
        verify: &mut F,
    ) -> Result<(), BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        let authorization = session.authorization().clone();
        let (revision, waiver) = self.waivers.decision(&authorization)?;
        if before_revision == revision && (matches!(request, Request::Revoke { .. }) || !active) {
            return Ok(());
        }
        let changed = session.posture_evidence.waiver_revision != revision;
        session.posture_evidence.waiver_revision = revision;
        session
            .posture_evidence
            .conformance_authorization
            .waiver
            .clone_from(&waiver);
        if changed {
            session.posture_evidence.current_conformance = None;
        }
        if matches!(request, Request::Revoke { .. }) || active {
            let change = WaiverChange {
                schema: "louiselm.launch.waiver-change/1".into(),
                protocol_version: crate::launch::PROTOCOL_VERSION,
                request_id: format!("waiver-{revision}"),
                session_id: authorization.session_id,
                request_digest: authorization.request_digest,
                revision,
                waiver,
            };
            if let Err(error) = self.deliver_waiver(session, &change, verify) {
                session.close();
                return Err(error);
            }
        }
        Ok(())
    }

    fn deliver_waiver<F>(
        &self,
        session: &mut BrokerSession,
        change: &WaiverChange,
        verify: &mut F,
    ) -> Result<(), BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        crate::broker::service::send(session.channel(), change.canonical_bytes()?)?;
        loop {
            let packet = crate::broker::service::receive(session.channel())?;
            if let LauncherPacket::Response(response) = &packet.packet {
                if response.request_id != change.request_id {
                    return Err(WaiverError::Unavailable.into());
                }
                return match &response.result {
                    ResponseResult::WaiverChanged { change: confirmed }
                        if **confirmed == *change =>
                    {
                        Ok(())
                    }
                    ResponseResult::Error { error } => Err(error.clone().into()),
                    _ => Err(WaiverError::Unavailable.into()),
                };
            }
            self.lifecycle_packet(session, packet, verify)?;
        }
    }
}
