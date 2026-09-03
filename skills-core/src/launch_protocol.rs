//! Closed lifecycle messages shared by the Launch supervisor and Control broker.
//!
//! This module describes bytes and pure compare-and-swap decisions. It does
//! not own transport, authorization policy, persistence, or process mechanics.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{
    canonical::Digest,
    launch_receipt::{
        Authorization, RECEIPT_SCHEMA, ReceiptAuthority, ReceiptError, ReceiptHead, ReceiptOutcome,
        ReceiptPayload, SessionState, SignedReceipt,
    },
};

/// Protocol version carried by every launcher message.
pub const PROTOCOL_VERSION: u32 = 1;

/// Longest accepted encoded protocol message.
pub const MAX_PROTOCOL_MESSAGE_BYTES: usize = 64 * 1024;

/// Longest accepted opaque identifier.
pub const MAX_IDENTIFIER_BYTES: usize = 128;

/// Schema for an authorized lifecycle mutation.
pub const LIFECYCLE_REQUEST_SCHEMA: &str = "louiselm.launch.lifecycle-request/1";

/// Schema for a read-only status request.
pub const STATUS_REQUEST_SCHEMA: &str = "louiselm.launch.status-request/1";

/// Schema for durable receipt acknowledgement.
pub const RECEIPT_ACK_SCHEMA: &str = "louiselm.launch.receipt-ack/1";

/// Schema for the mechanical supervisor status.
pub const SUPERVISOR_STATUS_SCHEMA: &str = "louiselm.launch.supervisor-status/1";

/// Schema for broker-composed canonical Session status.
pub const SESSION_STATUS_SCHEMA: &str = "louiselm.launch.session-status/1";

/// Schema for a response to a request.
pub const RESPONSE_SCHEMA: &str = "louiselm.launch.response/1";

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
    /// Establish the sequence-zero launch receipt.
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
    /// Enabling remains blocked until the broker acknowledges durable bytes.
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
    /// Receipt bytes or their chain do not verify.
    ReceiptChainInvalid,
    /// Receipt signing is temporarily unavailable.
    SigningUnavailable,
    /// Durable receipt storage is temporarily unavailable.
    DurabilityUnavailable,
    /// The Control broker is unavailable.
    BrokerUnavailable,
    /// No isolated host identity is currently free for a new Session.
    SessionIdentityExhausted,
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
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("a lifecycle request is always serializable")
    }

    /// Returns the content address used to distinguish retry from conflict.
    #[must_use]
    pub fn digest(&self) -> Digest {
        Digest::of(&self.canonical_bytes())
    }

    /// Validates version, schema, bounded identifiers, and CAS shape.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_schema(&self.schema, LIFECYCLE_REQUEST_SCHEMA)?;
        validate_version(self.protocol_version)?;
        validate_identifier(&self.request_id)?;
        validate_identifier(&self.session_id)?;
        validate_identifier(&self.run_id)?;
        validate_identifier(&self.authorization_id)?;
        let sequence_shape_matches = match self.expected_state {
            SessionState::Starting => self.expected_receipt_sequence.is_none(),
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
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("a status request is always serializable")
    }

    /// Validates schema, version, and bounded identifiers.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_schema(&self.schema, STATUS_REQUEST_SCHEMA)?;
        validate_version(self.protocol_version)?;
        validate_identifier(&self.request_id)?;
        validate_identifier(&self.session_id)?;
        validate_identifier(&self.run_id)
    }
}

/// Confirmation that one exact signed receipt is durably stored.
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
}

impl ReceiptAcknowledgement {
    /// Serializes the acknowledgement deterministically.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("a receipt acknowledgement is always serializable")
    }

    /// Validates schema, version, subject, and canonical digest spelling.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_schema(&self.schema, RECEIPT_ACK_SCHEMA)?;
        validate_version(self.protocol_version)?;
        validate_identifier(&self.session_id)?;
        validate_identifier(&self.run_id)?;
        validate_digest(&self.receipt_digest)
    }

    /// Whether this acknowledgement names one exact expected receipt head.
    #[must_use]
    pub fn matches(&self, session_id: &str, run_id: &str, head: &ReceiptHead) -> bool {
        self.validate().is_ok()
            && self.session_id == session_id
            && self.run_id == run_id
            && self.sequence == head.sequence
            && self.receipt_digest == head.digest
    }
}

/// One decoded inbound supervisor message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProtocolMessage {
    /// Authorized lifecycle mutation.
    Lifecycle(LifecycleRequest),
    /// Read-only status request.
    Status(StatusRequest),
    /// Durable receipt acknowledgement.
    ReceiptAcknowledgement(ReceiptAcknowledgement),
}

/// Decodes one bounded, closed, versioned inbound message.
pub fn decode_message(bytes: &[u8]) -> Result<ProtocolMessage, ProtocolError> {
    if bytes.len() > MAX_PROTOCOL_MESSAGE_BYTES {
        return Err(ProtocolError::new(ErrorCode::MessageTooLarge, None, None));
    }
    let header: MessageHeader = serde_json::from_slice(bytes)
        .map_err(|_| ProtocolError::new(ErrorCode::MalformedMessage, None, None))?;
    validate_version(header.protocol_version)?;
    match header.schema.as_str() {
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
    pub receipt_head: Option<ReceiptHead>,
    /// Serialized operation still in progress.
    pub pending_operation: Option<PendingOperation>,
    /// Latest stable failure, if any.
    pub last_failure: Option<ProtocolError>,
}

impl SupervisorStatus {
    /// Serializes the mechanical status deterministically.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("supervisor status is always serializable")
    }

    /// Validates a closed supervisor status value.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_schema(&self.schema, SUPERVISOR_STATUS_SCHEMA)?;
        validate_version(self.protocol_version)?;
        validate_identifier(&self.session_id)?;
        validate_identifier(&self.run_id)?;
        validate_status_shape(
            self.state,
            self.broker_connection,
            self.channel_state,
            self.receipt_head.as_ref(),
            self.pending_operation.as_ref(),
            self.last_failure.as_ref(),
        )
    }

    /// Parses exact canonical status bytes.
    pub fn parse_canonical(bytes: &[u8]) -> Result<Self, ProtocolError> {
        if bytes.len() > MAX_PROTOCOL_MESSAGE_BYTES {
            return Err(ProtocolError::new(ErrorCode::MessageTooLarge, None, None));
        }
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
    /// Non-authoritative summary of broker-owned posture evidence.
    pub posture: PostureSummary,
    /// Supervisor/broker connection state.
    pub broker_connection: BrokerConnection,
    /// Applied capability-envelope revision.
    pub envelope_revision: u64,
    /// Aggregate capability-channel state.
    pub channel_state: ChannelState,
    /// Latest signed receipt.
    pub receipt_head: Option<ReceiptHead>,
    /// Serialized operation still in progress.
    pub pending_operation: Option<PendingOperation>,
    /// Actor- and policy-filtered lifecycle actions.
    pub allowed_actions: Vec<LifecycleAction>,
    /// Latest stable failure, if any.
    pub last_failure: Option<ProtocolError>,
}

impl SessionStatus {
    /// Composes broker-owned posture/actions with mechanical supervisor facts.
    pub fn compose(
        supervisor: SupervisorStatus,
        posture: PostureSummary,
        mut allowed_actions: Vec<LifecycleAction>,
    ) -> Result<Self, ProtocolError> {
        supervisor.validate()?;
        allowed_actions.sort_unstable();
        if allowed_actions.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(ProtocolError::new(
                ErrorCode::InvalidRequest,
                Some(supervisor.state),
                supervisor.receipt_head.as_ref().map(|head| head.sequence),
            ));
        }
        let status = Self {
            schema: SESSION_STATUS_SCHEMA.to_owned(),
            protocol_version: PROTOCOL_VERSION,
            session_id: supervisor.session_id,
            run_id: supervisor.run_id,
            state: supervisor.state,
            posture,
            broker_connection: supervisor.broker_connection,
            envelope_revision: supervisor.envelope_revision,
            channel_state: supervisor.channel_state,
            receipt_head: supervisor.receipt_head,
            pending_operation: supervisor.pending_operation,
            allowed_actions,
            last_failure: supervisor.last_failure,
        };
        status.validate()?;
        Ok(status)
    }

    /// Serializes canonical Session status deterministically.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("Session status is always serializable")
    }

    /// Validates subject, head, errors, and allowed-action mechanics.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_schema(&self.schema, SESSION_STATUS_SCHEMA)?;
        validate_version(self.protocol_version)?;
        validate_identifier(&self.session_id)?;
        validate_identifier(&self.run_id)?;
        validate_status_shape(
            self.state,
            self.broker_connection,
            self.channel_state,
            self.receipt_head.as_ref(),
            self.pending_operation.as_ref(),
            self.last_failure.as_ref(),
        )?;
        if self.pending_operation.is_some() && !self.allowed_actions.is_empty() {
            return Err(invalid_status(self.state, self.receipt_head.as_ref()));
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
            return Err(invalid_status(self.state, self.receipt_head.as_ref()));
        }
        Ok(())
    }

    /// Parses exact canonical status bytes.
    pub fn parse_canonical(bytes: &[u8]) -> Result<Self, ProtocolError> {
        if bytes.len() > MAX_PROTOCOL_MESSAGE_BYTES {
            return Err(ProtocolError::new(ErrorCode::MessageTooLarge, None, None));
        }
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
pub fn evaluate_request(
    status: &SupervisorStatus,
    completed: Option<&CompletedRequest>,
    request: &LifecycleRequest,
) -> Result<RequestDisposition, ProtocolError> {
    status.validate()?;
    request.validate()?;
    let current_sequence = status.receipt_head.as_ref().map(|head| head.sequence);
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
    let head = status.receipt_head.as_ref().ok_or_else(|| {
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
        | ReceiptOutcome::Park { .. }
        | ReceiptOutcome::Disposal { .. } => None,
    }
}

/// Computes the only legal public transition for `state` and `action`.
pub fn transition(
    state: SessionState,
    action: LifecycleAction,
) -> Result<SessionState, ProtocolError> {
    match (state, action) {
        (SessionState::Running, LifecycleAction::Park) => Ok(SessionState::Parked),
        (SessionState::Running, LifecycleAction::Interrupt) => Ok(SessionState::Running),
        (SessionState::Running, LifecycleAction::Disposal)
        | (SessionState::Parked, LifecycleAction::Disposal) => Ok(SessionState::Terminal),
        (SessionState::Parked, LifecycleAction::Resume) => Ok(SessionState::Running),
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
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("a protocol response is always serializable")
    }

    /// Validates every nested response value.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.canonical_bytes().len() > MAX_PROTOCOL_MESSAGE_BYTES {
            return Err(ProtocolError::new(ErrorCode::MessageTooLarge, None, None));
        }
        validate_schema(&self.schema, RESPONSE_SCHEMA)?;
        validate_version(self.protocol_version)?;
        validate_identifier(&self.request_id)?;
        match &self.result {
            ResponseResult::SupervisorStatus { status } => status.validate(),
            ResponseResult::SessionStatus { status } => status.validate(),
            ResponseResult::Receipt { receipt } => {
                receipt
                    .validate()
                    .map_err(|_| ProtocolError::new(ErrorCode::ReceiptChainInvalid, None, None))?;
                if receipt.payload.request_id != self.request_id {
                    return Err(ProtocolError::new(ErrorCode::InvalidRequest, None, None));
                }
                Ok(())
            }
            ResponseResult::Error { error } => error.validate(),
        }
    }

    /// Parses one exact canonical response.
    pub fn parse_canonical(bytes: &[u8]) -> Result<Self, ProtocolError> {
        if bytes.len() > MAX_PROTOCOL_MESSAGE_BYTES {
            return Err(ProtocolError::new(ErrorCode::MessageTooLarge, None, None));
        }
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

fn validate_status_shape(
    state: SessionState,
    broker_connection: BrokerConnection,
    channel_state: ChannelState,
    receipt_head: Option<&ReceiptHead>,
    pending_operation: Option<&PendingOperation>,
    last_failure: Option<&ProtocolError>,
) -> Result<(), ProtocolError> {
    if let Some(head) = receipt_head {
        validate_digest(&head.digest)?;
    }
    if let Some(pending) = pending_operation {
        validate_identifier(&pending.request_id)?;
        if pending.phase == PendingPhase::AwaitingDurableAck && receipt_head.is_none() {
            return Err(invalid_status(state, receipt_head));
        }
        let pending_state_matches = match (pending.action, pending.phase) {
            (PendingAction::Launch, _) => state == SessionState::Starting,
            (PendingAction::Park, PendingPhase::Applying) => state == SessionState::Running,
            (PendingAction::Park, PendingPhase::Signing | PendingPhase::AwaitingDurableAck) => {
                state == SessionState::Parked
            }
            (PendingAction::Resume, _) => state == SessionState::Parked,
            (PendingAction::Interrupt, _) => state == SessionState::Running,
            (PendingAction::Disposal, PendingPhase::Applying) => {
                matches!(state, SessionState::Running | SessionState::Parked)
            }
            (PendingAction::Disposal, PendingPhase::Signing | PendingPhase::AwaitingDurableAck) => {
                state == SessionState::Terminal
            }
        };
        if !pending_state_matches {
            return Err(invalid_status(state, receipt_head));
        }
    }
    if let Some(failure) = last_failure {
        failure.validate()?;
    }
    let state_matches = match state {
        SessionState::Starting => channel_state == ChannelState::Disabled,
        SessionState::Running => channel_state != ChannelState::Closed && receipt_head.is_some(),
        SessionState::Parked => {
            matches!(
                channel_state,
                ChannelState::Disabled | ChannelState::Revoked
            ) && receipt_head.is_some()
        }
        SessionState::Terminal => {
            channel_state == ChannelState::Closed
                && (receipt_head.is_some()
                    || (pending_operation.is_none() && last_failure.is_some()))
        }
    };
    let connection_matches =
        broker_connection == BrokerConnection::Connected || channel_state != ChannelState::Enabled;
    if state_matches && connection_matches {
        Ok(())
    } else {
        Err(invalid_status(state, receipt_head))
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
    }
}
