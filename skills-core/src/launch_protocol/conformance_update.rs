//! Ordered current facts sent only by the authenticated owning supervisor.

use super::{ErrorCode, LaunchAuthorization, ProtocolError, validate_digest, validate_identifier};
use crate::{conformance::admission::Condition, launch_receipt::ConformanceEvidence};
use serde::{Deserialize, Serialize};

/// Schema for current host evidence; it never replaces admission history.
pub const CONFORMANCE_UPDATE_SCHEMA: &str = "louiselm.launch.conformance-update/2";
/// Maximum age of a successful supervisor check, in milliseconds.
pub const CONFORMANCE_FRESHNESS_MS: u64 = 5_000;

/// Sanitized reason a required validity check does not establish current evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConformanceFailure {
    /// Protected measurements or failure history could not be read.
    Unavailable,
    /// Five seconds passed without a successful check.
    Deadline,
    /// The current host condition has no applicable authorization.
    Condition(Condition),
}

/// A check's validated result, not an Agent-authored posture verdict.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ConformanceCheck {
    /// Fresh trusted inspection accepted the exact evidence or waiver.
    Current {
        /// Actual evidence inspected, never unevaluated.
        evidence: ConformanceEvidence,
    },
    /// Current inspection or its independent freshness deadline failed.
    Invalid {
        /// Closed diagnostic; no host paths or probe payloads.
        failure: ConformanceFailure,
    },
}

/// Closed, bounded, ordered facts from one lifetime-pinned supervisor.
/// Parsing proves only shape; the broker must authenticate its producer and
/// validate the exact retained authorization before accepting these facts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConformanceUpdate {
    /// Must be [`CONFORMANCE_UPDATE_SCHEMA`].
    pub schema: String,
    /// Owning Session, never a replacement.
    pub session_id: String,
    /// Owning Run.
    pub run_id: String,
    /// Exact original launch authorization.
    pub authorization_id: String,
    /// Exact launch request, binding the remaining approved inputs.
    pub request_digest: String,
    /// Current authorized envelope revision.
    pub envelope_revision: u64,
    /// Strictly increasing within this supervisor lifetime.
    pub sequence: u64,
    /// Current broker waiver decision revision, independent of immutable admission.
    pub waiver_revision: u64,
    /// Check input time, or failure observation time; transport never renews it.
    pub observed_at_ms: u64,
    /// Most recent successful validity check, preserved across failures.
    pub last_success_at_ms: Option<u64>,
    /// Capability authority remains suspended pending an explicit successful Resume.
    pub suspended: bool,
    /// Actual current result.
    pub check: ConformanceCheck,
}

impl ConformanceUpdate {
    /// Validate bounded shape without granting evidence authority.
    /// # Errors
    /// Refuses contradictory chronology, empty bindings and unevaluated success.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        let invalid = || ProtocolError::new(ErrorCode::InvalidRequest, None, None);
        if self.schema != CONFORMANCE_UPDATE_SCHEMA
            || self.sequence == 0
            || self.envelope_revision == 0
        {
            return Err(invalid());
        }
        for id in [&self.session_id, &self.run_id, &self.authorization_id] {
            validate_identifier(id)?;
        }
        validate_digest(&self.request_digest)?;
        if self
            .last_success_at_ms
            .is_some_and(|at| at > self.observed_at_ms)
        {
            return Err(invalid());
        }
        if matches!(self.check, ConformanceCheck::Invalid { .. }) && !self.suspended {
            return Err(invalid());
        }
        if let ConformanceCheck::Current { evidence } = &self.check {
            super::conformance::validate_admission_history(evidence)?;
            if *evidence == ConformanceEvidence::Unevaluated
                || self.last_success_at_ms != Some(self.observed_at_ms)
            {
                return Err(invalid());
            }
        }
        Ok(())
    }

    /// Validate the exact retained Session authority, without renewing a waiver.
    /// # Errors
    /// Refuses foreign bindings and success under an expired or different waiver.
    pub fn validate_for(&self, authorization: &LaunchAuthorization) -> Result<(), ProtocolError> {
        self.validate()?;
        let invalid = || ProtocolError::new(ErrorCode::SubjectMismatch, None, None);
        if self.session_id != authorization.session_id
            || self.run_id != authorization.run_id
            || self.authorization_id != authorization.authorization_id
            || self.request_digest != authorization.request_digest
            || self.envelope_revision != authorization.envelope_revision
        {
            return Err(invalid());
        }
        if let ConformanceCheck::Current {
            evidence: ConformanceEvidence::Waived { condition, .. },
        } = &self.check
        {
            authorization.conformance.validate_for(
                &self.session_id,
                &self.request_digest,
                authorization.controller_uid,
                self.observed_at_ms,
            )?;
            if authorization
                .conformance
                .waiver
                .as_ref()
                .is_none_or(|waiver| waiver.condition != *condition)
            {
                return Err(invalid());
            }
        }
        Ok(())
    }

    /// Serialize exact canonical wire bytes.
    /// # Errors
    /// Refuses invalid fields or serialization failure.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ProtocolError> {
        self.validate()?;
        serde_json::to_vec(self)
            .map_err(|_| ProtocolError::new(ErrorCode::InvalidRequest, None, None))
    }

    /// Parse bounded canonical wire bytes; this does not authenticate the sender.
    /// # Errors
    /// Refuses unknown fields, malformed/noncanonical bytes and invalid records.
    pub fn parse_canonical(bytes: &[u8]) -> Result<Self, ProtocolError> {
        let invalid = || ProtocolError::new(ErrorCode::InvalidRequest, None, None);
        if bytes.len() > 4096 {
            return Err(invalid());
        }
        let value: Self = serde_json::from_slice(bytes).map_err(|_| invalid())?;
        if value.canonical_bytes()? != bytes {
            return Err(invalid());
        }
        Ok(value)
    }
}
