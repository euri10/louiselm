//! Zero-authority execution authorized by the authenticated Control broker.

use serde::{Deserialize, Serialize};

use super::{ErrorCode, ProtocolError, validate_identifier, validate_schema, validate_version};

/// Broker-only command schema; no identity, mounts, environment or grants are accepted.
pub const TOOL_EXECUTION_SCHEMA: &str = "louiselm.launch.tool-execution/1";
/// Maximum bytes returned from each output stream, after UTF-8 replacement.
pub const MAX_TOOL_OUTPUT_BYTES: usize = 4_096;

/// One broker-authorized command in the Session's workspace, with no capabilities.
///
/// The supervisor accepts this only over its authenticated broker connection.
/// `command` is data passed to a fixed shell inside separate confinement, never
/// a privileged command or a declaration of mounts, identity or environment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolExecutionRequest {
    /// Fixed schema.
    pub schema: String,
    /// Fixed protocol version.
    pub protocol_version: u32,
    /// Correlation identifier, never a filesystem path.
    pub request_id: String,
    /// Authorized Session.
    pub session_id: String,
    /// Owning Run.
    pub run_id: String,
    /// Current envelope revision.
    pub envelope_revision: u64,
    /// Strictly increasing execution number, starting at one; never replayed.
    pub sequence: u64,
    /// Shell input, bounded to 16 KiB and never logged.
    pub command: String,
    /// Execution deadline, between one millisecond and thirty seconds.
    pub timeout_ms: u32,
}

impl ToolExecutionRequest {
    /// Checks the closed request's bounds before any execution.
    ///
    /// # Errors
    /// Refuses malformed identifiers, schema/version, empty/NUL commands or bounds.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.canonical_bytes().len() > super::MAX_PROTOCOL_MESSAGE_BYTES {
            return Err(ProtocolError::new(ErrorCode::MessageTooLarge, None, None));
        }
        validate_schema(&self.schema, TOOL_EXECUTION_SCHEMA)?;
        validate_version(self.protocol_version)?;
        for id in [&self.request_id, &self.session_id, &self.run_id] {
            validate_identifier(id)?;
        }
        if self.sequence == 0
            || self.command.is_empty()
            || self.command.len() > 16_384
            || self.command.contains('\0')
            || !(1..=30_000).contains(&self.timeout_ms)
        {
            return Err(ProtocolError::new(ErrorCode::InvalidRequest, None, None));
        }
        Ok(())
    }

    /// Returns deterministic wire bytes.
    ///
    /// # Panics
    /// Only a future fallible custom serializer could make this derived schema fail.
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "Derived JSON-native fields have no fallible custom serializer."
    )]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("tool request is serializable")
    }
}

/// Bounded untrusted tool output, returned only after descendant cleanup.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolExecutionResult {
    /// Exit status; negative values represent signals.
    pub exit_code: i32,
    /// Bounded stdout, decoded lossily as UTF-8. Never trusted runtime state.
    pub stdout: String,
    /// Bounded stderr, decoded lossily as UTF-8. Never logged by the supervisor.
    pub stderr: String,
    /// Whether either stream exceeded its returned limit.
    pub truncated: bool,
    /// Whether the deadline ended the tool tree.
    pub timed_out: bool,
}

impl ToolExecutionResult {
    pub(super) fn validate(&self) -> Result<(), ProtocolError> {
        if self.stdout.len() > MAX_TOOL_OUTPUT_BYTES || self.stderr.len() > MAX_TOOL_OUTPUT_BYTES {
            return Err(ProtocolError::new(ErrorCode::MessageTooLarge, None, None));
        }
        Ok(())
    }
}
