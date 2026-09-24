//! Pending launch decisions serialize with authorization consumption.
use super::{Context, Outcome, Request, WaiverError};
use crate::{
    Digest,
    broker::{BrokerError, BrokerService},
    conformance::{admission::Attendance, preparation::Preparation},
};

impl BrokerService {
    /// Inspect or revoke a pending decision without renewing its observation.
    /// Expired preparation and grant history remain inspectable and revocable.
    /// # Errors
    /// Refuses approval, foreign operators, consumed grants and unreadable history.
    pub fn pending_waiver_history(
        &self,
        operator_uid: u32,
        session_id: &str,
        request: &Request,
        now_ms: u64,
    ) -> Result<Outcome, BrokerError> {
        if !matches!(
            request,
            Request::Inspect | Request::Result { .. } | Request::Revoke { .. }
        ) {
            return Err(WaiverError::InvalidRequest.into());
        }
        self.authorizations().with_pending(session_id, |pending| {
            if pending.controller_uid != operator_uid {
                return Err(WaiverError::WrongOperator.into());
            }
            if pending.conformance.attendance != Attendance::Interactive {
                return Err(WaiverError::Unattended.into());
            }
            let context = Context {
                preparation: None,
                session_id: pending.session_id.clone(),
                run_id: pending.run_id.clone(),
                authorization_id: pending.authorization_id.clone(),
                request_digest: pending.request_digest.clone(),
                envelope_revision: pending.envelope_revision,
                operator_uid,
                attendance: pending.conformance.attendance,
                condition: crate::conformance::admission::Condition::Missing,
                receipt_head: String::new(),
            };
            let mut outcome = self.waivers.control(&context, request, now_ms)?;
            outcome.active &= now_ms < pending.expires_at_ms;
            Ok(outcome)
        })
    }

    /// Apply the authenticated operator flow to a trusted root-owned preparation.
    /// The installed adapter must read the observation from protected storage;
    /// accepting an operator- or Agent-supplied observation grants no such authority.
    /// No process or receipt chain is created here. Blocking private storage I/O
    /// is serialized with single-use launch consumption.
    /// # Errors
    /// Refuses foreign, expired, unattended, consumed, mismatched or stale launches.
    pub fn pre_admission_waiver(
        &self,
        operator_uid: u32,
        observation: &Preparation,
        request: &Request,
        now_ms: u64,
    ) -> Result<Outcome, BrokerError> {
        self.authorizations()
            .with_pending(&observation.session_id, |pending| {
                if pending.controller_uid != operator_uid
                    || observation.operator_uid != operator_uid
                {
                    return Err(WaiverError::WrongOperator.into());
                }
                if pending.conformance.attendance != Attendance::Interactive {
                    return Err(WaiverError::Unattended.into());
                }
                if pending.request_digest != observation.request_digest {
                    return Err(WaiverError::StalePlan.into());
                }
                if now_ms >= pending.expires_at_ms {
                    return Err(WaiverError::Expired.into());
                }
                observation
                    .validate(now_ms)
                    .map_err(|_| WaiverError::NotWaivable)?;
                let context = Context {
                    preparation: Some(observation.clone()),
                    session_id: pending.session_id.clone(),
                    run_id: pending.run_id.clone(),
                    authorization_id: pending.authorization_id.clone(),
                    request_digest: pending.request_digest.clone(),
                    envelope_revision: pending.envelope_revision,
                    operator_uid,
                    attendance: pending.conformance.attendance,
                    condition: observation.condition,
                    receipt_head: Digest::of(
                        &serde_json::to_vec(observation).map_err(|_| WaiverError::Unavailable)?,
                    )
                    .to_string(),
                };
                self.waivers
                    .control(&context, request, now_ms)
                    .map_err(Into::into)
            })
    }
}
