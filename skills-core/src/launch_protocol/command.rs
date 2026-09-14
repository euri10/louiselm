//! Closed command-decision records on the authenticated supervisor connection.

use serde::{Deserialize, Serialize};

use super::{
    ErrorCode, ProtocolError, ToolExecutionRequest, ToolExecutionResult, validate_digest,
    validate_identifier, validate_schema, validate_version,
};

/// Versioned command authorization, outcome and revocation exchange.
pub const COMMAND_SCHEMA: &str = "louiselm.launch.command/1";

/// Supervisor-authored attribution, never a process handle or Agent input.
///
/// The supervisor derives this from its retained kernel pin and each packets
/// credentials. The broker accepts it only over the authenticated supervisor
/// connection; it must never reconstruct a kernel pin from the numeric PID.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandPrincipal {
    /// Supervisor-owned channel identity for this principal lifetime.
    pub channel_id: String,
    /// Actual sender, in the supervisors host PID namespace.
    pub pid: u32,
    /// Actual sender UID, from kernel credentials.
    pub uid: u32,
    /// Actual sender GID, from kernel credentials.
    pub gid: u32,
}

impl CommandPrincipal {
    pub(super) fn validate(&self) -> Result<(), ProtocolError> {
        validate_identifier(&self.channel_id)?;
        if self.pid == 0 {
            return Err(invalid());
        }
        Ok(())
    }
}

/// Agent-requested attenuation of an existing operator approval.
/// No executable, process identity, mount, environment or network selection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrantRequest {
    /// Agent-local delegation sequence, starting at one.
    pub sequence: u64,
    /// Exact bounded shell input digest.
    pub command_digest: String,
    /// Maximum command duration in milliseconds.
    pub timeout_ms: u32,
    /// Optional non-refundable reservation (1..=64); omission requests no count quota.
    /// Only an uncapped parent may authorize an uncapped grant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uses: Option<u32>,
    /// Requested lifetime, anchored before forwarding to the broker.
    pub valid_for_ms: u32,
}

impl GrantRequest {
    /// Checks closed protocol bounds; the broker separately checks attenuation.
    ///
    /// # Errors
    /// Refuses malformed digest, zero sequence or out-of-range bounds.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_digest(&self.command_digest)?;
        if self.sequence == 0
            || !(1..=30_000).contains(&self.timeout_ms)
            || self.uses.is_some_and(|uses| !(1..=64).contains(&uses))
            || !(1..=30_000).contains(&self.valid_for_ms)
        {
            return Err(invalid());
        }
        Ok(())
    }
}

/// One actual or uncertain effect outcome; unknown never means safe to retry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CommandOutcome {
    /// Actual result after descendant cleanup, even if authority was revoked.
    Completed {
        /// Bounded untrusted output, never included in normalized audit.
        output: ToolExecutionResult,
    },
    /// Supervisor refused before crossing the actual start boundary.
    NotStarted {
        /// Stable reason, without commands or arbitrary error strings.
        error: ErrorCode,
    },
    /// Execution may have begun; a later authenticated result may resolve it.
    Unknown,
}

/// Direction-specific operations; each endpoint rejects operations it cannot own.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CommandOperation {
    /// Authenticated Agent asks for its own read-only canonical Session status.
    StatusRequest {},
    /// Broker-derived self status; never carries lifecycle authority.
    StatusResult {
        /// Canonical, redacted status for this exact message subject.
        status: Box<super::SessionStatus>,
    },
    /// Read-only refusal without subject existence or operator evidence.
    StatusRefused {
        /// Stable refusal; no mechanical state or receipt position is disclosed.
        error: ErrorCode,
    },
    /// Agent asks the supervisor to create the fixed measured isolated helper.
    Delegate {
        /// Attenuation request; never an operator approval.
        grant: GrantRequest,
        /// Initial bounded work for the fixed measured helper, not permission to execute.
        command: ToolExecutionRequest,
    },
    /// Supervisor-authenticated Agent request and separately pinned isolated tool.
    DelegationRequest {
        /// Actual authenticated requesting Agent.
        principal: CommandPrincipal,
        /// Trusted launch evidence, conveyed only by the supervisor.
        tool: CommandPrincipal,
        /// Original bounded Agent request.
        grant: GrantRequest,
    },
    /// Broker has durably reserved this grant's aggregate budget.
    Granted {
        /// Exact isolated tool lifetime and channel.
        tool: CommandPrincipal,
        /// Original Agent delegation sequence.
        grant: u64,
        /// Remaining lifetime, anchored before the original forwarding.
        valid_for_ms: u32,
    },
    /// Broker has closed this grant; other principals retain their authority.
    RevokeGrant {
        /// Exact grant to enforce.
        grant: u64,
    },
    /// Supervisor confirms the named grant's enforcement and tree termination.
    GrantRevoked {
        /// Exact grant that was stopped.
        grant: u64,
        /// False means cleanup is unproven and identity remains poisoned.
        enforced: bool,
    },
    /// Supervisor-to-Agent result. Unknown does not authorize a retry.
    Result {
        /// Actual, refused-before-start, or uncertain outcome.
        outcome: CommandOutcome,
    },
    /// Supervisor forwards a packet it authenticated against the actual Agent.
    Request {
        /// Supervisor-owned per-packet attribution.
        principal: CommandPrincipal,
        /// Exact bounded request; its sequence is principal-local.
        command: ToolExecutionRequest,
    },
    /// Broker has durably spent this single-use authorization and its budget.
    Authorize {
        /// Exact authenticated principal that requested the command.
        principal: CommandPrincipal,
        /// Original principal-local sequence.
        principal_sequence: u64,
        /// Independent, monotonically increasing Session-wide effect sequence.
        dispatch_sequence: u64,
        /// Exact command bytes; this digest is not an interpretation of shell input.
        command_digest: String,
        /// Maximum execution timeout from the approved request.
        timeout_ms: u32,
        /// Remaining validity, anchored by the supervisor BEFORE forwarding the
        /// request, not on receipt of this reply. Transport delay only narrows it.
        valid_for_ms: u32,
    },
    /// Broker refused; no execution authorization exists.
    Reject {
        /// Stable normalized refusal.
        error: ErrorCode,
    },
    /// Supervisor reports an outcome for an already-spent authorization.
    Outcome {
        /// Session-wide effect sequence, not a new request number.
        dispatch_sequence: u64,
        /// Actual result or explicit uncertainty.
        outcome: CommandOutcome,
    },
    /// Broker durably recorded the normalized outcome.
    OutcomeAcknowledged {
        /// Exactly the effect whose outcome became durable.
        dispatch_sequence: u64,
    },
    /// Broker has stopped approvals; supervisor must block starts and cancel work.
    Revoke,
    /// Supervisor has settled cancellation; failure never acknowledges enforcement.
    Revoked {
        /// True only after queued starts are denied and running descendants ended.
        enforced: bool,
    },
}

/// Correlated, closed command message for one immutable Session/Run/revision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandMessage {
    /// Fixed schema.
    pub schema: String,
    /// Fixed protocol version.
    pub protocol_version: u32,
    /// Correlation ID, never a path or authority by itself.
    pub request_id: String,
    /// Exact Session.
    pub session_id: String,
    /// Owning Run.
    pub run_id: String,
    /// Exact operator-approved envelope revision.
    pub envelope_revision: u64,
    /// Request, decision, result or revocation operation.
    pub operation: CommandOperation,
}

impl CommandMessage {
    /// Checks bounds and contradictory subjects before any state transition.
    ///
    /// # Errors
    /// Rejects malformed schema, identifiers, bounds, digest or nested context.
    #[expect(
        clippy::too_many_lines,
        reason = "One closed message validates every operation and its shared Session context before state changes."
    )]
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_schema(&self.schema, COMMAND_SCHEMA)?;
        validate_version(self.protocol_version)?;
        for id in [&self.request_id, &self.session_id, &self.run_id] {
            validate_identifier(id)?;
        }
        match &self.operation {
            CommandOperation::StatusResult { status } => {
                status.validate()?;
                if status.session_id != self.session_id
                    || status.run_id != self.run_id
                    || status.envelope_revision != self.envelope_revision
                    || !status.allowed_actions.is_empty()
                {
                    return Err(invalid());
                }
            }
            CommandOperation::Delegate { grant, command } => {
                grant.validate()?;
                command.validate()?;
                if command.session_id != self.session_id
                    || command.run_id != self.run_id
                    || command.envelope_revision != self.envelope_revision
                    || command.sequence != 1
                {
                    return Err(invalid());
                }
            }
            CommandOperation::DelegationRequest {
                principal,
                tool,
                grant,
            } => {
                principal.validate()?;
                tool.validate()?;
                grant.validate()?;
                if principal.pid == tool.pid || principal.channel_id == tool.channel_id {
                    return Err(invalid());
                }
            }
            CommandOperation::Granted {
                tool,
                grant,
                valid_for_ms,
            } => {
                tool.validate()?;
                if *grant == 0 || !(1..=30_000).contains(valid_for_ms) {
                    return Err(invalid());
                }
            }
            CommandOperation::RevokeGrant { grant }
            | CommandOperation::GrantRevoked { grant, .. } => {
                if *grant == 0 {
                    return Err(invalid());
                }
            }
            CommandOperation::Result {
                outcome: CommandOutcome::Completed { output },
            } => output.validate()?,
            CommandOperation::Request { principal, command } => {
                principal.validate()?;
                command.validate()?;
                if command.request_id != self.request_id
                    || command.session_id != self.session_id
                    || command.run_id != self.run_id
                    || command.envelope_revision != self.envelope_revision
                {
                    return Err(invalid());
                }
            }
            CommandOperation::Authorize {
                principal,
                principal_sequence,
                dispatch_sequence,
                command_digest,
                timeout_ms,
                valid_for_ms,
            } => {
                principal.validate()?;
                validate_digest(command_digest)?;
                if *principal_sequence == 0
                    || *dispatch_sequence == 0
                    || !(1..=30_000).contains(timeout_ms)
                    || !(1..=30_000).contains(valid_for_ms)
                {
                    return Err(invalid());
                }
            }
            CommandOperation::Outcome {
                dispatch_sequence,
                outcome,
            } => {
                if *dispatch_sequence == 0 {
                    return Err(invalid());
                }
                if let CommandOutcome::Completed { output } = outcome {
                    output.validate()?;
                }
            }
            CommandOperation::OutcomeAcknowledged { dispatch_sequence }
                if *dispatch_sequence == 0 =>
            {
                return Err(invalid());
            }
            CommandOperation::Result { .. }
            | CommandOperation::StatusRequest {}
            | CommandOperation::StatusRefused { .. }
            | CommandOperation::Reject { .. }
            | CommandOperation::OutcomeAcknowledged { .. }
            | CommandOperation::Revoke
            | CommandOperation::Revoked { .. } => {}
        }
        if self.canonical_bytes().len() > super::MAX_PROTOCOL_MESSAGE_BYTES {
            return Err(ProtocolError::new(ErrorCode::MessageTooLarge, None, None));
        }
        Ok(())
    }

    /// Deterministic JSON encoding.
    ///
    /// # Panics
    /// Only a future fallible custom serializer could make this derived schema fail.
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "Derived JSON-native fields have no fallible serializer."
    )]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("command message is serializable")
    }
}

fn invalid() -> ProtocolError {
    ProtocolError::new(ErrorCode::InvalidRequest, None, None)
}
