//! Operator-approved extension of a held Run's Provider budget
//! (`louiselm-qbr.5.1.3.2.3.3`).
//!
//! A Provider budget hold is lifted only by an explicit extension from the
//! Session's controller, for an interactive Session. Extensions are durable and
//! append-only: each one lifts exactly the hold it was approved against, adds
//! units to the Run's total and may move the Provider permission's expiry later,
//! never past the launch's own expiry. An extension never resumes a Session;
//! the operator Resumes separately.

use serde::{Deserialize, Serialize};

use super::provider_requests::ProviderHold;

/// Wire schema of a successful extension outcome.
pub const OUTCOME_SCHEMA: &str = "louiselm.provider-extension-outcome/1";

/// One operator extension, retried with the same `request_id` and fields.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionRequest {
    /// Operator-chosen retry identity; reusing it with other fields conflicts.
    pub request_id: String,
    /// Units added to the Run's shared total.
    pub additional_requests: u32,
    /// New exclusive Provider permission expiry, when moved later.
    pub expires_at_ms: Option<u64>,
}

/// Durable extension record; also the idempotent answer to a retry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Extension {
    /// Extended Run.
    pub run_id: String,
    /// The approved request.
    pub request: ExtensionRequest,
    /// Authenticated controller UID that approved it.
    pub operator_uid: u32,
    /// Approval time.
    pub approved_at_ms: u64,
    /// The exact hold this extension lifted.
    pub lifts: ProviderHold,
}

/// What the Run may spend after an extension.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionOutcome {
    /// Always [`OUTCOME_SCHEMA`].
    pub schema: String,
    /// Session the operator addressed.
    pub session_id: String,
    /// The recorded extension.
    pub extension: Extension,
    /// Run total after every extension so far.
    pub total_requests: u32,
    /// Units already spent, including unknown outcomes.
    pub spent: u32,
    /// Effective exclusive Provider permission expiry.
    pub expires_at_ms: u64,
}

/// Stable refusal at the operator extension boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionError {
    /// Malformed operator input.
    #[error("invalid Provider extension request")]
    InvalidRequest,
    /// Only the Session's authenticated controller may extend.
    #[error("Provider extension operator does not match")]
    WrongOperator,
    /// Unattended Runs cannot be extended.
    #[error("unattended Runs cannot be extended")]
    Unattended,
    /// The Session has no Provider permission, or its Run is not held.
    #[error("Run has no Provider budget hold to lift")]
    NotHeld,
    /// The extension would not lift the hold's cause: no added units for an
    /// exhausted Run, or no later expiry for an expired one.
    #[error("extension does not cover the hold")]
    Insufficient,
    /// The expiry is not later than now, or passes the launch's own expiry.
    #[error("extension expiry is out of range")]
    ExpiryOutOfRange,
    /// A retry identity was reused with different input.
    #[error("extension retry identity conflicts")]
    Conflict,
    /// No live Session matches.
    #[error("extension subject is unknown")]
    Unknown,
    /// Trusted state could not be read or persisted.
    #[error("extension state is unavailable")]
    Unavailable,
}

impl ExtensionError {
    /// Fixed machine-readable recovery action, never external prose.
    #[must_use]
    pub const fn next_action(self) -> &'static str {
        match self {
            Self::InvalidRequest | Self::Conflict => "check_request",
            Self::WrongOperator => "use_configured_operator",
            Self::Unattended => "relaunch_attended",
            Self::NotHeld => "inspect_session",
            Self::Insufficient | Self::ExpiryOutOfRange => "extend_further",
            Self::Unknown => "check_session",
            Self::Unavailable => "inspect_broker_and_session",
        }
    }
}
