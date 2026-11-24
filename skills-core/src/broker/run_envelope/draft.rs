//! Non-executing intent and one-time conversion to a fixed Run deadline.

use serde::{Deserialize, Serialize};

use super::{BrokerError, RunAuthorization, RunEnvelope, RunEnvelopeStore};
use crate::Digest;

/// Reviewed execution time; waiting for Start spends none of a duration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExecutionLimit {
    /// Relative allowance, activated after current readiness checks.
    Duration {
        /// Positive finite execution allowance in milliseconds.
        duration_ms: u64,
    },
    /// Explicit operator-selected absolute cutoff, never shifted.
    StopBy {
        /// Exclusive Unix millisecond cutoff.
        expires_at_ms: u64,
    },
}

impl Default for ExecutionLimit {
    fn default() -> Self {
        Self::Duration {
            duration_ms: 1_800_000,
        }
    }
}

/// Persistable reviewed intent, deliberately invalid as execution authority.
/// All envelope and capability expiries must be zero; dispatch sets them once.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunDraft {
    /// Closed versioned intent schema.
    pub schema: String,
    /// Exact selected policy, with zero expiry placeholders and revision one.
    pub envelope: RunEnvelope,
    /// Defaults to thirty minutes of execution after dispatch.
    #[serde(default)]
    pub execution: ExecutionLimit,
}

impl RunDraft {
    /// Binds the complete reviewed policy and time selection.
    /// # Errors
    /// Returns invalid grant if canonical serialization fails.
    pub fn digest(&self) -> Result<Digest, BrokerError> {
        serde_json::to_vec(self)
            .map(|bytes| Digest::of(&bytes))
            .map_err(|_| BrokerError::InvalidGrant)
    }

    /// Materializes authority without persisting or consuming anything.
    /// Call only after readiness; inspection and reopening must not call this.
    /// # Errors
    /// Refuses active templates, invalid policy, overflow or an elapsed cutoff.
    pub fn activate(&self, now_ms: u64) -> Result<RunEnvelope, BrokerError> {
        if self.schema != "louiselm.broker.run-draft/1"
            || self.envelope.envelope_revision != 1
            || self.envelope.expires_at_ms != 0
            || self
                .envelope
                .bead_scope
                .as_ref()
                .is_some_and(|scope| scope.expires_at_ms != 0)
            || self
                .envelope
                .provider_requests
                .as_ref()
                .is_some_and(|scope| scope.expires_at_ms != 0)
            || self
                .envelope
                .skill_requests
                .as_ref()
                .is_some_and(|scope| scope.expires_at_ms != 0)
            || self
                .envelope
                .commands
                .as_ref()
                .is_some_and(|scope| scope.expires_at_ms != 0)
        {
            return Err(BrokerError::InvalidGrant);
        }
        let expiry = match self.execution {
            ExecutionLimit::Duration { duration_ms: 0 } => return Err(BrokerError::InvalidGrant),
            ExecutionLimit::Duration { duration_ms } => now_ms
                .checked_add(duration_ms)
                .ok_or(BrokerError::InvalidGrant)?,
            ExecutionLimit::StopBy { expires_at_ms } => expires_at_ms,
        };
        let mut envelope = self.envelope.clone();
        envelope.expires_at_ms = expiry;
        if let Some(scope) = envelope.bead_scope.as_mut() {
            scope.expires_at_ms = expiry;
        }
        if let Some(scope) = envelope.provider_requests.as_mut() {
            scope.expires_at_ms = expiry;
        }
        if let Some(scope) = envelope.skill_requests.as_mut() {
            scope.expires_at_ms = expiry;
        }
        if let Some(scope) = envelope.commands.as_mut() {
            scope.expires_at_ms = expiry;
        }
        envelope.validate(now_ms)?;
        Ok(envelope)
    }
}

/// Exact activated authority returned after one durable dispatch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunDispatch {
    /// Closed dispatch response schema.
    pub schema: String,
    /// Digest of the reviewed, non-executing intent.
    pub draft_digest: String,
    /// Broker clock sampled after readiness, under the Run approval lock.
    pub dispatched_at_ms: u64,
    /// Fixed authority; retries, Park and resume retain these dates and caps.
    pub envelope: RunEnvelope,
    /// Approval of the exact materialized envelope.
    pub authorization: RunAuthorization,
}

impl RunEnvelopeStore {
    /// Checks readiness and dispatches one draft, holding the approval lock.
    /// The clock is sampled after the guard; a failed check creates no authority.
    /// A duplicate Start, including after restart or a lost reply, is refused.
    /// # Errors
    /// Refuses reused Run IDs, invalid intent, failed readiness or storage errors.
    pub fn dispatch_with(
        &self,
        draft: &RunDraft,
        clock: impl FnOnce() -> Result<u64, BrokerError>,
        ready: impl FnOnce() -> Result<(), BrokerError>,
    ) -> Result<RunDispatch, BrokerError> {
        let _serial = super::lock(&self.serial);
        if self.current(&draft.envelope.run_id)?.is_some() {
            return Err(BrokerError::InvalidGrant);
        }
        ready()?;
        let dispatched_at_ms = clock()?;
        let envelope = draft.activate(dispatched_at_ms)?;
        let authorization = self.write_approval(&envelope)?;
        Ok(RunDispatch {
            schema: "louiselm.broker.run-dispatch/1".into(),
            draft_digest: draft.digest()?.to_string(),
            dispatched_at_ms,
            envelope,
            authorization,
        })
    }
}
