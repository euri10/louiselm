//! Exact interactive host-conformance exceptions, owned by the Control broker.

use crate::conformance::admission::{Attendance, Condition};
use serde::{Deserialize, Serialize};

mod pre_admission;
mod service;
mod store;
pub(super) use store::Waivers;

/// Exact review artifact. Applying names its digest, never edited review fields.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    /// Closed artifact schema.
    pub schema: String,
    /// Owning Session.
    pub session_id: String,
    /// Owning Run.
    pub run_id: String,
    /// Immutable capability-envelope revision.
    pub envelope_revision: u64,
    /// Authenticated operator creating the proposal.
    pub operator_uid: u32,
    /// Exact proposed exception and private explanation.
    pub proposal: Proposal,
    /// Digest of the bound preview, including the prior decision and launch identity.
    pub digest: String,
}

/// Immutable evidence of an explicit approval; not proof of present applicability.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    /// Exact artifact the operator approved.
    pub plan: Plan,
    /// Original approval time, never refreshed on retry.
    pub approved_at_ms: u64,
    /// Canonical receipt digest used by the enforcing source.
    pub digest: String,
}

/// Explicit operator request; no Agent channel accepts this type.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    /// Inspect the current decision without renewing it.
    Inspect,
    /// Create an exact review artifact, granting no authority.
    Plan {
        /// Proposed condition, explanation and exclusive expiry.
        proposal: Proposal,
    },
    /// Approve precisely the previously returned preview.
    Apply {
        /// Digest returned by planning.
        plan_digest: String,
    },
    /// Withdraw precisely the named approval; retries cannot withdraw its replacement.
    Revoke {
        /// Canonical receipt to revoke.
        receipt_digest: String,
    },
    /// Retrieve the immutable outcome of an earlier preview.
    Result {
        /// Digest returned by planning.
        plan_digest: String,
    },
}

/// Bounded operator result. Approval alone never resumes a suspended Session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Outcome {
    /// Closed result schema.
    pub schema: String,
    /// Exact subject.
    pub session_id: String,
    /// Preview, where requested.
    pub plan: Option<Plan>,
    /// Immutable approval, if one exists.
    pub receipt: Option<Receipt>,
    /// Whether this remains the unrevoked and unexpired broker decision.
    /// Mechanical applicability still requires the supervisor's fresh check.
    pub active: bool,
}

/// An operator's proposed exception. It grants nothing until its preview is applied.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Proposal {
    /// Retry identity, unique within this Session.
    pub request_id: String,
    /// Exact unavailable-evidence condition; never a containment failure.
    pub condition: Condition,
    /// Operator explanation, retained privately and never projected into Attention.
    pub rationale: String,
    /// Exclusive absolute expiry in Unix milliseconds.
    pub expires_at_ms: u64,
}

/// Stable refusal at the operator-waiver boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum WaiverError {
    /// Malformed or oversized operator input.
    #[error("invalid waiver request")]
    InvalidRequest,
    /// Only the authenticated owning operator may decide.
    #[error("waiver operator does not match")]
    WrongOperator,
    /// Unattended Runs cannot use exceptions.
    #[error("unattended Runs cannot waive conformance")]
    Unattended,
    /// Required controls, unreadable evidence and containment failures cannot be waived.
    #[error("current failure cannot be waived")]
    NotWaivable,
    /// The exact reviewed Session state or condition changed.
    #[error("waiver preview is stale")]
    StalePlan,
    /// A retry identity was reused with different input.
    #[error("waiver retry identity conflicts")]
    Conflict,
    /// The exclusive expiry has passed.
    #[error("waiver has expired")]
    Expired,
    /// No live Session or known operation matches.
    #[error("waiver subject or operation is unknown")]
    Unknown,
    /// Trusted state could not be read, persisted or acknowledged.
    #[error("waiver state is unavailable")]
    Unavailable,
}

impl WaiverError {
    /// Fixed machine-readable recovery action, never external prose.
    #[must_use]
    pub const fn next_action(self) -> &'static str {
        match self {
            Self::StalePlan | Self::Expired => "inspect_and_plan_again",
            Self::Conflict | Self::InvalidRequest => "check_request",
            Self::WrongOperator => "use_configured_operator",
            Self::Unattended | Self::NotWaivable => "restore_conformance",
            Self::Unknown => "check_session_or_operation",
            Self::Unavailable => "inspect_broker_and_session",
        }
    }
}

/// Authenticated facts, constructed only by the broker's owning Session worker.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Context {
    preparation: Option<crate::conformance::preparation::Preparation>,
    session_id: String,
    run_id: String,
    authorization_id: String,
    request_digest: String,
    envelope_revision: u64,
    operator_uid: u32,
    attendance: Attendance,
    condition: Condition,
    receipt_head: String,
}

fn validate_plan(context: &Context, proposal: &Proposal, now_ms: u64) -> Result<(), WaiverError> {
    if context.attendance != Attendance::Interactive {
        return Err(WaiverError::Unattended);
    }
    if context.condition == Condition::ContainmentFailure {
        return Err(WaiverError::NotWaivable);
    }
    if !super::is_record_identifier(&proposal.request_id)
        || proposal.rationale.trim().is_empty()
        || proposal.rationale.len() > 1024
        || proposal.rationale.chars().any(char::is_control)
    {
        return Err(WaiverError::InvalidRequest);
    }
    if proposal.condition != context.condition {
        return Err(WaiverError::StalePlan);
    }
    if proposal.expires_at_ms <= now_ms {
        return Err(WaiverError::Expired);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
