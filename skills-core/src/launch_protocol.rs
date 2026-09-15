//! Closed launch and lifecycle messages shared by the supervisor and broker.
//!
//! This module describes bytes and pure compare-and-swap decisions. It does
//! not own transport, authorization policy, persistence, or process mechanics.

use std::{collections::BTreeSet, fmt};

use serde::{Deserialize, Serialize};

mod command;
mod posture;
pub use posture::{
    DimensionStatus, EvidenceFreshness, FreshnessBasis, PostureStatus, StatusEvidence,
};
mod recovery;
pub(crate) use recovery::validate_reconstruction;
mod verification;
pub use verification::{
    VERIFICATION_SCHEMA, VerificationExecution, VerificationExport, VerificationOperation,
    VerificationRequest, VerificationStep,
};
mod tool;
pub use command::{
    COMMAND_SCHEMA, CommandMessage, CommandOperation, CommandOutcome, CommandPrincipal,
    GrantRequest,
};
pub use recovery::{
    RECOVERY_REQUEST_SCHEMA, RECOVERY_RESTORE_SCHEMA, RETENTION_EVIDENCE_SCHEMA, RecoveryReadiness,
    RecoveryRequest, RecoveryRestoreRequest, RecoveryUnavailableReason, RetentionEvidence,
    RetentionRequest,
};
pub use tool::{
    MAX_TOOL_OUTPUT_BYTES, TOOL_EXECUTION_SCHEMA, ToolExecutionRequest, ToolExecutionResult,
};

pub use crate::launch::PROTOCOL_VERSION;

use crate::{
    canonical::Digest,
    launch::{LaunchError, LaunchRequest, REQUEST_SCHEMA},
    launch_receipt::{
        Authorization, ProcessExitClassification, RECEIPT_SCHEMA, ReceiptAuthority, ReceiptError,
        ReceiptHead, ReceiptOutcome, ReceiptPayload, SessionState, SignedReceipt,
    },
};

pub use crate::launch::MAX_BROKER_LOSS_GRACE_MS;

/// Longest accepted encoded protocol message.
pub const MAX_PROTOCOL_MESSAGE_BYTES: usize = 64 * 1024;

/// Longest accepted opaque identifier.
pub const MAX_IDENTIFIER_BYTES: usize = 128;

/// Most receipt intents or exact envelopes retained while durability is unavailable.
pub const MAX_PENDING_RECEIPTS: u32 = 8;

/// Most cross-Session identity occupants disclosed on an operator-only result.
pub const MAX_IDENTITY_OCCUPANTS: usize = 32;

/// Schema for an authorized lifecycle mutation.
pub const LIFECYCLE_REQUEST_SCHEMA: &str = "louiselm.launch.lifecycle-request/1";

/// Schema for a read-only status request.
pub const STATUS_REQUEST_SCHEMA: &str = "louiselm.launch.status-request/1";

/// Schema for an exact durable receipt disposition.
pub const RECEIPT_ACK_SCHEMA: &str = "louiselm.launch.receipt-ack/2";

/// Schema for a broker-consumed single-use launch authorization.
pub const LAUNCH_AUTHORIZATION_SCHEMA: &str = "louiselm.launch.authorization/2";

/// Schema for exact receipt-head exchange during authenticated broker reattachment.
pub const BROKER_RECONNECT_SCHEMA: &str = "louiselm.launch.broker-reconnect/1";

/// Schema for the supervisor's exact controller-loss settlement request.
pub const CONTROLLER_LOSS_SETTLEMENT_SCHEMA: &str = "louiselm.launch.controller-loss-settlement/1";

/// Schema for a broker's durable controller-loss settlement acknowledgement.
pub const CONTROLLER_LOSS_ACK_SCHEMA: &str = "louiselm.launch.controller-loss-ack/1";

/// Schema for the mechanical supervisor status.
pub const SUPERVISOR_STATUS_SCHEMA: &str = "louiselm.launch.supervisor-status/3";

/// Schema for broker-composed canonical Session status.
pub const SESSION_STATUS_SCHEMA: &str = "louiselm.launch.session-status/5";

/// Schema for a response to a request.
pub const RESPONSE_SCHEMA: &str = "louiselm.launch.response/2";

/// An authorized public lifecycle mutation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleAction {
    /// Freeze the whole Session process tree.
    Park,
    /// Unfreeze a durably authorized parked Session.
    Resume,
    /// Deliver an interrupt to the Session workload.
    Interrupt,
    /// Terminate descendants and release Session resources.
    Disposal,
}

/// An operation visible while the supervisor is still completing it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PendingAction {
    /// Complete the two-receipt launch transaction.
    Launch,
    /// Park the Session.
    Park,
    /// Resume the Session.
    Resume,
    /// Interrupt the workload.
    Interrupt,
    /// Dispose the Session.
    Disposal,
}

impl From<LifecycleAction> for PendingAction {
    fn from(action: LifecycleAction) -> Self {
        match action {
            LifecycleAction::Park => Self::Park,
            LifecycleAction::Resume => Self::Resume,
            LifecycleAction::Interrupt => Self::Interrupt,
            LifecycleAction::Disposal => Self::Disposal,
        }
    }
}

/// Where an in-flight lifecycle operation is blocked.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PendingPhase {
    /// The narrowing or enabling mechanic is being applied.
    Applying,
    /// The resulting receipt is being signed.
    Signing,
    /// Completion remains blocked until the broker acknowledges durable bytes.
    AwaitingDurableAck,
}

/// One serialized operation in progress.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingOperation {
    /// Idempotency key for the operation.
    pub request_id: String,
    /// Operation being completed.
    pub action: PendingAction,
    /// Current completion boundary.
    pub phase: PendingPhase,
}

/// The supervisor's view of its broker connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrokerConnection {
    /// Authenticated broker is attached.
    Connected,
    /// A bounded broker-loss grace interval is active.
    Grace,
    /// The broker is absent and the Session is fail-closed.
    Disconnected,
    /// Receipt prefixes are being reconciled.
    Reconciling,
}

/// Aggregate state of the Session's capability channels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelState {
    /// Channels have not been enabled.
    Disabled,
    /// Authorized channels are live.
    Enabled,
    /// Previously enabled channels have been revoked.
    Revoked,
    /// Channels and their resources are terminally closed.
    Closed,
}

/// A non-authoritative status summary of the broker-owned Verified posture.
///
/// This deliberately is not [`crate::posture::Posture`]: deserializing a
/// status message must never manufacture trusted posture evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PostureSummary {
    /// Trusted posture has not been evaluated yet.
    Pending,
    /// Every required dimension has trusted evidence.
    FullyVerified,
    /// No dimension failed, but at least one was explicitly waived.
    Waived,
    /// At least one required dimension is not verified.
    Unverified,
}

/// Stable machine-readable protocol failure code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// Encoded message exceeded the fixed boundary.
    MessageTooLarge,
    /// Bytes were not one complete well-typed JSON message.
    MalformedMessage,
    /// Message named a schema this build does not implement.
    UnsupportedSchema,
    /// Message named a protocol version this build does not implement.
    UnsupportedVersion,
    /// A decoded field was malformed or contradictory.
    InvalidRequest,
    /// Request names a different Session or Run.
    SubjectMismatch,
    /// One request ID was reused for different canonical bytes.
    RequestIdConflict,
    /// Another serialized operation is still pending.
    OperationPending,
    /// Expected state no longer matches current state.
    StateMismatch,
    /// Expected receipt head no longer matches current head.
    ReceiptSequenceMismatch,
    /// Expected capability-envelope revision is stale.
    EnvelopeRevisionMismatch,
    /// The requested action is not valid from the current state.
    InvalidTransition,
    /// The authorized whole-tree lifecycle mechanic failed.
    LifecycleMechanicUnavailable,
    /// Receipt bytes or their chain do not verify.
    ReceiptChainInvalid,
    /// Installed administrative revocation invalidates this Session's signing key.
    SigningKeyRevoked,
    /// Receipt signing is temporarily unavailable.
    SigningUnavailable,
    /// Durable receipt storage is temporarily unavailable.
    DurabilityUnavailable,
    /// The Control broker is unavailable.
    BrokerUnavailable,
    /// No isolated host identity is currently free for a new Session.
    SessionIdentityExhausted,
    /// The broker assigned a slot that the installed launcher cannot safely use.
    IdentityAssignmentInvalid,
}

/// Stable recovery direction for a protocol failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NextAction {
    /// Retry the exact same request ID and bytes.
    RetrySameRequest,
    /// Read current canonical status before deciding again.
    RefreshStatus,
    /// Wait for the serialized operation to finish.
    Wait,
    /// Reattach the authenticated Control broker.
    ReconnectBroker,
    /// Compare and validate receipt prefixes.
    InspectReceiptChain,
    /// Use a fresh request ID for a genuinely different command.
    NewRequestId,
    /// Escalate a fixed installation or compatibility problem.
    ContactOperator,
    /// No automatic recovery is safe.
    None,
}

/// A stable, sanitized protocol failure.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtocolError {
    /// Machine-readable failure code.
    pub code: ErrorCode,
    /// Fixed safe message derived from `code`.
    pub message: String,
    /// Whether retry can succeed without changing the request.
    pub retryable: bool,
    /// Current state when a subject was known.
    pub current_state: Option<SessionState>,
    /// Current expected receipt sequence when known.
    pub expected_sequence: Option<u64>,
    /// Stable recovery direction.
    pub next_action: NextAction,
}

impl ProtocolError {
    /// Builds an error whose presentation fields are derived from its code.
    #[must_use]
    pub fn new(
        code: ErrorCode,
        current_state: Option<SessionState>,
        expected_sequence: Option<u64>,
    ) -> Self {
        let (message, retryable, next_action) = error_metadata(code);
        Self {
            code,
            message: message.to_owned(),
            retryable,
            current_state,
            expected_sequence,
            next_action,
        }
    }

    /// Rejects a serialized error whose derived fields contradict its code.
    ///
    /// # Errors
    /// Rejects an error whose message, retryability, or next action contradicts its code and context.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        let expected = Self::new(self.code, self.current_state, self.expected_sequence);
        if *self == expected {
            Ok(())
        } else {
            Err(Self::new(ErrorCode::InvalidRequest, None, None))
        }
    }
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ProtocolError {}

/// A compare-and-swap lifecycle mutation from the Control broker.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LifecycleRequest {
    /// Message schema.
    pub schema: String,
    /// Protocol version.
    pub protocol_version: u32,
    /// Idempotency key.
    pub request_id: String,
    /// Target Session.
    pub session_id: String,
    /// Target Run.
    pub run_id: String,
    /// Durable broker authorization this request realizes.
    pub authorization_id: String,
    /// Requested mechanic.
    pub action: LifecycleAction,
    /// State the authorizer observed.
    pub expected_state: SessionState,
    /// Current receipt head sequence the authorizer observed.
    pub expected_receipt_sequence: Option<u64>,
    /// Capability-envelope revision the authorizer observed.
    pub envelope_revision: u64,
}

impl LifecycleRequest {
    /// Serializes the request to deterministic bytes used for idempotency.
    ///
    /// # Panics
    /// Panics only if serialization fails after a future schema change introduces
    /// a fallible serializer. The current derived schema has only JSON-native values.
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "Derived schema has only JSON-native values and string-keyed maps, with no custom serializers."
    )]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("a lifecycle request is always serializable")
    }

    /// Returns the content address used to distinguish retry from conflict.
    #[must_use]
    pub fn digest(&self) -> Digest {
        Digest::of(&self.canonical_bytes())
    }

    /// Validates version, schema, bounded identifiers, and CAS shape.
    ///
    /// # Errors
    /// Rejects unsupported schema/version, invalid identifiers, or inconsistent compare-and-swap fields.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_schema(&self.schema, LIFECYCLE_REQUEST_SCHEMA)?;
        validate_version(self.protocol_version)?;
        validate_identifier(&self.request_id)?;
        validate_identifier(&self.session_id)?;
        validate_identifier(&self.run_id)?;
        validate_identifier(&self.authorization_id)?;
        let sequence_shape_matches = match self.expected_state {
            SessionState::Starting => {
                matches!(self.expected_receipt_sequence, None | Some(0))
            }
            SessionState::Running | SessionState::Parked | SessionState::Terminal => {
                self.expected_receipt_sequence.is_some()
            }
        };
        if !sequence_shape_matches {
            return Err(ProtocolError::new(ErrorCode::InvalidRequest, None, None));
        }
        Ok(())
    }
}

/// A CAS-free read of canonical Session status.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatusRequest {
    /// Message schema.
    pub schema: String,
    /// Protocol version.
    pub protocol_version: u32,
    /// Correlation identifier.
    pub request_id: String,
    /// Target Session.
    pub session_id: String,
    /// Target Run.
    pub run_id: String,
}

impl StatusRequest {
    /// Serializes the request in declaration order without whitespace.
    ///
    /// # Panics
    /// Panics only if serialization fails after a future schema change introduces
    /// a fallible serializer. The current derived schema has only JSON-native values.
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "Derived schema has only JSON-native values and string-keyed maps, with no custom serializers."
    )]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("a status request is always serializable")
    }

    /// Validates schema, version, and bounded identifiers.
    ///
    /// # Errors
    /// Rejects unsupported schema/version or invalid request, Session, or Run identifiers.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_schema(&self.schema, STATUS_REQUEST_SCHEMA)?;
        validate_version(self.protocol_version)?;
        validate_identifier(&self.request_id)?;
        validate_identifier(&self.session_id)?;
        validate_identifier(&self.run_id)
    }
}

/// Exact receipt checkpoint exchanged while reattaching an authenticated broker.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrokerReconnect {
    /// Message schema.
    pub schema: String,
    /// Protocol version.
    pub protocol_version: u32,
    /// Correlation identifier shared by request and response.
    pub request_id: String,
    /// Reattaching Session.
    pub session_id: String,
    /// Owning Run.
    pub run_id: String,
    /// Capability-envelope revision that must remain unchanged.
    pub envelope_revision: u64,
    /// Exact receipt sequence held by this side.
    pub sequence: u64,
    /// Digest of the exact canonical signed receipt envelope at `sequence`.
    pub receipt_digest: String,
}

impl BrokerReconnect {
    /// Serializes this checkpoint deterministically.
    ///
    /// # Panics
    /// Panics only if serialization fails after a future schema change introduces
    /// a fallible serializer. The current derived schema has only JSON-native values.
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "Derived schema has only JSON-native values and string-keyed maps, with no custom serializers."
    )]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("a broker reconnect is always serializable")
    }

    /// Validates the closed checkpoint shape.
    ///
    /// # Errors
    /// Rejects unsupported schema/version, invalid identifiers, or a noncanonical receipt digest.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_schema(&self.schema, BROKER_RECONNECT_SCHEMA)?;
        validate_version(self.protocol_version)?;
        validate_identifier(&self.request_id)?;
        validate_identifier(&self.session_id)?;
        validate_identifier(&self.run_id)?;
        validate_digest(&self.receipt_digest)
    }

    /// Checks that a broker checkpoint answers this exact launcher request.
    ///
    /// # Errors
    /// Rejects invalid checkpoints or mismatched request, Session, Run, or envelope-revision bindings.
    pub fn validate_response_to(&self, request: &Self) -> Result<(), ProtocolError> {
        self.validate()?;
        request.validate()?;
        if self.request_id == request.request_id
            && self.session_id == request.session_id
            && self.run_id == request.run_id
            && self.envelope_revision == request.envelope_revision
        {
            Ok(())
        } else {
            Err(ProtocolError::new(ErrorCode::InvalidRequest, None, None))
        }
    }

    /// Parses one bounded exact canonical checkpoint.
    ///
    /// # Errors
    /// Rejects oversized, malformed, unsupported-schema/version, or noncanonical bytes and any failure from [`Self::validate`].
    pub fn parse_canonical(bytes: &[u8]) -> Result<Self, ProtocolError> {
        if bytes.len() > MAX_PROTOCOL_MESSAGE_BYTES {
            return Err(ProtocolError::new(ErrorCode::MessageTooLarge, None, None));
        }
        validate_message_header(bytes, BROKER_RECONNECT_SCHEMA)?;
        let reconnect: Self = decode_closed(bytes)?;
        if reconnect.canonical_bytes() != bytes {
            return Err(ProtocolError::new(ErrorCode::InvalidRequest, None, None));
        }
        reconnect.validate()?;
        Ok(reconnect)
    }
}

/// Exact frozen checkpoint offered to the broker after controller loss.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControllerLossSettlement {
    /// Message schema.
    pub schema: String,
    /// Protocol version.
    pub protocol_version: u32,
    /// Correlation identifier for this settlement.
    pub request_id: String,
    /// Frozen Session.
    pub session_id: String,
    /// Owning Run.
    pub run_id: String,
    /// Capability-envelope revision fixed for the Session.
    pub envelope_revision: u64,
    /// Exact durable Park receipt that proves the old tree is frozen.
    pub parked_head: ReceiptHead,
}

impl ControllerLossSettlement {
    /// Serializes the settlement request deterministically.
    ///
    /// # Panics
    /// Panics only if serialization fails after a future schema change introduces
    /// a fallible serializer. The current derived schema has only JSON-native values.
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "Derived schema has only JSON-native values and string-keyed maps, with no custom serializers."
    )]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("a controller-loss settlement is always serializable")
    }

    /// Validates the closed request shape.
    ///
    /// # Errors
    /// Rejects unsupported schema/version, invalid identifiers/digest, or a zero envelope revision.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_schema(&self.schema, CONTROLLER_LOSS_SETTLEMENT_SCHEMA)?;
        validate_version(self.protocol_version)?;
        validate_identifier(&self.request_id)?;
        validate_identifier(&self.session_id)?;
        validate_identifier(&self.run_id)?;
        if self.envelope_revision == 0 {
            return Err(ProtocolError::new(ErrorCode::InvalidRequest, None, None));
        }
        validate_digest(&self.parked_head.digest)
    }

    /// Parses one exact canonical settlement request.
    ///
    /// # Errors
    /// Rejects oversized, malformed, unsupported-schema/version, or noncanonical bytes and any failure from [`Self::validate`].
    pub fn parse_canonical(bytes: &[u8]) -> Result<Self, ProtocolError> {
        if bytes.len() > MAX_PROTOCOL_MESSAGE_BYTES {
            return Err(ProtocolError::new(ErrorCode::MessageTooLarge, None, None));
        }
        validate_message_header(bytes, CONTROLLER_LOSS_SETTLEMENT_SCHEMA)?;
        let settlement: Self = decode_closed(bytes)?;
        if settlement.canonical_bytes() != bytes {
            return Err(ProtocolError::new(ErrorCode::InvalidRequest, None, None));
        }
        settlement.validate()?;
        Ok(settlement)
    }
}

/// Durable broker decision after a controller-loss Park was recorded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ControllerLossDisposition {
    /// The broker durably retained recovery material and projected Attention.
    Recoverable {
        /// Opaque broker-owned ACP recovery reference.
        acp_recovery_reference: String,
        /// Opaque identifier for the durable Attention projection.
        attention_projection_id: String,
    },
    /// Recovery is unavailable, but the abnormal loss projection is durable.
    NoRecovery {
        /// Opaque identifier for the durable Attention projection.
        attention_projection_id: String,
    },
}

impl ControllerLossDisposition {
    fn validate(&self) -> Result<(), ProtocolError> {
        match self {
            Self::Recoverable {
                acp_recovery_reference,
                attention_projection_id,
            } => {
                validate_identifier(acp_recovery_reference)?;
                validate_identifier(attention_projection_id)
            }
            Self::NoRecovery {
                attention_projection_id,
            } => validate_identifier(attention_projection_id),
        }
    }
}

/// Broker proof that controller-loss recovery policy and projection are durable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControllerLossAcknowledgement {
    /// Message schema.
    pub schema: String,
    /// Protocol version.
    pub protocol_version: u32,
    /// Correlation identifier shared with the settlement request.
    pub request_id: String,
    /// Frozen Session.
    pub session_id: String,
    /// Owning Run.
    pub run_id: String,
    /// Capability-envelope revision fixed for the Session.
    pub envelope_revision: u64,
    /// Exact durable Park receipt accepted by the broker.
    pub parked_head: ReceiptHead,
    /// Broker-owned durable recovery decision.
    pub disposition: ControllerLossDisposition,
}

impl ControllerLossAcknowledgement {
    /// Serializes the acknowledgement deterministically.
    ///
    /// # Panics
    /// Panics only if serialization fails after a future schema change introduces
    /// a fallible serializer. The current derived schema has only JSON-native values.
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "Derived schema has only JSON-native values and string-keyed maps, with no custom serializers."
    )]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("a controller-loss acknowledgement is always serializable")
    }

    /// Validates the closed acknowledgement shape.
    ///
    /// # Errors
    /// Rejects invalid schema/version, identifiers, receipt digest, envelope revision, or disposition.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_schema(&self.schema, CONTROLLER_LOSS_ACK_SCHEMA)?;
        validate_version(self.protocol_version)?;
        validate_identifier(&self.request_id)?;
        validate_identifier(&self.session_id)?;
        validate_identifier(&self.run_id)?;
        if self.envelope_revision == 0 {
            return Err(ProtocolError::new(ErrorCode::InvalidRequest, None, None));
        }
        validate_digest(&self.parked_head.digest)?;
        self.disposition.validate()
    }

    /// Requires every authority-bearing field to match the frozen request.
    ///
    /// # Errors
    /// Rejects invalid messages or any request, subject, envelope, or Park-head mismatch.
    pub fn validate_for(&self, settlement: &ControllerLossSettlement) -> Result<(), ProtocolError> {
        self.validate()?;
        settlement.validate()?;
        if self.request_id == settlement.request_id
            && self.session_id == settlement.session_id
            && self.run_id == settlement.run_id
            && self.envelope_revision == settlement.envelope_revision
            && self.parked_head == settlement.parked_head
        {
            Ok(())
        } else {
            Err(ProtocolError::new(ErrorCode::InvalidRequest, None, None))
        }
    }

    /// Parses one exact canonical acknowledgement.
    ///
    /// # Errors
    /// Rejects oversized, malformed, unsupported-schema/version, or noncanonical bytes and any failure from [`Self::validate`].
    pub fn parse_canonical(bytes: &[u8]) -> Result<Self, ProtocolError> {
        if bytes.len() > MAX_PROTOCOL_MESSAGE_BYTES {
            return Err(ProtocolError::new(ErrorCode::MessageTooLarge, None, None));
        }
        validate_message_header(bytes, CONTROLLER_LOSS_ACK_SCHEMA)?;
        let acknowledgement: Self = decode_closed(bytes)?;
        if acknowledgement.canonical_bytes() != bytes {
            return Err(ProtocolError::new(ErrorCode::InvalidRequest, None, None));
        }
        acknowledgement.validate()?;
        Ok(acknowledgement)
    }
}

/// Minimal broker-owned occupancy fact disclosed only to authenticated operators.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OccupiedSessionIdentity {
    /// Session holding the installed identity.
    pub session_id: String,
    /// Current nonterminal Session state.
    pub state: SessionState,
    /// Installed identity-pool slot.
    pub slot: u32,
}

impl OccupiedSessionIdentity {
    fn validate(&self) -> Result<(), ProtocolError> {
        validate_identifier(&self.session_id)?;
        if matches!(
            self.state,
            SessionState::Starting | SessionState::Running | SessionState::Parked
        ) {
            Ok(())
        } else {
            Err(ProtocolError::new(ErrorCode::InvalidRequest, None, None))
        }
    }
}

/// Bounded operator-only evidence explaining identity-pool exhaustion.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdentityExhaustion {
    /// Stable exhaustion failure safe to expose without occupancy details.
    pub error: ProtocolError,
    /// Canonically slot-sorted live occupants.
    pub occupied_sessions: Vec<OccupiedSessionIdentity>,
    /// Whether higher-slot occupants were omitted to enforce the disclosure bound.
    pub truncated: bool,
}

impl IdentityExhaustion {
    /// Sorts and bounds broker-owned occupancy evidence for operator delivery.
    ///
    /// # Errors
    /// Rejects invalid/terminal occupants or duplicate slots/Session identifiers before bounding disclosure.
    pub fn compose(
        mut occupied_sessions: Vec<OccupiedSessionIdentity>,
    ) -> Result<Self, ProtocolError> {
        for occupant in &occupied_sessions {
            occupant.validate()?;
        }
        occupied_sessions.sort_by_key(|occupant| occupant.slot);
        let mut session_ids = BTreeSet::new();
        for pair in occupied_sessions.windows(2) {
            if pair[0].slot == pair[1].slot {
                return Err(ProtocolError::new(ErrorCode::InvalidRequest, None, None));
            }
        }
        if occupied_sessions
            .iter()
            .any(|occupant| !session_ids.insert(occupant.session_id.as_str()))
        {
            return Err(ProtocolError::new(ErrorCode::InvalidRequest, None, None));
        }
        let truncated = occupied_sessions.len() > MAX_IDENTITY_OCCUPANTS;
        occupied_sessions.truncate(MAX_IDENTITY_OCCUPANTS);
        let exhaustion = Self {
            error: ProtocolError::new(ErrorCode::SessionIdentityExhausted, None, None),
            occupied_sessions,
            truncated,
        };
        exhaustion.validate()?;
        Ok(exhaustion)
    }

    /// Validates received evidence without silently sorting or truncating it.
    ///
    /// # Errors
    /// Rejects the wrong error code, excessive or unsorted occupancy, duplicate slots/Session IDs, or invalid occupants.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.error != ProtocolError::new(ErrorCode::SessionIdentityExhausted, None, None)
            || self.occupied_sessions.len() > MAX_IDENTITY_OCCUPANTS
        {
            return Err(ProtocolError::new(ErrorCode::InvalidRequest, None, None));
        }
        let mut session_ids = BTreeSet::new();
        let mut previous_slot = None;
        for occupant in &self.occupied_sessions {
            occupant.validate()?;
            if previous_slot.is_some_and(|slot| slot >= occupant.slot)
                || !session_ids.insert(occupant.session_id.as_str())
            {
                return Err(ProtocolError::new(ErrorCode::InvalidRequest, None, None));
            }
            previous_slot = Some(occupant.slot);
        }
        Ok(())
    }

    /// Returns the cross-Session-safe failure used by Agent/self status.
    #[must_use]
    pub fn redacted_error(&self) -> ProtocolError {
        ProtocolError::new(ErrorCode::SessionIdentityExhausted, None, None)
    }
}

/// Broker disposition for one exact signed receipt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptDisposition {
    /// The exact signed envelope is durably stored.
    DurablyStored,
    /// The broker refused to store the exact signed envelope.
    Rejected,
}

/// Disposition of one exact signed receipt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptAcknowledgement {
    /// Message schema.
    pub schema: String,
    /// Protocol version.
    pub protocol_version: u32,
    /// Receipt Session.
    pub session_id: String,
    /// Receipt Run.
    pub run_id: String,
    /// Receipt sequence.
    pub sequence: u64,
    /// Digest of the exact canonical signed envelope.
    pub receipt_digest: String,
    /// Whether the exact envelope was durably stored or rejected.
    pub disposition: ReceiptDisposition,
}

impl ReceiptAcknowledgement {
    /// Serializes the acknowledgement deterministically.
    ///
    /// # Panics
    /// Panics only if serialization fails after a future schema change introduces
    /// a fallible serializer. The current derived schema has only JSON-native values.
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "Derived schema has only JSON-native values and string-keyed maps, with no custom serializers."
    )]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("a receipt acknowledgement is always serializable")
    }

    /// Validates schema, version, subject, and canonical digest spelling.
    ///
    /// # Errors
    /// Rejects unsupported schema/version, invalid subjects, or a noncanonical receipt digest.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_schema(&self.schema, RECEIPT_ACK_SCHEMA)?;
        validate_version(self.protocol_version)?;
        validate_identifier(&self.session_id)?;
        validate_identifier(&self.run_id)?;
        validate_digest(&self.receipt_digest)
    }

    /// Returns the disposition only when this message names the exact expected head.
    #[must_use]
    pub fn exact_disposition(
        &self,
        session_id: &str,
        run_id: &str,
        head: &ReceiptHead,
    ) -> Option<ReceiptDisposition> {
        (self.validate().is_ok()
            && self.session_id == session_id
            && self.run_id == run_id
            && self.sequence == head.sequence
            && self.receipt_digest == head.digest)
            .then_some(self.disposition)
    }
}

/// A pending launch authorization atomically consumed by the Control broker.
///
/// The broker returns this record at most once. The record binds its identity
/// assignment and expiry to one exact canonical [`LaunchRequest`]; it does not
/// carry a command, environment, path, backend, or other launch mechanic.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchAuthorization {
    /// Authorization schema.
    pub schema: String,
    /// Protocol version.
    pub protocol_version: u32,
    /// Single-use authorization identity.
    pub authorization_id: String,
    /// Correlated launch request identity.
    pub request_id: String,
    /// Digest of the exact canonical launch request.
    pub request_digest: String,
    /// Unprivileged controller identity authorized by the broker.
    pub controller_uid: u32,
    /// Authorized Session.
    pub session_id: String,
    /// Authorized Run.
    pub run_id: String,
    /// Authorized capability-envelope revision.
    pub envelope_revision: u64,
    /// Slot assigned from the installed launcher identity pool.
    pub identity_slot: u32,
    /// Host UID assigned to the Session.
    pub assigned_uid: u32,
    /// Host GID assigned to the Session.
    pub assigned_gid: u32,
    /// Exclusive millisecond expiry; `now >= expires_at_ms` is expired.
    pub expires_at_ms: u64,
    /// Signed fail-closed interval allowed for authenticated broker reattachment.
    pub broker_loss_grace_ms: u32,
}

impl LaunchAuthorization {
    /// Validates the closed authorization's own wire shape.
    ///
    /// # Errors
    /// Rejects invalid schema/version, identifiers/digest, privileged/zero identities, expiry, or excessive broker-loss grace.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_schema(&self.schema, LAUNCH_AUTHORIZATION_SCHEMA)?;
        validate_version(self.protocol_version)?;
        validate_identifier(&self.authorization_id)?;
        validate_identifier(&self.request_id)?;
        validate_digest(&self.request_digest)?;
        validate_identifier(&self.session_id)?;
        validate_identifier(&self.run_id)?;
        if self.controller_uid == 0
            || self.assigned_uid == 0
            || self.assigned_gid == 0
            || self.expires_at_ms == 0
            || self.broker_loss_grace_ms > MAX_BROKER_LOSS_GRACE_MS
        {
            return Err(ProtocolError::new(ErrorCode::InvalidRequest, None, None));
        }
        Ok(())
    }

    /// Checks every broker-owned binding against one invocation.
    ///
    /// Single-use consumption is the broker's atomic storage operation. This
    /// method checks the consumed record the supervisor received, including
    /// the exclusive expiry boundary, without weakening that storage rule.
    ///
    /// # Errors
    /// Rejects invalid requests/authorizations, mismatched bindings, or an authorization at or beyond its exclusive expiry.
    pub fn validate_for(
        &self,
        request: &LaunchRequest,
        controller_uid: u32,
        now_ms: u64,
    ) -> Result<(), ProtocolError> {
        self.validate()?;
        request.validate().map_err(protocol_error_from_launch)?;
        if self.authorization_id != request.authorization_id
            || self.request_id != request.request_id
            || self.request_digest != request.digest().to_string()
            || self.controller_uid != controller_uid
            || self.session_id != request.session_id
            || self.run_id != request.run_id
            || self.envelope_revision != request.envelope_revision
            || now_ms >= self.expires_at_ms
        {
            return Err(ProtocolError::new(ErrorCode::InvalidRequest, None, None));
        }
        Ok(())
    }
}

/// One decoded inbound supervisor message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProtocolMessage {
    /// Restore selected protected bytes into a distinct frozen target.
    RecoveryRestore(Box<RecoveryRestoreRequest>),
    /// Exact broker-approved producer export or independent verification job.
    Verification(VerificationRequest),
    /// Retain recovery bytes at an exact durable Park checkpoint.
    Recovery(RecoveryRequest),
    /// Authenticated supervisor/broker command authorization exchange.
    Command(CommandMessage),
    /// Broker-authorized zero-capability workspace command.
    ToolExecution(ToolExecutionRequest),
    /// Query that atomically consumes one pending launch authorization.
    LaunchAuthorization(LaunchRequest),
    /// Authorized lifecycle mutation.
    Lifecycle(LifecycleRequest),
    /// Read-only status request.
    Status(StatusRequest),
    /// Exact checkpoint offered while reconnecting an authenticated broker.
    BrokerReconnect(BrokerReconnect),
    /// Frozen receipt checkpoint offered for durable controller-loss settlement.
    ControllerLossSettlement(ControllerLossSettlement),
    /// Durable receipt acknowledgement.
    ReceiptAcknowledgement(ReceiptAcknowledgement),
}

/// Decodes one bounded, closed, versioned inbound message.
///
/// # Errors
/// Rejects oversized, malformed, unsupported-version/schema, noncanonical, or structurally invalid inbound messages.
pub fn decode_message(bytes: &[u8]) -> Result<ProtocolMessage, ProtocolError> {
    if bytes.len() > MAX_PROTOCOL_MESSAGE_BYTES {
        return Err(ProtocolError::new(ErrorCode::MessageTooLarge, None, None));
    }
    let header: MessageHeader = serde_json::from_slice(bytes)
        .map_err(|_| ProtocolError::new(ErrorCode::MalformedMessage, None, None))?;
    validate_version(header.protocol_version)?;
    match header.schema.as_str() {
        RECOVERY_RESTORE_SCHEMA => {
            let request: RecoveryRestoreRequest = decode_closed(bytes)?;
            request.validate()?;
            Ok(ProtocolMessage::RecoveryRestore(Box::new(request)))
        }
        VERIFICATION_SCHEMA => {
            let request: VerificationRequest = decode_closed(bytes)?;
            request.validate()?;
            Ok(ProtocolMessage::Verification(request))
        }
        RECOVERY_REQUEST_SCHEMA => {
            let request: RecoveryRequest = decode_closed(bytes)?;
            request.validate()?;
            Ok(ProtocolMessage::Recovery(request))
        }
        COMMAND_SCHEMA => {
            let request: CommandMessage = decode_closed(bytes)?;
            request.validate()?;
            Ok(ProtocolMessage::Command(request))
        }
        TOOL_EXECUTION_SCHEMA => {
            let request: ToolExecutionRequest = decode_closed(bytes)?;
            request.validate()?;
            Ok(ProtocolMessage::ToolExecution(request))
        }
        REQUEST_SCHEMA => {
            let request =
                LaunchRequest::parse_canonical(bytes).map_err(protocol_error_from_launch)?;
            Ok(ProtocolMessage::LaunchAuthorization(request))
        }
        LIFECYCLE_REQUEST_SCHEMA => {
            let request: LifecycleRequest = decode_closed(bytes)?;
            request.validate()?;
            Ok(ProtocolMessage::Lifecycle(request))
        }
        STATUS_REQUEST_SCHEMA => {
            let request: StatusRequest = decode_closed(bytes)?;
            request.validate()?;
            Ok(ProtocolMessage::Status(request))
        }
        BROKER_RECONNECT_SCHEMA => {
            let reconnect: BrokerReconnect = decode_closed(bytes)?;
            reconnect.validate()?;
            Ok(ProtocolMessage::BrokerReconnect(reconnect))
        }
        CONTROLLER_LOSS_SETTLEMENT_SCHEMA => {
            let settlement: ControllerLossSettlement = decode_closed(bytes)?;
            settlement.validate()?;
            Ok(ProtocolMessage::ControllerLossSettlement(settlement))
        }
        RECEIPT_ACK_SCHEMA => {
            let acknowledgement: ReceiptAcknowledgement = decode_closed(bytes)?;
            acknowledgement.validate()?;
            Ok(ProtocolMessage::ReceiptAcknowledgement(acknowledgement))
        }
        _ => Err(ProtocolError::new(ErrorCode::UnsupportedSchema, None, None)),
    }
}

#[derive(Deserialize)]
struct MessageHeader {
    schema: String,
    protocol_version: u32,
}

fn decode_closed<'a, T: Deserialize<'a>>(bytes: &'a [u8]) -> Result<T, ProtocolError> {
    serde_json::from_slice(bytes)
        .map_err(|_| ProtocolError::new(ErrorCode::MalformedMessage, None, None))
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "Result::map_err transfers ownership of the error into this conversion."
)]
fn protocol_error_from_launch(error: LaunchError) -> ProtocolError {
    let code = match error {
        LaunchError::RequestTooLarge { .. } => ErrorCode::MessageTooLarge,
        LaunchError::MalformedRequest => ErrorCode::MalformedMessage,
        LaunchError::Schema { .. } => ErrorCode::UnsupportedSchema,
        LaunchError::ProtocolVersion { .. } => ErrorCode::UnsupportedVersion,
        LaunchError::NonCanonical
        | LaunchError::MalformedIdentifier { .. }
        | LaunchError::Registry(_) => ErrorCode::InvalidRequest,
    };
    ProtocolError::new(code, None, None)
}

/// Mechanical status authored by one Launch supervisor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupervisorStatus {
    /// Status schema.
    pub schema: String,
    /// Protocol version.
    pub protocol_version: u32,
    /// Supervised Session.
    pub session_id: String,
    /// Owning Run.
    pub run_id: String,
    /// Current mechanical state.
    pub state: SessionState,
    /// Connection to the authenticated broker.
    pub broker_connection: BrokerConnection,
    /// Applied capability-envelope revision.
    pub envelope_revision: u64,
    /// Aggregate capability-channel state.
    pub channel_state: ChannelState,
    /// Latest signed receipt known to this supervisor.
    pub launcher_head: Option<ReceiptHead>,
    /// Latest exact receipt durably acknowledged by the Control broker.
    pub broker_head: Option<ReceiptHead>,
    /// Ordered receipt intents or exact envelopes awaiting durable storage.
    pub pending_receipt_count: u32,
    /// Serialized operation still in progress.
    pub pending_operation: Option<PendingOperation>,
    /// Sanitized process result when natural exit caused terminal cleanup.
    pub process_exit: Option<ProcessExitClassification>,
    /// Latest stable failure, if any.
    pub last_failure: Option<ProtocolError>,
}

impl SupervisorStatus {
    /// Serializes the mechanical status deterministically.
    ///
    /// # Panics
    /// Panics only if serialization fails after a future schema change introduces
    /// a fallible serializer. The current derived schema has only JSON-native values.
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "Derived schema has only JSON-native values and string-keyed maps, with no custom serializers."
    )]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("supervisor status is always serializable")
    }

    /// Validates a closed supervisor status value.
    ///
    /// # Errors
    /// Rejects schema/version/subject errors or contradictory mechanical state, channel reachability, receipt heads, pending work, exit, or failure fields.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_schema(&self.schema, SUPERVISOR_STATUS_SCHEMA)?;
        validate_version(self.protocol_version)?;
        validate_identifier(&self.session_id)?;
        validate_identifier(&self.run_id)?;
        validate_status_shape(StatusShape {
            state: self.state,
            broker_connection: self.broker_connection,
            channel_state: self.channel_state,
            launcher_head: self.launcher_head.as_ref(),
            broker_head: self.broker_head.as_ref(),
            pending_receipt_count: self.pending_receipt_count,
            pending_operation: self.pending_operation.as_ref(),
            process_exit: self.process_exit,
            last_failure: self.last_failure.as_ref(),
        })
    }

    /// Parses exact canonical status bytes.
    ///
    /// # Errors
    /// Rejects oversized, malformed, unsupported-schema/version, or noncanonical bytes and any failure from [`Self::validate`].
    pub fn parse_canonical(bytes: &[u8]) -> Result<Self, ProtocolError> {
        if bytes.len() > MAX_PROTOCOL_MESSAGE_BYTES {
            return Err(ProtocolError::new(ErrorCode::MessageTooLarge, None, None));
        }
        validate_message_header(bytes, SUPERVISOR_STATUS_SCHEMA)?;
        let status: Self = decode_closed(bytes)?;
        if status.canonical_bytes() != bytes {
            return Err(ProtocolError::new(ErrorCode::InvalidRequest, None, None));
        }
        status.validate()?;
        Ok(status)
    }
}

/// Broker-composed status presented to operators and the scoped Agent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionStatus {
    /// Status schema.
    pub schema: String,
    /// Protocol version.
    pub protocol_version: u32,
    /// Session being described.
    pub session_id: String,
    /// Owning Run.
    pub run_id: String,
    /// Current mechanical state.
    pub state: SessionState,
    /// Non-authoritative projection of all six broker-owned posture dimensions.
    pub posture: Box<PostureStatus>,
    /// Broker-owned retained-point readiness; not lossless continuation or admission.
    pub recovery: RecoveryReadiness,
    /// Supervisor/broker connection state.
    pub broker_connection: BrokerConnection,
    /// Applied capability-envelope revision.
    pub envelope_revision: u64,
    /// Aggregate capability-channel state.
    pub channel_state: ChannelState,
    /// Latest signed receipt known to the Launch supervisor.
    pub launcher_head: Option<ReceiptHead>,
    /// Latest exact receipt durably acknowledged by the Control broker.
    pub broker_head: Option<ReceiptHead>,
    /// Ordered receipt intents or exact envelopes awaiting durable storage.
    pub pending_receipt_count: u32,
    /// Serialized operation still in progress.
    pub pending_operation: Option<PendingOperation>,
    /// Actor- and policy-filtered lifecycle actions.
    pub allowed_actions: Vec<LifecycleAction>,
    /// Sanitized process result when natural exit caused terminal cleanup.
    pub process_exit: Option<ProcessExitClassification>,
    /// Latest stable failure, if any.
    pub last_failure: Option<ProtocolError>,
}

impl SessionStatus {
    /// Composes broker-owned posture, recovery and actions with supervisor facts.
    ///
    /// # Errors
    /// Rejects invalid supervisor, posture or recovery fields, duplicate actions,
    /// or actions inconsistent with the resulting Session status.
    pub fn compose(
        supervisor: SupervisorStatus,
        posture: PostureStatus,
        recovery: RecoveryReadiness,
        mut allowed_actions: Vec<LifecycleAction>,
    ) -> Result<Self, ProtocolError> {
        supervisor.validate()?;
        allowed_actions.sort_unstable();
        if allowed_actions.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(ProtocolError::new(
                ErrorCode::InvalidRequest,
                Some(supervisor.state),
                supervisor.broker_head.as_ref().map(|head| head.sequence),
            ));
        }
        let status = Self {
            schema: SESSION_STATUS_SCHEMA.to_owned(),
            protocol_version: PROTOCOL_VERSION,
            session_id: supervisor.session_id,
            run_id: supervisor.run_id,
            state: supervisor.state,
            posture: Box::new(posture),
            recovery,
            broker_connection: supervisor.broker_connection,
            envelope_revision: supervisor.envelope_revision,
            channel_state: supervisor.channel_state,
            launcher_head: supervisor.launcher_head,
            broker_head: supervisor.broker_head,
            pending_receipt_count: supervisor.pending_receipt_count,
            pending_operation: supervisor.pending_operation,
            allowed_actions,
            process_exit: supervisor.process_exit,
            last_failure: supervisor.last_failure,
        };
        status.validate()?;
        Ok(status)
    }

    /// Serializes canonical Session status deterministically.
    ///
    /// # Panics
    /// Panics only if serialization fails after a future schema change introduces
    /// a fallible serializer. The current derived schema has only JSON-native values.
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "Derived schema has only JSON-native values and string-keyed maps, with no custom serializers."
    )]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("Session status is always serializable")
    }

    /// Validates subject, head, errors, and allowed-action mechanics.
    ///
    /// # Errors
    /// Rejects inconsistent posture or recovery details, Pending outside startup, invalid
    /// status fields, pending work with allowed actions, or invalid actions.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_schema(&self.schema, SESSION_STATUS_SCHEMA)?;
        validate_version(self.protocol_version)?;
        validate_identifier(&self.session_id)?;
        validate_identifier(&self.run_id)?;
        self.posture.validate()?;
        self.recovery.validate()?;
        if self.posture.state == PostureSummary::Pending && self.state != SessionState::Starting {
            return Err(ProtocolError::new(ErrorCode::InvalidRequest, None, None));
        }
        validate_status_shape(StatusShape {
            state: self.state,
            broker_connection: self.broker_connection,
            channel_state: self.channel_state,
            launcher_head: self.launcher_head.as_ref(),
            broker_head: self.broker_head.as_ref(),
            pending_receipt_count: self.pending_receipt_count,
            pending_operation: self.pending_operation.as_ref(),
            process_exit: self.process_exit,
            last_failure: self.last_failure.as_ref(),
        })?;
        if self.pending_operation.is_some() && !self.allowed_actions.is_empty() {
            return Err(invalid_status(self.state, self.broker_head.as_ref()));
        }
        if self
            .allowed_actions
            .windows(2)
            .any(|pair| pair[0] >= pair[1])
            || self
                .allowed_actions
                .iter()
                .any(|action| transition(self.state, *action).is_err())
        {
            return Err(invalid_status(self.state, self.broker_head.as_ref()));
        }
        Ok(())
    }

    /// Parses exact canonical status bytes.
    ///
    /// # Errors
    /// Rejects oversized, malformed, unsupported-schema/version, or noncanonical bytes and any failure from [`Self::validate`].
    pub fn parse_canonical(bytes: &[u8]) -> Result<Self, ProtocolError> {
        if bytes.len() > MAX_PROTOCOL_MESSAGE_BYTES {
            return Err(ProtocolError::new(ErrorCode::MessageTooLarge, None, None));
        }
        validate_message_header(bytes, SESSION_STATUS_SCHEMA)?;
        let status: Self = decode_closed(bytes)?;
        if status.canonical_bytes() != bytes {
            return Err(ProtocolError::new(ErrorCode::InvalidRequest, None, None));
        }
        status.validate()?;
        Ok(status)
    }
}

/// A completed request record supplied by the broker's durable lookup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletedRequest {
    /// Request ID used for the lookup.
    pub request_id: String,
    /// Digest of the original canonical request bytes.
    pub request_digest: String,
    /// Exact signed receipt previously returned.
    pub receipt: SignedReceipt,
}

impl CompletedRequest {
    /// Records the request identity used by a durable completed-request lookup.
    ///
    /// [`evaluate_request`] validates the receipt correlation again before it
    /// can be replayed, including for values reconstructed from storage.
    #[must_use]
    pub fn new(request: &LifecycleRequest, receipt: SignedReceipt) -> Self {
        Self {
            request_id: request.request_id.clone(),
            request_digest: request.digest().to_string(),
            receipt,
        }
    }
}

/// Pure instructions for producing one authorized lifecycle receipt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReceiptIntent {
    /// Target Session.
    pub session_id: String,
    /// Target Run.
    pub run_id: String,
    /// Authorization realized by the action.
    pub authorization_id: String,
    /// Idempotency key.
    pub request_id: String,
    /// Digest of the exact canonical request.
    pub request_digest: String,
    /// Authorized action.
    pub action: LifecycleAction,
    /// Applied envelope revision.
    pub envelope_revision: u64,
    /// New receipt sequence.
    pub sequence: u64,
    /// Digest of the previous canonical signed envelope.
    pub previous_receipt_digest: String,
    /// Mechanical state after the action.
    pub resulting_state: SessionState,
}

impl ReceiptIntent {
    /// Builds the canonical receipt statement after the mechanic succeeds.
    ///
    /// The caller supplies identities measured at the trusted launcher
    /// boundary; malformed identities are rejected by receipt validation.
    ///
    /// # Errors
    /// Returns receipt-validation errors for invalid pinned release/key identities or inconsistent intent/outcome fields.
    pub fn receipt_payload(
        &self,
        release_id: &str,
        signing_key_id: &str,
    ) -> Result<ReceiptPayload, ReceiptError> {
        let authorization = Authorization {
            authorization_id: self.authorization_id.clone(),
            request_id: self.request_id.clone(),
            request_digest: self.request_digest.clone(),
        };
        let outcome = match self.action {
            LifecycleAction::Park => ReceiptOutcome::Park {
                authority: ReceiptAuthority::Authorized(authorization),
            },
            LifecycleAction::Resume => ReceiptOutcome::Resume { authorization },
            LifecycleAction::Interrupt => ReceiptOutcome::Interrupt { authorization },
            LifecycleAction::Disposal => ReceiptOutcome::Disposal {
                authority: ReceiptAuthority::Authorized(authorization),
            },
        };
        let payload = ReceiptPayload {
            schema: RECEIPT_SCHEMA.to_owned(),
            session_id: self.session_id.clone(),
            run_id: self.run_id.clone(),
            request_id: self.request_id.clone(),
            envelope_revision: self.envelope_revision,
            sequence: self.sequence,
            previous_receipt_digest: Some(self.previous_receipt_digest.clone()),
            release_id: release_id.to_owned(),
            signing_key_id: signing_key_id.to_owned(),
            outcome,
            resulting_state: self.resulting_state,
        };
        payload.validate()?;
        Ok(payload)
    }
}

/// Result of stateless request evaluation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RequestDisposition {
    /// Return the exact original receipt without executing again.
    Replay(SignedReceipt),
    /// Execute once and produce the described receipt.
    Execute(ReceiptIntent),
}

impl RequestDisposition {
    /// Borrows the execution intent when this is a new request.
    #[must_use]
    pub fn execute_intent(&self) -> Option<&ReceiptIntent> {
        match self {
            Self::Execute(intent) => Some(intent),
            Self::Replay(_) => None,
        }
    }

    /// Borrows the exact prior receipt when this is an idempotent retry.
    #[must_use]
    pub fn replayed_receipt(&self) -> Option<&SignedReceipt> {
        match self {
            Self::Replay(receipt) => Some(receipt),
            Self::Execute(_) => None,
        }
    }
}

/// Evaluates one lifecycle mutation without changing supervisor or broker state.
///
/// `completed` is the durable broker lookup for `request.request_id`. Exact
/// replay is deliberately decided before pending/CAS checks so a retry still
/// returns its original bytes after the Session advances.
///
/// # Errors
/// Rejects invalid status/request/receipt data, subject or request-ID conflicts, pending work, stale compare-and-swap fields, invalid transitions, or receipt-sequence overflow.
pub fn evaluate_request(
    status: &SupervisorStatus,
    completed: Option<&CompletedRequest>,
    request: &LifecycleRequest,
) -> Result<RequestDisposition, ProtocolError> {
    status.validate()?;
    request.validate()?;
    let current_sequence = status.broker_head.as_ref().map(|head| head.sequence);
    if request.session_id != status.session_id || request.run_id != status.run_id {
        return Err(ProtocolError::new(
            ErrorCode::SubjectMismatch,
            Some(status.state),
            current_sequence,
        ));
    }
    if let Some(completed) = completed {
        if completed.request_id != request.request_id {
            return Err(ProtocolError::new(
                ErrorCode::InvalidRequest,
                Some(status.state),
                current_sequence,
            ));
        }
        validate_completed(completed, status.state, current_sequence)?;
        if completed.request_digest == request.digest().to_string() {
            validate_replay(completed, request, status.state, current_sequence)?;
            return Ok(RequestDisposition::Replay(completed.receipt.clone()));
        }
        return Err(ProtocolError::new(
            ErrorCode::RequestIdConflict,
            Some(status.state),
            current_sequence,
        ));
    }
    if status.pending_operation.is_some() {
        return Err(ProtocolError::new(
            ErrorCode::OperationPending,
            Some(status.state),
            current_sequence,
        ));
    }
    if request.action == LifecycleAction::Resume
        && (status.pending_receipt_count != 0 || status.launcher_head != status.broker_head)
    {
        return Err(ProtocolError::new(
            ErrorCode::DurabilityUnavailable,
            Some(status.state),
            current_sequence,
        ));
    }
    if request.expected_state != status.state {
        return Err(ProtocolError::new(
            ErrorCode::StateMismatch,
            Some(status.state),
            current_sequence,
        ));
    }
    if request.expected_receipt_sequence != current_sequence {
        return Err(ProtocolError::new(
            ErrorCode::ReceiptSequenceMismatch,
            Some(status.state),
            current_sequence,
        ));
    }
    if request.envelope_revision != status.envelope_revision {
        return Err(ProtocolError::new(
            ErrorCode::EnvelopeRevisionMismatch,
            Some(status.state),
            current_sequence,
        ));
    }
    let resulting_state = transition(status.state, request.action).map_err(|_| {
        ProtocolError::new(
            ErrorCode::InvalidTransition,
            Some(status.state),
            current_sequence,
        )
    })?;
    let head = status.launcher_head.as_ref().ok_or_else(|| {
        ProtocolError::new(ErrorCode::ReceiptSequenceMismatch, Some(status.state), None)
    })?;
    let sequence = head.sequence.checked_add(1).ok_or_else(|| {
        ProtocolError::new(
            ErrorCode::ReceiptSequenceMismatch,
            Some(status.state),
            Some(head.sequence),
        )
    })?;
    Ok(RequestDisposition::Execute(ReceiptIntent {
        session_id: request.session_id.clone(),
        run_id: request.run_id.clone(),
        authorization_id: request.authorization_id.clone(),
        request_id: request.request_id.clone(),
        request_digest: request.digest().to_string(),
        action: request.action,
        envelope_revision: request.envelope_revision,
        sequence,
        previous_receipt_digest: head.digest.clone(),
        resulting_state,
    }))
}

fn validate_completed(
    completed: &CompletedRequest,
    state: SessionState,
    sequence: Option<u64>,
) -> Result<(), ProtocolError> {
    if validate_identifier(&completed.request_id).is_err()
        || validate_digest(&completed.request_digest).is_err()
    {
        return Err(ProtocolError::new(
            ErrorCode::ReceiptChainInvalid,
            Some(state),
            sequence,
        ));
    }
    completed
        .receipt
        .validate()
        .map_err(|_| ProtocolError::new(ErrorCode::ReceiptChainInvalid, Some(state), sequence))?;
    let (authorization, _) = authorized_lifecycle_outcome(&completed.receipt)
        .ok_or_else(|| ProtocolError::new(ErrorCode::ReceiptChainInvalid, Some(state), sequence))?;
    if completed.receipt.payload.request_id != completed.request_id
        || authorization.request_id != completed.request_id
        || authorization.request_digest != completed.request_digest
    {
        return Err(ProtocolError::new(
            ErrorCode::ReceiptChainInvalid,
            Some(state),
            sequence,
        ));
    }
    Ok(())
}

fn validate_replay(
    completed: &CompletedRequest,
    request: &LifecycleRequest,
    state: SessionState,
    sequence: Option<u64>,
) -> Result<(), ProtocolError> {
    let receipt = &completed.receipt;
    let (authorization, action) = authorized_lifecycle_outcome(receipt)
        .ok_or_else(|| ProtocolError::new(ErrorCode::ReceiptChainInvalid, Some(state), sequence))?;
    let expected_result = transition(request.expected_state, request.action)
        .map_err(|_| ProtocolError::new(ErrorCode::ReceiptChainInvalid, Some(state), sequence))?;
    let expected_receipt_sequence = request
        .expected_receipt_sequence
        .and_then(|value| value.checked_add(1));
    if receipt.payload.session_id != request.session_id
        || receipt.payload.run_id != request.run_id
        || receipt.payload.envelope_revision != request.envelope_revision
        || Some(receipt.payload.sequence) != expected_receipt_sequence
        || action != request.action
        || receipt.payload.resulting_state != expected_result
        || authorization.authorization_id != request.authorization_id
    {
        return Err(ProtocolError::new(
            ErrorCode::ReceiptChainInvalid,
            Some(state),
            sequence,
        ));
    }
    Ok(())
}

fn authorized_lifecycle_outcome(
    receipt: &SignedReceipt,
) -> Option<(&Authorization, LifecycleAction)> {
    match &receipt.payload.outcome {
        ReceiptOutcome::Park {
            authority: ReceiptAuthority::Authorized(authorization),
        } => Some((authorization, LifecycleAction::Park)),
        ReceiptOutcome::Resume { authorization } => Some((authorization, LifecycleAction::Resume)),
        ReceiptOutcome::Interrupt { authorization } => {
            Some((authorization, LifecycleAction::Interrupt))
        }
        ReceiptOutcome::Disposal {
            authority: ReceiptAuthority::Authorized(authorization),
        } => Some((authorization, LifecycleAction::Disposal)),
        ReceiptOutcome::Launch { .. }
        | ReceiptOutcome::Start { .. }
        | ReceiptOutcome::Park { .. }
        | ReceiptOutcome::Disposal { .. } => None,
    }
}

/// Computes the only legal public transition for `state` and `action`.
///
/// # Errors
/// Returns `InvalidTransition` when the action is not defined for the current mechanical state.
pub fn transition(
    state: SessionState,
    action: LifecycleAction,
) -> Result<SessionState, ProtocolError> {
    match (state, action) {
        (SessionState::Running, LifecycleAction::Park)
        | (SessionState::Parked, LifecycleAction::Interrupt) => Ok(SessionState::Parked),
        (SessionState::Running, LifecycleAction::Interrupt)
        | (SessionState::Parked, LifecycleAction::Resume) => Ok(SessionState::Running),
        (SessionState::Running | SessionState::Parked, LifecycleAction::Disposal) => {
            Ok(SessionState::Terminal)
        }
        _ => Err(ProtocolError::new(
            ErrorCode::InvalidTransition,
            Some(state),
            None,
        )),
    }
}

/// Closed result carried by a protocol response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ResponseResult {
    /// Exact protected bytes published read-only for the dedicated broker.
    VerificationTransfer {
        /// Original authenticated operation.
        request: Box<VerificationRequest>,
        /// Supervisor-selected private transfer path; never a caller-selected write target.
        directory: String,
    },
    /// Authenticated proof of the exact durable copy, not a successful ACP load.
    RecoveryRestored {
        /// Original exact operation; authority depends on the responding supervisor.
        request: Box<RecoveryRestoreRequest>,
    },
    /// Actual frozen producer export and retained exact job.
    VerificationExport {
        /// Supervisor-authenticated export observation.
        evidence: VerificationExport,
    },
    /// Actual bounded plan outcomes and descendant cleanup evidence.
    VerificationExecution {
        /// Supervisor-authenticated observations, never an Agent claim.
        evidence: VerificationExecution,
    },
    /// Authenticated supervisor proof of durably retained recovery bytes.
    RecoveryRetention {
        /// Exact mechanical result; serialization alone grants no authority.
        evidence: RetentionEvidence,
    },
    /// Bounded untrusted command output after cleanup.
    ToolExecution {
        /// Result from the isolated tool tree.
        output: ToolExecutionResult,
    },
    /// Broker-consumed authorization for one exact launch request.
    LaunchAuthorization {
        /// Single-use authorization and assigned host identity.
        authorization: LaunchAuthorization,
    },
    /// Mechanical status authored by the Launch supervisor.
    SupervisorStatus {
        /// Status value.
        status: SupervisorStatus,
    },
    /// Canonical status composed by the Control broker.
    SessionStatus {
        /// Status value.
        status: SessionStatus,
    },
    /// Broker's exact durable checkpoint during authenticated reattachment.
    BrokerReconnect {
        /// Broker-owned receipt head and request correlation.
        reconnect: BrokerReconnect,
    },
    /// Broker proof that controller-loss recovery state and Attention are durable.
    ControllerLossAcknowledgement {
        /// Exact settlement acknowledgement.
        acknowledgement: ControllerLossAcknowledgement,
    },
    /// Operator-only bounded evidence that no installed identity is free.
    IdentityExhaustion {
        /// Broker-composed occupancy evidence.
        exhaustion: IdentityExhaustion,
    },
    /// Signed lifecycle receipt.
    Receipt {
        /// Receipt value.
        receipt: SignedReceipt,
    },
    /// Stable typed failure.
    Error {
        /// Failure value.
        error: ProtocolError,
    },
}

/// One correlated response from supervisor or broker.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtocolResponse {
    /// Response schema.
    pub schema: String,
    /// Protocol version.
    pub protocol_version: u32,
    /// Request being answered.
    pub request_id: String,
    /// Closed result body.
    pub result: ResponseResult,
}

impl ProtocolResponse {
    /// Serializes the response deterministically.
    ///
    /// # Panics
    /// Panics only if serialization fails after a future schema change introduces
    /// a fallible serializer. The current derived schema has only JSON-native values.
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "Derived schema has only JSON-native values and string-keyed maps, with no custom serializers."
    )]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("a protocol response is always serializable")
    }

    /// Validates every nested response value.
    ///
    /// # Errors
    /// Rejects oversized/invalid response headers, invalid nested payloads, or mismatched correlation identifiers.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.canonical_bytes().len() > MAX_PROTOCOL_MESSAGE_BYTES {
            return Err(ProtocolError::new(ErrorCode::MessageTooLarge, None, None));
        }
        validate_schema(&self.schema, RESPONSE_SCHEMA)?;
        validate_version(self.protocol_version)?;
        validate_identifier(&self.request_id)?;
        match &self.result {
            ResponseResult::VerificationTransfer { request, directory } => {
                request.validate()?;
                if request.request_id != self.request_id
                    || !matches!(request.operation, VerificationOperation::Transfer { .. })
                    || directory.len() > 4096
                    || !directory.starts_with('/')
                    || directory.contains('\0')
                {
                    return Err(ProtocolError::new(ErrorCode::InvalidRequest, None, None));
                }
                Ok(())
            }
            ResponseResult::RecoveryRestored { request } => {
                request.validate()?;
                if request.request_id != self.request_id {
                    return Err(ProtocolError::new(ErrorCode::InvalidRequest, None, None));
                }
                Ok(())
            }
            ResponseResult::VerificationExport { evidence } => {
                evidence.validate()?;
                if evidence.request.request_id != self.request_id {
                    return Err(ProtocolError::new(ErrorCode::InvalidRequest, None, None));
                }
                Ok(())
            }
            ResponseResult::VerificationExecution { evidence } => {
                evidence.validate()?;
                if evidence.request.request_id != self.request_id {
                    return Err(ProtocolError::new(ErrorCode::InvalidRequest, None, None));
                }
                Ok(())
            }
            ResponseResult::RecoveryRetention { evidence } => {
                evidence.validate()?;
                if evidence.request.request_id != self.request_id {
                    return Err(ProtocolError::new(ErrorCode::InvalidRequest, None, None));
                }
                Ok(())
            }
            ResponseResult::ToolExecution { output } => output.validate(),
            ResponseResult::LaunchAuthorization { authorization } => {
                authorization.validate()?;
                if authorization.request_id != self.request_id {
                    return Err(ProtocolError::new(ErrorCode::InvalidRequest, None, None));
                }
                Ok(())
            }
            ResponseResult::SupervisorStatus { status } => status.validate(),
            ResponseResult::SessionStatus { status } => status.validate(),
            ResponseResult::BrokerReconnect { reconnect } => {
                reconnect.validate()?;
                if reconnect.request_id != self.request_id {
                    return Err(ProtocolError::new(ErrorCode::InvalidRequest, None, None));
                }
                Ok(())
            }
            ResponseResult::ControllerLossAcknowledgement { acknowledgement } => {
                acknowledgement.validate()?;
                if acknowledgement.request_id != self.request_id {
                    return Err(ProtocolError::new(ErrorCode::InvalidRequest, None, None));
                }
                Ok(())
            }
            ResponseResult::IdentityExhaustion { exhaustion } => exhaustion.validate(),
            ResponseResult::Receipt { receipt } => {
                receipt
                    .validate()
                    .map_err(|_| ProtocolError::new(ErrorCode::ReceiptChainInvalid, None, None))?;
                if receipt.payload.request_id != self.request_id {
                    return Err(ProtocolError::new(ErrorCode::InvalidRequest, None, None));
                }
                Ok(())
            }
            ResponseResult::Error { error } => {
                error.validate()?;
                if error.code == ErrorCode::SessionIdentityExhausted {
                    return Err(ProtocolError::new(ErrorCode::InvalidRequest, None, None));
                }
                Ok(())
            }
        }
    }

    /// Parses one exact canonical response.
    ///
    /// # Errors
    /// Rejects oversized, malformed, unsupported-schema/version, or noncanonical bytes and any failure from [`Self::validate`].
    pub fn parse_canonical(bytes: &[u8]) -> Result<Self, ProtocolError> {
        if bytes.len() > MAX_PROTOCOL_MESSAGE_BYTES {
            return Err(ProtocolError::new(ErrorCode::MessageTooLarge, None, None));
        }
        validate_message_header(bytes, RESPONSE_SCHEMA)?;
        let response: Self = decode_closed(bytes)?;
        if response.canonical_bytes() != bytes {
            return Err(ProtocolError::new(ErrorCode::InvalidRequest, None, None));
        }
        response.validate()?;
        Ok(response)
    }
}

fn validate_schema(value: &str, expected: &str) -> Result<(), ProtocolError> {
    if value == expected {
        Ok(())
    } else {
        Err(ProtocolError::new(ErrorCode::UnsupportedSchema, None, None))
    }
}

fn validate_version(value: u32) -> Result<(), ProtocolError> {
    if value == PROTOCOL_VERSION {
        Ok(())
    } else {
        Err(ProtocolError::new(
            ErrorCode::UnsupportedVersion,
            None,
            None,
        ))
    }
}

fn validate_identifier(value: &str) -> Result<(), ProtocolError> {
    if value.is_empty()
        || value.len() > MAX_IDENTIFIER_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        Err(ProtocolError::new(ErrorCode::InvalidRequest, None, None))
    } else {
        Ok(())
    }
}

fn validate_digest(value: &str) -> Result<(), ProtocolError> {
    match Digest::parse(value) {
        Ok(digest) if digest.to_string() == value => Ok(()),
        _ => Err(ProtocolError::new(ErrorCode::InvalidRequest, None, None)),
    }
}

fn validate_message_header(bytes: &[u8], expected_schema: &str) -> Result<(), ProtocolError> {
    let header: MessageHeader = decode_closed(bytes)?;
    validate_schema(&header.schema, expected_schema)?;
    validate_version(header.protocol_version)
}

#[derive(Clone, Copy)]
struct StatusShape<'a> {
    state: SessionState,
    broker_connection: BrokerConnection,
    channel_state: ChannelState,
    launcher_head: Option<&'a ReceiptHead>,
    broker_head: Option<&'a ReceiptHead>,
    pending_receipt_count: u32,
    pending_operation: Option<&'a PendingOperation>,
    process_exit: Option<ProcessExitClassification>,
    last_failure: Option<&'a ProtocolError>,
}

#[expect(
    clippy::too_many_lines,
    reason = "Cross-field state, head, channel and pending-operation invariants form one validation boundary."
)]
fn validate_status_shape(shape: StatusShape<'_>) -> Result<(), ProtocolError> {
    let StatusShape {
        state,
        broker_connection,
        channel_state,
        launcher_head,
        broker_head,
        pending_receipt_count,
        pending_operation,
        process_exit,
        last_failure,
    } = shape;
    if let Some(head) = launcher_head {
        validate_digest(&head.digest)?;
    }
    if let Some(head) = broker_head {
        validate_digest(&head.digest)?;
    }
    let signed_gap = match (launcher_head, broker_head) {
        (None, None) => 0,
        (Some(launcher), None) => launcher
            .sequence
            .checked_add(1)
            .ok_or_else(|| invalid_status(state, broker_head))?,
        (Some(launcher), Some(broker)) if launcher.sequence > broker.sequence => {
            launcher.sequence - broker.sequence
        }
        (Some(launcher), Some(broker))
            if launcher.sequence == broker.sequence && launcher.digest == broker.digest =>
        {
            0
        }
        _ => return Err(invalid_status(state, broker_head)),
    };
    if pending_receipt_count > MAX_PENDING_RECEIPTS || u64::from(pending_receipt_count) < signed_gap
    {
        return Err(invalid_status(state, broker_head));
    }
    if let Some(pending) = pending_operation {
        validate_identifier(&pending.request_id)?;
        // A relay failure can also prove terminal cleanup while an earlier
        // mechanic's receipt is pending. Its process_exit remains unset.
        let terminated_after_mechanic =
            state == SessionState::Terminal && pending.phase != PendingPhase::Applying;
        let phase_receipt_matches = match pending.phase {
            PendingPhase::Applying => u64::from(pending_receipt_count) >= signed_gap,
            PendingPhase::Signing => u64::from(pending_receipt_count) > signed_gap,
            PendingPhase::AwaitingDurableAck => {
                signed_gap > 0 && u64::from(pending_receipt_count) >= signed_gap
            }
        };
        if !phase_receipt_matches {
            return Err(invalid_status(state, broker_head));
        }
        let pending_state_matches = match (pending.action, pending.phase) {
            (PendingAction::Launch, PendingPhase::Applying) => state == SessionState::Starting,
            (PendingAction::Launch, PendingPhase::Signing | PendingPhase::AwaitingDurableAck) => {
                matches!(state, SessionState::Starting | SessionState::Running)
            }
            (PendingAction::Park, PendingPhase::Applying) => state == SessionState::Running,
            (PendingAction::Park, PendingPhase::Signing | PendingPhase::AwaitingDurableAck) => {
                state == SessionState::Parked || terminated_after_mechanic
            }
            (PendingAction::Resume, PendingPhase::Applying) => state == SessionState::Parked,
            (PendingAction::Resume, PendingPhase::Signing | PendingPhase::AwaitingDurableAck) => {
                state == SessionState::Running || terminated_after_mechanic
            }
            (PendingAction::Interrupt, _) => {
                matches!(state, SessionState::Running | SessionState::Parked)
                    || terminated_after_mechanic
            }
            (PendingAction::Disposal, PendingPhase::Applying) => {
                matches!(state, SessionState::Running | SessionState::Parked)
            }
            (PendingAction::Disposal, PendingPhase::Signing | PendingPhase::AwaitingDurableAck) => {
                state == SessionState::Terminal
            }
        };
        if !pending_state_matches {
            return Err(invalid_status(state, broker_head));
        }
        let launcher_sequence = launcher_head.map(|head| head.sequence);
        let broker_sequence = broker_head.map(|head| head.sequence);
        let launch_head_matches = match (state, pending.action, pending.phase) {
            (SessionState::Starting, PendingAction::Launch, PendingPhase::Applying) => {
                matches!(launcher_sequence, None | Some(0))
            }
            (SessionState::Starting, PendingAction::Launch, PendingPhase::Signing) => {
                launcher_sequence.is_none() && broker_sequence.is_none()
            }
            (SessionState::Starting, PendingAction::Launch, PendingPhase::AwaitingDurableAck) => {
                launcher_sequence == Some(0) && broker_sequence.is_none()
            }
            (SessionState::Running, PendingAction::Launch, PendingPhase::Signing) => {
                launcher_sequence == Some(0) && broker_sequence == Some(0)
            }
            (SessionState::Running, PendingAction::Launch, PendingPhase::AwaitingDurableAck) => {
                launcher_sequence == Some(1) && broker_sequence == Some(0)
            }
            (_, PendingAction::Launch, _) => false,
            _ => true,
        };
        if !launch_head_matches {
            return Err(invalid_status(state, broker_head));
        }
    } else if pending_receipt_count == 0 && launcher_head != broker_head {
        return Err(invalid_status(state, broker_head));
    }
    if let Some(failure) = last_failure {
        failure.validate()?;
    }
    if process_exit.is_some() && state != SessionState::Terminal {
        return Err(invalid_status(state, broker_head));
    }
    let state_matches = match state {
        SessionState::Starting => {
            channel_state == ChannelState::Disabled
                && launcher_head.is_none_or(|head| head.sequence == 0)
        }
        SessionState::Running => {
            let receipt_head_matches = launcher_head.is_some_and(|head| {
                head.sequence > 0
                    || matches!(
                        pending_operation,
                        Some(PendingOperation {
                            action: PendingAction::Launch,
                            phase: PendingPhase::Signing,
                            ..
                        })
                    )
            });
            channel_state != ChannelState::Closed && receipt_head_matches
        }
        SessionState::Parked => {
            matches!(
                channel_state,
                ChannelState::Disabled | ChannelState::Revoked
            ) && launcher_head.is_some()
        }
        SessionState::Terminal => {
            channel_state == ChannelState::Closed
                && (launcher_head.is_some()
                    || (pending_operation.is_none() && last_failure.is_some()))
        }
    };
    let connection_matches =
        broker_connection == BrokerConnection::Connected || channel_state != ChannelState::Enabled;
    if state_matches && connection_matches {
        Ok(())
    } else {
        Err(invalid_status(state, broker_head))
    }
}

fn invalid_status(state: SessionState, receipt_head: Option<&ReceiptHead>) -> ProtocolError {
    ProtocolError::new(
        ErrorCode::InvalidRequest,
        Some(state),
        receipt_head.map(|head| head.sequence),
    )
}

fn error_metadata(code: ErrorCode) -> (&'static str, bool, NextAction) {
    match code {
        ErrorCode::MessageTooLarge => ("protocol message is too large", false, NextAction::None),
        ErrorCode::MalformedMessage => ("protocol message is malformed", false, NextAction::None),
        ErrorCode::UnsupportedSchema => (
            "protocol schema is not supported",
            false,
            NextAction::ContactOperator,
        ),
        ErrorCode::UnsupportedVersion => (
            "protocol version is not supported",
            false,
            NextAction::ContactOperator,
        ),
        ErrorCode::InvalidRequest => ("protocol request is invalid", false, NextAction::None),
        ErrorCode::SubjectMismatch => (
            "request subject does not match this session",
            false,
            NextAction::RefreshStatus,
        ),
        ErrorCode::RequestIdConflict => (
            "request id was reused for different bytes",
            false,
            NextAction::NewRequestId,
        ),
        ErrorCode::OperationPending => (
            "another lifecycle operation is pending",
            true,
            NextAction::Wait,
        ),
        ErrorCode::StateMismatch => (
            "expected session state is stale",
            false,
            NextAction::RefreshStatus,
        ),
        ErrorCode::ReceiptSequenceMismatch => (
            "expected receipt sequence is stale",
            false,
            NextAction::RefreshStatus,
        ),
        ErrorCode::EnvelopeRevisionMismatch => (
            "expected envelope revision is stale",
            false,
            NextAction::RefreshStatus,
        ),
        ErrorCode::InvalidTransition => (
            "lifecycle action is invalid from current state",
            false,
            NextAction::RefreshStatus,
        ),
        ErrorCode::LifecycleMechanicUnavailable => (
            "Session lifecycle mechanic is unavailable",
            false,
            NextAction::ContactOperator,
        ),
        ErrorCode::SigningKeyRevoked => (
            "Launcher signing key revoked",
            false,
            NextAction::ContactOperator,
        ),
        ErrorCode::ReceiptChainInvalid => (
            "launcher receipt chain is invalid",
            false,
            NextAction::InspectReceiptChain,
        ),
        ErrorCode::SigningUnavailable => (
            "launcher receipt signing is unavailable",
            true,
            NextAction::RetrySameRequest,
        ),
        ErrorCode::DurabilityUnavailable => (
            "durable receipt storage is unavailable",
            true,
            NextAction::RetrySameRequest,
        ),
        ErrorCode::BrokerUnavailable => (
            "control broker is unavailable",
            true,
            NextAction::ReconnectBroker,
        ),
        ErrorCode::SessionIdentityExhausted => {
            ("no session identity is available", true, NextAction::Wait)
        }
        ErrorCode::IdentityAssignmentInvalid => (
            "broker-assigned session identity is invalid",
            false,
            NextAction::ContactOperator,
        ),
    }
}
