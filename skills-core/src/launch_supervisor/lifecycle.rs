//! Long-lived ownership of one launched Session's lifecycle authorities.

#[path = "tool_dispatch.rs"]
mod tool_dispatch;

#[path = "recovery_dispatch.rs"]
mod recovery_dispatch;
#[path = "restore_dispatch.rs"]
mod restore_dispatch;
#[path = "verification_dispatch.rs"]
mod verification_dispatch;

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex, mpsc},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use crate::{
    Digest,
    launch::PROTOCOL_VERSION,
    launch_protocol::{
        BROKER_RECONNECT_SCHEMA, BrokerConnection, BrokerReconnect,
        CONTROLLER_LOSS_SETTLEMENT_SCHEMA, ChannelState, CompletedRequest,
        ControllerLossAcknowledgement, ControllerLossSettlement, ErrorCode,
        LIFECYCLE_REQUEST_SCHEMA, LifecycleAction, LifecycleRequest, MAX_IDENTIFIER_BYTES,
        MAX_PENDING_RECEIPTS, PendingOperation, PendingPhase, ProtocolError, ProtocolMessage,
        ProtocolResponse, RESPONSE_SCHEMA, ReceiptAcknowledgement, ReceiptDisposition,
        ReceiptIntent, RequestDisposition, ResponseResult, SUPERVISOR_STATUS_SCHEMA, StatusRequest,
        SupervisorStatus, evaluate_request,
    },
    launch_receipt::{
        ProcessExitClassification, RECEIPT_SCHEMA, ReceiptAuthority, ReceiptCause, ReceiptError,
        ReceiptHead, ReceiptOutcome, ReceiptPayload, SIGNED_RECEIPT_SCHEMA, SessionState,
        SignedReceipt,
    },
};

use super::{
    CapabilityBinding, CapabilityGate, IdentityGuard, LaunchBroker, LaunchSigner, MechanicFailure,
    RelayStdio, RunningAgent, RunningAgentEvent, SupervisorError, SupervisorTimer,
};

const EVENT_QUEUE_CAPACITY: usize = 32;
const FAILED_REQUEST_CAPACITY: usize = 64;

#[derive(Default)]
pub(super) struct SessionResources {
    process: Option<Box<dyn RunningAgent>>,
    capability: Option<Box<dyn CapabilityGate>>,
    identity: Option<Box<dyn IdentityGuard>>,
    broker: Option<Arc<dyn LaunchBroker>>,
}

impl SessionResources {
    pub(super) fn new(
        process: Box<dyn RunningAgent>,
        capability: Box<dyn CapabilityGate>,
        identity: Box<dyn IdentityGuard>,
        broker: Arc<dyn LaunchBroker>,
    ) -> Self {
        Self {
            process: Some(process),
            capability: Some(capability),
            identity: Some(identity),
            broker: Some(broker),
        }
    }

    fn terminate_session(&mut self) -> Result<(), SupervisorError> {
        if let Some(mut capability) = self.capability.take() {
            capability.close();
        }
        let process_result = self
            .process
            .as_mut()
            .map_or(Ok(()), |process| process.dispose());
        if process_result.is_ok() {
            self.process = None;
        }
        let Some(identity) = self.identity.take() else {
            return process_result;
        };
        if process_result.is_ok() {
            identity.release()
        } else {
            let _ = identity.poison();
            Err(SupervisorError::CleanupUnproven)
        }
    }

    fn cleanup(&mut self) -> Result<(), SupervisorError> {
        let terminal = self.terminate_session();
        if let Some(broker) = self.broker.take() {
            broker.close();
        }
        terminal
    }
}

impl Drop for SessionResources {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

/// Successfully launched Session plus every authority retained until cleanup.
pub struct LaunchedSession {
    owner: Option<JoinHandle<Result<i32, SupervisorError>>>,
    sender: mpsc::SyncSender<OwnerEvent>,
    controller: Option<mpsc::SyncSender<RelayStdio>>,
    receipt: SignedReceipt,
    binding: CapabilityBinding,
}

impl LaunchedSession {
    #[expect(
        clippy::expect_used,
        reason = "run_launch hands off only after collecting durable genesis and start receipts."
    )]
    pub(super) fn new(
        resources: SessionResources,
        signer: Arc<dyn LaunchSigner>,
        receipts: Vec<SignedReceipt>,
        binding: CapabilityBinding,
        timeout: Duration,
        timer: Arc<dyn SupervisorTimer>,
        broker_loss_grace: Duration,
    ) -> Result<(Self, mpsc::Receiver<()>), SupervisorError> {
        debug_assert!(!receipts.is_empty());
        let receipt = receipts
            .last()
            .expect("a launched Session has a receipt")
            .clone();
        let (controller, attachment) = mpsc::sync_channel(1);
        let owner = SessionOwner::new(
            resources,
            signer,
            receipts,
            binding.clone(),
            timeout,
            timer,
            broker_loss_grace,
        );
        let sender = owner.sender.clone();
        let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
        let (coordinator_lifetime, owner_finished) = mpsc::channel::<()>();
        let worker = thread::Builder::new()
            .name("louiselm-launch-session-owner".to_owned())
            .spawn(move || {
                // Disconnect only after this worker's resources have been cleaned up,
                // including during unwinding, so the spawning coordinator stays alive.
                let _coordinator_lifetime = coordinator_lifetime;
                let mut owner = owner;
                let result = owner.run(attachment, &ready_sender);
                // Terminal cleanup must not join a callback blocked on
                // a receiver that the owner will never service again.
                let (_, disconnected) = mpsc::sync_channel(1);
                owner.receiver = disconnected;
                let cleanup = owner.resources.cleanup();
                match (result, cleanup) {
                    (_, Err(error)) => Err(error),
                    (result, Ok(())) => result,
                }
            })
            .map_err(|_| SupervisorError::WorkerUnavailable)?;
        match ready_receiver.recv() {
            Ok(Ok(())) => Ok((
                Self {
                    owner: Some(worker),
                    sender,
                    controller: Some(controller),
                    receipt,
                    binding,
                },
                owner_finished,
            )),
            Ok(Err(error)) => {
                let _ = worker.join();
                Err(error)
            }
            Err(_) => {
                let _ = worker.join();
                Err(SupervisorError::WorkerUnavailable)
            }
        }
    }

    /// Latest exact receipt durably acknowledged before this value existed.
    #[must_use]
    pub fn receipt(&self) -> &SignedReceipt {
        &self.receipt
    }

    /// Capability metadata fixed before its listener was enabled.
    #[must_use]
    pub fn capability_binding(&self) -> &CapabilityBinding {
        &self.binding
    }

    /// Relays opaque ACP bytes while one serialized owner services broker lifecycle requests.
    ///
    /// # Errors
    /// Returns terminal/descriptor-cleanup failure, or `WorkerUnavailable` if the owner cannot be joined.
    pub fn relay_stdio(mut self, controller: RelayStdio) -> Result<i32, SupervisorError> {
        // A receiver closed by an already-terminal owner is not resurrected.
        // Close rejected descriptors explicitly so restoration errors cannot
        // turn an already-terminal owner into false successful relay cleanup.
        let sender = self.controller.take().ok_or(SupervisorError::RelayFailed)?;
        let attachment = sender.send(controller).err().map_or(Ok(()), |error| {
            error
                .0
                .close()
                .map_err(|_| SupervisorError::CleanupUnproven)
        });
        let terminal = self.join_owner();
        attachment.and(terminal)
    }

    /// Ends controller ownership and waits for the broker-settled terminal outcome.
    ///
    /// # Errors
    /// Returns the lifecycle owner's terminal failure, or `WorkerUnavailable` if it cannot be joined.
    pub fn dispose(mut self) -> Result<(), SupervisorError> {
        self.detach_controller();
        self.join_owner().map(drop)
    }

    fn detach_controller(&mut self) {
        let _ = self.sender.send(OwnerEvent::ControllerDetached);
        self.controller = None;
    }

    fn join_owner(&mut self) -> Result<i32, SupervisorError> {
        self.owner
            .take()
            .ok_or(SupervisorError::WorkerUnavailable)?
            .join()
            .map_err(|_| SupervisorError::WorkerUnavailable)?
    }
}

impl Drop for LaunchedSession {
    fn drop(&mut self) {
        if self.owner.is_some() {
            self.detach_controller();
            let _ = self.owner.take();
        }
    }
}

enum OwnerEvent {
    RestoreFinished,
    RecoveryFinished,
    VerificationFinished,
    ToolFinished,
    CommandDeadline {
        request_id: String,
    },
    ControllerDetached,
    BrokerRequest {
        connection_epoch: u64,
        result: Box<Result<ProtocolMessage, SupervisorError>>,
    },
    Signed {
        operation_epoch: u64,
        result: Result<String, SupervisorError>,
    },
    ReceiptSent {
        operation_epoch: u64,
        result: Result<(), SupervisorError>,
    },
    OperationDeadline {
        operation_epoch: u64,
    },
    ResponseSent {
        connection_epoch: u64,
        finish: Option<Result<i32, SupervisorError>>,
        result: Result<(), SupervisorError>,
    },
    BrokerReconnected {
        connection_epoch: u64,
        attempt_epoch: u64,
        result: Result<BrokerReconnect, SupervisorError>,
    },
    BrokerReconnectDeadline {
        connection_epoch: u64,
        attempt_epoch: u64,
    },
    BrokerReconnectRetry {
        connection_epoch: u64,
    },
    BrokerGraceExpired {
        connection_epoch: u64,
    },
    ReconciliationReceiptSent {
        connection_epoch: u64,
        reconciliation_epoch: u64,
        receipt_index: usize,
        result: Result<(), SupervisorError>,
    },
    ReconciliationReceiptDeadline {
        connection_epoch: u64,
        reconciliation_epoch: u64,
        receipt_index: usize,
    },
    DeferredReceiptSigned {
        connection_epoch: u64,
        operation_epoch: u64,
        result: Result<String, SupervisorError>,
    },
    DeferredReceiptSent {
        connection_epoch: u64,
        operation_epoch: u64,
        result: Result<(), SupervisorError>,
    },
    DeferredReceiptDeadline {
        connection_epoch: u64,
        operation_epoch: u64,
    },
    ControllerLossSettled {
        settlement_epoch: u64,
        result: Result<ControllerLossAcknowledgement, SupervisorError>,
    },
    ControllerLossDeadline {
        settlement_epoch: u64,
    },
    RunningAgent {
        process_epoch: u64,
        event: RunningAgentEvent,
    },
    RelayQuiesced,
}

struct ActiveOperation {
    epoch: u64,
    request: LifecycleRequest,
    intent: ReceiptIntent,
    phase: PendingPhase,
    receipt: Option<SignedReceipt>,
    payload_override: Option<ReceiptPayload>,
    respond: bool,
    finish: Option<Result<i32, SupervisorError>>,
}

enum DeferredReceipt {
    Payload(Box<ReceiptPayload>),
    CausalPark {
        request_id: String,
        envelope_revision: u64,
        cause: ReceiptCause,
    },
}

impl DeferredReceipt {
    fn validate(&self) -> bool {
        match self {
            Self::Payload(payload) => payload.validate().is_ok(),
            Self::CausalPark {
                request_id,
                envelope_revision,
                cause,
            } => {
                !request_id.is_empty()
                    && request_id.len() <= MAX_IDENTIFIER_BYTES
                    && request_id
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
                    && *envelope_revision > 0
                    && matches!(
                        cause,
                        ReceiptCause::AcknowledgementFailed | ReceiptCause::BrokerLost
                    )
            }
        }
    }
}

struct FailedRequest {
    request_id: String,
    request_digest: String,
    response: ProtocolResponse,
    finish: Option<Result<i32, SupervisorError>>,
}

struct ReceiptReconciliation {
    operation_epoch: u64,
    awaiting_receipt_index: usize,
}

enum DeferredReceiptPhase {
    Signing(Box<ReceiptPayload>),
    AwaitingDurableAck { receipt_index: usize },
}

struct DeferredReceiptReconciliation {
    operation_epoch: u64,
    phase: DeferredReceiptPhase,
}

struct ActiveControllerLossSettlement {
    epoch: u64,
    request: ControllerLossSettlement,
}

#[derive(Clone, Copy)]
enum TerminalEvent {
    ProcessExited(ProcessExitClassification),
    AgentIdentityLost,
    RelayFailed,
}

struct QueuedTerminalEvent {
    event: TerminalEvent,
    previous_state: SessionState,
}

enum ParkResult {
    Parked,
    Running,
    Terminal(ProcessExitClassification),
    Ambiguous,
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "Owner flags track independent I/O, timer, and terminal obligations, not one exclusive state."
)]
struct SessionOwner {
    restore: restore_dispatch::RestoreDispatch,
    recovery: recovery_dispatch::RecoveryDispatch,
    verification: verification_dispatch::VerificationDispatch,
    commands: tool_dispatch::CommandDispatch,
    resources: SessionResources,
    signer: Arc<dyn LaunchSigner>,
    receipts: Vec<SignedReceipt>,
    binding: CapabilityBinding,
    timeout: Duration,
    timer: Arc<dyn SupervisorTimer>,
    broker_loss_grace: Duration,
    broker_loss_deadline: Option<Instant>,
    state: SessionState,
    process_exit: Option<ProcessExitClassification>,
    broker_connection: BrokerConnection,
    channel_state: ChannelState,
    broker_head: ReceiptHead,
    pending: Option<ActiveOperation>,
    deferred: VecDeque<DeferredReceipt>,
    completed: VecDeque<CompletedRequest>,
    failed: VecDeque<FailedRequest>,
    saturated_failed_park: Option<FailedRequest>,
    failure_cache_saturation: Option<ProtocolError>,
    last_failure: Option<ProtocolError>,
    connection_epoch: u64,
    reconnect_request: Option<BrokerReconnect>,
    reconnect_retry_fallback_epoch: Option<u64>,
    reconnect_attempt_epoch: u64,
    active_reconnect_attempt: Option<u64>,
    reconciliation: Option<ReceiptReconciliation>,
    deferred_reconciliation: Option<DeferredReceiptReconciliation>,
    restore_after_reconnect: bool,
    controller_loss_unresolved: bool,
    controller_loss_park_request_id: Option<String>,
    controller_loss_settlement: Option<ActiveControllerLossSettlement>,
    cleanup_unproven: bool,
    quarantined: bool,
    queued_terminal_event: Option<QueuedTerminalEvent>,
    widening_blocked: bool,
    operation_epoch: u64,
    process_epoch: u64,
    relay_quiescence_epoch: Option<u64>,
    relay_quiescence_result: Option<Result<(), SupervisorError>>,
    relay_quiescence_mailbox: Arc<Mutex<Option<Result<(), SupervisorError>>>>,
    finish_after_relay_quiescence: Option<Result<i32, SupervisorError>>,
    finish_when_backlog_drained: Option<Result<i32, SupervisorError>>,
    finished: Option<Result<i32, SupervisorError>>,
    sender: mpsc::SyncSender<OwnerEvent>,
    receiver: mpsc::Receiver<OwnerEvent>,
}

impl SessionOwner {
    #[expect(
        clippy::expect_used,
        reason = "LaunchedSession supplies a nonempty receipt chain; the owner only appends to it."
    )]
    fn new(
        resources: SessionResources,
        signer: Arc<dyn LaunchSigner>,
        receipts: Vec<SignedReceipt>,
        binding: CapabilityBinding,
        timeout: Duration,
        timer: Arc<dyn SupervisorTimer>,
        broker_loss_grace: Duration,
    ) -> Self {
        let broker_head = receipt_head(receipts.last().expect("a launched Session has a receipt"));
        let (sender, receiver) = mpsc::sync_channel(EVENT_QUEUE_CAPACITY);
        Self {
            resources,
            restore: restore_dispatch::RestoreDispatch::default(),
            recovery: recovery_dispatch::RecoveryDispatch::default(),
            verification: verification_dispatch::VerificationDispatch::default(),
            commands: tool_dispatch::CommandDispatch::default(),
            signer,
            receipts,
            binding,
            timeout,
            timer,
            broker_loss_grace,
            broker_loss_deadline: None,
            state: SessionState::Running,
            process_exit: None,
            broker_connection: BrokerConnection::Connected,
            channel_state: ChannelState::Enabled,
            broker_head,
            pending: None,
            deferred: VecDeque::new(),
            completed: VecDeque::new(),
            failed: VecDeque::new(),
            saturated_failed_park: None,
            failure_cache_saturation: None,
            last_failure: None,
            connection_epoch: 1,
            reconnect_request: None,
            reconnect_retry_fallback_epoch: None,
            reconnect_attempt_epoch: 0,
            active_reconnect_attempt: None,
            reconciliation: None,
            deferred_reconciliation: None,
            restore_after_reconnect: false,
            controller_loss_unresolved: false,
            controller_loss_park_request_id: None,
            controller_loss_settlement: None,
            cleanup_unproven: false,
            quarantined: false,
            queued_terminal_event: None,
            widening_blocked: false,
            operation_epoch: 0,
            process_epoch: 1,
            relay_quiescence_epoch: None,
            relay_quiescence_result: None,
            relay_quiescence_mailbox: Arc::new(Mutex::new(None)),
            finish_after_relay_quiescence: None,
            finish_when_backlog_drained: None,
            finished: None,
            sender,
            receiver,
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "One Session owner dispatches its typed events and collects nonblocking completion mailboxes."
    )]
    fn run(
        &mut self,
        controller: mpsc::Receiver<RelayStdio>,
        ready: &mpsc::SyncSender<Result<(), SupervisorError>>,
    ) -> Result<i32, SupervisorError> {
        if let Err(error) = self.start_relay(controller) {
            let _ = ready.send(Err(error.clone()));
            return Err(error);
        }
        self.arm_broker_receive();
        self.arm_agent_receive();
        let _ = ready.send(Ok(()));
        loop {
            let event = self
                .receiver
                .recv()
                .map_err(|_| SupervisorError::WorkerUnavailable)?;
            match event {
                OwnerEvent::CommandDeadline { request_id } => self.command_deadline(&request_id),
                OwnerEvent::ControllerDetached => self.begin_controller_loss(),
                OwnerEvent::BrokerRequest {
                    connection_epoch,
                    result,
                } => self.handle_broker_event(connection_epoch, *result),
                OwnerEvent::Signed {
                    operation_epoch,
                    result,
                } => self.handle_signature(operation_epoch, result),
                OwnerEvent::ReceiptSent {
                    operation_epoch,
                    result,
                } => self.handle_receipt_sent(operation_epoch, &result),
                OwnerEvent::OperationDeadline { operation_epoch } => {
                    self.handle_operation_deadline(operation_epoch);
                }
                OwnerEvent::ResponseSent {
                    connection_epoch,
                    finish,
                    result,
                } => self.handle_response_sent(connection_epoch, finish, result),
                OwnerEvent::BrokerReconnected {
                    connection_epoch,
                    attempt_epoch,
                    result,
                } => self.handle_broker_reconnected(connection_epoch, attempt_epoch, result),
                OwnerEvent::BrokerReconnectDeadline {
                    connection_epoch,
                    attempt_epoch,
                } => self.handle_broker_reconnect_deadline(connection_epoch, attempt_epoch),
                OwnerEvent::BrokerReconnectRetry { connection_epoch } => {
                    self.handle_broker_reconnect_retry(connection_epoch);
                }
                OwnerEvent::BrokerGraceExpired { connection_epoch } => {
                    self.handle_broker_grace_expired(connection_epoch);
                }
                OwnerEvent::ReconciliationReceiptSent {
                    connection_epoch,
                    reconciliation_epoch,
                    receipt_index,
                    result,
                } => self.handle_reconciliation_receipt_sent(
                    connection_epoch,
                    reconciliation_epoch,
                    receipt_index,
                    &result,
                ),
                OwnerEvent::ReconciliationReceiptDeadline {
                    connection_epoch,
                    reconciliation_epoch,
                    receipt_index,
                } => self.handle_reconciliation_receipt_deadline(
                    connection_epoch,
                    reconciliation_epoch,
                    receipt_index,
                ),
                OwnerEvent::DeferredReceiptSigned {
                    connection_epoch,
                    operation_epoch,
                    result,
                } => self.handle_deferred_receipt_signed(connection_epoch, operation_epoch, result),
                OwnerEvent::DeferredReceiptSent {
                    connection_epoch,
                    operation_epoch,
                    result,
                } => self.handle_deferred_receipt_sent(connection_epoch, operation_epoch, &result),
                OwnerEvent::DeferredReceiptDeadline {
                    connection_epoch,
                    operation_epoch,
                } => self.handle_deferred_receipt_deadline(connection_epoch, operation_epoch),
                OwnerEvent::ControllerLossSettled {
                    settlement_epoch,
                    result,
                } => self.handle_controller_loss_settled(settlement_epoch, &result),
                OwnerEvent::ControllerLossDeadline { settlement_epoch } => {
                    self.handle_controller_loss_deadline(settlement_epoch);
                }
                OwnerEvent::RunningAgent {
                    process_epoch,
                    event,
                } if process_epoch == self.process_epoch => self.handle_running_agent_event(event),
                OwnerEvent::RunningAgent { .. }
                | OwnerEvent::RecoveryFinished
                | OwnerEvent::RestoreFinished
                | OwnerEvent::VerificationFinished
                | OwnerEvent::RelayQuiesced
                | OwnerEvent::ToolFinished => {}
            }
            self.collect_relay_quiescence();
            self.collect_recovery();
            self.collect_restore();
            self.collect_verification();
            self.collect_tool_result();
            if let Some(result) = self.finished.take() {
                return result;
            }
        }
    }

    fn start_relay(
        &mut self,
        controller: mpsc::Receiver<RelayStdio>,
    ) -> Result<(), SupervisorError> {
        let process_epoch = self.process_epoch;
        let sender = self.sender.clone();
        self.resources
            .process
            .as_mut()
            .ok_or(SupervisorError::RelayFailed)?
            .start_relay(
                controller,
                Arc::new(move |event| {
                    sender
                        .try_send(OwnerEvent::RunningAgent {
                            process_epoch,
                            event,
                        })
                        .is_ok()
                }),
            )
    }

    fn arm_broker_receive(&self) {
        if self.quarantined {
            return;
        }
        let connection_epoch = self.connection_epoch;
        let sender = self.sender.clone();
        let result = self
            .resources
            .broker
            .as_ref()
            .ok_or(SupervisorError::BrokerUnavailable)
            .and_then(|broker| {
                broker.receive_session_request(Box::new(move |result| {
                    let _ = sender.send(OwnerEvent::BrokerRequest {
                        connection_epoch,
                        result: Box::new(result),
                    });
                }))
            });
        if let Err(error) = result {
            let _ = self.sender.send(OwnerEvent::BrokerRequest {
                connection_epoch,
                result: Box::new(Err(error)),
            });
        }
    }

    fn handle_broker_event(
        &mut self,
        connection_epoch: u64,
        result: Result<ProtocolMessage, SupervisorError>,
    ) {
        if connection_epoch != self.connection_epoch || self.quarantined {
            return;
        }
        let message = match result {
            Ok(message) => message,
            Err(error) => {
                self.lose_broker(error);
                return;
            }
        };
        match message {
            ProtocolMessage::RecoveryRestore(request) => self.handle_restore(*request),
            ProtocolMessage::Recovery(request) => self.handle_recovery(request),
            ProtocolMessage::Verification(request) => self.handle_verification(request),
            ProtocolMessage::Command(request) => self.handle_command(request),
            ProtocolMessage::ToolExecution(request) => self.handle_tool(request),
            ProtocolMessage::Lifecycle(request) => self.handle_lifecycle(request),
            ProtocolMessage::Status(request) => self.handle_status(request),
            ProtocolMessage::BrokerReconnect(reconnect) => {
                self.send_error(
                    reconnect.request_id,
                    ProtocolError::new(
                        ErrorCode::InvalidRequest,
                        Some(self.state),
                        Some(self.broker_head.sequence),
                    ),
                );
            }
            ProtocolMessage::ControllerLossSettlement(settlement) => {
                self.send_error(
                    settlement.request_id,
                    ProtocolError::new(
                        ErrorCode::InvalidRequest,
                        Some(self.state),
                        Some(self.broker_head.sequence),
                    ),
                );
            }
            ProtocolMessage::ReceiptAcknowledgement(acknowledgement) => {
                self.handle_receipt_acknowledgement(&acknowledgement);
            }
            ProtocolMessage::LaunchAuthorization(request) => {
                self.send_error(
                    request.request_id,
                    ProtocolError::new(
                        ErrorCode::InvalidRequest,
                        Some(self.state),
                        Some(self.broker_head.sequence),
                    ),
                );
            }
        }
        if !self.quarantined
            && matches!(
                self.broker_connection,
                BrokerConnection::Connected | BrokerConnection::Reconciling
            )
        {
            self.arm_broker_receive();
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Request validation and compare-and-swap admission are one ordered state-machine boundary."
    )]
    fn handle_lifecycle(&mut self, request: LifecycleRequest) {
        if let Some(failed) = self
            .failed
            .iter()
            .chain(self.saturated_failed_park.iter())
            .find(|failed| failed.request_id == request.request_id)
        {
            if failed.request_digest == request.digest().to_string() {
                let response = failed.response.clone();
                let finish = failed.finish.clone();
                self.send_response_with_finish(response, finish);
            } else {
                self.send_error(
                    request.request_id,
                    ProtocolError::new(
                        ErrorCode::RequestIdConflict,
                        Some(self.state),
                        Some(self.broker_head.sequence),
                    ),
                );
            }
            return;
        }
        let status = self.status();
        if let Some(completed) = self
            .completed
            .iter()
            .find(|completed| completed.request_id == request.request_id)
            .cloned()
        {
            match evaluate_request(&status, Some(&completed), &request) {
                Ok(RequestDisposition::Replay(receipt)) => {
                    let finish = (request.action == LifecycleAction::Disposal
                        && self.state == SessionState::Terminal)
                        .then_some(Ok(0));
                    self.send_response_with_finish(
                        receipt_response(request.request_id, receipt),
                        finish,
                    );
                }
                Ok(RequestDisposition::Execute(_)) => self.send_error(
                    request.request_id,
                    ProtocolError::new(
                        ErrorCode::InvalidRequest,
                        Some(self.state),
                        Some(self.broker_head.sequence),
                    ),
                ),
                Err(error) => self.send_error(request.request_id, error),
            }
            return;
        }
        let pending_receipts = self.pending_receipt_count();
        let pending_limit = match request.action {
            LifecycleAction::Interrupt => MAX_PENDING_RECEIPTS.saturating_sub(2),
            LifecycleAction::Park => MAX_PENDING_RECEIPTS.saturating_sub(1),
            LifecycleAction::Resume => 1,
            LifecycleAction::Disposal => MAX_PENDING_RECEIPTS,
        };
        if request.action != LifecycleAction::Disposal && pending_receipts >= pending_limit {
            self.reject_at_failure_cache_bound(request.request_id);
            return;
        }
        if self.failed.len() == FAILED_REQUEST_CAPACITY
            && request.action != LifecycleAction::Disposal
            && !(request.action == LifecycleAction::Park && self.saturated_failed_park.is_none())
        {
            self.reject_at_failure_cache_bound(request.request_id);
            return;
        }
        if request.action == LifecycleAction::Resume && self.widening_blocked {
            self.send_error(
                request.request_id,
                ProtocolError::new(
                    ErrorCode::DurabilityUnavailable,
                    Some(self.state),
                    Some(self.broker_head.sequence),
                ),
            );
            return;
        }
        if request.action == LifecycleAction::Interrupt
            && (self.controller_loss_unresolved || self.cleanup_unproven)
        {
            self.send_error(
                request.request_id,
                ProtocolError::new(
                    ErrorCode::DurabilityUnavailable,
                    Some(self.state),
                    Some(self.broker_head.sequence),
                ),
            );
            return;
        }
        match evaluate_request(&status, None, &request) {
            Ok(RequestDisposition::Replay(_)) => self.send_error(
                request.request_id,
                ProtocolError::new(
                    ErrorCode::InvalidRequest,
                    Some(self.state),
                    Some(self.broker_head.sequence),
                ),
            ),
            Ok(RequestDisposition::Execute(intent)) if request.action == LifecycleAction::Park => {
                self.begin_park(request, intent);
            }
            Ok(RequestDisposition::Execute(intent))
                if request.action == LifecycleAction::Resume =>
            {
                self.begin_resume(request, intent);
            }
            Ok(RequestDisposition::Execute(intent))
                if request.action == LifecycleAction::Interrupt =>
            {
                self.begin_interrupt(request, intent);
            }
            Ok(RequestDisposition::Execute(intent))
                if request.action == LifecycleAction::Disposal =>
            {
                self.begin_disposal(request, intent);
            }
            Ok(RequestDisposition::Execute(_)) => self.send_error(
                request.request_id,
                ProtocolError::new(
                    ErrorCode::InvalidTransition,
                    Some(self.state),
                    Some(self.broker_head.sequence),
                ),
            ),
            Err(error) => self.send_error(request.request_id, error),
        }
    }

    fn begin_park(&mut self, request: LifecycleRequest, intent: ReceiptIntent) {
        let receipt_backlog = self.has_receipt_backlog();
        let Some(operation_epoch) = self.operation_epoch.checked_add(1) else {
            self.send_error(
                request.request_id,
                ProtocolError::new(
                    ErrorCode::InvalidRequest,
                    Some(self.state),
                    Some(self.broker_head.sequence),
                ),
            );
            return;
        };
        self.operation_epoch = operation_epoch;
        self.pending = Some(ActiveOperation {
            epoch: operation_epoch,
            request,
            intent,
            phase: PendingPhase::Applying,
            receipt: None,
            payload_override: None,
            respond: true,
            finish: None,
        });
        let deadline_armed = self.schedule_operation_deadline(operation_epoch).is_ok();

        let revoke = self
            .resources
            .capability
            .as_mut()
            .ok_or(SupervisorError::CapabilityUnavailable)
            .and_then(|capability| capability.revoke());
        self.channel_state = ChannelState::Revoked;

        match self.attempt_park() {
            ParkResult::Parked => {}
            ParkResult::Running => {
                self.fail_mechanic();
                return;
            }
            ParkResult::Terminal(classification) => {
                self.fail_mechanic_in_state(Some(SessionState::Terminal));
                self.begin_process_exit(classification);
                return;
            }
            ParkResult::Ambiguous => {
                self.quarantine_pending_mechanic();
                return;
            }
        }
        if revoke.is_err() {
            self.fail_mechanic();
            return;
        }
        if receipt_backlog {
            self.fail_receipt_operation(ErrorCode::DurabilityUnavailable);
            return;
        }
        if !deadline_armed {
            self.fail_receipt_operation(ErrorCode::DurabilityUnavailable);
            return;
        }
        if let Some(pending) = self.pending.as_mut() {
            pending.phase = PendingPhase::Signing;
        }
        self.start_signing(operation_epoch);
    }

    fn begin_resume(&mut self, request: LifecycleRequest, intent: ReceiptIntent) {
        let Some(operation_epoch) = self.operation_epoch.checked_add(1) else {
            self.send_error(
                request.request_id,
                ProtocolError::new(
                    ErrorCode::InvalidRequest,
                    Some(self.state),
                    Some(self.broker_head.sequence),
                ),
            );
            return;
        };
        self.operation_epoch = operation_epoch;
        self.pending = Some(ActiveOperation {
            epoch: operation_epoch,
            request,
            intent,
            phase: PendingPhase::Applying,
            receipt: None,
            payload_override: None,
            respond: true,
            finish: None,
        });
        if self.schedule_operation_deadline(operation_epoch).is_err() {
            self.fail_mechanic();
            return;
        }

        let resume = self
            .resources
            .process
            .as_mut()
            .map_or(Err(MechanicFailure::Ambiguous), |process| process.resume());
        match resume {
            Ok(()) | Err(MechanicFailure::Running) => {
                self.state = SessionState::Running;
            }
            Err(MechanicFailure::Parked) => {
                self.state = SessionState::Parked;
                self.fail_mechanic();
                return;
            }
            Err(MechanicFailure::Terminal(classification)) => {
                self.fail_mechanic_in_state(Some(SessionState::Terminal));
                self.begin_process_exit(classification);
                return;
            }
            Err(MechanicFailure::Ambiguous) => {
                self.quarantine_pending_mechanic();
                return;
            }
        }
        if let Some(pending) = self.pending.as_mut() {
            pending.phase = PendingPhase::Signing;
        }
        self.start_signing(operation_epoch);
    }

    fn begin_interrupt(&mut self, request: LifecycleRequest, intent: ReceiptIntent) {
        let receipt_backlog = self.has_receipt_backlog();
        let Some(operation_epoch) = self.operation_epoch.checked_add(1) else {
            self.send_error(
                request.request_id,
                ProtocolError::new(
                    ErrorCode::InvalidRequest,
                    Some(self.state),
                    Some(self.broker_head.sequence),
                ),
            );
            return;
        };
        self.operation_epoch = operation_epoch;
        self.pending = Some(ActiveOperation {
            epoch: operation_epoch,
            request,
            intent,
            phase: PendingPhase::Applying,
            receipt: None,
            payload_override: None,
            respond: true,
            finish: None,
        });
        let deadline_armed = self.schedule_operation_deadline(operation_epoch).is_ok();
        let previous_state = self.state;
        let interrupted = self
            .resources
            .process
            .as_mut()
            .map_or(Err(MechanicFailure::Ambiguous), |process| {
                process.interrupt()
            });
        match interrupted {
            Ok(()) => {}
            Err(MechanicFailure::Running) if previous_state == SessionState::Running => {
                self.state = SessionState::Running;
                self.fail_mechanic();
                return;
            }
            Err(MechanicFailure::Parked) if previous_state == SessionState::Parked => {
                self.state = SessionState::Parked;
                self.fail_mechanic();
                return;
            }
            Err(MechanicFailure::Terminal(classification)) => {
                self.fail_mechanic_in_state(Some(SessionState::Terminal));
                self.begin_process_exit(classification);
                return;
            }
            Err(
                MechanicFailure::Running | MechanicFailure::Parked | MechanicFailure::Ambiguous,
            ) => {
                self.quarantine_pending_mechanic();
                return;
            }
        }
        if receipt_backlog {
            self.fail_receipt_operation(ErrorCode::DurabilityUnavailable);
            return;
        }
        if !deadline_armed {
            self.fail_receipt_operation(ErrorCode::DurabilityUnavailable);
            return;
        }
        if let Some(pending) = self.pending.as_mut() {
            pending.phase = PendingPhase::Signing;
        }
        self.start_signing(operation_epoch);
    }

    #[expect(
        clippy::expect_used,
        reason = "This method sets pending before calling the mechanic; callbacks only enqueue events and cannot remove it here."
    )]
    fn begin_disposal(&mut self, request: LifecycleRequest, intent: ReceiptIntent) {
        let receipt_backlog = self.has_receipt_backlog();
        self.controller_loss_unresolved = false;
        self.controller_loss_park_request_id = None;
        self.controller_loss_settlement = None;
        let Some(operation_epoch) = self.operation_epoch.checked_add(1) else {
            self.send_error(
                request.request_id,
                ProtocolError::new(
                    ErrorCode::InvalidRequest,
                    Some(self.state),
                    Some(self.broker_head.sequence),
                ),
            );
            return;
        };
        self.operation_epoch = operation_epoch;
        self.process_epoch = self.process_epoch.saturating_add(1);
        self.begin_terminal_relay_quiescence();
        self.pending = Some(ActiveOperation {
            epoch: operation_epoch,
            request,
            intent,
            phase: PendingPhase::Applying,
            receipt: None,
            payload_override: None,
            respond: true,
            finish: Some(Ok(0)),
        });
        let deadline_armed = self.schedule_operation_deadline(operation_epoch).is_ok();
        if self.resources.terminate_session().is_err() {
            self.cleanup_unproven = true;
            self.widening_blocked = true;
            self.channel_state = ChannelState::Revoked;
            let request = self
                .pending
                .take()
                .expect("a failed Disposal has a pending operation")
                .request;
            let error = ProtocolError::new(
                ErrorCode::LifecycleMechanicUnavailable,
                Some(self.state),
                Some(self.broker_head.sequence),
            );
            self.last_failure = Some(error.clone());
            self.cache_and_send_error_with_finish(
                request,
                error,
                Some(Err(SupervisorError::CleanupUnproven)),
            );
            return;
        }
        self.cleanup_unproven = false;
        self.state = SessionState::Terminal;
        self.channel_state = ChannelState::Closed;
        if receipt_backlog {
            self.fail_receipt_operation(ErrorCode::DurabilityUnavailable);
            return;
        }
        if !deadline_armed {
            self.fail_receipt_operation(ErrorCode::DurabilityUnavailable);
            return;
        }
        if let Some(pending) = self.pending.as_mut() {
            pending.phase = PendingPhase::Signing;
        }
        self.start_signing(operation_epoch);
    }

    fn begin_process_exit(&mut self, classification: ProcessExitClassification) {
        self.begin_terminal_event(TerminalEvent::ProcessExited(classification));
    }

    fn begin_terminal_event(&mut self, event: TerminalEvent) {
        if self.state == SessionState::Terminal {
            return;
        }
        self.controller_loss_unresolved = false;
        self.controller_loss_park_request_id = None;
        self.controller_loss_settlement = None;
        let previous_state = self.state;
        self.process_epoch = self.process_epoch.saturating_add(1);
        self.begin_terminal_relay_quiescence();
        let cleanup = self.resources.terminate_session();
        self.cleanup_unproven = cleanup.is_err();
        if cleanup.is_err() {
            self.widening_blocked = true;
            self.channel_state = ChannelState::Revoked;
            self.request_finish(Err(SupervisorError::CleanupUnproven));
            return;
        }
        self.state = SessionState::Terminal;
        self.channel_state = ChannelState::Closed;
        if let TerminalEvent::ProcessExited(classification) = event {
            self.process_exit = Some(classification);
        }
        if self.pending.is_some() {
            self.queued_terminal_event = Some(QueuedTerminalEvent {
                event,
                previous_state,
            });
            return;
        }
        self.begin_terminal_receipt(event, previous_state);
    }

    fn begin_terminal_receipt(&mut self, event: TerminalEvent, previous_state: SessionState) {
        let receipt_backlog = self.has_receipt_backlog();
        let Some(operation_epoch) = self.operation_epoch.checked_add(1) else {
            self.request_finish(Err(SupervisorError::CleanupUnproven));
            return;
        };
        let head = self.receipt();
        let Some(sequence) = head.payload.sequence.checked_add(1) else {
            self.request_finish(Err(SupervisorError::CleanupUnproven));
            return;
        };
        let (event_id, authority, finish) = match event {
            TerminalEvent::ProcessExited(classification) => (
                "exit",
                ReceiptAuthority::ProcessExited { classification },
                Ok(i32::from(
                    classification != ProcessExitClassification::Success,
                )),
            ),
            TerminalEvent::RelayFailed => (
                "relay-failed",
                ReceiptAuthority::Cause {
                    cause: ReceiptCause::RelayFailed,
                },
                Err(SupervisorError::RelayFailed),
            ),
            TerminalEvent::AgentIdentityLost => (
                "agent-identity-lost",
                ReceiptAuthority::Cause {
                    cause: ReceiptCause::AgentIdentityLost,
                },
                Err(SupervisorError::AgentIdentityRejected),
            ),
        };
        let request_id = format!("{event_id}-{}", head.digest().hex());
        let request = LifecycleRequest {
            schema: LIFECYCLE_REQUEST_SCHEMA.to_owned(),
            protocol_version: PROTOCOL_VERSION,
            request_id: request_id.clone(),
            session_id: self.binding.session_id.clone(),
            run_id: head.payload.run_id.clone(),
            authorization_id: event_id.to_owned(),
            action: LifecycleAction::Disposal,
            expected_state: previous_state,
            expected_receipt_sequence: Some(head.payload.sequence),
            envelope_revision: self.binding.envelope_revision,
        };
        let intent = ReceiptIntent {
            session_id: self.binding.session_id.clone(),
            run_id: head.payload.run_id.clone(),
            authorization_id: event_id.to_owned(),
            request_id: request_id.clone(),
            request_digest: request.digest().to_string(),
            action: LifecycleAction::Disposal,
            envelope_revision: self.binding.envelope_revision,
            sequence,
            previous_receipt_digest: head.digest().to_string(),
            resulting_state: SessionState::Terminal,
        };
        let payload = ReceiptPayload {
            schema: RECEIPT_SCHEMA.to_owned(),
            session_id: self.binding.session_id.clone(),
            run_id: head.payload.run_id.clone(),
            request_id,
            envelope_revision: self.binding.envelope_revision,
            sequence,
            previous_receipt_digest: Some(head.digest().to_string()),
            release_id: self.signer.release_id().to_owned(),
            signing_key_id: self.signer.signing_key_id().to_owned(),
            outcome: ReceiptOutcome::Disposal { authority },
            resulting_state: SessionState::Terminal,
        };
        self.operation_epoch = operation_epoch;
        self.pending = Some(ActiveOperation {
            epoch: operation_epoch,
            request,
            intent,
            phase: PendingPhase::Applying,
            receipt: None,
            payload_override: Some(payload),
            respond: false,
            finish: Some(finish),
        });
        if receipt_backlog {
            self.fail_receipt_operation(ErrorCode::DurabilityUnavailable);
            return;
        }
        let deadline_armed = self.schedule_operation_deadline(operation_epoch).is_ok();
        if !deadline_armed {
            self.fail_receipt_operation(ErrorCode::DurabilityUnavailable);
            return;
        }
        if let Some(pending) = self.pending.as_mut() {
            pending.phase = PendingPhase::Signing;
        }
        self.start_signing(operation_epoch);
    }

    fn handle_running_agent_event(&mut self, event: RunningAgentEvent) {
        match event {
            RunningAgentEvent::ControllerEof => self.begin_controller_loss(),
            RunningAgentEvent::ProcessExited(classification) => {
                self.begin_process_exit(classification);
            }
            RunningAgentEvent::RelayFailed => {
                self.begin_terminal_event(TerminalEvent::RelayFailed);
            }
            RunningAgentEvent::AgentIdentityLost => {
                self.begin_terminal_event(TerminalEvent::AgentIdentityLost);
            }
        }
    }

    fn begin_terminal_relay_quiescence(&mut self) {
        if self.relay_quiescence_epoch.is_some() {
            return;
        }
        let process_epoch = self.process_epoch;
        self.relay_quiescence_epoch = Some(process_epoch);
        self.relay_quiescence_result = None;
        let sender = self.sender.clone();
        let mailbox = Arc::clone(&self.relay_quiescence_mailbox);
        let result = self
            .resources
            .process
            .as_mut()
            .ok_or(SupervisorError::RelayFailed)
            .and_then(|process| {
                process.quiesce_relay(Box::new(move |result| {
                    *mailbox
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(result);
                    // If full, an already-queued event wakes the owner, which
                    // checks the mailbox after every dispatch. Never block a join.
                    let _ = sender.try_send(OwnerEvent::RelayQuiesced);
                }))
            });
        if let Err(error) = result {
            self.handle_relay_quiesced(process_epoch, Err(error));
        }
    }

    fn collect_relay_quiescence(&mut self) {
        let result = self
            .relay_quiescence_mailbox
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let (Some(process_epoch), Some(result)) = (self.relay_quiescence_epoch, result) {
            self.handle_relay_quiesced(process_epoch, result);
        }
    }

    fn handle_relay_quiesced(&mut self, process_epoch: u64, result: Result<(), SupervisorError>) {
        if self.relay_quiescence_epoch != Some(process_epoch)
            || self.relay_quiescence_result.is_some()
        {
            return;
        }
        self.relay_quiescence_result = Some(result);
        if let Some(finish) = self.finish_after_relay_quiescence.take() {
            self.request_finish(finish);
        }
    }

    fn request_finish(&mut self, finish: Result<i32, SupervisorError>) {
        if !self.quarantined && self.state == SessionState::Terminal && self.has_receipt_backlog() {
            self.finish_when_backlog_drained.get_or_insert(finish);
            return;
        }
        let Some(quiescence) = self.relay_quiescence_result.as_ref() else {
            self.finish_after_relay_quiescence.get_or_insert(finish);
            return;
        };
        self.finished = Some(match (finish, quiescence) {
            (Err(error), _) => Err(error),
            (Ok(code), Ok(())) => Ok(code),
            (Ok(_), Err(_)) => Err(SupervisorError::RelayFailed),
        });
    }

    fn begin_controller_loss(&mut self) {
        if self.state == SessionState::Terminal
            || self.controller_loss_unresolved
            || self.controller_loss_park_request_id.is_some()
            || self.controller_loss_settlement.is_some()
        {
            return;
        }
        let already_parked = self.state == SessionState::Parked;
        self.controller_loss_unresolved = true;
        self.widening_blocked = true;
        if self.pending.is_some() {
            self.fail_receipt_operation(ErrorCode::DurabilityUnavailable);
        }
        let revoke = self
            .resources
            .capability
            .as_mut()
            .ok_or(SupervisorError::CapabilityUnavailable)
            .and_then(|capability| capability.revoke());
        self.channel_state = ChannelState::Revoked;
        let park = if self.state == SessionState::Parked {
            ParkResult::Parked
        } else {
            self.attempt_park()
        };
        match park {
            ParkResult::Parked | ParkResult::Running => {}
            ParkResult::Terminal(classification) => {
                self.begin_process_exit(classification);
                return;
            }
            ParkResult::Ambiguous => {
                self.quarantine_mechanic(None);
                return;
            }
        }
        if revoke.is_err() || self.state != SessionState::Parked {
            self.last_failure = Some(ProtocolError::new(
                ErrorCode::LifecycleMechanicUnavailable,
                Some(self.state),
                Some(self.broker_head.sequence),
            ));
            return;
        }
        if already_parked {
            self.maybe_begin_controller_loss_settlement();
        } else {
            self.begin_controller_loss_park();
        }
    }

    fn begin_controller_loss_park(&mut self) {
        let receipt_backlog = self.has_receipt_backlog();
        let Some(operation_epoch) = self.operation_epoch.checked_add(1) else {
            self.last_failure = Some(ProtocolError::new(
                ErrorCode::DurabilityUnavailable,
                Some(self.state),
                Some(self.broker_head.sequence),
            ));
            return;
        };
        let head = self.receipt();
        let Some(sequence) = head.payload.sequence.checked_add(1) else {
            self.last_failure = Some(ProtocolError::new(
                ErrorCode::DurabilityUnavailable,
                Some(self.state),
                Some(self.broker_head.sequence),
            ));
            return;
        };
        let request_id = format!("controller-loss-park-{}", head.digest().hex());
        let request = LifecycleRequest {
            schema: LIFECYCLE_REQUEST_SCHEMA.to_owned(),
            protocol_version: PROTOCOL_VERSION,
            request_id: request_id.clone(),
            session_id: self.binding.session_id.clone(),
            run_id: head.payload.run_id.clone(),
            authorization_id: "controller-loss".to_owned(),
            action: LifecycleAction::Park,
            expected_state: SessionState::Running,
            expected_receipt_sequence: Some(head.payload.sequence),
            envelope_revision: self.binding.envelope_revision,
        };
        let intent = ReceiptIntent {
            session_id: self.binding.session_id.clone(),
            run_id: head.payload.run_id.clone(),
            authorization_id: "controller-loss".to_owned(),
            request_id: request_id.clone(),
            request_digest: request.digest().to_string(),
            action: LifecycleAction::Park,
            envelope_revision: self.binding.envelope_revision,
            sequence,
            previous_receipt_digest: head.digest().to_string(),
            resulting_state: SessionState::Parked,
        };
        let payload = ReceiptPayload {
            schema: RECEIPT_SCHEMA.to_owned(),
            session_id: self.binding.session_id.clone(),
            run_id: head.payload.run_id.clone(),
            request_id: request_id.clone(),
            envelope_revision: self.binding.envelope_revision,
            sequence,
            previous_receipt_digest: Some(head.digest().to_string()),
            release_id: self.signer.release_id().to_owned(),
            signing_key_id: self.signer.signing_key_id().to_owned(),
            outcome: ReceiptOutcome::Park {
                authority: ReceiptAuthority::Cause {
                    cause: ReceiptCause::ControllerLost,
                },
            },
            resulting_state: SessionState::Parked,
        };
        self.operation_epoch = operation_epoch;
        self.controller_loss_park_request_id = Some(request_id);
        self.pending = Some(ActiveOperation {
            epoch: operation_epoch,
            request,
            intent,
            phase: PendingPhase::Signing,
            receipt: None,
            payload_override: Some(payload),
            respond: false,
            finish: None,
        });
        if receipt_backlog {
            self.fail_receipt_operation(ErrorCode::DurabilityUnavailable);
            return;
        }
        if self.schedule_operation_deadline(operation_epoch).is_err() {
            self.fail_receipt_operation(ErrorCode::DurabilityUnavailable);
            return;
        }
        self.start_signing(operation_epoch);
    }

    fn begin_controller_loss_settlement(&mut self) {
        let Some(settlement_epoch) = self.operation_epoch.checked_add(1) else {
            self.last_failure = Some(ProtocolError::new(
                ErrorCode::DurabilityUnavailable,
                Some(self.state),
                Some(self.broker_head.sequence),
            ));
            return;
        };
        self.operation_epoch = settlement_epoch;
        let settlement = ControllerLossSettlement {
            schema: CONTROLLER_LOSS_SETTLEMENT_SCHEMA.to_owned(),
            protocol_version: PROTOCOL_VERSION,
            request_id: format!("controller-loss-settle-{}", self.receipt().digest().hex()),
            session_id: self.binding.session_id.clone(),
            run_id: self.receipt().payload.run_id.clone(),
            envelope_revision: self.binding.envelope_revision,
            parked_head: self.broker_head.clone(),
        };
        if settlement.validate().is_err() {
            self.last_failure = Some(ProtocolError::new(
                ErrorCode::DurabilityUnavailable,
                Some(self.state),
                Some(self.broker_head.sequence),
            ));
            return;
        }
        self.controller_loss_settlement = Some(ActiveControllerLossSettlement {
            epoch: settlement_epoch,
            request: settlement.clone(),
        });
        let deadline_sender = self.sender.clone();
        if self
            .timer
            .schedule(
                self.timeout,
                Box::new(move || {
                    let _ = deadline_sender
                        .send(OwnerEvent::ControllerLossDeadline { settlement_epoch });
                }),
            )
            .is_err()
        {
            self.fail_controller_loss_settlement();
            return;
        }
        let sender = self.sender.clone();
        let result = self
            .resources
            .broker
            .as_ref()
            .ok_or(SupervisorError::BrokerUnavailable)
            .and_then(|broker| {
                broker.settle_controller_loss(
                    settlement,
                    Box::new(move |result| {
                        let _ = sender.send(OwnerEvent::ControllerLossSettled {
                            settlement_epoch,
                            result,
                        });
                    }),
                )
            });
        if result.is_err() {
            self.fail_controller_loss_settlement();
        }
    }

    fn handle_controller_loss_settled(
        &mut self,
        settlement_epoch: u64,
        result: &Result<ControllerLossAcknowledgement, SupervisorError>,
    ) {
        let Some(active) = self.controller_loss_settlement.as_ref() else {
            return;
        };
        if active.epoch != settlement_epoch {
            return;
        }
        let valid = result
            .as_ref()
            .is_ok_and(|acknowledgement| acknowledgement.validate_for(&active.request).is_ok());
        if !valid {
            self.fail_controller_loss_settlement();
            return;
        }
        self.controller_loss_settlement = None;
        self.begin_controller_loss_disposal();
    }

    fn handle_controller_loss_deadline(&mut self, settlement_epoch: u64) {
        if self
            .controller_loss_settlement
            .as_ref()
            .is_some_and(|active| active.epoch == settlement_epoch)
        {
            self.fail_controller_loss_settlement();
        }
    }

    fn fail_controller_loss_settlement(&mut self) {
        self.controller_loss_settlement = None;
        self.widening_blocked = true;
        self.last_failure = Some(ProtocolError::new(
            ErrorCode::DurabilityUnavailable,
            Some(self.state),
            Some(self.broker_head.sequence),
        ));
    }

    fn clear_durable_controller_loss_park_marker(&mut self) {
        let Some(request_id) = self.controller_loss_park_request_id.as_deref() else {
            return;
        };
        let park_is_durable = self.receipts.iter().any(|receipt| {
            receipt.payload.request_id == request_id
                && receipt.payload.sequence <= self.broker_head.sequence
        });
        if park_is_durable {
            self.controller_loss_park_request_id = None;
        }
    }

    fn maybe_begin_controller_loss_settlement(&mut self) {
        if self.controller_loss_unresolved
            && self.controller_loss_park_request_id.is_none()
            && self.controller_loss_settlement.is_none()
            && self.state == SessionState::Parked
            && self.broker_connection == BrokerConnection::Connected
            && self.pending.is_none()
            && self.reconciliation.is_none()
            && self.deferred_reconciliation.is_none()
            && !self.has_receipt_backlog()
        {
            self.begin_controller_loss_settlement();
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Controller loss follows one fail-closed sequence through relay shutdown, disposal and receipt settlement."
    )]
    fn begin_controller_loss_disposal(&mut self) {
        let receipt_backlog = self.has_receipt_backlog();
        let Some(operation_epoch) = self.operation_epoch.checked_add(1) else {
            self.last_failure = Some(ProtocolError::new(
                ErrorCode::DurabilityUnavailable,
                Some(self.state),
                Some(self.broker_head.sequence),
            ));
            return;
        };
        let head = self.receipt();
        let Some(sequence) = head.payload.sequence.checked_add(1) else {
            self.last_failure = Some(ProtocolError::new(
                ErrorCode::DurabilityUnavailable,
                Some(self.state),
                Some(self.broker_head.sequence),
            ));
            return;
        };
        let request_id = format!("controller-loss-dispose-{}", head.digest().hex());
        let request = LifecycleRequest {
            schema: LIFECYCLE_REQUEST_SCHEMA.to_owned(),
            protocol_version: PROTOCOL_VERSION,
            request_id: request_id.clone(),
            session_id: self.binding.session_id.clone(),
            run_id: head.payload.run_id.clone(),
            authorization_id: "controller-loss".to_owned(),
            action: LifecycleAction::Disposal,
            expected_state: SessionState::Parked,
            expected_receipt_sequence: Some(head.payload.sequence),
            envelope_revision: self.binding.envelope_revision,
        };
        let intent = ReceiptIntent {
            session_id: self.binding.session_id.clone(),
            run_id: head.payload.run_id.clone(),
            authorization_id: "controller-loss".to_owned(),
            request_id: request_id.clone(),
            request_digest: request.digest().to_string(),
            action: LifecycleAction::Disposal,
            envelope_revision: self.binding.envelope_revision,
            sequence,
            previous_receipt_digest: head.digest().to_string(),
            resulting_state: SessionState::Terminal,
        };
        let payload = ReceiptPayload {
            schema: RECEIPT_SCHEMA.to_owned(),
            session_id: self.binding.session_id.clone(),
            run_id: head.payload.run_id.clone(),
            request_id,
            envelope_revision: self.binding.envelope_revision,
            sequence,
            previous_receipt_digest: Some(head.digest().to_string()),
            release_id: self.signer.release_id().to_owned(),
            signing_key_id: self.signer.signing_key_id().to_owned(),
            outcome: ReceiptOutcome::Disposal {
                authority: ReceiptAuthority::Cause {
                    cause: ReceiptCause::ControllerLost,
                },
            },
            resulting_state: SessionState::Terminal,
        };
        self.operation_epoch = operation_epoch;
        self.process_epoch = self.process_epoch.saturating_add(1);
        self.begin_terminal_relay_quiescence();
        self.pending = Some(ActiveOperation {
            epoch: operation_epoch,
            request,
            intent,
            phase: PendingPhase::Applying,
            receipt: None,
            payload_override: Some(payload),
            respond: false,
            finish: Some(Ok(0)),
        });
        let deadline_armed = self.schedule_operation_deadline(operation_epoch).is_ok();
        if self.resources.terminate_session().is_err() {
            self.cleanup_unproven = true;
            self.widening_blocked = true;
            self.pending = None;
            self.last_failure = Some(ProtocolError::new(
                ErrorCode::LifecycleMechanicUnavailable,
                Some(self.state),
                Some(self.broker_head.sequence),
            ));
            return;
        }
        self.cleanup_unproven = false;
        self.controller_loss_unresolved = false;
        self.state = SessionState::Terminal;
        self.channel_state = ChannelState::Closed;
        if receipt_backlog {
            self.fail_receipt_operation(ErrorCode::DurabilityUnavailable);
            return;
        }
        if !deadline_armed {
            self.fail_receipt_operation(ErrorCode::DurabilityUnavailable);
            return;
        }
        if let Some(pending) = self.pending.as_mut() {
            pending.phase = PendingPhase::Signing;
        }
        self.start_signing(operation_epoch);
    }

    fn schedule_operation_deadline(&self, operation_epoch: u64) -> Result<(), SupervisorError> {
        let sender = self.sender.clone();
        self.timer.schedule(
            self.timeout,
            Box::new(move || {
                let _ = sender.send(OwnerEvent::OperationDeadline { operation_epoch });
            }),
        )
    }

    fn operation_payload(
        &self,
        operation: &ActiveOperation,
    ) -> Result<ReceiptPayload, ReceiptError> {
        match &operation.payload_override {
            Some(payload) => {
                payload.validate()?;
                Ok(payload.clone())
            }
            None => operation
                .intent
                .receipt_payload(self.signer.release_id(), self.signer.signing_key_id()),
        }
    }

    fn start_signing(&mut self, operation_epoch: u64) {
        let Some(Ok(payload)) = self.pending.as_ref().and_then(|pending| {
            (pending.epoch == operation_epoch).then(|| self.operation_payload(pending))
        }) else {
            self.fail_receipt_operation(ErrorCode::SigningUnavailable);
            return;
        };
        let sender = self.sender.clone();
        if self
            .signer
            .sign(
                payload.canonical_bytes(),
                Box::new(move |result| {
                    let _ = sender.send(OwnerEvent::Signed {
                        operation_epoch,
                        result,
                    });
                }),
            )
            .is_err()
        {
            self.fail_receipt_operation(ErrorCode::SigningUnavailable);
        }
    }

    fn handle_signature(&mut self, operation_epoch: u64, result: Result<String, SupervisorError>) {
        if !self.pending_matches(operation_epoch, PendingPhase::Signing) {
            return;
        }
        let Ok(signature) = result else {
            self.fail_receipt_operation(ErrorCode::SigningUnavailable);
            return;
        };
        let Some(payload) = self
            .pending
            .as_ref()
            .and_then(|pending| self.operation_payload(pending).ok())
        else {
            self.fail_receipt_operation(ErrorCode::SigningUnavailable);
            return;
        };
        let receipt = SignedReceipt {
            schema: SIGNED_RECEIPT_SCHEMA.to_owned(),
            payload,
            signature,
        };
        if receipt.validate().is_err() {
            self.fail_receipt_operation(ErrorCode::SigningUnavailable);
            return;
        }
        let bytes = receipt.canonical_bytes();
        self.receipts.push(receipt.clone());
        if let Some(pending) = self.pending.as_mut() {
            pending.phase = PendingPhase::AwaitingDurableAck;
            pending.receipt = Some(receipt);
        }
        let sender = self.sender.clone();
        let result = self
            .resources
            .broker
            .as_ref()
            .ok_or(SupervisorError::BrokerUnavailable)
            .and_then(|broker| {
                broker.send_session_receipt(
                    bytes,
                    Box::new(move |result| {
                        let _ = sender.send(OwnerEvent::ReceiptSent {
                            operation_epoch,
                            result,
                        });
                    }),
                )
            });
        if let Err(error) = result {
            self.handle_receipt_sent(operation_epoch, &Err(error));
        }
    }

    fn handle_receipt_sent(&mut self, operation_epoch: u64, result: &Result<(), SupervisorError>) {
        if !self.pending_matches(operation_epoch, PendingPhase::AwaitingDurableAck) {
            return;
        }
        if result.is_err() {
            self.fail_receipt_operation(ErrorCode::DurabilityUnavailable);
        }
    }

    fn handle_operation_deadline(&mut self, operation_epoch: u64) {
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.epoch == operation_epoch)
        {
            self.fail_receipt_operation(ErrorCode::DurabilityUnavailable);
        }
    }

    #[expect(
        clippy::expect_used,
        reason = "AwaitingDurableAck is entered only after handle_signature stores the signed receipt."
    )]
    fn handle_receipt_acknowledgement(&mut self, acknowledgement: &ReceiptAcknowledgement) {
        if self.deferred_reconciliation.is_some() {
            self.handle_deferred_receipt_acknowledgement(acknowledgement);
            return;
        }
        if self.reconciliation.is_some() {
            self.handle_reconciliation_acknowledgement(acknowledgement);
            return;
        }
        let Some(pending) = self.pending.as_ref() else {
            return;
        };
        if pending.phase != PendingPhase::AwaitingDurableAck {
            self.fail_receipt_operation(ErrorCode::ReceiptChainInvalid);
            return;
        }
        let receipt = pending
            .receipt
            .as_ref()
            .expect("an acknowledgement wait owns a signed receipt");
        let head = receipt_head(receipt);
        match acknowledgement.exact_disposition(
            &receipt.payload.session_id,
            &receipt.payload.run_id,
            &head,
        ) {
            Some(ReceiptDisposition::DurablyStored) => {}
            Some(ReceiptDisposition::Rejected) => {
                self.fail_receipt_operation(ErrorCode::DurabilityUnavailable);
                return;
            }
            None => {
                self.fail_receipt_operation(ErrorCode::ReceiptChainInvalid);
                return;
            }
        }

        self.broker_head = head;
        self.complete_pending_receipt(true);
    }

    #[expect(
        clippy::expect_used,
        reason = "Both callers first prove a pending signed receipt matches the acknowledged broker head."
    )]
    fn complete_pending_receipt(&mut self, permit_resume_enable: bool) {
        let pending = self.pending.take().expect("pending operation was checked");
        let starts_controller_loss_settlement = self
            .controller_loss_park_request_id
            .as_deref()
            .is_some_and(|request_id| request_id == pending.request.request_id);
        let receipt = pending
            .receipt
            .expect("an acknowledged operation owns its receipt");
        if pending.respond {
            self.remember_completed(CompletedRequest::new(&pending.request, receipt.clone()));
        }
        if pending.request.action == LifecycleAction::Resume
            && permit_resume_enable
            && self.state != SessionState::Terminal
        {
            let enabled = self
                .resources
                .capability
                .as_mut()
                .ok_or(SupervisorError::CapabilityUnavailable)
                .and_then(|capability| capability.enable_after_resume());
            if enabled.is_ok() {
                self.channel_state = ChannelState::Enabled;
                self.last_failure = None;
                self.resume_command_reception();
            } else {
                self.channel_state = ChannelState::Revoked;
                self.last_failure = Some(ProtocolError::new(
                    ErrorCode::LifecycleMechanicUnavailable,
                    Some(self.state),
                    Some(self.broker_head.sequence),
                ));
            }
        } else if pending.request.action != LifecycleAction::Resume {
            self.last_failure = None;
            if pending.request.action == LifecycleAction::Park
                && !self.controller_loss_unresolved
                && !self.cleanup_unproven
                && !self.has_receipt_backlog()
            {
                self.widening_blocked = false;
            }
        }
        if pending.respond {
            let response = receipt_response(pending.request.request_id, receipt);
            self.send_response_with_finish(response, pending.finish);
        } else if let Some(finish) = pending.finish {
            self.request_finish(finish);
        }
        if starts_controller_loss_settlement {
            self.controller_loss_park_request_id = None;
        }
        if let Some(terminal) = self.queued_terminal_event.take() {
            self.begin_terminal_receipt(terminal.event, terminal.previous_state);
        } else if starts_controller_loss_settlement {
            self.maybe_begin_controller_loss_settlement();
        }
    }

    fn handle_status(&mut self, request: StatusRequest) {
        let result = request.validate().and_then(|()| {
            if request.session_id == self.binding.session_id
                && request.run_id == self.receipt().payload.run_id
            {
                Ok(self.status())
            } else {
                Err(ProtocolError::new(
                    ErrorCode::SubjectMismatch,
                    Some(self.state),
                    Some(self.broker_head.sequence),
                ))
            }
        });
        match result {
            Ok(status) => self.send_response(ProtocolResponse {
                schema: RESPONSE_SCHEMA.to_owned(),
                protocol_version: PROTOCOL_VERSION,
                request_id: request.request_id,
                result: ResponseResult::SupervisorStatus { status },
            }),
            Err(error) => self.send_error(request.request_id, error),
        }
    }

    fn send_error(&mut self, request_id: String, error: ProtocolError) {
        self.send_response(ProtocolResponse {
            schema: RESPONSE_SCHEMA.to_owned(),
            protocol_version: PROTOCOL_VERSION,
            request_id,
            result: ResponseResult::Error { error },
        });
    }

    fn send_response(&mut self, response: ProtocolResponse) {
        self.send_response_with_finish(response, None);
    }

    fn send_response_with_finish(
        &mut self,
        response: ProtocolResponse,
        finish: Option<Result<i32, SupervisorError>>,
    ) {
        if response.validate().is_err() {
            self.last_failure = Some(ProtocolError::new(
                ErrorCode::InvalidRequest,
                Some(self.state),
                Some(self.broker_head.sequence),
            ));
            return;
        }
        let connection_epoch = self.connection_epoch;
        let sender = self.sender.clone();
        let callback_finish = finish.clone();
        let result = self
            .resources
            .broker
            .as_ref()
            .ok_or(SupervisorError::BrokerUnavailable)
            .and_then(|broker| {
                broker.send_session_response(
                    response,
                    Box::new(move |result| {
                        let _ = sender.send(OwnerEvent::ResponseSent {
                            connection_epoch,
                            finish: callback_finish,
                            result,
                        });
                    }),
                )
            });
        if let Err(error) = result {
            self.handle_response_sent(connection_epoch, finish, Err(error));
        }
    }

    fn handle_response_sent(
        &mut self,
        connection_epoch: u64,
        finish: Option<Result<i32, SupervisorError>>,
        result: Result<(), SupervisorError>,
    ) {
        if connection_epoch != self.connection_epoch {
            return;
        }
        match result {
            Ok(()) => {
                if let Some(finish) = finish {
                    self.request_finish(finish);
                }
            }
            Err(error) => self.lose_broker(error),
        }
    }

    fn fail_mechanic(&mut self) {
        self.fail_mechanic_in_state(Some(self.state));
    }

    fn attempt_park(&mut self) -> ParkResult {
        match self
            .resources
            .process
            .as_mut()
            .map_or(Err(MechanicFailure::Ambiguous), |process| process.park())
        {
            Ok(()) | Err(MechanicFailure::Parked) => {
                self.state = SessionState::Parked;
                ParkResult::Parked
            }
            Err(MechanicFailure::Running) => {
                self.state = SessionState::Running;
                ParkResult::Running
            }
            Err(MechanicFailure::Terminal(classification)) => ParkResult::Terminal(classification),
            Err(MechanicFailure::Ambiguous) => ParkResult::Ambiguous,
        }
    }

    #[expect(
        clippy::expect_used,
        reason = "Only an admitted pending lifecycle mechanic calls this handler; owner events are serialized."
    )]
    fn fail_mechanic_in_state(&mut self, current_state: Option<SessionState>) {
        let request = self
            .pending
            .take()
            .expect("a mechanic failure has a pending operation")
            .request;
        self.widening_blocked = true;
        if self.channel_state == ChannelState::Enabled {
            if let Some(capability) = self.resources.capability.as_mut() {
                let _ = capability.revoke();
            }
            self.channel_state = ChannelState::Revoked;
        }
        let error = ProtocolError::new(
            ErrorCode::LifecycleMechanicUnavailable,
            current_state,
            Some(self.broker_head.sequence),
        );
        self.last_failure = Some(error.clone());
        self.cache_and_send_error(request, error);
    }

    #[expect(
        clippy::expect_used,
        reason = "Only an admitted pending lifecycle mechanic calls this ambiguity handler; owner events are serialized."
    )]
    fn quarantine_pending_mechanic(&mut self) {
        let pending = self
            .pending
            .take()
            .expect("an ambiguous mechanic failure has a pending operation");
        self.quarantine_mechanic(Some(pending));
    }

    fn quarantine_mechanic(&mut self, pending: Option<ActiveOperation>) {
        self.quarantined = true;
        self.widening_blocked = true;
        self.controller_loss_unresolved = false;
        self.controller_loss_park_request_id = None;
        self.controller_loss_settlement = None;
        self.queued_terminal_event = None;
        self.finish_when_backlog_drained = None;
        self.process_epoch = self.process_epoch.saturating_add(1);
        self.begin_terminal_relay_quiescence();
        let cleanup = self.resources.terminate_session();
        self.cleanup_unproven = cleanup.is_err();
        self.channel_state = ChannelState::Closed;
        if cleanup.is_ok() {
            self.state = SessionState::Terminal;
        }
        let current_state = cleanup.is_ok().then_some(SessionState::Terminal);
        let error = ProtocolError::new(
            ErrorCode::LifecycleMechanicUnavailable,
            current_state,
            Some(self.broker_head.sequence),
        );
        self.last_failure = Some(error.clone());
        let finish = Err(if cleanup.is_ok() {
            SupervisorError::LifecycleMechanicUnavailable
        } else {
            SupervisorError::CleanupUnproven
        });
        match pending {
            Some(pending) if pending.respond => {
                self.cache_and_send_error_with_finish(pending.request, error, Some(finish));
            }
            Some(_) | None => self.request_finish(finish),
        }
    }

    fn fail_receipt_operation(&mut self, code: ErrorCode) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        let resume_failed = pending.request.action == LifecycleAction::Resume;
        let finish = pending.finish.clone();
        self.widening_blocked = true;
        if self.channel_state == ChannelState::Enabled {
            if let Some(capability) = self.resources.capability.as_mut() {
                let _ = capability.revoke();
            }
            self.channel_state = ChannelState::Revoked;
        }
        let unsigned_payload = pending
            .receipt
            .is_none()
            .then(|| self.operation_payload(&pending));
        let mut receipt_retained = pending.receipt.is_some();
        let mut resume_park_result = None;
        // Terminal cleanup already closed the channel and disposed the tree.
        // Retain the earlier Resume audit without attempting a fictitious Park.
        if resume_failed && self.state != SessionState::Terminal {
            resume_park_result = Some(self.repark_after_failed_resume());
            if let Some(Ok(payload)) = unsigned_payload {
                receipt_retained |= self.defer(DeferredReceipt::Payload(Box::new(payload)));
            }
            if matches!(resume_park_result, Some(ParkResult::Parked)) {
                let request_digest = pending.request.digest();
                receipt_retained |= self.defer(DeferredReceipt::CausalPark {
                    request_id: format!("ack-failed-{}", request_digest.hex()),
                    envelope_revision: pending.request.envelope_revision,
                    cause: ReceiptCause::AcknowledgementFailed,
                });
            }
        } else if let Some(Ok(payload)) = unsigned_payload {
            receipt_retained |= self.defer(DeferredReceipt::Payload(Box::new(payload)));
        }
        if self.controller_loss_park_request_id.as_deref()
            == Some(pending.request.request_id.as_str())
            && !receipt_retained
        {
            self.controller_loss_park_request_id = None;
        }
        if matches!(resume_park_result, Some(ParkResult::Ambiguous)) {
            self.quarantine_mechanic(Some(pending));
            return;
        }
        let terminal_classification = match resume_park_result {
            Some(ParkResult::Terminal(classification)) => Some(classification),
            _ => None,
        };
        let error = ProtocolError::new(
            code,
            terminal_classification
                .map(|_| SessionState::Terminal)
                .or(Some(self.state)),
            Some(self.broker_head.sequence),
        );
        self.last_failure = Some(error.clone());
        if pending.respond {
            self.cache_and_send_error_with_finish(pending.request, error, finish);
        } else if let Some(finish) = finish {
            self.request_finish(finish);
        }
        if let Some(classification) = terminal_classification {
            self.queued_terminal_event = None;
            self.begin_process_exit(classification);
        } else if let Some(terminal) = self.queued_terminal_event.take() {
            self.begin_terminal_receipt(terminal.event, terminal.previous_state);
        }
    }

    fn repark_after_failed_resume(&mut self) -> ParkResult {
        if let Some(capability) = self.resources.capability.as_mut() {
            let _ = capability.revoke();
        }
        if self.channel_state != ChannelState::Closed {
            self.channel_state = ChannelState::Revoked;
        }
        self.attempt_park()
    }

    fn defer(&mut self, receipt: DeferredReceipt) -> bool {
        debug_assert!(receipt.validate());
        let signed_gap = usize::try_from(
            self.receipt()
                .payload
                .sequence
                .saturating_sub(self.broker_head.sequence),
        )
        .unwrap_or(usize::MAX);
        if (signed_gap.saturating_add(self.deferred.len()) as u64) < u64::from(MAX_PENDING_RECEIPTS)
        {
            self.deferred.push_back(receipt);
            true
        } else {
            self.last_failure = Some(ProtocolError::new(
                ErrorCode::DurabilityUnavailable,
                Some(self.state),
                Some(self.broker_head.sequence),
            ));
            false
        }
    }

    fn cache_and_send_error(&mut self, request: LifecycleRequest, error: ProtocolError) {
        self.cache_and_send_error_with_finish(request, error, None);
    }

    fn cache_and_send_error_with_finish(
        &mut self,
        request: LifecycleRequest,
        error: ProtocolError,
        finish: Option<Result<i32, SupervisorError>>,
    ) {
        let request_digest = request.digest().to_string();
        let response = ProtocolResponse {
            schema: RESPONSE_SCHEMA.to_owned(),
            protocol_version: PROTOCOL_VERSION,
            request_id: request.request_id.clone(),
            result: ResponseResult::Error { error },
        };
        let failed = FailedRequest {
            request_id: request.request_id,
            request_digest,
            response: response.clone(),
            finish: finish.clone(),
        };
        if self.failed.len() < FAILED_REQUEST_CAPACITY {
            self.failed.push_back(failed);
        } else if request.action == LifecycleAction::Park && self.saturated_failed_park.is_none() {
            self.saturated_failed_park = Some(failed);
        }
        self.send_response_with_finish(response, finish);
    }

    fn reject_at_failure_cache_bound(&mut self, request_id: String) {
        self.widening_blocked = true;
        if let Some(capability) = self.resources.capability.as_mut() {
            let _ = capability.revoke();
        }
        if self.channel_state != ChannelState::Closed {
            self.channel_state = ChannelState::Revoked;
        }
        let error = self
            .failure_cache_saturation
            .get_or_insert_with(|| {
                ProtocolError::new(
                    ErrorCode::DurabilityUnavailable,
                    Some(self.state),
                    Some(self.broker_head.sequence),
                )
            })
            .clone();
        self.last_failure = Some(error.clone());
        self.send_error(request_id, error);
    }

    fn lose_broker(&mut self, _error: SupervisorError) {
        if !matches!(
            self.broker_connection,
            BrokerConnection::Connected | BrokerConnection::Reconciling
        ) {
            return;
        }
        self.commands.closed = true;
        let Some(connection_epoch) = self.connection_epoch.checked_add(1) else {
            self.finished = Some(Err(SupervisorError::BrokerUnavailable));
            return;
        };
        self.connection_epoch = connection_epoch;
        self.reconnect_retry_fallback_epoch = None;
        self.reconciliation = None;
        self.deferred_reconciliation = None;
        self.controller_loss_settlement = None;
        self.broker_connection = BrokerConnection::Grace;
        let now = self.timer.now();
        self.broker_loss_deadline = Some(now.checked_add(self.broker_loss_grace).unwrap_or(now));
        if let Some(capability) = self.resources.capability.as_mut() {
            let _ = capability.revoke();
        }
        if self.channel_state != ChannelState::Closed {
            self.channel_state = ChannelState::Revoked;
        }
        let current_head = receipt_head(self.receipt());
        let receipt_state_can_restore = match self.pending.as_ref() {
            None => current_head == self.broker_head,
            Some(pending) => {
                pending.phase == PendingPhase::AwaitingDurableAck
                    && pending
                        .receipt
                        .as_ref()
                        .is_some_and(|receipt| receipt_head(receipt) == current_head)
            }
        };
        self.restore_after_reconnect = !self.broker_loss_grace.is_zero()
            && self.state == SessionState::Running
            && self.deferred.is_empty()
            && receipt_state_can_restore;
        self.last_failure = Some(ProtocolError::new(
            ErrorCode::BrokerUnavailable,
            Some(self.state),
            Some(self.broker_head.sequence),
        ));

        if self.broker_loss_grace.is_zero() {
            self.handle_broker_grace_expired(connection_epoch);
        } else {
            let sender = self.sender.clone();
            if self
                .timer
                .schedule(
                    self.broker_loss_grace,
                    Box::new(move || {
                        let _ = sender.send(OwnerEvent::BrokerGraceExpired { connection_epoch });
                    }),
                )
                .is_err()
            {
                let _ = self
                    .sender
                    .send(OwnerEvent::BrokerGraceExpired { connection_epoch });
            }
        }

        let head = current_head;
        let reconnect = BrokerReconnect {
            schema: BROKER_RECONNECT_SCHEMA.to_owned(),
            protocol_version: PROTOCOL_VERSION,
            request_id: format!("reconnect-{connection_epoch}"),
            session_id: self.binding.session_id.clone(),
            run_id: self.receipt().payload.run_id.clone(),
            envelope_revision: self.binding.envelope_revision,
            sequence: head.sequence,
            receipt_digest: head.digest,
        };
        self.reconnect_request = Some(reconnect.clone());
        self.start_broker_reconnect(connection_epoch, reconnect);
    }

    fn start_broker_reconnect(&mut self, connection_epoch: u64, reconnect: BrokerReconnect) {
        if connection_epoch != self.connection_epoch || self.active_reconnect_attempt.is_some() {
            return;
        }
        let Some(attempt_epoch) = self.reconnect_attempt_epoch.checked_add(1) else {
            self.finished = Some(Err(SupervisorError::BrokerUnavailable));
            return;
        };
        self.reconnect_attempt_epoch = attempt_epoch;
        self.active_reconnect_attempt = Some(attempt_epoch);
        let deadline_sender = self.sender.clone();
        if self
            .timer
            .schedule(
                self.timeout,
                Box::new(move || {
                    let _ = deadline_sender.send(OwnerEvent::BrokerReconnectDeadline {
                        connection_epoch,
                        attempt_epoch,
                    });
                }),
            )
            .is_err()
        {
            self.active_reconnect_attempt = None;
            self.schedule_broker_reconnect_retry(connection_epoch);
            return;
        }
        let sender = self.sender.clone();
        let result = self
            .resources
            .broker
            .as_ref()
            .ok_or(SupervisorError::BrokerUnavailable)
            .and_then(|broker| {
                broker.reconnect_session(
                    reconnect,
                    Box::new(move |result| {
                        let _ = sender.send(OwnerEvent::BrokerReconnected {
                            connection_epoch,
                            attempt_epoch,
                            result,
                        });
                    }),
                )
            });
        if result.is_err() {
            self.active_reconnect_attempt = None;
            self.schedule_broker_reconnect_retry(connection_epoch);
        }
    }

    fn handle_broker_reconnect_deadline(&mut self, connection_epoch: u64, attempt_epoch: u64) {
        if connection_epoch != self.connection_epoch
            || self.active_reconnect_attempt != Some(attempt_epoch)
        {
            return;
        }
        self.active_reconnect_attempt = None;
        if let Some(broker) = self.resources.broker.as_ref() {
            broker.cancel_reconnect();
        }
        self.schedule_broker_reconnect_retry(connection_epoch);
    }

    fn schedule_broker_reconnect_retry(&mut self, connection_epoch: u64) {
        self.last_failure = Some(ProtocolError::new(
            ErrorCode::BrokerUnavailable,
            Some(self.state),
            Some(self.broker_head.sequence),
        ));
        if connection_epoch != self.connection_epoch
            || !matches!(
                self.broker_connection,
                BrokerConnection::Grace | BrokerConnection::Disconnected
            )
        {
            return;
        }
        let retry_delay = if self.broker_connection == BrokerConnection::Grace {
            (self.broker_loss_grace / 2).min(Duration::from_millis(100))
        } else {
            Duration::from_millis(100)
        };
        let sender = self.sender.clone();
        if self
            .timer
            .schedule(
                retry_delay,
                Box::new(move || {
                    let _ = sender.send(OwnerEvent::BrokerReconnectRetry { connection_epoch });
                }),
            )
            .is_err()
            && self.reconnect_retry_fallback_epoch != Some(connection_epoch)
        {
            self.reconnect_retry_fallback_epoch = Some(connection_epoch);
            if let Some(reconnect) = self.reconnect_request.clone() {
                self.start_broker_reconnect(connection_epoch, reconnect);
            }
        }
    }

    fn handle_broker_reconnect_retry(&mut self, connection_epoch: u64) {
        if connection_epoch != self.connection_epoch
            || !matches!(
                self.broker_connection,
                BrokerConnection::Grace | BrokerConnection::Disconnected
            )
        {
            return;
        }
        if let Some(reconnect) = self.reconnect_request.clone() {
            self.start_broker_reconnect(connection_epoch, reconnect);
        }
    }

    fn handle_broker_reconnected(
        &mut self,
        connection_epoch: u64,
        attempt_epoch: u64,
        result: Result<BrokerReconnect, SupervisorError>,
    ) {
        if connection_epoch != self.connection_epoch
            || self.active_reconnect_attempt != Some(attempt_epoch)
        {
            return;
        }
        self.active_reconnect_attempt = None;
        self.expire_broker_grace_if_elapsed(connection_epoch);
        let Some(request) = self.reconnect_request.as_ref() else {
            return;
        };
        let response = match result {
            Ok(response) if response.validate_response_to(request).is_ok() => response,
            Ok(_) => {
                self.fail_broker_reconciliation(ErrorCode::ReceiptChainInvalid);
                self.arm_broker_receive();
                return;
            }
            Err(_) => {
                self.schedule_broker_reconnect_retry(connection_epoch);
                return;
            }
        };
        let Some(receipt_index) = self
            .receipts
            .iter()
            .position(|receipt| receipt.payload.sequence == response.sequence)
        else {
            self.fail_broker_reconciliation(ErrorCode::ReceiptChainInvalid);
            self.arm_broker_receive();
            return;
        };
        let response_head = ReceiptHead {
            sequence: response.sequence,
            digest: response.receipt_digest,
        };
        if receipt_head(&self.receipts[receipt_index]) != response_head {
            self.fail_broker_reconciliation(ErrorCode::ReceiptChainInvalid);
            self.arm_broker_receive();
            return;
        }
        self.broker_head = response_head;

        let next_receipt_index = receipt_index + 1;
        if next_receipt_index < self.receipts.len() {
            self.restore_after_reconnect = false;
            if self.pending.is_some() {
                self.fail_receipt_operation(ErrorCode::DurabilityUnavailable);
            }
            let reconciliation_parked = self.state == SessionState::Running;
            if !self.park_for_reconciliation() {
                if self.quarantined || self.state == SessionState::Terminal {
                    return;
                }
                self.fail_broker_reconciliation(ErrorCode::LifecycleMechanicUnavailable);
                self.arm_broker_receive();
                return;
            }
            if reconciliation_parked {
                self.defer_broker_loss_park();
            }
            self.broker_connection = BrokerConnection::Reconciling;
            let Some(reconciliation_epoch) = self.operation_epoch.checked_add(1) else {
                self.fail_broker_reconciliation(ErrorCode::DurabilityUnavailable);
                self.arm_broker_receive();
                return;
            };
            self.operation_epoch = reconciliation_epoch;
            self.reconciliation = Some(ReceiptReconciliation {
                operation_epoch: reconciliation_epoch,
                awaiting_receipt_index: next_receipt_index,
            });
            self.arm_broker_receive();
            self.send_reconciliation_receipt(next_receipt_index);
            return;
        }

        self.finish_broker_reconnect(true);
    }

    fn expire_broker_grace_if_elapsed(&mut self, connection_epoch: u64) {
        if self.broker_connection == BrokerConnection::Grace
            && self
                .broker_loss_deadline
                .is_some_and(|deadline| self.timer.now() >= deadline)
        {
            self.handle_broker_grace_expired(connection_epoch);
        }
    }

    fn handle_broker_grace_expired(&mut self, connection_epoch: u64) {
        if connection_epoch != self.connection_epoch {
            return;
        }
        self.restore_after_reconnect = false;
        if self.broker_connection != BrokerConnection::Grace {
            return;
        }
        if self.pending.is_some() {
            self.fail_receipt_operation(ErrorCode::DurabilityUnavailable);
        }
        if self.state != SessionState::Running {
            self.broker_connection = BrokerConnection::Disconnected;
            return;
        }
        if !self.park_for_reconciliation() {
            if self.quarantined || self.state == SessionState::Terminal {
                return;
            }
            self.broker_connection = BrokerConnection::Disconnected;
            self.last_failure = Some(ProtocolError::new(
                ErrorCode::LifecycleMechanicUnavailable,
                Some(self.state),
                Some(self.broker_head.sequence),
            ));
            return;
        }
        self.broker_connection = BrokerConnection::Disconnected;
        self.begin_broker_loss_park();
    }

    fn park_for_reconciliation(&mut self) -> bool {
        if self.state != SessionState::Running {
            return true;
        }
        match self.attempt_park() {
            ParkResult::Parked => true,
            ParkResult::Running => false,
            ParkResult::Terminal(classification) => {
                self.begin_process_exit(classification);
                false
            }
            ParkResult::Ambiguous => {
                self.quarantine_mechanic(None);
                false
            }
        }
    }

    fn begin_broker_loss_park(&mut self) {
        let receipt_backlog = self.has_receipt_backlog();
        let Some(operation_epoch) = self.operation_epoch.checked_add(1) else {
            self.last_failure = Some(ProtocolError::new(
                ErrorCode::DurabilityUnavailable,
                Some(self.state),
                Some(self.broker_head.sequence),
            ));
            return;
        };
        let head = self.receipt();
        let Some(sequence) = head.payload.sequence.checked_add(1) else {
            self.last_failure = Some(ProtocolError::new(
                ErrorCode::DurabilityUnavailable,
                Some(self.state),
                Some(self.broker_head.sequence),
            ));
            return;
        };
        let request_id = format!("broker-loss-{}", head.digest().hex());
        let request = LifecycleRequest {
            schema: LIFECYCLE_REQUEST_SCHEMA.to_owned(),
            protocol_version: PROTOCOL_VERSION,
            request_id: request_id.clone(),
            session_id: self.binding.session_id.clone(),
            run_id: head.payload.run_id.clone(),
            authorization_id: "broker-loss".to_owned(),
            action: LifecycleAction::Park,
            expected_state: SessionState::Running,
            expected_receipt_sequence: Some(head.payload.sequence),
            envelope_revision: self.binding.envelope_revision,
        };
        let intent = ReceiptIntent {
            session_id: self.binding.session_id.clone(),
            run_id: head.payload.run_id.clone(),
            authorization_id: "broker-loss".to_owned(),
            request_id: request_id.clone(),
            request_digest: request.digest().to_string(),
            action: LifecycleAction::Park,
            envelope_revision: self.binding.envelope_revision,
            sequence,
            previous_receipt_digest: head.digest().to_string(),
            resulting_state: SessionState::Parked,
        };
        let payload = ReceiptPayload {
            schema: RECEIPT_SCHEMA.to_owned(),
            session_id: self.binding.session_id.clone(),
            run_id: head.payload.run_id.clone(),
            request_id,
            envelope_revision: self.binding.envelope_revision,
            sequence,
            previous_receipt_digest: Some(head.digest().to_string()),
            release_id: self.signer.release_id().to_owned(),
            signing_key_id: self.signer.signing_key_id().to_owned(),
            outcome: ReceiptOutcome::Park {
                authority: ReceiptAuthority::Cause {
                    cause: ReceiptCause::BrokerLost,
                },
            },
            resulting_state: SessionState::Parked,
        };
        self.operation_epoch = operation_epoch;
        self.pending = Some(ActiveOperation {
            epoch: operation_epoch,
            request,
            intent,
            phase: PendingPhase::Signing,
            receipt: None,
            payload_override: Some(payload),
            respond: false,
            finish: None,
        });
        if receipt_backlog {
            self.fail_receipt_operation(ErrorCode::DurabilityUnavailable);
            return;
        }
        if self.schedule_operation_deadline(operation_epoch).is_err() {
            self.fail_receipt_operation(ErrorCode::DurabilityUnavailable);
            return;
        }
        self.start_signing(operation_epoch);
    }

    fn start_deferred_receipt_reconciliation(&mut self) {
        if self.deferred_reconciliation.is_some() || self.deferred.is_empty() {
            return;
        }
        let Some(operation_epoch) = self.operation_epoch.checked_add(1) else {
            self.fail_deferred_receipt_reconciliation(ErrorCode::DurabilityUnavailable);
            return;
        };
        let head = self.receipt();
        let Some(sequence) = head.payload.sequence.checked_add(1) else {
            self.fail_deferred_receipt_reconciliation(ErrorCode::DurabilityUnavailable);
            return;
        };
        let previous_receipt_digest = head.digest().to_string();
        let payload = match self.deferred.front() {
            Some(DeferredReceipt::Payload(payload)) => {
                let mut payload = (**payload).clone();
                payload.sequence = sequence;
                payload.previous_receipt_digest = Some(previous_receipt_digest);
                payload
            }
            Some(DeferredReceipt::CausalPark {
                request_id,
                envelope_revision,
                cause,
            }) => ReceiptPayload {
                schema: RECEIPT_SCHEMA.to_owned(),
                session_id: self.binding.session_id.clone(),
                run_id: head.payload.run_id.clone(),
                request_id: request_id.clone(),
                envelope_revision: *envelope_revision,
                sequence,
                previous_receipt_digest: Some(previous_receipt_digest),
                release_id: self.signer.release_id().to_owned(),
                signing_key_id: self.signer.signing_key_id().to_owned(),
                outcome: ReceiptOutcome::Park {
                    authority: ReceiptAuthority::Cause { cause: *cause },
                },
                resulting_state: SessionState::Parked,
            },
            None => return,
        };
        if payload.validate().is_err() {
            self.fail_deferred_receipt_reconciliation(ErrorCode::ReceiptChainInvalid);
            return;
        }
        self.operation_epoch = operation_epoch;
        self.deferred_reconciliation = Some(DeferredReceiptReconciliation {
            operation_epoch,
            phase: DeferredReceiptPhase::Signing(Box::new(payload.clone())),
        });
        let connection_epoch = self.connection_epoch;
        let deadline_sender = self.sender.clone();
        if self
            .timer
            .schedule(
                self.timeout,
                Box::new(move || {
                    let _ = deadline_sender.send(OwnerEvent::DeferredReceiptDeadline {
                        connection_epoch,
                        operation_epoch,
                    });
                }),
            )
            .is_err()
        {
            self.fail_deferred_receipt_reconciliation(ErrorCode::DurabilityUnavailable);
            return;
        }
        let sender = self.sender.clone();
        if self
            .signer
            .sign(
                payload.canonical_bytes(),
                Box::new(move |result| {
                    let _ = sender.send(OwnerEvent::DeferredReceiptSigned {
                        connection_epoch,
                        operation_epoch,
                        result,
                    });
                }),
            )
            .is_err()
        {
            self.fail_deferred_receipt_reconciliation(ErrorCode::SigningUnavailable);
        }
    }

    fn handle_deferred_receipt_signed(
        &mut self,
        connection_epoch: u64,
        operation_epoch: u64,
        result: Result<String, SupervisorError>,
    ) {
        if connection_epoch != self.connection_epoch {
            return;
        }
        let Some(active) = self.deferred_reconciliation.as_ref() else {
            return;
        };
        let DeferredReceiptPhase::Signing(payload) = &active.phase else {
            return;
        };
        if active.operation_epoch != operation_epoch {
            return;
        }
        let Ok(signature) = result else {
            self.fail_deferred_receipt_reconciliation(ErrorCode::SigningUnavailable);
            return;
        };
        let receipt = SignedReceipt {
            schema: SIGNED_RECEIPT_SCHEMA.to_owned(),
            payload: (**payload).clone(),
            signature,
        };
        if receipt.validate().is_err() {
            self.fail_deferred_receipt_reconciliation(ErrorCode::SigningUnavailable);
            return;
        }
        let receipt_index = self.receipts.len();
        let bytes = receipt.canonical_bytes();
        self.receipts.push(receipt);
        self.deferred.pop_front();
        if let Some(active) = self.deferred_reconciliation.as_mut() {
            active.phase = DeferredReceiptPhase::AwaitingDurableAck { receipt_index };
        }
        let sender = self.sender.clone();
        let result = self
            .resources
            .broker
            .as_ref()
            .ok_or(SupervisorError::BrokerUnavailable)
            .and_then(|broker| {
                broker.send_session_receipt(
                    bytes,
                    Box::new(move |result| {
                        let _ = sender.send(OwnerEvent::DeferredReceiptSent {
                            connection_epoch,
                            operation_epoch,
                            result,
                        });
                    }),
                )
            });
        if let Err(error) = result {
            self.handle_deferred_receipt_sent(connection_epoch, operation_epoch, &Err(error));
        }
    }

    fn handle_deferred_receipt_sent(
        &mut self,
        connection_epoch: u64,
        operation_epoch: u64,
        result: &Result<(), SupervisorError>,
    ) {
        if connection_epoch != self.connection_epoch
            || self
                .deferred_reconciliation
                .as_ref()
                .is_none_or(|active| active.operation_epoch != operation_epoch)
        {
            return;
        }
        if result.is_err() {
            self.fail_deferred_receipt_reconciliation(ErrorCode::DurabilityUnavailable);
        }
    }

    fn handle_deferred_receipt_acknowledgement(
        &mut self,
        acknowledgement: &ReceiptAcknowledgement,
    ) {
        let Some(receipt_index) =
            self.deferred_reconciliation
                .as_ref()
                .and_then(|active| match active.phase {
                    DeferredReceiptPhase::AwaitingDurableAck { receipt_index } => {
                        Some(receipt_index)
                    }
                    DeferredReceiptPhase::Signing(_) => None,
                })
        else {
            self.fail_deferred_receipt_reconciliation(ErrorCode::ReceiptChainInvalid);
            return;
        };
        let receipt = &self.receipts[receipt_index];
        let head = receipt_head(receipt);
        if acknowledgement.exact_disposition(
            &receipt.payload.session_id,
            &receipt.payload.run_id,
            &head,
        ) != Some(ReceiptDisposition::DurablyStored)
        {
            self.fail_deferred_receipt_reconciliation(ErrorCode::ReceiptChainInvalid);
            return;
        }
        self.broker_head = head;
        self.deferred_reconciliation = None;
        if self.deferred.is_empty() {
            self.finish_broker_reconnect(false);
        } else {
            self.start_deferred_receipt_reconciliation();
        }
    }

    fn handle_deferred_receipt_deadline(&mut self, connection_epoch: u64, operation_epoch: u64) {
        if connection_epoch == self.connection_epoch
            && self
                .deferred_reconciliation
                .as_ref()
                .is_some_and(|active| active.operation_epoch == operation_epoch)
        {
            self.fail_deferred_receipt_reconciliation(ErrorCode::DurabilityUnavailable);
        }
    }

    fn fail_deferred_receipt_reconciliation(&mut self, code: ErrorCode) {
        self.deferred_reconciliation = None;
        self.restore_after_reconnect = false;
        self.widening_blocked = true;
        self.broker_connection = BrokerConnection::Connected;
        self.broker_loss_deadline = None;
        self.channel_state = if self.state == SessionState::Terminal {
            ChannelState::Closed
        } else {
            ChannelState::Revoked
        };
        self.last_failure = Some(ProtocolError::new(
            code,
            Some(self.state),
            Some(self.broker_head.sequence),
        ));
    }

    fn send_reconciliation_receipt(&mut self, receipt_index: usize) {
        let Some(reconciliation_epoch) = self
            .reconciliation
            .as_ref()
            .map(|reconciliation| reconciliation.operation_epoch)
        else {
            return;
        };
        let deadline_sender = self.sender.clone();
        let connection_epoch = self.connection_epoch;
        if self
            .timer
            .schedule(
                self.timeout,
                Box::new(move || {
                    let _ = deadline_sender.send(OwnerEvent::ReconciliationReceiptDeadline {
                        connection_epoch,
                        reconciliation_epoch,
                        receipt_index,
                    });
                }),
            )
            .is_err()
        {
            self.lose_broker(SupervisorError::WorkerUnavailable);
            return;
        }
        let bytes = self.receipts[receipt_index].canonical_bytes();
        let sender = self.sender.clone();
        let result = self
            .resources
            .broker
            .as_ref()
            .ok_or(SupervisorError::BrokerUnavailable)
            .and_then(|broker| {
                broker.send_session_receipt(
                    bytes,
                    Box::new(move |result| {
                        let _ = sender.send(OwnerEvent::ReconciliationReceiptSent {
                            connection_epoch,
                            reconciliation_epoch,
                            receipt_index,
                            result,
                        });
                    }),
                )
            });
        if let Err(error) = result {
            self.handle_reconciliation_receipt_sent(
                connection_epoch,
                reconciliation_epoch,
                receipt_index,
                &Err(error),
            );
        }
    }

    fn handle_reconciliation_receipt_sent(
        &mut self,
        connection_epoch: u64,
        reconciliation_epoch: u64,
        receipt_index: usize,
        result: &Result<(), SupervisorError>,
    ) {
        if connection_epoch != self.connection_epoch
            || self.reconciliation.as_ref().is_none_or(|reconciliation| {
                reconciliation.operation_epoch != reconciliation_epoch
                    || reconciliation.awaiting_receipt_index != receipt_index
            })
        {
            return;
        }
        if result.is_err() {
            self.lose_broker(SupervisorError::BrokerUnavailable);
        }
    }

    fn handle_reconciliation_receipt_deadline(
        &mut self,
        connection_epoch: u64,
        reconciliation_epoch: u64,
        receipt_index: usize,
    ) {
        if connection_epoch == self.connection_epoch
            && self.reconciliation.as_ref().is_some_and(|reconciliation| {
                reconciliation.operation_epoch == reconciliation_epoch
                    && reconciliation.awaiting_receipt_index == receipt_index
            })
        {
            self.lose_broker(SupervisorError::BrokerTimeout);
        }
    }

    fn handle_reconciliation_acknowledgement(&mut self, acknowledgement: &ReceiptAcknowledgement) {
        let Some(receipt_index) = self
            .reconciliation
            .as_ref()
            .map(|reconciliation| reconciliation.awaiting_receipt_index)
        else {
            return;
        };
        let receipt = &self.receipts[receipt_index];
        let head = receipt_head(receipt);
        match acknowledgement.exact_disposition(
            &receipt.payload.session_id,
            &receipt.payload.run_id,
            &head,
        ) {
            Some(ReceiptDisposition::DurablyStored) => {}
            Some(ReceiptDisposition::Rejected) | None => {
                self.fail_broker_reconciliation(ErrorCode::ReceiptChainInvalid);
                return;
            }
        }
        self.broker_head = head;
        let next_receipt_index = receipt_index + 1;
        if next_receipt_index < self.receipts.len() {
            if let Some(reconciliation) = self.reconciliation.as_mut() {
                reconciliation.awaiting_receipt_index = next_receipt_index;
            }
            self.send_reconciliation_receipt(next_receipt_index);
        } else {
            self.finish_broker_reconnect(false);
        }
    }

    fn finish_broker_reconnect(&mut self, arm_receive: bool) {
        self.reconnect_request = None;
        self.reconciliation = None;

        let pending_matches_head = self.pending.as_ref().is_some_and(|pending| {
            pending.phase == PendingPhase::AwaitingDurableAck
                && pending
                    .receipt
                    .as_ref()
                    .is_some_and(|receipt| receipt_head(receipt) == self.broker_head)
        });
        if pending_matches_head {
            self.complete_pending_receipt(false);
        }

        if !self.deferred.is_empty() {
            if self.state == SessionState::Running {
                if !self.park_for_reconciliation() {
                    if self.quarantined || self.state == SessionState::Terminal {
                        return;
                    }
                    self.restore_after_reconnect = false;
                    self.widening_blocked = true;
                    self.broker_connection = BrokerConnection::Connected;
                    self.channel_state = ChannelState::Revoked;
                    self.last_failure = Some(ProtocolError::new(
                        ErrorCode::LifecycleMechanicUnavailable,
                        Some(self.state),
                        Some(self.broker_head.sequence),
                    ));
                    if arm_receive {
                        self.arm_broker_receive();
                    }
                    return;
                }
                self.defer_broker_loss_park();
            }
            self.broker_connection = BrokerConnection::Reconciling;
            self.restore_after_reconnect = false;
            if arm_receive {
                self.arm_broker_receive();
            }
            self.start_deferred_receipt_reconciliation();
            return;
        }

        self.broker_connection = BrokerConnection::Connected;
        self.broker_loss_deadline = None;

        let restore = self.restore_after_reconnect
            && !self.controller_loss_unresolved
            && !self.cleanup_unproven
            && self.state == SessionState::Running
            && self.pending.is_none()
            && self.deferred.is_empty()
            && receipt_head(self.receipt()) == self.broker_head;
        self.restore_after_reconnect = false;
        if restore {
            let enabled = self
                .resources
                .capability
                .as_mut()
                .is_some_and(|capability| capability.enable().is_ok());
            if enabled {
                self.channel_state = ChannelState::Enabled;
                self.last_failure = None;
            } else {
                self.channel_state = ChannelState::Revoked;
                self.last_failure = Some(ProtocolError::new(
                    ErrorCode::LifecycleMechanicUnavailable,
                    Some(self.state),
                    Some(self.broker_head.sequence),
                ));
            }
        } else {
            self.last_failure = None;
        }
        self.clear_durable_controller_loss_park_marker();
        self.maybe_begin_controller_loss_settlement();
        if self.controller_loss_park_request_id.is_none()
            && self.controller_loss_settlement.is_none()
            && !self.controller_loss_unresolved
            && !self.cleanup_unproven
            && self.state != SessionState::Terminal
            && receipt_head(self.receipt()) == self.broker_head
        {
            self.widening_blocked = false;
        }
        if let Some(finish) = self.finish_when_backlog_drained.take() {
            self.request_finish(finish);
        }
        if arm_receive {
            self.arm_broker_receive();
        }
    }

    fn fail_broker_reconciliation(&mut self, code: ErrorCode) {
        self.restore_after_reconnect = false;
        self.reconnect_request = None;
        self.reconciliation = None;
        self.widening_blocked = true;
        let reconciliation_parked = self.state == SessionState::Running;
        let park_succeeded = self.park_for_reconciliation();
        if self.quarantined || self.state == SessionState::Terminal {
            return;
        }
        if reconciliation_parked && self.state == SessionState::Parked {
            self.defer_broker_loss_park();
        }
        self.broker_connection = BrokerConnection::Connected;
        if self.channel_state != ChannelState::Closed {
            self.channel_state = ChannelState::Revoked;
        }
        self.last_failure = Some(ProtocolError::new(
            if reconciliation_parked && !park_succeeded {
                ErrorCode::LifecycleMechanicUnavailable
            } else {
                code
            },
            Some(self.state),
            Some(self.broker_head.sequence),
        ));
    }

    fn defer_broker_loss_park(&mut self) {
        let head = self.receipt();
        let inserted = self.defer(DeferredReceipt::CausalPark {
            request_id: format!("reconcile-park-{}", head.digest().hex()),
            envelope_revision: self.binding.envelope_revision,
            cause: ReceiptCause::BrokerLost,
        });
        debug_assert!(
            inserted,
            "Running receipt capacity reserves a causal Park slot"
        );
        self.widening_blocked = true;
    }

    fn pending_matches(&self, operation_epoch: u64, phase: PendingPhase) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|pending| pending.epoch == operation_epoch && pending.phase == phase)
    }

    fn remember_completed(&mut self, completed: CompletedRequest) {
        self.completed.push_back(completed);
    }

    fn status(&self) -> SupervisorStatus {
        let pending_operation = self.pending.as_ref().map(|pending| PendingOperation {
            request_id: pending.request.request_id.clone(),
            action: pending.request.action.into(),
            phase: pending.phase,
        });
        SupervisorStatus {
            schema: SUPERVISOR_STATUS_SCHEMA.to_owned(),
            protocol_version: PROTOCOL_VERSION,
            session_id: self.binding.session_id.clone(),
            run_id: self.receipt().payload.run_id.clone(),
            state: self.state,
            broker_connection: self.broker_connection,
            envelope_revision: self.binding.envelope_revision,
            channel_state: self.channel_state,
            launcher_head: Some(receipt_head(self.receipt())),
            broker_head: Some(self.broker_head.clone()),
            pending_receipt_count: self.pending_receipt_count(),
            pending_operation,
            process_exit: self.process_exit,
            last_failure: self.last_failure.clone(),
        }
    }

    #[expect(
        clippy::expect_used,
        reason = "Construction requires a nonempty receipt chain, which is only appended to and never cleared."
    )]
    fn receipt(&self) -> &SignedReceipt {
        self.receipts
            .last()
            .expect("a Session owner retains its receipt chain")
    }

    fn has_receipt_backlog(&self) -> bool {
        receipt_head(self.receipt()) != self.broker_head || !self.deferred.is_empty()
    }

    fn pending_receipt_count(&self) -> u32 {
        let signed_gap = self
            .receipt()
            .payload
            .sequence
            .saturating_sub(self.broker_head.sequence);
        let active_unsigned = self.pending.as_ref().map_or(0, |pending| {
            u64::from(pending.phase == PendingPhase::Signing)
        });
        signed_gap
            .saturating_add(active_unsigned)
            .saturating_add(u64::try_from(self.deferred.len()).unwrap_or(u64::MAX))
            .try_into()
            .unwrap_or(u32::MAX)
    }
}

fn receipt_head(receipt: &SignedReceipt) -> ReceiptHead {
    ReceiptHead {
        sequence: receipt.payload.sequence,
        digest: Digest::of(&receipt.canonical_bytes()).to_string(),
    }
}

fn receipt_response(request_id: String, receipt: SignedReceipt) -> ProtocolResponse {
    ProtocolResponse {
        schema: RESPONSE_SCHEMA.to_owned(),
        protocol_version: PROTOCOL_VERSION,
        request_id,
        result: ResponseResult::Receipt { receipt },
    }
}
