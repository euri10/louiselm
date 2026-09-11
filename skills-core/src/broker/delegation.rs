//! Explicit, process-bound delegation of the existing isolated command effect.
//!
//! The launch owner supplies authenticated process/channel pairs and an already
//! approved policy. Peer requests cannot construct that policy. This component
//! does not prove tool isolation or install a broker; the production launch
//! integration must supply the supervisor's isolation and process proofs.

mod state;

use std::{sync::Arc, time::Instant};

use thiserror::Error;

use crate::{
    Digest,
    launch_protocol::{ToolExecutionRequest, ToolExecutionResult},
    launch_transport::{KernelProcess, SeqpacketChannel},
};

pub use state::{CommittedEffect, DelegatedTool, PendingEffect, ToolDelegation};

/// One exact command with a bounded timeout and total invocation budget.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandScope {
    /// Digest of exact shell input; neither that input nor its output is audited.
    pub command_digest: Digest,
    /// Maximum timeout in milliseconds, at most the existing protocol's 30 seconds.
    pub timeout_ms: u32,
    /// Total effects, including failed or abandoned admitted requests (1..=64).
    pub uses: u32,
}

impl CommandScope {
    /// Whether this valid scope grants no more than `parent`.
    #[must_use]
    pub fn is_within(&self, parent: &Self) -> bool {
        self.valid()
            && parent.valid()
            && self.command_digest == parent.command_digest
            && self.timeout_ms <= parent.timeout_ms
            && self.uses <= parent.uses
    }

    fn valid(&self) -> bool {
        (1..=30_000).contains(&self.timeout_ms) && (1..=64).contains(&self.uses)
    }

    fn permits(&self, request: &ToolExecutionRequest) -> bool {
        self.command_digest == Digest::of(request.command.as_bytes())
            && request.timeout_ms <= self.timeout_ms
    }
}

/// Trusted operator authorization after intersection with the active Run policy.
#[derive(Clone, Debug)]
pub struct DelegationPolicy {
    /// Durable launch authorization used for normalized attribution.
    pub authorization_id: String,
    /// Exact effect and aggregate budget available to this Agent lifetime.
    pub scope: CommandScope,
    /// Existing operator approval must explicitly permit delegation.
    pub allow_delegation: bool,
    /// Exclusive monotonic deadline. No grant or pending effect may outlive it.
    pub expires_at: Instant,
}

/// Agent request for one narrower tool grant, not an operator authorization.
#[derive(Clone, Debug)]
pub struct ToolGrantRequest {
    /// Exact Session to which the grant belongs.
    pub session_id: String,
    /// Exact owning Run.
    pub run_id: String,
    /// Revision of the already approved envelope.
    pub envelope_revision: u64,
    /// Strictly increasing grant request sequence starting at one.
    pub sequence: u64,
    /// Requested subset of the Agent's approved command and budget.
    pub scope: CommandScope,
    /// Exclusive deadline, no later than the operator authorization.
    pub expires_at: Instant,
}

/// A channel and kernel lifetime established by the trusted launch owner.
///
/// This type is not a wire record. Never construct it from a peer's PID or a
/// first connector. For tools, the owner must additionally prove the existing
/// enforced isolation boundary before offering the pair to the broker.
pub struct BoundProcess {
    /// Supervisor-authenticated process, retained through its kernel lifetime.
    pub process: Arc<KernelProcess>,
    /// Channel whose peer must match that exact process.
    pub channel: SeqpacketChannel,
}

/// One exactly-once asynchronous effect result. Output stays out of the audit.
pub type EffectCompletion =
    Box<dyn FnOnce(Result<ToolExecutionResult, DelegationError>) + Send + 'static>;

/// Adapter for the existing isolated command effect.
///
/// Preparation may run asynchronously. The worker must use
/// [`PendingEffect::commit`] immediately around the first irreversible action;
/// queuing a closure is preparation, not commitment. The permit cannot expose
/// an authorized request without that final check. After commitment, report the
/// actual outcome even if authority is revoked while completion is in flight.
pub trait ToolEffect: Send + Sync {
    /// Admits asynchronous preparation and eventually calls `complete` once.
    ///
    /// # Errors
    /// Returns an admission failure without calling `complete`; admitted work
    /// reports its outcome through `complete`, including commit-time refusal.
    fn execute(
        &self,
        effect: PendingEffect,
        complete: EffectCompletion,
    ) -> Result<(), DelegationError>;
}

/// Stable refusal or preserved internal cause at the delegation boundary.
#[derive(Debug, Error)]
pub enum DelegationError {
    /// Malformed or contradictory trusted context or peer request.
    #[error("invalid capability request")]
    InvalidRequest,
    /// Packet or connected peer does not match the authenticated principal.
    #[error("capability process identity mismatch")]
    IdentityMismatch,
    /// Operator authorization does not permit delegation.
    #[error("operator authorization does not permit delegation")]
    DelegationDenied,
    /// A request exceeds its exact scope, revision, subject or deadline ceiling.
    #[error("request exceeds its capability scope")]
    ScopeMismatch,
    /// A monotonic deadline has passed.
    #[error("capability has expired")]
    Expired,
    /// The Agent or tool lifetime/channel ended, or the owner revoked authority.
    #[error("capability has been revoked")]
    Revoked,
    /// A sequence was repeated or skipped.
    #[error("capability request sequence mismatch")]
    Replay,
    /// All approved invocations have already been reserved or spent.
    #[error("capability budget exhausted")]
    BudgetExhausted,
    /// The bounded effect adapter could not perform its operation.
    #[error("isolated effect failed")]
    EffectFailed,
    /// Kernel lifetime observation failed; authority is denied.
    #[error("capability identity observation failed")]
    IdentityUnavailable(#[source] std::io::Error),
    /// Durable audit failed; authority remains revoked and no new effect starts.
    #[error("capability audit unavailable")]
    Audit(#[from] super::BrokerError),
    /// The actual effect result is preserved even though its audit append failed.
    #[error("effect outcome was not durably recorded; the effect may have completed")]
    CompletionAudit {
        /// Actual adapter outcome; never turn this into an automatic retry.
        outcome: Box<Result<ToolExecutionResult, DelegationError>>,
        /// Internal storage cause, sanitized at presentation boundaries.
        #[source]
        source: super::BrokerError,
    },
    /// An internal worker failed while holding authority; the state stays closed.
    #[error("capability owner unavailable")]
    OwnerUnavailable,
}
