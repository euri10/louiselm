//! Behavioral coverage for launch supervisor.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Test fixtures abort on setup failure and assert failures directly."
)]
#![cfg(target_os = "linux")]

//! One authorized launch through fake broker, signer, and privileged platform ports.

mod support;

use std::{
    env, fs,
    io::{self, BufReader, Cursor, Read, Write},
    net::Shutdown,
    os::{
        fd::OwnedFd,
        unix::{
            fs::PermissionsExt,
            net::{UnixListener, UnixStream},
            process::{CommandExt, ExitStatusExt},
        },
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Condvar, Mutex, MutexGuard,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, Receiver},
    },
    thread,
    time::{Duration, Instant},
};

use louiselm_skills::{
    canonical::Digest,
    isolation::{
        CONTRACT_VERSION, Dimension, DimensionEvidence, IsolationEvidence, KernelPrerequisites,
    },
    launch::{LaunchRequest, MAX_REQUEST_BYTES, PROTOCOL_VERSION, REQUEST_SCHEMA},
    launch_protocol::{
        BROKER_RECONNECT_SCHEMA, BrokerConnection, BrokerReconnect, CONTROLLER_LOSS_ACK_SCHEMA,
        CONTROLLER_LOSS_SETTLEMENT_SCHEMA, ChannelState, ControllerLossAcknowledgement,
        ControllerLossDisposition, ControllerLossSettlement, ErrorCode, IdentityExhaustion,
        LAUNCH_AUTHORIZATION_SCHEMA, LIFECYCLE_REQUEST_SCHEMA, LaunchAuthorization,
        LifecycleAction, LifecycleRequest, MAX_PENDING_RECEIPTS, OccupiedSessionIdentity,
        PendingAction, PendingPhase, ProtocolMessage, ProtocolResponse, RECEIPT_ACK_SCHEMA,
        RESPONSE_SCHEMA, ReceiptAcknowledgement, ReceiptDisposition, ResponseResult,
        STATUS_REQUEST_SCHEMA, StatusRequest, SupervisorStatus,
    },
    launch_receipt::{
        ChainAnchor, ConformanceEvidence, ProcessExitClassification, ReceiptAuthority,
        ReceiptCause, ReceiptHead, ReceiptOutcome, SessionState, SignedReceipt, verify_chain,
    },
    launch_supervisor::{
        AgentAuthentication, CapabilityBinding, CapabilityGate, IdentityGuard, LaunchBroker,
        LaunchPlatform, LaunchSigner, LaunchSupervisor, LaunchedSession, MechanicFailure,
        PreparedAgent, ProcessMembership, RelayStdio, RunningAgent, RunningAgentEvent,
        RunningAgentEvents, SupervisorCompletion, SupervisorError, SupervisorTimer,
        SystemRunningAgent, read_launch_frame,
    },
    launcher_install::Identity,
    registry::Registry,
    sandbox::{
        BubblewrapBackend, Channel, ConfinementPlan, IdentityPlan, PreparedSession, ProcessTree,
        SandboxError,
    },
};
use support::{Fixture, write_file, write_registry};

const CONTROLLER_UID: u32 = 1_000;
const NOW_MS: u64 = 1_000;
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(2);
const PRIVILEGED_TIMEOUT: Duration = Duration::from_secs(5);
const SUPERVISOR_TIMEOUT: Duration = Duration::from_millis(500);
const BROKER_LOSS_GRACE_MS: u32 = 250;

type Events = Arc<Mutex<Vec<String>>>;

fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn record(events: &Events, event: &str) {
    lock(events).push(event.to_owned());
}

fn event_snapshot(events: &Events) -> Vec<String> {
    lock(events).clone()
}

#[derive(Clone, Copy)]
enum AppendBehavior {
    Hold,
    Reject,
    NeverComplete,
}

#[derive(Clone, Copy)]
enum SessionReceiptSendBehavior {
    Complete,
    Reject,
}

struct BrokerState {
    authorization: Option<LaunchAuthorization>,
    authorization_error: Option<SupervisorError>,
    hold_authorization: bool,
    pending_authorization: Option<(
        Option<LaunchAuthorization>,
        SupervisorCompletion<LaunchAuthorization>,
    )>,
    append_behavior: AppendBehavior,
    receipts: Vec<Vec<u8>>,
    reports: Vec<Option<Vec<u8>>>,
    pending_append: Option<(usize, SupervisorCompletion<ReceiptAcknowledgement>)>,
    pending_session_request: Option<SupervisorCompletion<ProtocolMessage>>,
    session_receipt_send_behavior: SessionReceiptSendBehavior,
    session_receipts: Vec<Vec<u8>>,
    session_responses: Vec<ProtocolResponse>,
    held_session_response_request_id: Option<String>,
    pending_session_response: Option<SupervisorCompletion<()>>,
    stale_session_request: Option<SupervisorCompletion<ProtocolMessage>>,
    reconnects: Vec<BrokerReconnect>,
    pending_reconnect: Option<SupervisorCompletion<BrokerReconnect>>,
    stale_reconnects: Vec<SupervisorCompletion<BrokerReconnect>>,
    controller_loss_settlements: Vec<ControllerLossSettlement>,
    pending_controller_loss_settlement: Option<SupervisorCompletion<ControllerLossAcknowledgement>>,
    closed: bool,
}

struct FakeBroker {
    events: Events,
    state: Mutex<BrokerState>,
    changed: Condvar,
    receipt_root: PathBuf,
}

impl FakeBroker {
    fn new(
        events: Events,
        authorization: Option<LaunchAuthorization>,
        append_behavior: AppendBehavior,
        receipt_root: PathBuf,
    ) -> Self {
        Self {
            events,
            state: Mutex::new(BrokerState {
                authorization,
                authorization_error: None,
                hold_authorization: false,
                pending_authorization: None,
                append_behavior,
                receipts: Vec::new(),
                reports: Vec::new(),
                pending_append: None,
                pending_session_request: None,
                session_receipt_send_behavior: SessionReceiptSendBehavior::Complete,
                session_receipts: Vec::new(),
                session_responses: Vec::new(),
                held_session_response_request_id: None,
                pending_session_response: None,
                stale_session_request: None,
                reconnects: Vec::new(),
                pending_reconnect: None,
                stale_reconnects: Vec::new(),
                controller_loss_settlements: Vec::new(),
                pending_controller_loss_settlement: None,
                closed: false,
            }),
            changed: Condvar::new(),
            receipt_root,
        }
    }

    fn hold_authorization(&self) {
        lock(&self.state).hold_authorization = true;
    }

    fn fail_authorization(&self, error: SupervisorError) {
        let mut state = lock(&self.state);
        state.authorization = None;
        state.authorization_error = Some(error);
    }

    fn wait_for_authorization(&self) {
        let state = lock(&self.state);
        let (state, timeout) = self
            .changed
            .wait_timeout_while(state, CALLBACK_TIMEOUT, |state| {
                state.pending_authorization.is_none()
            })
            .expect("broker condition variable remains usable");
        assert!(
            !timeout.timed_out(),
            "supervisor never requested authorization"
        );
        assert!(state.pending_authorization.is_some());
    }

    fn release_authorization(&self) {
        let (authorization, complete) = lock(&self.state)
            .pending_authorization
            .take()
            .expect("one pending authorization exists");
        thread::Builder::new()
            .name("fake-broker-held-authorization".to_owned())
            .spawn(move || complete(authorization.ok_or(SupervisorError::AuthorizationRejected)))
            .expect("held authorization callback worker starts")
            .join()
            .expect("held authorization callback worker finishes");
    }

    fn set_append_behavior(&self, behavior: AppendBehavior) {
        lock(&self.state).append_behavior = behavior;
    }

    fn wait_for_append(&self, index: usize) {
        let state = lock(&self.state);
        let (state, timeout) = self
            .changed
            .wait_timeout_while(state, CALLBACK_TIMEOUT, |state| {
                state.receipts.len() <= index
            })
            .expect("broker condition variable remains usable");
        assert!(!timeout.timed_out(), "supervisor never submitted a receipt");
        assert!(matches!(state.pending_append, Some((pending, _)) if pending == index));
    }

    fn receipt_bytes(&self, index: usize) -> Vec<u8> {
        let expected = lock(&self.state)
            .receipts
            .get(index)
            .cloned()
            .expect("receipt was submitted");
        let persisted = fs::read(self.receipt_root.join(format!("{index}.json")))
            .expect("receipt was persisted");
        assert_eq!(persisted, expected, "persisted exact receipt bytes differ");
        persisted
    }

    fn receipts(&self) -> Vec<Vec<u8>> {
        lock(&self.state).receipts.clone()
    }

    fn acknowledge(&self) {
        self.acknowledge_with(|_| {});
    }

    fn acknowledge_with(&self, edit: impl FnOnce(&mut ReceiptAcknowledgement)) {
        let (complete, bytes) = {
            let mut state = lock(&self.state);
            let (index, complete) = state
                .pending_append
                .take()
                .expect("one pending append exists");
            (complete, state.receipts[index].clone())
        };
        let receipt = SignedReceipt::parse_canonical(&bytes).expect("broker receives a receipt");
        record(&self.events, "broker.ack");
        let mut acknowledgement = ReceiptAcknowledgement {
            schema: RECEIPT_ACK_SCHEMA.to_owned(),
            protocol_version: PROTOCOL_VERSION,
            session_id: receipt.payload.session_id,
            run_id: receipt.payload.run_id,
            sequence: receipt.payload.sequence,
            receipt_digest: Digest::of(&bytes).to_string(),
            disposition: ReceiptDisposition::DurablyStored,
        };
        edit(&mut acknowledgement);
        thread::Builder::new()
            .name("fake-broker-receipt-ack".to_owned())
            .spawn(move || complete(Ok(acknowledgement)))
            .expect("receipt ACK callback worker starts")
            .join()
            .expect("receipt ACK callback worker finishes");
    }

    fn wait_for_session_request(&self) {
        let state = lock(&self.state);
        let (state, timeout) = self
            .changed
            .wait_timeout_while(state, CALLBACK_TIMEOUT, |state| {
                state.pending_session_request.is_none()
            })
            .expect("broker condition variable remains usable");
        assert!(
            !timeout.timed_out(),
            "supervisor never armed its Session request receive"
        );
        assert!(state.pending_session_request.is_some());
    }

    fn deliver_session_request(&self, message: ProtocolMessage) {
        let complete = lock(&self.state)
            .pending_session_request
            .take()
            .expect("one Session request receive is armed");
        record(&self.events, "broker.session_request");
        thread::Builder::new()
            .name("fake-broker-session-request".to_owned())
            .spawn(move || complete(Ok(message)))
            .expect("Session request callback worker starts")
            .join()
            .expect("Session request callback worker finishes");
    }

    fn disconnect_session(&self) {
        let complete = lock(&self.state)
            .pending_session_request
            .take()
            .expect("one Session request receive is armed");
        record(&self.events, "broker.disconnect");
        thread::Builder::new()
            .name("fake-broker-disconnect".to_owned())
            .spawn(move || complete(Err(SupervisorError::BrokerUnavailable)))
            .expect("broker disconnect callback worker starts")
            .join()
            .expect("broker disconnect callback worker finishes");
    }

    fn detach_session_request_as_stale(&self) {
        let mut state = lock(&self.state);
        assert!(
            state.stale_session_request.is_none(),
            "only one stale Session request callback may be retained"
        );
        let complete = state
            .pending_session_request
            .take()
            .expect("one old Session request receive is armed");
        state.stale_session_request = Some(complete);
    }

    fn fail_stale_session_request(&self) {
        let complete = lock(&self.state)
            .stale_session_request
            .take()
            .expect("one stale Session request callback is retained");
        record(&self.events, "broker.stale_session_request");
        thread::Builder::new()
            .name("fake-broker-stale-session-request".to_owned())
            .spawn(move || complete(Err(SupervisorError::BrokerUnavailable)))
            .expect("stale broker callback worker starts")
            .join()
            .expect("stale broker callback worker finishes");
    }

    fn wait_for_session_receipt(&self, index: usize) {
        let state = lock(&self.state);
        let (state, timeout) = self
            .changed
            .wait_timeout_while(state, CALLBACK_TIMEOUT, |state| {
                state.session_receipts.len() <= index
            })
            .expect("broker condition variable remains usable");
        assert!(
            !timeout.timed_out(),
            "supervisor never sent the Session receipt; events: {:?}; responses: {:?}",
            event_snapshot(&self.events),
            state.session_responses,
        );
        assert!(state.session_receipts.get(index).is_some());
    }

    fn session_receipt_bytes(&self, index: usize) -> Vec<u8> {
        lock(&self.state)
            .session_receipts
            .get(index)
            .cloned()
            .expect("Session receipt was sent")
    }

    fn session_receipt_count(&self) -> usize {
        lock(&self.state).session_receipts.len()
    }

    fn set_session_receipt_send_behavior(&self, behavior: SessionReceiptSendBehavior) {
        lock(&self.state).session_receipt_send_behavior = behavior;
    }

    fn session_receipt_acknowledgement(&self, index: usize) -> ReceiptAcknowledgement {
        let bytes = self.session_receipt_bytes(index);
        let receipt =
            SignedReceipt::parse_canonical(&bytes).expect("broker receives a canonical receipt");
        ReceiptAcknowledgement {
            schema: RECEIPT_ACK_SCHEMA.to_owned(),
            protocol_version: PROTOCOL_VERSION,
            session_id: receipt.payload.session_id,
            run_id: receipt.payload.run_id,
            sequence: receipt.payload.sequence,
            receipt_digest: Digest::of(&bytes).to_string(),
            disposition: ReceiptDisposition::DurablyStored,
        }
    }

    fn wait_for_session_response(&self, request_id: &str, occurrence: usize) -> ProtocolResponse {
        let state = lock(&self.state);
        let (state, timeout) = self
            .changed
            .wait_timeout_while(state, CALLBACK_TIMEOUT, |state| {
                state
                    .session_responses
                    .iter()
                    .filter(|response| response.request_id == request_id)
                    .count()
                    <= occurrence
            })
            .expect("broker condition variable remains usable");
        assert!(
            !timeout.timed_out(),
            "supervisor never sent response {occurrence} for {request_id}; events: {:?}; responses: {:?}",
            event_snapshot(&self.events),
            state.session_responses,
        );
        state
            .session_responses
            .iter()
            .filter(|response| response.request_id == request_id)
            .nth(occurrence)
            .cloned()
            .expect("requested Session response exists")
    }

    fn session_response_count(&self) -> usize {
        lock(&self.state).session_responses.len()
    }

    fn session_response_count_for(&self, request_id: &str) -> usize {
        lock(&self.state)
            .session_responses
            .iter()
            .filter(|response| response.request_id == request_id)
            .count()
    }

    fn hold_session_response(&self, request_id: &str) {
        lock(&self.state).held_session_response_request_id = Some(request_id.to_owned());
    }

    fn wait_for_held_session_response(&self) {
        let state = lock(&self.state);
        let (state, timeout) = self
            .changed
            .wait_timeout_while(state, CALLBACK_TIMEOUT, |state| {
                state.pending_session_response.is_none()
            })
            .expect("broker condition variable remains usable");
        assert!(
            !timeout.timed_out(),
            "supervisor never sent the held Session response"
        );
        assert!(state.pending_session_response.is_some());
    }

    fn release_session_response(&self) {
        self.release_session_response_with(Ok(()));
    }

    fn release_session_response_with(&self, result: Result<(), SupervisorError>) {
        let complete = lock(&self.state)
            .pending_session_response
            .take()
            .expect("one Session response callback is held");
        thread::Builder::new()
            .name("fake-held-session-response".to_owned())
            .spawn(move || complete(result))
            .expect("held Session response callback worker starts")
            .join()
            .expect("held Session response callback worker finishes");
    }

    fn wait_for_reconnect(&self, index: usize) {
        let state = lock(&self.state);
        let (state, timeout) = self
            .changed
            .wait_timeout_while(state, CALLBACK_TIMEOUT, |state| {
                state.reconnects.len() <= index
            })
            .expect("broker condition variable remains usable");
        assert!(
            !timeout.timed_out(),
            "supervisor never started reconnect {index}"
        );
        assert!(state.pending_reconnect.is_some());
    }

    fn reconnect(&self, index: usize) -> BrokerReconnect {
        lock(&self.state)
            .reconnects
            .get(index)
            .cloned()
            .expect("requested reconnect exists")
    }

    fn complete_reconnect(&self, response: BrokerReconnect) {
        let complete = lock(&self.state)
            .pending_reconnect
            .take()
            .expect("one reconnect callback is held");
        thread::Builder::new()
            .name("fake-broker-reconnect".to_owned())
            .spawn(move || complete(Ok(response)))
            .expect("broker reconnect callback worker starts")
            .join()
            .expect("broker reconnect callback worker finishes");
    }

    fn fail_reconnect(&self, error: SupervisorError) {
        let complete = lock(&self.state)
            .pending_reconnect
            .take()
            .expect("one reconnect callback is held");
        thread::Builder::new()
            .name("fake-broker-reconnect-failure".to_owned())
            .spawn(move || complete(Err(error)))
            .expect("broker reconnect failure callback worker starts")
            .join()
            .expect("broker reconnect failure callback worker finishes");
    }

    fn complete_stale_reconnect(&self, index: usize, response: BrokerReconnect) {
        let complete = lock(&self.state).stale_reconnects.remove(index);
        thread::Builder::new()
            .name("fake-stale-broker-reconnect".to_owned())
            .spawn(move || complete(Ok(response)))
            .expect("stale broker reconnect callback worker starts")
            .join()
            .expect("stale broker reconnect callback worker finishes");
    }

    fn wait_for_controller_loss_settlement(&self, index: usize) {
        let state = lock(&self.state);
        let (state, timeout) = self
            .changed
            .wait_timeout_while(state, CALLBACK_TIMEOUT, |state| {
                state.controller_loss_settlements.len() <= index
            })
            .expect("broker condition variable remains usable");
        assert!(
            !timeout.timed_out(),
            "supervisor never requested controller-loss settlement {index}"
        );
        assert!(state.pending_controller_loss_settlement.is_some());
    }

    fn controller_loss_settlement(&self, index: usize) -> ControllerLossSettlement {
        lock(&self.state)
            .controller_loss_settlements
            .get(index)
            .cloned()
            .expect("requested controller-loss settlement exists")
    }

    fn complete_controller_loss_settlement(
        &self,
        result: Result<ControllerLossAcknowledgement, SupervisorError>,
    ) {
        let complete = lock(&self.state)
            .pending_controller_loss_settlement
            .take()
            .expect("one controller-loss settlement callback is held");
        record(&self.events, "broker.controller_loss_settlement.complete");
        thread::Builder::new()
            .name("fake-controller-loss-settlement".to_owned())
            .spawn(move || complete(result))
            .expect("controller-loss settlement callback worker starts")
            .join()
            .expect("controller-loss settlement callback worker finishes");
    }

    fn is_closed(&self) -> bool {
        lock(&self.state).closed
    }

    fn wait_for_close(&self) {
        let state = lock(&self.state);
        let (state, timeout) = self
            .changed
            .wait_timeout_while(state, CALLBACK_TIMEOUT, |state| !state.closed)
            .expect("broker condition variable remains usable");
        assert!(!timeout.timed_out(), "supervisor never closed the broker");
        assert!(state.closed);
    }
}

impl LaunchBroker for FakeBroker {
    fn send_command(
        &self,
        _message: louiselm_skills::launch_protocol::CommandMessage,
        _complete: SupervisorCompletion<()>,
    ) -> Result<(), SupervisorError> {
        Err(SupervisorError::BrokerUnavailable)
    }
    fn consume_authorization(
        &self,
        _request: LaunchRequest,
        complete: SupervisorCompletion<LaunchAuthorization>,
    ) -> Result<(), SupervisorError> {
        record(&self.events, "broker.consume");
        let authorization = {
            let mut state = lock(&self.state);
            let error = state.authorization_error.take();
            let authorization = state.authorization.take();
            if state.hold_authorization {
                assert!(error.is_none(), "held authorization cannot also fail");
                state.pending_authorization = Some((authorization, complete));
                self.changed.notify_all();
                return Ok(());
            }
            error.map_or_else(
                || authorization.ok_or(SupervisorError::AuthorizationRejected),
                Err,
            )
        };
        thread::Builder::new()
            .name("fake-broker-authorization".to_owned())
            .spawn(move || complete(authorization))
            .map(drop)
            .map_err(|_| SupervisorError::BrokerUnavailable)
    }

    fn append_receipt(
        &self,
        receipt_bytes: Vec<u8>,
        report_bytes: Option<Vec<u8>>,
        complete: SupervisorCompletion<ReceiptAcknowledgement>,
    ) -> Result<(), SupervisorError> {
        record(&self.events, "broker.append");
        let behavior = lock(&self.state).append_behavior;
        if matches!(behavior, AppendBehavior::Reject) {
            self.changed.notify_all();
            return thread::Builder::new()
                .name("fake-broker-append-rejection".to_owned())
                .spawn(move || complete(Err(SupervisorError::DurabilityUnavailable)))
                .map(drop)
                .map_err(|_| SupervisorError::BrokerUnavailable);
        }

        let index = lock(&self.state).receipts.len();
        let receipt_path = self.receipt_root.join(format!("{index}.json"));
        let persisted = fs::create_dir_all(&self.receipt_root)
            .map_err(|_| SupervisorError::DurabilityUnavailable)
            .and_then(|()| {
                fs::File::create(&receipt_path).map_err(|_| SupervisorError::DurabilityUnavailable)
            })
            .and_then(|mut file| {
                file.write_all(&receipt_bytes)
                    .and_then(|()| file.sync_all())
                    .map_err(|_| SupervisorError::DurabilityUnavailable)
            })
            .and_then(|()| {
                fs::File::open(&self.receipt_root)
                    .and_then(|directory| directory.sync_all())
                    .map_err(|_| SupervisorError::DurabilityUnavailable)
            });
        if let Err(error) = persisted {
            complete(Err(error));
            self.changed.notify_all();
            return Ok(());
        }
        record(&self.events, "broker.fsync");
        let mut state = lock(&self.state);
        state.receipts.push(receipt_bytes);
        state.reports.push(report_bytes);
        state.pending_append = Some((index, complete));
        self.changed.notify_all();
        Ok(())
    }

    fn receive_session_request(
        &self,
        complete: SupervisorCompletion<ProtocolMessage>,
    ) -> Result<(), SupervisorError> {
        record(&self.events, "broker.receive_session_request");
        let mut state = lock(&self.state);
        assert!(
            state.pending_session_request.is_none(),
            "only one Session request receive may be armed"
        );
        state.pending_session_request = Some(complete);
        self.changed.notify_all();
        Ok(())
    }

    fn reconnect_session(
        &self,
        reconnect: BrokerReconnect,
        complete: SupervisorCompletion<BrokerReconnect>,
    ) -> Result<(), SupervisorError> {
        record(&self.events, "broker.reconnect_session");
        let mut state = lock(&self.state);
        state.reconnects.push(reconnect);
        let previous = state.pending_reconnect.replace(complete);
        assert!(previous.is_none(), "only one reconnect may be pending");
        self.changed.notify_all();
        Ok(())
    }

    fn cancel_reconnect(&self) {
        record(&self.events, "broker.cancel_reconnect");
        let mut state = lock(&self.state);
        if let Some(complete) = state.pending_reconnect.take() {
            state.stale_reconnects.push(complete);
        }
        self.changed.notify_all();
    }

    fn settle_controller_loss(
        &self,
        settlement: ControllerLossSettlement,
        complete: SupervisorCompletion<ControllerLossAcknowledgement>,
    ) -> Result<(), SupervisorError> {
        record(&self.events, "broker.settle_controller_loss");
        let mut state = lock(&self.state);
        state.controller_loss_settlements.push(settlement);
        let previous = state.pending_controller_loss_settlement.replace(complete);
        assert!(
            previous.is_none(),
            "only one controller-loss settlement may be pending"
        );
        self.changed.notify_all();
        Ok(())
    }

    fn send_session_receipt(
        &self,
        receipt_bytes: Vec<u8>,
        complete: SupervisorCompletion<()>,
    ) -> Result<(), SupervisorError> {
        record(&self.events, "broker.send_session_receipt");
        let behavior = {
            let mut state = lock(&self.state);
            state.session_receipts.push(receipt_bytes);
            state.session_receipt_send_behavior
        };
        self.changed.notify_all();
        thread::Builder::new()
            .name("fake-broker-session-receipt-sent".to_owned())
            .spawn(move || {
                complete(match behavior {
                    SessionReceiptSendBehavior::Complete => Ok(()),
                    SessionReceiptSendBehavior::Reject => {
                        Err(SupervisorError::DurabilityUnavailable)
                    }
                });
            })
            .map_err(|_| SupervisorError::BrokerUnavailable)?
            .join()
            .map_err(|_| SupervisorError::BrokerUnavailable)
    }

    fn send_session_response(
        &self,
        response: ProtocolResponse,
        complete: SupervisorCompletion<()>,
    ) -> Result<(), SupervisorError> {
        record(&self.events, "broker.send_session_response");
        let mut state = lock(&self.state);
        let hold =
            state.held_session_response_request_id.as_deref() == Some(response.request_id.as_str());
        state.session_responses.push(response);
        if hold {
            let previous = state.pending_session_response.replace(complete);
            assert!(previous.is_none(), "only one Session response may be held");
            self.changed.notify_all();
            return Ok(());
        }
        drop(state);
        self.changed.notify_all();
        thread::Builder::new()
            .name("fake-broker-session-response-sent".to_owned())
            .spawn(move || complete(Ok(())))
            .map(drop)
            .map_err(|_| SupervisorError::BrokerUnavailable)
    }

    fn close(&self) {
        record(&self.events, "broker.close");
        lock(&self.state).closed = true;
        self.changed.notify_all();
    }
}

type PendingSignature = (
    Result<String, SupervisorError>,
    SupervisorCompletion<String>,
);

struct FakeSigner {
    authority_valid: AtomicBool,
    fail_completion: AtomicBool,
    containments: Mutex<Vec<louiselm_skills::launcher_install::KeyContainment>>,
    events: Events,
    release_id: String,
    signing_key_id: String,
    payloads: Mutex<Vec<Vec<u8>>>,
    fail_on_call: Mutex<Option<usize>>,
    hold_on_call: Mutex<Option<usize>>,
    pending_call: Mutex<Option<PendingSignature>>,
    changed: Condvar,
}

impl FakeSigner {
    fn wait_for_containment(&self) -> louiselm_skills::launcher_install::KeyContainment {
        let (observations, timeout) = self
            .changed
            .wait_timeout_while(lock(&self.containments), CALLBACK_TIMEOUT, |observations| {
                observations.is_empty()
            })
            .unwrap();
        assert!(
            !timeout.timed_out(),
            "key containment observation never arrived"
        );
        observations[0]
    }
    fn new(events: Events) -> Self {
        Self {
            authority_valid: AtomicBool::new(true),
            fail_completion: AtomicBool::new(false),
            containments: Mutex::new(Vec::new()),
            events,
            release_id: Digest::of(b"release").to_string(),
            signing_key_id: Digest::of(b"signing-key").to_string(),
            payloads: Mutex::new(Vec::new()),
            fail_on_call: Mutex::new(None),
            hold_on_call: Mutex::new(None),
            pending_call: Mutex::new(None),
            changed: Condvar::new(),
        }
    }

    fn fail_on_call(&self, index: usize) {
        *lock(&self.fail_on_call) = Some(index);
    }

    fn hold_on_call(&self, index: usize) {
        *lock(&self.hold_on_call) = Some(index);
    }

    fn wait_for_held_call(&self) {
        let pending = lock(&self.pending_call);
        let (pending, timeout) = self
            .changed
            .wait_timeout_while(pending, CALLBACK_TIMEOUT, |pending| pending.is_none())
            .expect("signer condition variable remains usable");
        assert!(!timeout.timed_out(), "signer callback was never held");
        assert!(pending.is_some());
    }

    fn release_held_call(&self) {
        let (result, complete) = lock(&self.pending_call)
            .take()
            .expect("one signer callback is held");
        thread::Builder::new()
            .name("fake-held-launch-signer".to_owned())
            .spawn(move || complete(result))
            .expect("held signer callback worker starts")
            .join()
            .expect("held signer callback worker finishes");
    }

    fn payloads(&self) -> Vec<Vec<u8>> {
        lock(&self.payloads).clone()
    }
}

impl LaunchSigner for FakeSigner {
    fn complete_session(
        &self,
        terminal: louiselm_skills::launch_receipt::ReceiptPayload,
        complete: SupervisorCompletion<()>,
    ) -> Result<(), SupervisorError> {
        assert_eq!(terminal.resulting_state, SessionState::Terminal);
        record(&self.events, "signer.complete");
        let result = if self.fail_completion.load(Ordering::SeqCst) {
            Err(SupervisorError::DurabilityUnavailable)
        } else {
            Ok(())
        };
        thread::spawn(move || complete(result));
        Ok(())
    }
    fn check_authority(&self, complete: SupervisorCompletion<()>) -> Result<(), SupervisorError> {
        let result = if self.authority_valid.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(SupervisorError::SigningUnavailable)
        };
        thread::spawn(move || complete(result));
        Ok(())
    }
    fn record_containment(
        &self,
        _: String,
        containment: louiselm_skills::launcher_install::KeyContainment,
        complete: SupervisorCompletion<()>,
    ) -> Result<(), SupervisorError> {
        lock(&self.containments).push(containment);
        self.changed.notify_all();
        thread::spawn(move || complete(Ok(())));
        Ok(())
    }
    fn release_id(&self) -> &str {
        &self.release_id
    }

    fn signing_key_id(&self) -> &str {
        &self.signing_key_id
    }

    fn sign(
        &self,
        payload_bytes: Vec<u8>,
        complete: SupervisorCompletion<String>,
    ) -> Result<(), SupervisorError> {
        record(&self.events, "signer.sign");
        let index = {
            let mut payloads = lock(&self.payloads);
            let index = payloads.len();
            payloads.push(payload_bytes);
            index
        };
        let result = if *lock(&self.fail_on_call) == Some(index) {
            Err(SupervisorError::SigningUnavailable)
        } else {
            Ok(format!("test-signature-{index}"))
        };
        if *lock(&self.hold_on_call) == Some(index) {
            let previous = lock(&self.pending_call).replace((result, complete));
            assert!(previous.is_none(), "only one signer callback may be held");
            self.changed.notify_all();
            return Ok(());
        }
        thread::Builder::new()
            .name("fake-launch-signer".to_owned())
            .spawn(move || complete(result))
            .map(drop)
            .map_err(|_| SupervisorError::SigningUnavailable)
    }
}

type TimerCallback = Box<dyn FnOnce() + Send + 'static>;

#[derive(Default)]
struct TimerState {
    delays: Vec<Duration>,
    callbacks: Vec<Option<TimerCallback>>,
    schedule_calls: usize,
    fail_on_schedule_call: Option<usize>,
}

struct FakeTimer {
    events: Events,
    now: Mutex<Instant>,
    state: Mutex<TimerState>,
    changed: Condvar,
}

impl FakeTimer {
    fn new(events: Events) -> Self {
        Self {
            events,
            now: Mutex::new(Instant::now()),
            state: Mutex::new(TimerState::default()),
            changed: Condvar::new(),
        }
    }

    fn wait_for_schedule(&self, index: usize) -> Duration {
        let state = lock(&self.state);
        let (state, timeout) = self
            .changed
            .wait_timeout_while(state, CALLBACK_TIMEOUT, |state| {
                state.callbacks.len() <= index
            })
            .expect("timer condition variable remains usable");
        assert!(!timeout.timed_out(), "timer {index} was never scheduled");
        state.delays[index]
    }

    fn fail_on_schedule_call(&self, index: usize) {
        lock(&self.state).fail_on_schedule_call = Some(index);
    }

    fn fire(&self, index: usize) {
        let complete = lock(&self.state)
            .callbacks
            .get_mut(index)
            .and_then(Option::take)
            .expect("scheduled timer callback fires once");
        record(&self.events, "timer.fire");
        thread::Builder::new()
            .name("fake-supervisor-timer".to_owned())
            .spawn(complete)
            .expect("timer callback worker starts")
            .join()
            .expect("timer callback worker finishes");
    }

    fn scheduled_count(&self) -> usize {
        lock(&self.state).callbacks.len()
    }

    fn advance(&self, duration: Duration) {
        let mut now = lock(&self.now);
        *now = now.checked_add(duration).unwrap_or(*now);
    }
}

impl SupervisorTimer for FakeTimer {
    fn now(&self) -> Instant {
        *lock(&self.now)
    }

    fn schedule(&self, delay: Duration, complete: TimerCallback) -> Result<(), SupervisorError> {
        record(&self.events, "timer.schedule");
        let mut state = lock(&self.state);
        let call = state.schedule_calls;
        state.schedule_calls += 1;
        if state.fail_on_schedule_call == Some(call) {
            state.fail_on_schedule_call = None;
            self.changed.notify_all();
            return Err(SupervisorError::WorkerUnavailable);
        }
        state.delays.push(delay);
        state.callbacks.push(Some(complete));
        self.changed.notify_all();
        Ok(())
    }
}

struct FakeIdentityGuard {
    events: Events,
    identity: Identity,
    poisoned: bool,
    released: bool,
}

impl IdentityGuard for FakeIdentityGuard {
    fn identity(&self) -> Identity {
        self.identity
    }

    fn release(mut self: Box<Self>) -> Result<(), SupervisorError> {
        self.released = true;
        record(&self.events, "identity.release");
        Ok(())
    }

    fn poison(mut self: Box<Self>) -> Result<(), SupervisorError> {
        self.poisoned = true;
        record(&self.events, "identity.poison");
        Ok(())
    }
}

impl Drop for FakeIdentityGuard {
    fn drop(&mut self) {
        if !self.poisoned && !self.released {
            record(&self.events, "identity.dropped_without_release");
        }
    }
}

#[derive(Default)]
struct GateState {
    binding: Option<CapabilityBinding>,
    enabled: bool,
    revoked: bool,
    closed: bool,
}

struct FakeCapabilityGate {
    events: Events,
    state: Arc<Mutex<GateState>>,
    channel: Channel,
    enable_fails: bool,
    revoke_fails: bool,
}

struct FakeProcessMembership {
    members: Vec<u32>,
}

impl ProcessMembership for FakeProcessMembership {
    fn contains(&self, pid: u32) -> Result<bool, SupervisorError> {
        Ok(self.members.contains(&pid))
    }
}

impl CapabilityGate for FakeCapabilityGate {
    fn command_enforcer(
        &self,
    ) -> Result<Arc<louiselm_skills::launch_supervisor::command::CommandEnforcer>, SupervisorError>
    {
        Err(SupervisorError::CapabilityUnavailable)
    }
    fn receive_command(
        &mut self,
        _complete: SupervisorCompletion<ProtocolMessage>,
    ) -> Result<(), SupervisorError> {
        Ok(())
    }
    fn authorize_command(
        &self,
        _request: &louiselm_skills::launch_protocol::ToolExecutionRequest,
        _decision: &louiselm_skills::launch_protocol::CommandMessage,
        _forwarded_at: Instant,
    ) -> Result<louiselm_skills::launch_supervisor::command::CommandPermit, SupervisorError> {
        Err(SupervisorError::AgentIdentityRejected)
    }
    fn send_command(
        &self,
        _message: louiselm_skills::launch_protocol::CommandMessage,
        _complete: SupervisorCompletion<()>,
    ) -> Result<(), SupervisorError> {
        Err(SupervisorError::CapabilityUnavailable)
    }
    fn channel(&self) -> Channel {
        self.channel.clone()
    }

    fn bind(
        &mut self,
        binding: CapabilityBinding,
        authentication: AgentAuthentication,
    ) -> Result<(), SupervisorError> {
        record(&self.events, "capability.bind");
        if binding.agent_pid != authentication.credentials.pid {
            return Err(SupervisorError::CapabilityUnavailable);
        }
        lock(&self.state).binding = Some(binding);
        Ok(())
    }

    fn enable(&mut self) -> Result<(), SupervisorError> {
        record(&self.events, "capability.enable");
        if self.enable_fails {
            return Err(SupervisorError::CapabilityUnavailable);
        }
        let mut state = lock(&self.state);
        state.enabled = true;
        state.revoked = false;
        Ok(())
    }

    fn revoke(&mut self) -> Result<(), SupervisorError> {
        record(&self.events, "capability.revoke");
        let mut state = lock(&self.state);
        state.enabled = false;
        state.revoked = true;
        if self.revoke_fails {
            Err(SupervisorError::CapabilityUnavailable)
        } else {
            Ok(())
        }
    }

    fn close(&mut self) {
        let mut state = lock(&self.state);
        if !state.closed {
            record(&self.events, "capability.close");
            state.closed = true;
        }
    }
}

#[derive(Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Independent failure injections and observed effects must be independently selectable in this test double."
)]
struct AgentState {
    hold_tool: bool,
    tool_completion:
        Option<SupervisorCompletion<louiselm_skills::launch_protocol::ToolExecutionResult>>,
    started: bool,
    parked: bool,
    resumed: bool,
    disposed: bool,
    dispose_attempts: usize,
    relayed_input: Vec<u8>,
    running_events: Option<RunningAgentEvents>,
    hold_relay_quiescence: bool,
    relay_quiescence: Option<SupervisorCompletion<()>>,
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "Independent failure injections and observed effects must be independently selectable in this test double."
)]
struct FakePreparedAgent {
    credentials: louiselm_skills::launch_transport::KernelCredentials,
    events: Events,
    state: Arc<Mutex<AgentState>>,
    agent_changed: Arc<Condvar>,
    evidence: IsolationEvidence,
    backend_id: String,
    sandbox_leader_pid: Option<u32>,
    membership: Arc<dyn ProcessMembership>,
    output: Vec<u8>,
    dispose_fails: bool,
    start_fails: bool,
    park_fails: bool,
    resume_fails: bool,
    resume_ambiguous_after_running: bool,
    repark_failure: Option<MechanicFailure>,
    active: bool,
}

impl PreparedAgent for FakePreparedAgent {
    fn evidence(&self) -> &IsolationEvidence {
        &self.evidence
    }

    fn backend_id(&self) -> &str {
        &self.backend_id
    }

    fn sandbox_leader_pid(&self) -> Option<u32> {
        self.sandbox_leader_pid
    }

    fn processes(&self) -> Result<Vec<u32>, SupervisorError> {
        Ok(self.sandbox_leader_pid.into_iter().collect())
    }

    fn process_membership(&self) -> Arc<dyn ProcessMembership> {
        Arc::clone(&self.membership)
    }

    fn start(mut self: Box<Self>) -> Result<Box<dyn RunningAgent>, SupervisorError> {
        record(&self.events, "agent.start");
        if self.start_fails {
            return match self.dispose() {
                Ok(()) => Err(SupervisorError::SpawnFailed),
                Err(_) => Err(SupervisorError::CleanupUnproven),
            };
        }
        lock(&self.state).started = true;
        self.active = false;
        Ok(Box::new(FakeRunningAgent {
            credentials: self.credentials,
            events: Arc::clone(&self.events),
            state: Arc::clone(&self.state),
            agent_changed: Arc::clone(&self.agent_changed),
            output: self.output.clone(),
            relay_worker: None,
            relay_stopped: Arc::new(AtomicBool::new(false)),
            park_fails: self.park_fails,
            resume_fails: self.resume_fails,
            resume_ambiguous_after_running: self.resume_ambiguous_after_running,
            repark_failure: self.repark_failure,
            dispose_fails: self.dispose_fails,
            active: true,
        }))
    }

    fn dispose(&mut self) -> Result<(), SupervisorError> {
        if self.active {
            record(&self.events, "agent.dispose");
            lock(&self.state).disposed = true;
            self.active = false;
        }
        if self.dispose_fails {
            Err(SupervisorError::CleanupUnproven)
        } else {
            Ok(())
        }
    }
}

impl Drop for FakePreparedAgent {
    fn drop(&mut self) {
        if self.active {
            record(&self.events, "agent.prepared_dropped_without_disposal");
        }
    }
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "Independent failure injections and observed effects must be independently selectable in this test double."
)]
struct FakeRunningAgent {
    credentials: louiselm_skills::launch_transport::KernelCredentials,
    events: Events,
    state: Arc<Mutex<AgentState>>,
    agent_changed: Arc<Condvar>,
    output: Vec<u8>,
    relay_worker: Option<thread::JoinHandle<()>>,
    relay_stopped: Arc<AtomicBool>,
    park_fails: bool,
    resume_fails: bool,
    resume_ambiguous_after_running: bool,
    repark_failure: Option<MechanicFailure>,
    dispose_fails: bool,
    active: bool,
}

impl FakeRunningAgent {
    fn stop_relay(&mut self) {
        self.relay_stopped.store(true, Ordering::Release);
        if let Some(worker) = self.relay_worker.take() {
            worker.join().expect("fake relay worker finishes");
        }
    }
}

fn run_fake_relay(
    receiver: &Receiver<RelayStdio>,
    state: &Mutex<AgentState>,
    output: &[u8],
    stopped: &AtomicBool,
) -> Result<(), SupervisorError> {
    let mut controller = loop {
        if stopped.load(Ordering::Acquire) {
            return Ok(());
        }
        match receiver.recv_timeout(Duration::from_millis(1)) {
            Ok(controller) => break controller,
            Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    };
    let mut input = Vec::new();
    let mut buffer = [0; 8192];
    while !stopped.load(Ordering::Acquire) {
        match controller.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => input.extend_from_slice(&buffer[..read]),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(1));
            }
            Err(error) => return Err(error.into()),
        }
    }
    lock(state).relayed_input = input;
    let mut remaining = output;
    while !remaining.is_empty() && !stopped.load(Ordering::Acquire) {
        match controller.write(remaining) {
            Ok(0) => return Err(SupervisorError::RelayFailed),
            Ok(written) => remaining = &remaining[written..],
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(1));
            }
            Err(error) => return Err(error.into()),
        }
    }
    controller.close().map_err(Into::into)
}

impl RunningAgent for FakeRunningAgent {
    fn launch_helper(
        &mut self,
        _request: louiselm_skills::launch_protocol::CommandMessage,
        _enforcer: Arc<louiselm_skills::launch_supervisor::command::CommandEnforcer>,
        _complete: SupervisorCompletion<louiselm_skills::launch_supervisor::HelperPrincipal>,
    ) -> Result<(), SupervisorError> {
        Err(SupervisorError::ToolIsolationUnproven)
    }
    fn cancel_helper(&mut self) -> Result<(), SupervisorError> {
        Ok(())
    }
    fn cancel_tool(&mut self) -> Result<(), SupervisorError> {
        Ok(())
    }
    fn execute_tool(
        &mut self,
        _permit: louiselm_skills::launch_supervisor::command::CommandPermit,
        complete: SupervisorCompletion<louiselm_skills::launch_protocol::ToolExecutionResult>,
    ) -> Result<(), SupervisorError> {
        record(&self.events, "agent.tool");
        if lock(&self.state).hold_tool {
            lock(&self.state).tool_completion = Some(complete);
            self.agent_changed.notify_all();
            return Ok(());
        }
        thread::spawn(move || {
            complete(Ok(louiselm_skills::launch_protocol::ToolExecutionResult {
                exit_code: 0,
                stdout: "tool output".to_owned(),
                stderr: String::new(),
                truncated: false,
                timed_out: false,
            }));
        });
        Ok(())
    }
    fn authentication(&self) -> Result<AgentAuthentication, SupervisorError> {
        Ok(AgentAuthentication {
            credentials: self.credentials,
            process: None,
            tool_isolation: None,
        })
    }

    fn start_relay(
        &mut self,
        controller: Receiver<RelayStdio>,
        complete: RunningAgentEvents,
    ) -> Result<(), SupervisorError> {
        record(&self.events, "agent.relay");
        let state = Arc::clone(&self.state);
        let output_bytes = self.output.clone();
        let stopped = Arc::clone(&self.relay_stopped);
        lock(&state).running_events = Some(Arc::clone(&complete));
        self.relay_worker = Some(
            thread::Builder::new()
                .name("fake-agent-relay".to_owned())
                .spawn(move || {
                    let result = run_fake_relay(&controller, &state, &output_bytes, &stopped);
                    if !stopped.load(Ordering::Acquire) {
                        complete(if result.is_ok() {
                            RunningAgentEvent::ControllerEof
                        } else {
                            RunningAgentEvent::RelayFailed
                        });
                    }
                })
                .map_err(|_| SupervisorError::RelayFailed)?,
        );
        Ok(())
    }

    fn quiesce_relay(&mut self, complete: SupervisorCompletion<()>) -> Result<(), SupervisorError> {
        self.stop_relay();
        record(&self.events, "agent.quiesce_relay");
        let mut state = lock(&self.state);
        if state.hold_relay_quiescence {
            let previous = state.relay_quiescence.replace(complete);
            assert!(
                previous.is_none(),
                "only one relay quiescence may be pending"
            );
            self.agent_changed.notify_all();
            return Ok(());
        }
        drop(state);
        thread::Builder::new()
            .name("fake-relay-quiescence".to_owned())
            .spawn(move || complete(Ok(())))
            .map(drop)
            .map_err(|_| SupervisorError::RelayFailed)
    }

    fn park(&mut self) -> Result<(), MechanicFailure> {
        record(&self.events, "agent.park");
        if lock(&self.state).resumed
            && let Some(failure) = self.repark_failure
        {
            return Err(failure);
        }
        if self.park_fails {
            return Err(MechanicFailure::Running);
        }
        let mut state = lock(&self.state);
        state.parked = true;
        state.resumed = false;
        Ok(())
    }

    fn resume(&mut self) -> Result<(), MechanicFailure> {
        record(&self.events, "agent.resume");
        if self.resume_ambiguous_after_running {
            let mut state = lock(&self.state);
            state.parked = false;
            state.resumed = true;
            return Err(MechanicFailure::Ambiguous);
        }
        if self.resume_fails {
            return Err(MechanicFailure::Parked);
        }
        let mut state = lock(&self.state);
        state.parked = false;
        state.resumed = true;
        Ok(())
    }

    fn interrupt(&mut self) -> Result<(), MechanicFailure> {
        record(&self.events, "agent.interrupt");
        Ok(())
    }

    fn dispose(&mut self) -> Result<(), SupervisorError> {
        self.stop_relay();
        if self.active {
            record(&self.events, "agent.dispose");
            let mut state = lock(&self.state);
            state.dispose_attempts += 1;
            if self.dispose_fails {
                return Err(SupervisorError::CleanupUnproven);
            }
            state.disposed = true;
            self.active = false;
        }
        Ok(())
    }
}

impl Drop for FakeRunningAgent {
    fn drop(&mut self) {
        self.stop_relay();
        if self.active {
            record(&self.events, "agent.running_dropped_without_disposal");
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct OuterProcessObservation {
    sandbox_leader_pid: u32,
    monitor_pid: u32,
    uids: Vec<u32>,
    gids: Vec<u32>,
    groups: Vec<u32>,
}

struct TreeMembership {
    tree: ProcessTree,
}

impl ProcessMembership for TreeMembership {
    fn contains(&self, pid: u32) -> Result<bool, SupervisorError> {
        self.tree.contains(pid).map_err(map_test_sandbox)
    }
}

struct BubblewrapPreparedAgent {
    prepared: Option<PreparedSession>,
    backend_id: String,
    tree: ProcessTree,
}

impl PreparedAgent for BubblewrapPreparedAgent {
    fn evidence(&self) -> &IsolationEvidence {
        self.prepared
            .as_ref()
            .expect("the real prepared Session remains owned")
            .evidence()
    }

    fn backend_id(&self) -> &str {
        &self.backend_id
    }

    fn sandbox_leader_pid(&self) -> Option<u32> {
        self.prepared
            .as_ref()
            .expect("the real prepared Session remains owned")
            .sandbox_leader_pid()
    }

    fn processes(&self) -> Result<Vec<u32>, SupervisorError> {
        self.prepared
            .as_ref()
            .expect("the real prepared Session remains owned")
            .processes()
            .map_err(map_test_sandbox)
    }

    fn process_membership(&self) -> Arc<dyn ProcessMembership> {
        Arc::new(TreeMembership {
            tree: self.tree.clone(),
        })
    }

    fn start(mut self: Box<Self>) -> Result<Box<dyn RunningAgent>, SupervisorError> {
        let prepared = self
            .prepared
            .take()
            .expect("the real prepared Session remains owned");
        let session = prepared.start().map_err(map_test_sandbox)?;
        Ok(Box::new(SystemRunningAgent::new(session)))
    }

    fn dispose(&mut self) -> Result<(), SupervisorError> {
        self.prepared
            .as_mut()
            .expect("the real prepared Session remains owned")
            .dispose()
            .map(|_| ())
            .map_err(map_test_sandbox)
    }
}

struct BubblewrapLaunchPlatform {
    events: Events,
    expected_identity: Identity,
    backend: BubblewrapBackend,
    backend_id: String,
    capability_root: PathBuf,
    observation: Arc<Mutex<Option<OuterProcessObservation>>>,
}

impl LaunchPlatform for BubblewrapLaunchPlatform {
    fn inspect_conformance(
        &self,
        _: &LaunchAuthorization,
        _: u64,
        complete: SupervisorCompletion<louiselm_skills::launch_supervisor::ConformanceAdmission>,
    ) -> Result<(), SupervisorError> {
        complete(Ok(
            louiselm_skills::launch_supervisor::ConformanceAdmission {
                evidence: ConformanceEvidence::Unevaluated,
                report_bytes: None,
            },
        ));
        Ok(())
    }

    fn check_integration(
        &self,
        _: &LaunchRequest,
        complete: SupervisorCompletion<()>,
    ) -> Result<(), SupervisorError> {
        complete(Ok(()));
        Ok(())
    }
    fn verify_tool_isolation(
        &self,
        _request: &LaunchRequest,
        _agent: &AgentAuthentication,
        complete: SupervisorCompletion<Digest>,
    ) -> Result<(), SupervisorError> {
        // This fixture proves host process identity only, not tool separation.
        complete(Ok(Digest::of(b"fixture-tool-isolation")));
        Ok(())
    }

    fn acquire_identity(
        &self,
        assigned: Identity,
    ) -> Result<Box<dyn IdentityGuard>, SupervisorError> {
        if assigned != self.expected_identity {
            return Err(SupervisorError::IdentityUnavailable);
        }
        Ok(Box::new(FakeIdentityGuard {
            events: Arc::clone(&self.events),
            identity: assigned,
            poisoned: false,
            released: false,
        }))
    }

    fn create_capability(
        &self,
        _request: &LaunchRequest,
        assigned: Identity,
    ) -> Result<Box<dyn CapabilityGate>, SupervisorError> {
        if assigned != self.expected_identity {
            return Err(SupervisorError::CapabilityUnavailable);
        }
        fs::create_dir_all(&self.capability_root)
            .map_err(|_| SupervisorError::CapabilityUnavailable)?;
        fs::set_permissions(&self.capability_root, fs::Permissions::from_mode(0o755))
            .map_err(|_| SupervisorError::CapabilityUnavailable)?;
        let path = self.capability_root.join("broker.sock");
        write_file(&path, "inert test capability\n");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644))
            .map_err(|_| SupervisorError::CapabilityUnavailable)?;
        Ok(Box::new(FakeCapabilityGate {
            events: Arc::clone(&self.events),
            state: Arc::new(Mutex::new(GateState::default())),
            channel: Channel::UnixSocket {
                id: "broker".to_owned(),
                host_path: path,
                guest_path: PathBuf::from("/tmp/louiselm-broker.sock"),
            },
            enable_fails: false,
            revoke_fails: false,
        }))
    }

    fn prepare(
        &self,
        _request: &LaunchRequest,
        plan: ConfinementPlan,
    ) -> Result<Box<dyn PreparedAgent>, SupervisorError> {
        if plan.identity
            != (IdentityPlan::HostIdentity {
                uid: self.expected_identity.uid,
                gid: self.expected_identity.gid,
            })
        {
            return Err(SupervisorError::IdentityUnavailable);
        }
        let prepared = self.backend.prepare(&plan).map_err(map_test_sandbox)?;
        let tree = prepared
            .process_tree()
            .ok_or(SupervisorError::IsolationRejected)?;
        let sandbox_leader_pid = prepared
            .sandbox_leader_pid()
            .ok_or(SupervisorError::IsolationRejected)?;
        *lock(&self.observation) = Some(observe_sandbox_leader(
            sandbox_leader_pid,
            prepared.monitor_pid(),
        )?);
        Ok(Box::new(BubblewrapPreparedAgent {
            prepared: Some(prepared),
            backend_id: self.backend_id.clone(),
            tree,
        }))
    }
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "Result::map_err transfers ownership of the error into this test conversion."
)]
fn map_test_sandbox(error: SandboxError) -> SupervisorError {
    match error {
        SandboxError::CleanupUnproven { .. } | SandboxError::Survivors { .. } => {
            SupervisorError::CleanupUnproven
        }
        _ => SupervisorError::SpawnFailed,
    }
}

fn observe_sandbox_leader(
    sandbox_leader_pid: u32,
    monitor_pid: u32,
) -> Result<OuterProcessObservation, SupervisorError> {
    let deadline = Instant::now() + CALLBACK_TIMEOUT;
    loop {
        if let (Some(uids), Some(gids), Some(groups)) = (
            process_status_values(sandbox_leader_pid, "Uid:"),
            process_status_values(sandbox_leader_pid, "Gid:"),
            process_status_values(sandbox_leader_pid, "Groups:"),
        ) {
            return Ok(OuterProcessObservation {
                sandbox_leader_pid,
                monitor_pid,
                uids,
                gids,
                groups,
            });
        }
        if Instant::now() >= deadline {
            return Err(SupervisorError::RelayFailed);
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn process_status_values(pid: u32, field: &str) -> Option<Vec<u32>> {
    fs::read_to_string(format!("/proc/{pid}/status"))
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix(field))?
        .split_whitespace()
        .map(str::parse)
        .collect::<Result<Vec<_>, _>>()
        .ok()
}

#[derive(Clone, Copy, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Independent failure injections and observed effects must be independently selectable in this test double."
)]
struct PlatformBehavior {
    conformance_refused: bool,
    agent_credentials: Option<louiselm_skills::launch_transport::KernelCredentials>,
    tool_isolation_unproven: bool,
    integration_unsupported: bool,
    identity_unavailable: bool,
    identity_occupied: bool,
    identity_poisoned: bool,
    identity_guard_mismatch: bool,
    prepare_fails: bool,
    unverified_evidence: bool,
    dispose_fails: bool,
    enable_fails: bool,
    start_fails: bool,
    revoke_fails: bool,
    park_fails: bool,
    resume_fails: bool,
    resume_ambiguous_after_running: bool,
    repark_failure: Option<MechanicFailure>,
}

#[derive(Default)]
struct PlatformState {
    conformance_report: Option<Vec<u8>>,
    plan: Option<ConfinementPlan>,
    gate: Option<Arc<Mutex<GateState>>>,
}

struct FakePlatform {
    events: Events,
    expected_identity: Identity,
    behavior: PlatformBehavior,
    state: Mutex<PlatformState>,
    agent: Arc<Mutex<AgentState>>,
    agent_changed: Arc<Condvar>,
    agent_output: Vec<u8>,
    capability_root: PathBuf,
}

impl FakePlatform {
    fn gate_state(&self) -> Arc<Mutex<GateState>> {
        lock(&self.state)
            .gate
            .clone()
            .expect("capability was created")
    }

    fn plan(&self) -> ConfinementPlan {
        lock(&self.state).plan.clone().expect("Agent was prepared")
    }

    fn send_running_event(&self, event: RunningAgentEvent) {
        let complete = lock(&self.agent)
            .running_events
            .clone()
            .expect("running Agent event callback is installed");
        let event_name = match &event {
            RunningAgentEvent::ControllerEof => "agent.event.controller_eof",
            RunningAgentEvent::ProcessExited(ProcessExitClassification::Success) => {
                "agent.event.process_exited.success"
            }
            RunningAgentEvent::ProcessExited(ProcessExitClassification::Failure) => {
                "agent.event.process_exited.failure"
            }
            RunningAgentEvent::ProcessExited(ProcessExitClassification::Signaled) => {
                "agent.event.process_exited.signaled"
            }
            RunningAgentEvent::RelayFailed => "agent.event.relay_failed",
            RunningAgentEvent::AgentIdentityLost => "agent.event.agent_identity_lost",
        };
        record(&self.events, event_name);
        thread::Builder::new()
            .name("fake-running-agent-event".to_owned())
            .spawn(move || complete(event))
            .expect("running Agent event worker starts")
            .join()
            .expect("running Agent event worker finishes");
    }

    fn hold_relay_quiescence(&self) {
        lock(&self.agent).hold_relay_quiescence = true;
    }

    fn wait_for_relay_quiescence(&self) {
        let state = lock(&self.agent);
        let (state, timeout) = self
            .agent_changed
            .wait_timeout_while(state, CALLBACK_TIMEOUT, |state| {
                state.relay_quiescence.is_none()
            })
            .expect("Agent condition variable remains usable");
        assert!(
            !timeout.timed_out(),
            "supervisor never requested relay quiescence"
        );
        assert!(state.relay_quiescence.is_some());
    }

    fn complete_relay_quiescence(&self) {
        let complete = lock(&self.agent)
            .relay_quiescence
            .take()
            .expect("one relay quiescence callback is held");
        record(&self.events, "agent.quiesce_relay.complete");
        thread::Builder::new()
            .name("fake-relay-quiescence".to_owned())
            .spawn(move || complete(Ok(())))
            .expect("relay-quiescence callback worker starts")
            .join()
            .expect("relay-quiescence callback worker finishes");
    }
}

impl LaunchPlatform for FakePlatform {
    fn inspect_conformance(
        &self,
        _: &LaunchAuthorization,
        _: u64,
        complete: SupervisorCompletion<louiselm_skills::launch_supervisor::ConformanceAdmission>,
    ) -> Result<(), SupervisorError> {
        let report_bytes = lock(&self.state).conformance_report.clone();
        let result = if self.behavior.conformance_refused {
            Err(SupervisorError::ConformanceRefused(
                louiselm_skills::conformance::admission::Condition::Missing,
            ))
        } else {
            Ok(louiselm_skills::launch_supervisor::ConformanceAdmission {
                evidence: report_bytes
                    .as_ref()
                    .map_or(ConformanceEvidence::Unevaluated, |bytes| {
                        ConformanceEvidence::Certified {
                            report_digest: Digest::of(bytes).to_string(),
                        }
                    }),
                report_bytes,
            })
        };
        thread::Builder::new()
            .name("fake-conformance-producer".into())
            .spawn(move || complete(result))
            .unwrap()
            .join()
            .unwrap();
        Ok(())
    }

    fn check_integration(
        &self,
        _: &LaunchRequest,
        complete: SupervisorCompletion<()>,
    ) -> Result<(), SupervisorError> {
        complete(if self.behavior.integration_unsupported {
            Err(SupervisorError::ToolIsolationUnproven)
        } else {
            Ok(())
        });
        Ok(())
    }
    fn verify_tool_isolation(
        &self,
        _request: &LaunchRequest,
        _agent: &AgentAuthentication,
        complete: SupervisorCompletion<Digest>,
    ) -> Result<(), SupervisorError> {
        record(&self.events, "platform.verify_tool_isolation");
        if self.behavior.tool_isolation_unproven {
            complete(Err(SupervisorError::ToolIsolationUnproven));
        } else {
            complete(Ok(Digest::of(b"fixture-tool-isolation")));
        }
        Ok(())
    }

    fn acquire_identity(
        &self,
        assigned: Identity,
    ) -> Result<Box<dyn IdentityGuard>, SupervisorError> {
        record(&self.events, "identity.acquire");
        if self.behavior.identity_unavailable {
            return Err(SupervisorError::IdentityUnavailable);
        }
        if self.behavior.identity_occupied
            || self.behavior.identity_poisoned
            || assigned != self.expected_identity
        {
            return Err(SupervisorError::IdentityAssignmentInvalid);
        }
        let identity = if self.behavior.identity_guard_mismatch {
            Identity {
                slot: assigned.slot + 1,
                ..assigned
            }
        } else {
            assigned
        };
        Ok(Box::new(FakeIdentityGuard {
            events: Arc::clone(&self.events),
            identity,
            poisoned: false,
            released: false,
        }))
    }

    fn create_capability(
        &self,
        _request: &LaunchRequest,
        _assigned: Identity,
    ) -> Result<Box<dyn CapabilityGate>, SupervisorError> {
        record(&self.events, "capability.create");
        let state = Arc::new(Mutex::new(GateState::default()));
        lock(&self.state).gate = Some(Arc::clone(&state));
        Ok(Box::new(FakeCapabilityGate {
            events: Arc::clone(&self.events),
            state,
            channel: Channel::UnixSocket {
                id: "broker".to_owned(),
                host_path: self.capability_root.join("broker.sock"),
                guest_path: PathBuf::from("/run/louiselm/broker.sock"),
            },
            enable_fails: self.behavior.enable_fails,
            revoke_fails: self.behavior.revoke_fails,
        }))
    }

    fn prepare(
        &self,
        _request: &LaunchRequest,
        plan: ConfinementPlan,
    ) -> Result<Box<dyn PreparedAgent>, SupervisorError> {
        record(&self.events, "platform.prepare");
        lock(&self.state).plan = Some(plan);
        if self.behavior.prepare_fails {
            return Err(SupervisorError::SpawnFailed);
        }
        Ok(Box::new(FakePreparedAgent {
            credentials: self.behavior.agent_credentials.unwrap_or(
                louiselm_skills::launch_transport::KernelCredentials {
                    pid: 42_425,
                    uid: self.expected_identity.uid,
                    gid: self.expected_identity.gid,
                },
            ),
            events: Arc::clone(&self.events),
            state: Arc::clone(&self.agent),
            agent_changed: Arc::clone(&self.agent_changed),
            evidence: isolation_evidence(!self.behavior.unverified_evidence),
            backend_id: Digest::of(b"bubblewrap-binary").to_string(),
            sandbox_leader_pid: Some(42_424),
            membership: Arc::new(FakeProcessMembership {
                members: vec![42_424, 42_425],
            }),
            output: self.agent_output.clone(),
            dispose_fails: self.behavior.dispose_fails,
            start_fails: self.behavior.start_fails,
            park_fails: self.behavior.park_fails,
            resume_fails: self.behavior.resume_fails,
            resume_ambiguous_after_running: self.behavior.resume_ambiguous_after_running,
            repark_failure: self.behavior.repark_failure,
            active: true,
        }))
    }
}

fn isolation_evidence(verified: bool) -> IsolationEvidence {
    let mut dimensions = Dimension::ALL
        .into_iter()
        .map(|dimension| DimensionEvidence {
            dimension,
            satisfied: true,
            mechanism: format!("test-{}", dimension.name()),
            detail: "deterministic fake evidence".to_owned(),
        })
        .collect::<Vec<_>>();
    if !verified {
        dimensions[0].satisfied = false;
    }
    IsolationEvidence {
        contract_version: CONTRACT_VERSION.to_owned(),
        native_sources: None,
        backend: "bubblewrap".to_owned(),
        backend_version: "0.12.0".to_owned(),
        kernel: KernelPrerequisites {
            user_namespaces: true,
            pid_namespaces: true,
            network_namespaces: true,
            cgroup_v2: true,
            details: vec!["deterministic test kernel".to_owned()],
        },
        dimensions,
    }
}

struct Setup {
    fixture: Fixture,
    request: LaunchRequest,
    broker: Arc<FakeBroker>,
    signer: Arc<FakeSigner>,
    platform: Arc<FakePlatform>,
    registry: Arc<Registry>,
    sessions_root: PathBuf,
    timeout: Duration,
    supervisor: LaunchSupervisor,
    events: Events,
}

impl Setup {
    fn fresh_supervisor(&self) -> LaunchSupervisor {
        LaunchSupervisor::new(
            self.broker.clone(),
            self.signer.clone(),
            self.platform.clone(),
            Arc::clone(&self.registry),
            self.sessions_root.clone(),
            self.timeout,
        )
    }

    fn fresh_supervisor_with_timer(&self, timer: Arc<dyn SupervisorTimer>) -> LaunchSupervisor {
        LaunchSupervisor::new_with_timer(
            self.broker.clone(),
            self.signer.clone(),
            self.platform.clone(),
            Arc::clone(&self.registry),
            self.sessions_root.clone(),
            self.timeout,
            timer,
        )
    }
}

fn setup(
    authorization_available: bool,
    edit_authorization: impl FnOnce(&mut LaunchAuthorization),
    append_behavior: AppendBehavior,
    platform_behavior: PlatformBehavior,
    timeout: Duration,
) -> Setup {
    let fixture = Fixture::new();
    let runtime_root = fixture.path("runtime");
    write_file(&runtime_root.join("bin/agent"), "#!/bin/sh\nexec cat\n");
    fs::set_permissions(
        runtime_root.join("bin/agent"),
        fs::Permissions::from_mode(0o755),
    )
    .expect("runtime executable becomes executable");
    write_file(&runtime_root.join("lib/adapter.js"), "// adapter\n");
    let registry_root = fixture.path("registry");
    write_registry(&registry_root, &runtime_root);
    write_file(
        &registry_root.join("agents.json"),
        r#"{"schema":"louiselm.launch.registry/1","entries":[{"id":"demo","provider":"demo-provider","runtime_id":"demo-runtime","arguments":["--acp","secret-argument"],"environment":{"SUPER_SECRET":"do-not-copy"}}]}"#,
    );
    let registry = Arc::new(Registry::open(&registry_root).expect("registry opens"));
    let request = LaunchRequest {
        schema: REQUEST_SCHEMA.to_owned(),
        protocol_version: PROTOCOL_VERSION,
        request_id: "launch-request-1".to_owned(),
        authorization_id: "authorization-1".to_owned(),
        session_id: "session-1".to_owned(),
        run_id: "run-1".to_owned(),
        agent_id: "demo".to_owned(),
        envelope_id: "denied".to_owned(),
        envelope_revision: 7,
        skill_generation_id: Digest::of(b"generation").to_string(),
        session_input_manifest_id: Digest::of(b"input").to_string(),
    };
    let expected_identity = Identity {
        slot: 3,
        uid: 200_003,
        gid: 300_003,
    };
    let mut authorization = LaunchAuthorization {
        conformance: louiselm_skills::launch_protocol::ConformanceAuthorization::default(),
        schema: LAUNCH_AUTHORIZATION_SCHEMA.to_owned(),
        protocol_version: PROTOCOL_VERSION,
        authorization_id: request.authorization_id.clone(),
        request_id: request.request_id.clone(),
        request_digest: request.digest().to_string(),
        controller_uid: CONTROLLER_UID,
        session_id: request.session_id.clone(),
        run_id: request.run_id.clone(),
        envelope_revision: request.envelope_revision,
        identity_slot: expected_identity.slot,
        assigned_uid: expected_identity.uid,
        assigned_gid: expected_identity.gid,
        expires_at_ms: NOW_MS + 1_000,
        broker_loss_grace_ms: BROKER_LOSS_GRACE_MS,
    };
    edit_authorization(&mut authorization);

    let events = Arc::new(Mutex::new(Vec::new()));
    let broker = Arc::new(FakeBroker::new(
        Arc::clone(&events),
        authorization_available.then_some(authorization),
        append_behavior,
        fixture.path("broker/receipts"),
    ));
    let signer = Arc::new(FakeSigner::new(Arc::clone(&events)));
    let platform = Arc::new(FakePlatform {
        events: Arc::clone(&events),
        expected_identity,
        behavior: platform_behavior,
        state: Mutex::new(PlatformState::default()),
        agent: Arc::new(Mutex::new(AgentState::default())),
        agent_changed: Arc::new(Condvar::new()),
        agent_output: vec![0x00, 0xff, b'o', b'u', b't', b'\n'],
        capability_root: fixture.path("capability"),
    });
    let sessions_root = fixture.path("sessions");
    let supervisor = LaunchSupervisor::new(
        broker.clone(),
        signer.clone(),
        platform.clone(),
        Arc::clone(&registry),
        sessions_root.clone(),
        timeout,
    );
    Setup {
        fixture,
        request,
        broker,
        signer,
        platform,
        registry,
        sessions_root,
        timeout,
        supervisor,
        events,
    }
}

fn begin_launch(
    setup: &Setup,
    controller_uid: u32,
) -> (
    Receiver<Result<LaunchedSession, SupervisorError>>,
    Arc<AtomicUsize>,
) {
    begin_launch_on(
        &setup.supervisor,
        &setup.request,
        &setup.events,
        controller_uid,
    )
}

fn begin_launch_on(
    supervisor: &LaunchSupervisor,
    request: &LaunchRequest,
    events: &Events,
    controller_uid: u32,
) -> (
    Receiver<Result<LaunchedSession, SupervisorError>>,
    Arc<AtomicUsize>,
) {
    let (sender, receiver) = mpsc::sync_channel(2);
    let completion_count = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&completion_count);
    let events = Arc::clone(events);
    supervisor
        .launch(
            request.clone(),
            controller_uid,
            NOW_MS,
            Box::new(move |result| {
                count.fetch_add(1, Ordering::SeqCst);
                record(&events, "complete");
                sender.send(result).expect("test receives completion");
            }),
        )
        .expect("launch registers");
    (receiver, completion_count)
}

#[derive(Clone)]
struct SharedWriter(Arc<Mutex<Vec<u8>>>);

impl Write for SharedWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        lock(&self.0).extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn complete_launch(setup: &Setup) -> LaunchedSession {
    complete_launch_on(setup, &setup.supervisor)
}

fn complete_launch_on(setup: &Setup, supervisor: &LaunchSupervisor) -> LaunchedSession {
    let (receiver, _) = begin_launch_on(supervisor, &setup.request, &setup.events, CONTROLLER_UID);
    setup.broker.wait_for_append(0);
    setup.broker.acknowledge();
    setup.broker.wait_for_append(1);
    setup.broker.acknowledge();
    receiver
        .recv_timeout(CALLBACK_TIMEOUT)
        .expect("launch completes after both launch receipts are acknowledged")
        .expect("launch succeeds")
}

fn lifecycle_request(
    setup: &Setup,
    request_id: &str,
    authorization_id: &str,
    expected_receipt_sequence: u64,
) -> LifecycleRequest {
    LifecycleRequest {
        schema: LIFECYCLE_REQUEST_SCHEMA.to_owned(),
        protocol_version: PROTOCOL_VERSION,
        request_id: request_id.to_owned(),
        session_id: setup.request.session_id.clone(),
        run_id: setup.request.run_id.clone(),
        authorization_id: authorization_id.to_owned(),
        action: LifecycleAction::Park,
        expected_state: SessionState::Running,
        expected_receipt_sequence: Some(expected_receipt_sequence),
        envelope_revision: setup.request.envelope_revision,
    }
}

fn resume_request(setup: &Setup, request_id: &str) -> LifecycleRequest {
    LifecycleRequest {
        schema: LIFECYCLE_REQUEST_SCHEMA.to_owned(),
        protocol_version: PROTOCOL_VERSION,
        request_id: request_id.to_owned(),
        session_id: setup.request.session_id.clone(),
        run_id: setup.request.run_id.clone(),
        authorization_id: format!("{request_id}-authorization"),
        action: LifecycleAction::Resume,
        expected_state: SessionState::Parked,
        expected_receipt_sequence: Some(2),
        envelope_revision: setup.request.envelope_revision,
    }
}

fn interrupt_request(
    setup: &Setup,
    request_id: &str,
    expected_state: SessionState,
    expected_receipt_sequence: u64,
) -> LifecycleRequest {
    LifecycleRequest {
        schema: LIFECYCLE_REQUEST_SCHEMA.to_owned(),
        protocol_version: PROTOCOL_VERSION,
        request_id: request_id.to_owned(),
        session_id: setup.request.session_id.clone(),
        run_id: setup.request.run_id.clone(),
        authorization_id: format!("{request_id}-authorization"),
        action: LifecycleAction::Interrupt,
        expected_state,
        expected_receipt_sequence: Some(expected_receipt_sequence),
        envelope_revision: setup.request.envelope_revision,
    }
}

fn disposal_request(setup: &Setup, request_id: &str) -> LifecycleRequest {
    LifecycleRequest {
        schema: LIFECYCLE_REQUEST_SCHEMA.to_owned(),
        protocol_version: PROTOCOL_VERSION,
        request_id: request_id.to_owned(),
        session_id: setup.request.session_id.clone(),
        run_id: setup.request.run_id.clone(),
        authorization_id: format!("{request_id}-authorization"),
        action: LifecycleAction::Disposal,
        expected_state: SessionState::Running,
        expected_receipt_sequence: Some(1),
        envelope_revision: setup.request.envelope_revision,
    }
}

fn status_request(setup: &Setup, request_id: &str) -> StatusRequest {
    StatusRequest {
        schema: STATUS_REQUEST_SCHEMA.to_owned(),
        protocol_version: PROTOCOL_VERSION,
        request_id: request_id.to_owned(),
        session_id: setup.request.session_id.clone(),
        run_id: setup.request.run_id.clone(),
    }
}

fn request_supervisor_status(setup: &Setup, request_id: &str) -> SupervisorStatus {
    let request = status_request(setup, request_id);
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Status(request.clone()));
    let response = setup
        .broker
        .wait_for_session_response(&request.request_id, 0);
    let ResponseResult::SupervisorStatus { status } = response.result else {
        panic!("authenticated status returns mechanical supervisor status");
    };
    status
}

fn event_count(events: &Events, expected: &str) -> usize {
    lock(events)
        .iter()
        .filter(|event| event.as_str() == expected)
        .count()
}

fn lifecycle_effect_counts(events: &Events) -> [usize; 4] {
    [
        event_count(events, "capability.revoke"),
        event_count(events, "agent.park"),
        event_count(events, "signer.sign"),
        event_count(events, "broker.send_session_receipt"),
    ]
}

fn capture_stdio(
    input: fs::File,
    output: Arc<Mutex<Vec<u8>>>,
) -> (RelayStdio, thread::JoinHandle<()>) {
    let (mut reader, writer) = UnixStream::pair().unwrap();
    let observer = thread::spawn(move || {
        io::copy(&mut reader, &mut SharedWriter(output)).expect("capture controller output");
    });
    (
        RelayStdio::new(BufReader::new(input), fs::File::from(OwnedFd::from(writer))).unwrap(),
        observer,
    )
}

fn begin_session_relay(
    session: LaunchedSession,
) -> (
    UnixStream,
    Receiver<Result<i32, SupervisorError>>,
    thread::JoinHandle<()>,
) {
    let (controller_input, supervisor_input) =
        UnixStream::pair().expect("test relay socket pair opens");
    let controller_output = Arc::new(Mutex::new(Vec::new()));
    let (sender, receiver) = mpsc::sync_channel(1);
    let worker = thread::Builder::new()
        .name("fake-supervised-relay".to_owned())
        .spawn(move || {
            let (stdio, output_worker) = capture_stdio(
                fs::File::from(OwnedFd::from(supervisor_input)),
                controller_output,
            );
            let result = session.relay_stdio(stdio);
            output_worker.join().expect("output observer finishes");
            sender.send(result).expect("test receives relay completion");
        })
        .expect("supervised relay worker starts");
    (controller_input, receiver, worker)
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "Terminal fixture cleanup consumes the relay resources, including the completion receiver, after observing the result."
)]
fn finish_session_relay(
    setup: &Setup,
    controller_input: UnixStream,
    receiver: Receiver<Result<i32, SupervisorError>>,
    worker: thread::JoinHandle<()>,
) {
    setup
        .broker
        .set_session_receipt_send_behavior(SessionReceiptSendBehavior::Complete);
    let receipt_index = setup.broker.session_receipt_count();
    setup
        .platform
        .send_running_event(RunningAgentEvent::ProcessExited(
            ProcessExitClassification::Success,
        ));
    setup.broker.wait_for_session_receipt(receipt_index);
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(receipt_index),
        ));
    drop(controller_input);
    assert_eq!(
        receiver
            .recv_timeout(CALLBACK_TIMEOUT)
            .expect("relay completes after controller input closes")
            .expect("relay and cleanup succeed"),
        0,
    );
    worker.join().expect("supervised relay worker finishes");
}

fn finish_session_relay_after_reconciling_backlog(
    setup: &Setup,
    controller_input: UnixStream,
    receiver: Receiver<Result<i32, SupervisorError>>,
    worker: thread::JoinHandle<()>,
    expected_pending_receipts: u32,
) {
    setup
        .broker
        .set_session_receipt_send_behavior(SessionReceiptSendBehavior::Complete);
    let before_exit = request_supervisor_status(setup, "cleanup-before-backlog-repair");
    assert_eq!(
        before_exit.pending_receipt_count, expected_pending_receipts,
        "cleanup must preserve the test's deferred audit"
    );
    let mut previous_head = before_exit
        .broker_head
        .expect("a launched Session has a durable broker head");
    let replay_start = setup.broker.session_receipt_count();

    setup
        .platform
        .send_running_event(RunningAgentEvent::ProcessExited(
            ProcessExitClassification::Success,
        ));
    let terminal = request_supervisor_status(setup, "cleanup-terminal-before-backlog-repair");
    assert_eq!(terminal.state, SessionState::Terminal);
    assert_eq!(terminal.channel_state, ChannelState::Closed);
    assert_eq!(
        terminal.process_exit,
        Some(ProcessExitClassification::Success)
    );
    assert_eq!(
        terminal.pending_receipt_count,
        expected_pending_receipts + 1,
        "terminal Disposal queues behind every earlier audit intent"
    );

    setup.broker.wait_for_session_request();
    setup.broker.disconnect_session();
    setup.broker.wait_for_reconnect(0);
    let mut reconnect = setup.broker.reconnect(0);
    reconnect.sequence = previous_head.sequence;
    reconnect.receipt_digest.clone_from(&previous_head.digest);
    setup.broker.complete_reconnect(reconnect);

    let repair_count = usize::try_from(expected_pending_receipts + 1)
        .expect("bounded protocol receipt count fits usize");
    for offset in 0..repair_count {
        let receipt_index = replay_start + offset;
        setup.broker.wait_for_session_receipt(receipt_index);
        let bytes = setup.broker.session_receipt_bytes(receipt_index);
        let receipt =
            SignedReceipt::parse_canonical(&bytes).expect("repaired receipt is canonical");
        assert_eq!(receipt.payload.sequence, previous_head.sequence + 1);
        assert_eq!(
            receipt.payload.previous_receipt_digest.as_deref(),
            Some(previous_head.digest.as_str()),
        );
        if offset + 1 == repair_count {
            assert_eq!(
                receipt.payload.outcome,
                ReceiptOutcome::Disposal {
                    authority: ReceiptAuthority::ProcessExited {
                        classification: ProcessExitClassification::Success,
                    },
                },
                "terminal Disposal cannot overtake an earlier deferred audit",
            );
            assert_eq!(receipt.payload.resulting_state, SessionState::Terminal);
        } else {
            assert!(
                !matches!(receipt.payload.outcome, ReceiptOutcome::Disposal { .. }),
                "only the final repaired receipt may record terminal Disposal",
            );
        }
        previous_head = ReceiptHead {
            sequence: receipt.payload.sequence,
            digest: Digest::of(&bytes).to_string(),
        };
        setup.broker.wait_for_session_request();
        setup
            .broker
            .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
                setup.broker.session_receipt_acknowledgement(receipt_index),
            ));
    }

    finish_terminal_session_relay(controller_input, receiver, worker);
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "Terminal fixture cleanup consumes the relay resources, including the completion receiver, after observing the result."
)]
fn finish_terminal_session_relay(
    controller_input: UnixStream,
    receiver: Receiver<Result<i32, SupervisorError>>,
    worker: thread::JoinHandle<()>,
) {
    drop(controller_input);
    receiver
        .recv_timeout(CALLBACK_TIMEOUT)
        .expect("terminal relay completes")
        .expect("terminal supervisor cleanup succeeds");
    worker.join().expect("terminal relay worker finishes");
}

fn park_launched_session(
    setup: &Setup,
    session: LaunchedSession,
) -> (
    UnixStream,
    Receiver<Result<i32, SupervisorError>>,
    thread::JoinHandle<()>,
    SignedReceipt,
) {
    let (controller_input, relay_receiver, relay_worker) = begin_session_relay(session);
    setup.broker.wait_for_session_request();
    let park = lifecycle_request(setup, "park-before-resume", "park-authorization", 1);
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(park.clone()));
    setup.broker.wait_for_session_receipt(0);
    let park_receipt = SignedReceipt::parse_canonical(&setup.broker.session_receipt_bytes(0))
        .expect("warm Park receipt is canonical");
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(0),
        ));
    let response = setup.broker.wait_for_session_response(&park.request_id, 0);
    let ResponseResult::Receipt { receipt } = response.result else {
        panic!("warm Park completes before Resume testing");
    };
    assert_eq!(receipt, park_receipt);
    assert_eq!(
        event_count(&setup.events, "signer.complete"),
        0,
        "Park retains the signing lifetime"
    );
    (controller_input, relay_receiver, relay_worker, park_receipt)
}

fn controller_loss_acknowledgement(
    settlement: &ControllerLossSettlement,
    disposition: ControllerLossDisposition,
) -> ControllerLossAcknowledgement {
    ControllerLossAcknowledgement {
        schema: CONTROLLER_LOSS_ACK_SCHEMA.to_owned(),
        protocol_version: PROTOCOL_VERSION,
        request_id: settlement.request_id.clone(),
        session_id: settlement.session_id.clone(),
        run_id: settlement.run_id.clone(),
        envelope_revision: settlement.envelope_revision,
        parked_head: settlement.parked_head.clone(),
        disposition,
    }
}

fn begin_controller_loss_settlement(
    setup: &Setup,
    session: LaunchedSession,
) -> (
    UnixStream,
    Receiver<Result<i32, SupervisorError>>,
    thread::JoinHandle<()>,
    SignedReceipt,
    ControllerLossSettlement,
) {
    let (controller_input, relay_receiver, relay_worker) = begin_session_relay(session);
    setup.broker.wait_for_session_request();
    setup
        .platform
        .send_running_event(RunningAgentEvent::ControllerEof);
    setup.broker.wait_for_session_receipt(0);
    let park_bytes = setup.broker.session_receipt_bytes(0);
    let park_receipt = SignedReceipt::parse_canonical(&park_bytes)
        .expect("controller-loss Park receipt is canonical");
    assert_eq!(
        park_receipt.payload.outcome,
        ReceiptOutcome::Park {
            authority: ReceiptAuthority::Cause {
                cause: ReceiptCause::ControllerLost,
            },
        }
    );
    assert_eq!(park_receipt.payload.resulting_state, SessionState::Parked);

    let events = event_snapshot(&setup.events);
    let revoke = events
        .iter()
        .rposition(|event| event == "capability.revoke")
        .expect("controller loss revokes capability channels");
    let park = events
        .iter()
        .rposition(|event| event == "agent.park")
        .expect("controller loss freezes the process tree");
    let sign = events
        .iter()
        .rposition(|event| event == "signer.sign")
        .expect("controller loss signs its Park truth");
    assert!(revoke < park && park < sign);

    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(0),
        ));
    setup.broker.wait_for_controller_loss_settlement(0);
    let settlement = setup.broker.controller_loss_settlement(0);
    let park_head = ReceiptHead {
        sequence: park_receipt.payload.sequence,
        digest: Digest::of(&park_bytes).to_string(),
    };
    assert_eq!(settlement.schema, CONTROLLER_LOSS_SETTLEMENT_SCHEMA);
    assert_eq!(settlement.protocol_version, PROTOCOL_VERSION);
    assert_eq!(settlement.session_id, setup.request.session_id);
    assert_eq!(settlement.run_id, setup.request.run_id);
    assert_eq!(
        settlement.envelope_revision,
        setup.request.envelope_revision
    );
    assert_eq!(settlement.parked_head, park_head);
    (
        controller_input,
        relay_receiver,
        relay_worker,
        park_receipt,
        settlement,
    )
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "Terminal fixture cleanup consumes the relay resources, including the completion receiver, after observing the result."
)]
fn dispose_retained_controller_loss_session(
    setup: &Setup,
    request_id: &str,
    expected_receipt_sequence: u64,
    controller_input: UnixStream,
    relay_receiver: Receiver<Result<i32, SupervisorError>>,
    relay_worker: thread::JoinHandle<()>,
) {
    let disposal = LifecycleRequest {
        schema: LIFECYCLE_REQUEST_SCHEMA.to_owned(),
        protocol_version: PROTOCOL_VERSION,
        request_id: request_id.to_owned(),
        session_id: setup.request.session_id.clone(),
        run_id: setup.request.run_id.clone(),
        authorization_id: format!("{request_id}-authorization"),
        action: LifecycleAction::Disposal,
        expected_state: SessionState::Parked,
        expected_receipt_sequence: Some(expected_receipt_sequence),
        envelope_revision: setup.request.envelope_revision,
    };
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(disposal.clone()));
    setup.broker.wait_for_session_receipt(1);
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(1),
        ));
    let response = setup
        .broker
        .wait_for_session_response(&disposal.request_id, 0);
    assert!(matches!(response.result, ResponseResult::Receipt { .. }));
    drop(controller_input);
    assert!(
        relay_receiver
            .recv_timeout(CALLBACK_TIMEOUT)
            .expect("authorized cleanup completes")
            .is_ok()
    );
    relay_worker
        .join()
        .expect("supervised relay worker finishes");
}

#[test]
fn capability_binding_waits_for_restricted_agent_startup() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let (receiver, _) = begin_launch(&setup, CONTROLLER_UID);
    setup.broker.wait_for_append(0);
    let bound_before_start = lock(&setup.platform.gate_state()).binding.is_some();
    setup.broker.acknowledge();
    setup.broker.wait_for_append(1);
    let events = event_snapshot(&setup.events);
    setup.broker.acknowledge();
    let session = receiver
        .recv_timeout(CALLBACK_TIMEOUT)
        .expect("launch completes")
        .expect("fake launch succeeds");
    drop(session);

    assert!(
        !bound_before_start,
        "a blocked reaper is not Agent identity"
    );
    let started = events
        .iter()
        .position(|event| event == "agent.start")
        .unwrap();
    let bound = events
        .iter()
        .position(|event| event == "capability.bind")
        .unwrap();
    let enabled = events
        .iter()
        .position(|event| event == "capability.enable")
        .unwrap();
    assert!(started < bound && bound < enabled);
}

#[test]
fn known_unsupported_integration_refuses_before_authorization_or_spawn() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior {
            integration_unsupported: true,
            ..PlatformBehavior::default()
        },
        SUPERVISOR_TIMEOUT,
    );
    let (receiver, count) = begin_launch(&setup, CONTROLLER_UID);
    assert_eq!(
        receiver.recv_timeout(CALLBACK_TIMEOUT).unwrap().err(),
        Some(SupervisorError::ToolIsolationUnproven)
    );
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert!(setup.broker.receipts().is_empty());
    let events = event_snapshot(&setup.events);
    assert!(!events.iter().any(|event| matches!(
        event.as_str(),
        "identity.acquire" | "agent.start" | "capability.enable"
    )));
}

#[test]
fn missing_tool_isolation_disposes_restricted_startup_without_enabling_effects() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior {
            tool_isolation_unproven: true,
            ..PlatformBehavior::default()
        },
        SUPERVISOR_TIMEOUT,
    );
    let (receiver, count) = begin_launch(&setup, CONTROLLER_UID);
    setup.broker.wait_for_append(0);
    setup.broker.acknowledge();
    assert_eq!(
        receiver.recv_timeout(CALLBACK_TIMEOUT).unwrap().err(),
        Some(SupervisorError::ToolIsolationUnproven)
    );
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert_eq!(setup.broker.receipts().len(), 1);
    let events = event_snapshot(&setup.events);
    assert!(events.iter().any(|event| event == "agent.start"));
    assert!(
        !events
            .iter()
            .any(|event| event == "capability.bind" || event == "capability.enable")
    );
    assert!(
        events
            .windows(3)
            .any(|events| events == ["capability.close", "agent.dispose", "identity.release"])
    );
}

#[test]
fn raw_broker_commands_never_execute_without_an_authenticated_agent_request() {
    use louiselm_skills::launch_protocol::{TOOL_EXECUTION_SCHEMA, ToolExecutionRequest};
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let session = complete_launch(&setup);
    let (controller, finished, worker) = begin_session_relay(session);
    let request = ToolExecutionRequest {
        schema: TOOL_EXECUTION_SCHEMA.to_owned(),
        protocol_version: 1,
        request_id: "tool-1".to_owned(),
        session_id: setup.request.session_id.clone(),
        run_id: setup.request.run_id.clone(),
        envelope_revision: setup.request.envelope_revision,
        sequence: 1,
        command: "printf tool".to_owned(),
        timeout_ms: 1000,
    };
    for (id, code) in [
        ("wrong-session", ErrorCode::InvalidRequest),
        ("wrong-revision", ErrorCode::InvalidRequest),
    ] {
        let mut invalid = request.clone();
        invalid.request_id = id.to_owned();
        if id == "wrong-session" {
            invalid.session_id = "another".to_owned();
        } else {
            invalid.envelope_revision += 1;
        }
        setup.broker.wait_for_session_request();
        setup
            .broker
            .deliver_session_request(ProtocolMessage::ToolExecution(invalid));
        assert!(
            matches!(setup.broker.wait_for_session_response(id, 0).result, ResponseResult::Error { error } if error.code == code)
        );
    }
    assert_eq!(event_count(&setup.events, "agent.tool"), 0);
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ToolExecution(request.clone()));
    assert!(
        matches!(setup.broker.wait_for_session_response("tool-1", 0).result, ResponseResult::Error {error} if error.code==ErrorCode::InvalidRequest)
    );
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ToolExecution(request));
    assert!(
        matches!(setup.broker.wait_for_session_response("tool-1", 1).result, ResponseResult::Error { error } if error.code == ErrorCode::InvalidRequest)
    );
    assert_eq!(event_count(&setup.events, "agent.tool"), 0);
    finish_session_relay(&setup, controller, finished, worker);
}

#[test]
fn unsolicited_broker_authorization_never_reaches_tool_mechanics() {
    use louiselm_skills::launch_protocol::{
        COMMAND_SCHEMA, CommandMessage, CommandOperation, CommandPrincipal,
    };
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let session = complete_launch(&setup);
    let (controller, finished, worker) = begin_session_relay(session);
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Command(CommandMessage {
            schema: COMMAND_SCHEMA.to_owned(),
            protocol_version: 1,
            request_id: "late-tool".to_owned(),
            session_id: setup.request.session_id.clone(),
            run_id: setup.request.run_id.clone(),
            envelope_revision: setup.request.envelope_revision,
            operation: CommandOperation::Authorize {
                principal: CommandPrincipal {
                    channel_id: "agent-capability".to_owned(),
                    pid: 42_425,
                    uid: 200_003,
                    gid: 300_003,
                },
                principal_sequence: 1,
                dispatch_sequence: 1,
                command_digest: Digest::of(b"sleep 1").to_string(),
                timeout_ms: 1000,
                valid_for_ms: 1000,
            },
        }));
    setup.broker.wait_for_session_request();
    assert_eq!(event_count(&setup.events, "agent.tool"), 0);
    finish_session_relay(&setup, controller, finished, worker);
    assert_eq!(setup.broker.session_response_count_for("late-tool"), 0);
}

#[test]
fn reaper_foreign_process_and_mismatched_agent_ids_never_bind_authority() {
    for (pid, uid, gid) in [
        (42_424, 200_003, 300_003),
        (0, 200_003, 300_003),
        (42_426, 200_003, 300_003),
        (42_425, 200_004, 300_003),
        (42_425, 200_003, 300_004),
    ] {
        let setup = setup(
            true,
            |_| {},
            AppendBehavior::Hold,
            PlatformBehavior {
                agent_credentials: Some(louiselm_skills::launch_transport::KernelCredentials {
                    pid,
                    uid,
                    gid,
                }),
                ..PlatformBehavior::default()
            },
            SUPERVISOR_TIMEOUT,
        );
        let (receiver, count) = begin_launch(&setup, CONTROLLER_UID);
        setup.broker.wait_for_append(0);
        setup.broker.acknowledge();
        assert_eq!(
            receiver.recv_timeout(CALLBACK_TIMEOUT).unwrap().err(),
            Some(SupervisorError::AgentIdentityRejected)
        );
        assert_eq!(count.load(Ordering::SeqCst), 1);
        let events = event_snapshot(&setup.events);
        assert!(
            !events
                .iter()
                .any(|event| event == "capability.bind" || event == "capability.enable")
        );
        assert_eq!(event_count(&setup.events, "agent.dispose"), 1);
    }
}

#[test]
fn conformance_refusal_cleans_prepared_tree_without_starting_or_signing() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior {
            conformance_refused: true,
            ..PlatformBehavior::default()
        },
        SUPERVISOR_TIMEOUT,
    );
    let (receiver, _) = begin_launch(&setup, CONTROLLER_UID);
    let result = receiver.recv_timeout(CALLBACK_TIMEOUT).unwrap();
    assert!(
        matches!(
            result,
            Err(SupervisorError::ConformanceRefused(
                louiselm_skills::conformance::admission::Condition::Missing,
            ))
        ),
        "conformance refusal must reach the launch consumer"
    );
    assert!(setup.signer.payloads().is_empty());
    assert!(!lock(&setup.platform.agent).started);
    let events = event_snapshot(&setup.events);
    assert!(events.iter().any(|event| event == "agent.dispose"));
    assert!(events.iter().any(|event| event == "identity.release"));
}

#[test]
fn conformance_report_is_bound_and_supplied_before_starting_the_agent() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    // This seam test checks byte ownership and ACK ordering, not certification;
    // the real sender/store integration uses a complete installed report.
    let bytes = b"trusted platform test observation bytes".to_vec();
    lock(&setup.platform.state).conformance_report = Some(bytes.clone());
    let (receiver, _) = begin_launch(&setup, CONTROLLER_UID);
    setup.broker.wait_for_append(0);
    let launch = SignedReceipt::parse_canonical(&setup.broker.receipt_bytes(0)).unwrap();
    let ReceiptOutcome::Launch { evidence, .. } = launch.payload.outcome else {
        panic!("launch receipt");
    };
    assert_eq!(
        evidence.conformance,
        ConformanceEvidence::Certified {
            report_digest: Digest::of(&bytes).to_string(),
        }
    );
    assert_eq!(lock(&setup.broker.state).reports, [Some(bytes)]);
    assert!(!lock(&setup.platform.agent).started);
    setup.broker.acknowledge();
    setup.broker.wait_for_append(1);
    assert!(lock(&setup.broker.state).reports[1].is_none());
    // Losing the final ACK cleans up through the existing bounded path.
    assert!(receiver.recv_timeout(CALLBACK_TIMEOUT).unwrap().is_err());
    assert_eq!(event_count(&setup.events, "agent.dispose"), 1);
    assert_eq!(event_count(&setup.events, "identity.release"), 1);
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "One launch acks starting then starts and acks linked running before success scenario keeps its causal steps and assertions together."
)]
fn launch_acks_starting_then_starts_and_acks_linked_running_before_success() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let (receiver, completion_count) = begin_launch(&setup, CONTROLLER_UID);
    setup.broker.wait_for_append(0);

    assert_eq!(
        event_snapshot(&setup.events),
        [
            "broker.consume",
            "identity.acquire",
            "capability.create",
            "platform.prepare",
            "signer.sign",
            "broker.append",
            "broker.fsync",
        ]
    );
    assert!(
        receiver.try_recv().is_err(),
        "launch cannot complete before ACK"
    );
    assert!(!lock(&setup.platform.agent).started);
    assert!(!lock(&setup.platform.gate_state()).enabled);

    let starting_bytes = setup.broker.receipt_bytes(0);
    let starting =
        SignedReceipt::parse_canonical(&starting_bytes).expect("Starting receipt is canonical");
    assert_eq!(
        setup.signer.payloads(),
        [starting.payload.canonical_bytes()],
    );
    assert_eq!(starting.payload.sequence, 0);
    assert_eq!(starting.payload.previous_receipt_digest, None);
    assert_eq!(starting.payload.request_id, setup.request.request_id);
    assert_eq!(starting.payload.envelope_revision, 7);
    assert_eq!(starting.payload.resulting_state, SessionState::Starting);
    let ReceiptOutcome::Launch {
        authorization,
        evidence,
    } = &starting.payload.outcome
    else {
        panic!("sequence zero must be a launch receipt");
    };
    assert_eq!(
        authorization.authorization_id,
        setup.request.authorization_id
    );
    assert_eq!(
        authorization.request_digest,
        setup.request.digest().to_string()
    );
    // The louiselm-d6fv.9 gate is not in force, so a real launch must claim no
    // host conformance. Upgrading this without wiring admission would assert a
    // property nothing checked; presentation reports it unverified meanwhile.
    assert_eq!(
        evidence.conformance,
        ConformanceEvidence::Unevaluated,
        "a pre-cutover launch records no conformance claim"
    );
    assert_eq!(
        evidence.launch_request_digest,
        setup.request.digest().to_string()
    );
    assert_eq!(evidence.capability_channel_ids, ["acp", "broker"]);
    assert_eq!(evidence.isolation_contract, CONTRACT_VERSION);
    for sensitive in ["do-not-copy", "secret-argument", "tool-payload"] {
        assert!(
            !String::from_utf8_lossy(&starting_bytes).contains(sensitive),
            "receipt copied sensitive value {sensitive}",
        );
    }

    setup.broker.acknowledge();
    setup.broker.wait_for_append(1);
    assert!(
        receiver.try_recv().is_err(),
        "launch cannot complete before the Running receipt ACK"
    );
    assert!(lock(&setup.platform.agent).started);
    assert!(lock(&setup.platform.gate_state()).enabled);

    let running_bytes = setup.broker.receipt_bytes(1);
    let running =
        SignedReceipt::parse_canonical(&running_bytes).expect("Running receipt is canonical");
    let starting_digest = starting.digest();
    let starting_digest_text = starting_digest.to_string();
    assert_eq!(running.payload.sequence, 1);
    assert_eq!(
        running.payload.previous_receipt_digest.as_deref(),
        Some(starting_digest_text.as_str()),
    );
    assert_eq!(
        running.payload.request_id,
        format!("start-{}", starting_digest.hex()),
    );
    assert_eq!(running.payload.session_id, starting.payload.session_id);
    assert_eq!(running.payload.run_id, starting.payload.run_id);
    assert_eq!(
        running.payload.envelope_revision,
        starting.payload.envelope_revision
    );
    assert_eq!(running.payload.release_id, starting.payload.release_id);
    assert_eq!(
        running.payload.signing_key_id,
        starting.payload.signing_key_id
    );
    assert_eq!(running.payload.resulting_state, SessionState::Running);
    assert_eq!(
        running.payload.outcome,
        ReceiptOutcome::Start {
            evidence: louiselm_skills::launch_receipt::StartEvidence {
                agent_pid: 42_425,
                assigned_uid: setup.platform.expected_identity.uid,
                assigned_gid: setup.platform.expected_identity.gid,
                tool_isolation_digest: Digest::of(b"fixture-tool-isolation").to_string(),
            },
            authority: ReceiptAuthority::Cause {
                cause: ReceiptCause::LaunchAcknowledged,
            },
        }
    );
    assert_eq!(
        setup.signer.payloads(),
        [
            starting.payload.canonical_bytes(),
            running.payload.canonical_bytes(),
        ],
    );

    setup.broker.acknowledge();
    let session = receiver
        .recv_timeout(CALLBACK_TIMEOUT)
        .expect("launch completes after the Running ACK")
        .expect("launch succeeds");
    assert_eq!(session.receipt().canonical_bytes(), running_bytes);
    assert_eq!(
        session.capability_binding(),
        &CapabilityBinding {
            session_id: "session-1".to_owned(),
            run_id: setup.request.run_id.clone(),
            channel_id: "broker".to_owned(),
            envelope_revision: 7,
            identity_slot: 3,
            assigned_uid: 200_003,
            assigned_gid: 300_003,
            agent_pid: 42_425,
        },
    );
    assert_eq!(
        lock(&setup.platform.gate_state()).binding,
        Some(session.capability_binding().clone()),
    );
    let plan = setup.platform.plan();
    assert_eq!(
        plan.identity,
        IdentityPlan::HostIdentity {
            uid: 200_003,
            gid: 300_003,
        }
    );
    assert_eq!(plan.arguments, ["--acp", "secret-argument"]);
    assert_eq!(
        plan.channels.iter().map(Channel::id).collect::<Vec<_>>(),
        ["acp", "broker"]
    );
    assert_eq!(
        event_snapshot(&setup.events),
        [
            "broker.consume",
            "identity.acquire",
            "capability.create",
            "platform.prepare",
            "signer.sign",
            "broker.append",
            "broker.fsync",
            "broker.ack",
            "agent.start",
            "platform.verify_tool_isolation",
            "capability.bind",
            "capability.enable",
            "signer.sign",
            "broker.append",
            "broker.fsync",
            "broker.ack",
            "agent.relay",
            "broker.receive_session_request",
            "complete",
        ]
    );

    let agent_input = vec![
        0xff, 0x00, b't', b'o', b'o', b'l', b'-', b'p', b'a', b'y', b'l', b'o', b'a', b'd', b'\n',
    ];
    let controller_output = Arc::new(Mutex::new(Vec::new()));
    let worker_output = Arc::clone(&controller_output);
    let worker_input = agent_input.clone();
    let (relay_sender, relay_receiver) = mpsc::sync_channel(1);
    let relay_worker = thread::Builder::new()
        .name("launch-relay-observation".to_owned())
        .spawn(move || {
            let (mut writer, reader) = UnixStream::pair().unwrap();
            writer.write_all(&worker_input).unwrap();
            drop(writer);
            let (stdio, output_worker) =
                capture_stdio(fs::File::from(OwnedFd::from(reader)), worker_output);
            let result = session.relay_stdio(stdio);
            output_worker.join().expect("output observer finishes");
            relay_sender
                .send(result)
                .expect("test receives relay completion");
        })
        .expect("observed relay worker starts");

    setup.broker.wait_for_session_receipt(0);
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(0),
        ));
    setup.broker.wait_for_controller_loss_settlement(0);
    let settlement = setup.broker.controller_loss_settlement(0);
    setup
        .broker
        .complete_controller_loss_settlement(Ok(controller_loss_acknowledgement(
            &settlement,
            ControllerLossDisposition::NoRecovery {
                attention_projection_id: "attention-launch-relay-eof".to_owned(),
            },
        )));
    setup.broker.wait_for_session_receipt(1);
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(1),
        ));
    assert_eq!(
        relay_receiver
            .recv_timeout(CALLBACK_TIMEOUT)
            .expect("relay completes after durable controller-loss settlement")
            .expect("opaque relay and cleanup succeed"),
        0,
    );
    relay_worker.join().expect("observed relay worker finishes");
    assert_eq!(lock(&setup.platform.agent).relayed_input, agent_input);
    assert_eq!(
        *lock(&controller_output),
        vec![0x00, 0xff, b'o', b'u', b't', b'\n']
    );
    assert_eq!(completion_count.load(Ordering::SeqCst), 1);
    let events = event_snapshot(&setup.events);
    assert!(
        events
            .windows(3)
            .any(|events| { events == ["capability.close", "agent.dispose", "identity.release"] })
    );
    assert!(setup.broker.is_closed());
}

#[test]
fn revoked_key_freezes_live_session_without_signing_a_containment_receipt() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let session = complete_launch(&setup);
    setup.signer.authority_valid.store(false, Ordering::SeqCst);
    let observations = lock(&setup.signer.containments);
    let (observations, timeout) = setup
        .signer
        .changed
        .wait_timeout_while(observations, CALLBACK_TIMEOUT, |observations| {
            observations.is_empty()
        })
        .unwrap();
    assert!(
        !timeout.timed_out(),
        "live authority withdrawal was never contained"
    );
    assert_eq!(
        *observations,
        vec![louiselm_skills::launcher_install::KeyContainment::Frozen]
    );
    drop(observations);
    assert!(lock(&setup.platform.agent).parked);
    assert!(
        !lock(&setup.platform.agent).disposed,
        "freeze retains the Session"
    );
    assert_eq!(
        setup.signer.payloads().len(),
        2,
        "compromised key must not sign containment"
    );
    let events = lock(&setup.events).clone();
    assert!(
        events
            .iter()
            .position(|e| e == "capability.revoke")
            .unwrap()
            < events.iter().position(|e| e == "agent.park").unwrap()
    );
    assert!(session.dispose().is_err());
}

#[test]
fn revoked_key_reports_failed_narrowing_and_cannot_leave_a_runnable_session() {
    for behavior in [
        PlatformBehavior {
            revoke_fails: true,
            ..PlatformBehavior::default()
        },
        PlatformBehavior {
            park_fails: true,
            ..PlatformBehavior::default()
        },
    ] {
        let setup = setup(
            true,
            |_| {},
            AppendBehavior::Hold,
            behavior,
            SUPERVISOR_TIMEOUT,
        );
        let session = complete_launch(&setup);
        setup.signer.authority_valid.store(false, Ordering::SeqCst);
        assert_eq!(
            setup.signer.wait_for_containment(),
            louiselm_skills::launcher_install::KeyContainment::Failed
        );
        assert!(session.dispose().is_err());
        assert!(lock(&setup.platform.agent).disposed);
        assert_eq!(setup.signer.payloads().len(), 2);
    }
}

#[test]
fn revoked_key_during_start_ack_cannot_report_a_successful_launch() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let (receiver, _) = begin_launch(&setup, CONTROLLER_UID);
    setup.broker.wait_for_append(0);
    setup.broker.acknowledge();
    setup.broker.wait_for_append(1);
    setup.signer.authority_valid.store(false, Ordering::SeqCst);
    setup.broker.acknowledge();
    assert!(matches!(
        receiver.recv_timeout(CALLBACK_TIMEOUT).unwrap(),
        Err(SupervisorError::SigningUnavailable)
    ));
    assert!(lock(&setup.platform.agent).disposed);
}

#[test]
fn revoked_key_cannot_complete_pending_resume_after_controller_loss() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let session = complete_launch(&setup);
    let (input, receiver, worker, _) = park_launched_session(&setup, session);
    setup.signer.hold_on_call(setup.signer.payloads().len());
    let resume = resume_request(&setup, "revoked-resume");
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(resume));
    setup.signer.wait_for_held_call();
    assert_eq!(event_count(&setup.events, "agent.resume"), 1);
    setup.signer.authority_valid.store(false, Ordering::SeqCst);
    assert_eq!(
        setup.signer.wait_for_containment(),
        louiselm_skills::launcher_install::KeyContainment::Frozen
    );
    setup.signer.release_held_call();
    input.shutdown(Shutdown::Write).unwrap();
    assert!(receiver.recv_timeout(CALLBACK_TIMEOUT).unwrap().is_err());
    worker.join().unwrap();
    assert_eq!(
        setup.broker.session_receipt_count(),
        1,
        "only the pre-compromise Park was stored"
    );
    assert_eq!(
        event_count(&setup.events, "capability.enable"),
        1,
        "late Resume cannot re-enable"
    );
}

#[test]
fn revoked_key_ignores_a_late_lifecycle_signature_and_unaffected_session_continues() {
    let affected = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let healthy = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let session = complete_launch(&affected);
    let healthy_session = complete_launch(&healthy);
    // A late valid signature cannot become trusted after withdrawal.
    affected.signer.hold_on_call(2);
    affected.broker.wait_for_session_request();
    let park = lifecycle_request(&affected, "park-before-compromise", "park-authority", 1);
    affected
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(park));
    affected.signer.wait_for_held_call();
    affected
        .signer
        .authority_valid
        .store(false, Ordering::SeqCst);
    assert_eq!(
        affected.signer.wait_for_containment(),
        louiselm_skills::launcher_install::KeyContainment::Frozen
    );
    affected.signer.release_held_call();
    assert!(session.dispose().is_err());
    assert_eq!(affected.broker.session_receipt_count(), 0);
    assert!(!lock(&healthy.platform.agent).parked);
    // The unaffected Session still uses its authenticated lifecycle channel.
    let (input, receiver, worker, _) = park_launched_session(&healthy, healthy_session);
    assert!(lock(&healthy.platform.agent).parked);
    finish_session_relay(&healthy, input, receiver, worker);
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "One authenticated park is serialized and status keeps actual and durable heads separate scenario keeps its causal steps and assertions together."
)]
fn authenticated_park_is_serialized_and_status_keeps_actual_and_durable_heads_separate() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let session = complete_launch(&setup);
    let running_bytes = session.receipt().canonical_bytes();
    let running_head = ReceiptHead {
        sequence: 1,
        digest: Digest::of(&running_bytes).to_string(),
    };
    let (mut controller_input, relay_receiver, relay_worker) = begin_session_relay(session);
    setup.broker.wait_for_session_request();

    let park = lifecycle_request(&setup, "park-request-1", "park-authorization-1", 1);
    controller_input
        .write_all(&park.canonical_bytes())
        .expect("lifecycle-shaped ACP bytes reach only Agent stdin");
    controller_input.flush().expect("ACP bytes flush");
    assert_eq!(
        event_count(&setup.events, "agent.park"),
        0,
        "Agent/controller bytes have no lifecycle authority",
    );
    assert_eq!(setup.broker.session_receipt_count(), 0);

    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(park.clone()));
    setup.broker.wait_for_session_receipt(0);

    let park_bytes = setup.broker.session_receipt_bytes(0);
    let park_receipt =
        SignedReceipt::parse_canonical(&park_bytes).expect("Park receipt is canonical");
    let park_head = ReceiptHead {
        sequence: 2,
        digest: Digest::of(&park_bytes).to_string(),
    };
    assert_eq!(park_receipt.payload.sequence, 2);
    assert_eq!(
        park_receipt.payload.previous_receipt_digest.as_deref(),
        Some(running_head.digest.as_str()),
    );
    assert_eq!(park_receipt.payload.request_id, park.request_id);
    assert_eq!(park_receipt.payload.resulting_state, SessionState::Parked);
    let ReceiptOutcome::Park {
        authority: ReceiptAuthority::Authorized(authorization),
    } = &park_receipt.payload.outcome
    else {
        panic!("authenticated Park must carry its broker authorization");
    };
    assert_eq!(authorization.authorization_id, park.authorization_id);
    assert_eq!(authorization.request_id, park.request_id);
    assert_eq!(authorization.request_digest, park.digest().to_string());

    let events = event_snapshot(&setup.events);
    let revoke = events
        .iter()
        .position(|event| event == "capability.revoke")
        .expect("Park revokes capability access");
    let freeze = events
        .iter()
        .position(|event| event == "agent.park")
        .expect("Park freezes the whole Agent tree");
    let sign = events
        .iter()
        .rposition(|event| event == "signer.sign")
        .expect("Park signs a receipt");
    let send = events
        .iter()
        .position(|event| event == "broker.send_session_receipt")
        .expect("Park sends the signed receipt");
    assert!(revoke < freeze && freeze < sign && sign < send);
    assert!(lock(&setup.platform.gate_state()).revoked);
    assert!(!lock(&setup.platform.gate_state()).enabled);
    assert!(lock(&setup.platform.agent).parked);
    assert!(!lock(&setup.platform.agent).disposed);
    for release in [
        "identity.release",
        "identity.poison",
        "identity.dropped_without_release",
    ] {
        assert_eq!(
            event_count(&setup.events, release),
            0,
            "warm Park retains its identity lease"
        );
    }

    setup.broker.wait_for_session_request();
    let stale = lifecycle_request(&setup, "stale-park", "stale-authorization", 0);
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(stale.clone()));
    let stale_response = setup.broker.wait_for_session_response(&stale.request_id, 0);
    assert_eq!(stale_response.schema, RESPONSE_SCHEMA);
    let ResponseResult::Error { error } = &stale_response.result else {
        panic!("a stale concurrent request must be rejected");
    };
    assert_eq!(error.code, ErrorCode::OperationPending);

    let effects_before_status = lifecycle_effect_counts(&setup.events);
    setup.broker.wait_for_session_request();
    let pending_status = status_request(&setup, "status-while-park-pending");
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Status(pending_status.clone()));
    let status_response = setup
        .broker
        .wait_for_session_response(&pending_status.request_id, 0);
    let ResponseResult::SupervisorStatus { status } = &status_response.result else {
        panic!("authenticated status returns mechanical supervisor status");
    };
    assert_eq!(status.state, SessionState::Parked);
    assert_eq!(status.broker_connection, BrokerConnection::Connected);
    assert_eq!(status.channel_state, ChannelState::Revoked);
    assert_eq!(status.launcher_head.as_ref(), Some(&park_head));
    assert_eq!(status.broker_head.as_ref(), Some(&running_head));
    assert_eq!(status.pending_receipt_count, 1);
    let pending = status
        .pending_operation
        .as_ref()
        .expect("Park remains pending until its durable ACK");
    assert_eq!(pending.request_id, park.request_id);
    assert_eq!(pending.action, PendingAction::Park);
    assert_eq!(pending.phase, PendingPhase::AwaitingDurableAck);
    assert_eq!(
        lifecycle_effect_counts(&setup.events),
        effects_before_status,
        "status reads do not repeat or widen mechanics"
    );

    setup.broker.wait_for_session_request();
    let park_ack = setup.broker.session_receipt_acknowledgement(0);
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(park_ack));
    let completed = setup.broker.wait_for_session_response(&park.request_id, 0);
    let ResponseResult::Receipt { receipt } = &completed.result else {
        panic!("durable Park completion returns its exact receipt");
    };
    assert_eq!(receipt.canonical_bytes(), park_bytes);

    let effects_before_retry = lifecycle_effect_counts(&setup.events);
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(park.clone()));
    let replay = setup.broker.wait_for_session_response(&park.request_id, 1);
    assert_eq!(replay.canonical_bytes(), completed.canonical_bytes());
    assert_eq!(
        lifecycle_effect_counts(&setup.events),
        effects_before_retry,
        "exact retry replays completion without repeating Park mechanics"
    );

    setup.broker.wait_for_session_request();
    let settled_status = status_request(&setup, "status-after-park");
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Status(settled_status.clone()));
    let settled_response = setup
        .broker
        .wait_for_session_response(&settled_status.request_id, 0);
    let ResponseResult::SupervisorStatus { status } = &settled_response.result else {
        panic!("settled status remains a mechanical supervisor status");
    };
    assert_eq!(status.state, SessionState::Parked);
    assert_eq!(status.launcher_head.as_ref(), Some(&park_head));
    assert_eq!(status.broker_head.as_ref(), Some(&park_head));
    assert_eq!(status.pending_receipt_count, 0);
    assert_eq!(status.pending_operation, None);

    finish_session_relay(&setup, controller_input, relay_receiver, relay_worker);
}

#[test]
fn park_attempts_every_narrowing_mechanic_and_never_receipts_partial_success() {
    for (name, behavior, expected_state, expected_parked) in [
        (
            "revoke-failure",
            PlatformBehavior {
                revoke_fails: true,
                ..PlatformBehavior::default()
            },
            SessionState::Parked,
            true,
        ),
        (
            "freeze-failure",
            PlatformBehavior {
                park_fails: true,
                ..PlatformBehavior::default()
            },
            SessionState::Running,
            false,
        ),
    ] {
        let setup = setup(
            true,
            |_| {},
            AppendBehavior::Hold,
            behavior,
            SUPERVISOR_TIMEOUT,
        );
        let session = complete_launch(&setup);
        let durable_head = ReceiptHead {
            sequence: 1,
            digest: Digest::of(&session.receipt().canonical_bytes()).to_string(),
        };
        let (controller_input, relay_receiver, relay_worker) = begin_session_relay(session);
        setup.broker.wait_for_session_request();
        let park = lifecycle_request(&setup, "partial-park", "park-authorization", 1);
        setup
            .broker
            .deliver_session_request(ProtocolMessage::Lifecycle(park.clone()));
        let response = setup.broker.wait_for_session_response(&park.request_id, 0);
        let ResponseResult::Error { error } = response.result else {
            panic!("{name}: partial Park must return a typed failure");
        };
        assert_eq!(error.code, ErrorCode::LifecycleMechanicUnavailable);
        assert_eq!(setup.broker.session_receipt_count(), 0, "case {name}");
        assert_eq!(setup.signer.payloads().len(), 2, "case {name}");
        assert_eq!(
            lock(&setup.platform.agent).parked,
            expected_parked,
            "case {name}",
        );
        assert!(
            lock(&setup.platform.gate_state()).revoked,
            "case {name}: revocation is best-effort fail-closed even when its audit reports failure",
        );
        let events = event_snapshot(&setup.events);
        let revoke = events
            .iter()
            .position(|event| event == "capability.revoke")
            .expect("revocation was attempted");
        let freeze = events
            .iter()
            .position(|event| event == "agent.park")
            .expect("freeze was attempted despite the other mechanic's outcome");
        assert!(revoke < freeze, "case {name}: {events:?}");

        setup.broker.wait_for_session_request();
        let status_request = status_request(&setup, "partial-park-status");
        setup
            .broker
            .deliver_session_request(ProtocolMessage::Status(status_request.clone()));
        let status_response = setup
            .broker
            .wait_for_session_response(&status_request.request_id, 0);
        let ResponseResult::SupervisorStatus { status } = status_response.result else {
            panic!("{name}: status remains available after partial Park");
        };
        assert_eq!(status.state, expected_state, "case {name}");
        assert_eq!(status.channel_state, ChannelState::Revoked, "case {name}");
        assert_eq!(status.launcher_head.as_ref(), Some(&durable_head));
        assert_eq!(status.broker_head.as_ref(), Some(&durable_head));
        assert_eq!(status.pending_receipt_count, 0);
        assert_eq!(
            status.last_failure.as_ref().map(|failure| failure.code),
            Some(ErrorCode::LifecycleMechanicUnavailable),
        );

        finish_session_relay(&setup, controller_input, relay_receiver, relay_worker);
    }
}

#[test]
fn interrupt_from_running_or_parked_signals_before_receipting_and_retries_once() {
    for parked in [false, true] {
        let setup = setup(
            true,
            |_| {},
            AppendBehavior::Hold,
            PlatformBehavior::default(),
            SUPERVISOR_TIMEOUT,
        );
        let session = complete_launch(&setup);
        let (
            controller_input,
            relay_receiver,
            relay_worker,
            expected_state,
            expected_sequence,
            receipt_index,
        ) = if parked {
            let (input, receiver, worker, _) = park_launched_session(&setup, session);
            (input, receiver, worker, SessionState::Parked, 2, 1)
        } else {
            let (input, receiver, worker) = begin_session_relay(session);
            setup.broker.wait_for_session_request();
            (input, receiver, worker, SessionState::Running, 1, 0)
        };
        setup.broker.wait_for_session_request();
        let label = if parked { "parked" } else { "running" };
        let interrupt = interrupt_request(
            &setup,
            &format!("interrupt-{label}"),
            expected_state,
            expected_sequence,
        );

        setup
            .broker
            .deliver_session_request(ProtocolMessage::Lifecycle(interrupt.clone()));
        setup.broker.wait_for_session_receipt(receipt_index);
        let interrupt_bytes = setup.broker.session_receipt_bytes(receipt_index);
        let interrupt_receipt = SignedReceipt::parse_canonical(&interrupt_bytes)
            .expect("Interrupt receipt is canonical");
        assert_eq!(interrupt_receipt.payload.resulting_state, expected_state);
        let ReceiptOutcome::Interrupt { authorization } = &interrupt_receipt.payload.outcome else {
            panic!("Interrupt records its broker authorization");
        };
        assert_eq!(authorization.authorization_id, interrupt.authorization_id);
        assert_eq!(authorization.request_id, interrupt.request_id);
        assert_eq!(authorization.request_digest, interrupt.digest().to_string());

        let events = event_snapshot(&setup.events);
        let mechanic = events
            .iter()
            .rposition(|event| event == "agent.interrupt")
            .expect("whole-tree interrupt runs");
        let sign = events
            .iter()
            .rposition(|event| event == "signer.sign")
            .expect("Interrupt receipt is signed");
        let send = events
            .iter()
            .rposition(|event| event == "broker.send_session_receipt")
            .expect("Interrupt receipt is sent");
        assert!(mechanic < sign && sign < send, "case {label}: {events:?}");
        assert_eq!(event_count(&setup.events, "agent.interrupt"), 1);
        assert_eq!(event_count(&setup.events, "identity.release"), 0);

        setup.broker.wait_for_session_request();
        setup
            .broker
            .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
                setup.broker.session_receipt_acknowledgement(receipt_index),
            ));
        let completed = setup
            .broker
            .wait_for_session_response(&interrupt.request_id, 0);
        let ResponseResult::Receipt { receipt } = &completed.result else {
            panic!("Interrupt completes after its exact ACK");
        };
        assert_eq!(receipt.canonical_bytes(), interrupt_bytes);
        let status = request_supervisor_status(&setup, &format!("interrupt-{label}-status"));
        assert_eq!(status.state, expected_state);
        assert_eq!(status.process_exit, None);

        setup.broker.wait_for_session_request();
        setup
            .broker
            .deliver_session_request(ProtocolMessage::Lifecycle(interrupt.clone()));
        let replay = setup
            .broker
            .wait_for_session_response(&interrupt.request_id, 1);
        assert_eq!(replay.canonical_bytes(), completed.canonical_bytes());
        assert_eq!(event_count(&setup.events, "agent.interrupt"), 1);
        assert_eq!(setup.broker.session_receipt_count(), receipt_index + 1);

        finish_session_relay(&setup, controller_input, relay_receiver, relay_worker);
    }
}

#[test]
fn failed_interrupt_audit_revokes_channels_without_signalling_twice() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let session = complete_launch(&setup);
    let (controller_input, relay_receiver, relay_worker) = begin_session_relay(session);
    setup.broker.wait_for_session_request();
    setup.signer.fail_on_call(setup.signer.payloads().len());
    let interrupt = interrupt_request(&setup, "failed-interrupt-audit", SessionState::Running, 1);

    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(interrupt.clone()));
    let failed = setup
        .broker
        .wait_for_session_response(&interrupt.request_id, 0);
    let ResponseResult::Error { error } = &failed.result else {
        panic!("failed Interrupt audit returns a typed error");
    };
    assert_eq!(error.code, ErrorCode::SigningUnavailable);
    let status = request_supervisor_status(&setup, "failed-interrupt-audit-status");

    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(interrupt.clone()));
    let replay = setup
        .broker
        .wait_for_session_response(&interrupt.request_id, 1);
    assert_eq!(replay.canonical_bytes(), failed.canonical_bytes());
    assert_eq!(event_count(&setup.events, "agent.interrupt"), 1);
    assert_eq!(setup.broker.session_receipt_count(), 0);
    assert_eq!(status.state, SessionState::Running);
    assert_eq!(status.channel_state, ChannelState::Revoked);
    assert_eq!(status.pending_receipt_count, 1);
    assert!(lock(&setup.platform.gate_state()).revoked);
    assert!(!lock(&setup.platform.gate_state()).enabled);
    finish_session_relay_after_reconciling_backlog(
        &setup,
        controller_input,
        relay_receiver,
        relay_worker,
        1,
    );
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "One failed interrupt retry remains idempotent at the receipt backlog bound scenario keeps its causal steps and assertions together."
)]
fn failed_interrupt_retry_remains_idempotent_at_the_receipt_backlog_bound() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let timer = Arc::new(FakeTimer::new(Arc::clone(&setup.events)));
    let timer_port: Arc<dyn SupervisorTimer> = timer.clone();
    let supervisor = setup.fresh_supervisor_with_timer(timer_port);
    let session = complete_launch_on(&setup, &supervisor);
    let initial_head = ReceiptHead {
        sequence: session.receipt().payload.sequence,
        digest: Digest::of(&session.receipt().canonical_bytes()).to_string(),
    };
    let (controller_input, relay_receiver, relay_worker) = begin_session_relay(session);

    let ordinary_intent_capacity =
        usize::try_from(MAX_PENDING_RECEIPTS).expect("protocol receipt bound fits usize") - 2;
    let mut first_request = None;
    let mut first_response = None;
    for index in 0..ordinary_intent_capacity {
        setup.broker.wait_for_session_request();
        let request = interrupt_request(
            &setup,
            &format!("failed-interrupt-backlog-pressure-{index}"),
            SessionState::Running,
            1,
        );
        if index == 0 {
            setup.signer.fail_on_call(setup.signer.payloads().len());
        }
        setup
            .broker
            .deliver_session_request(ProtocolMessage::Lifecycle(request.clone()));
        let response = setup
            .broker
            .wait_for_session_response(&request.request_id, 0);
        let ResponseResult::Error { error } = &response.result else {
            panic!("failed Interrupt audit returns a typed error");
        };
        let expected = if index == 0 {
            ErrorCode::SigningUnavailable
        } else {
            ErrorCode::DurabilityUnavailable
        };
        assert_eq!(error.code, expected, "ordinary backlog entry {index}");
        if index == 0 {
            first_request = Some(request);
            first_response = Some(response);
        }
    }
    assert_eq!(
        event_count(&setup.events, "agent.interrupt"),
        ordinary_intent_capacity
    );
    let at_bound = request_supervisor_status(&setup, "status-at-ordinary-receipt-intent-bound");
    assert_eq!(at_bound.pending_receipt_count, MAX_PENDING_RECEIPTS - 2);

    setup.broker.wait_for_session_request();
    let saturated = interrupt_request(
        &setup,
        "failed-interrupt-beyond-ordinary-backlog",
        SessionState::Running,
        1,
    );
    setup.signer.fail_on_call(usize::MAX);
    let sign_calls_before_saturation = setup.signer.payloads().len();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(saturated.clone()));
    let saturation = setup
        .broker
        .wait_for_session_response(&saturated.request_id, 0);
    let ResponseResult::Error { error } = &saturation.result else {
        panic!("failure-cache saturation returns a typed error");
    };
    assert_eq!(error.code, ErrorCode::DurabilityUnavailable);
    assert_eq!(
        event_count(&setup.events, "agent.interrupt"),
        ordinary_intent_capacity
    );
    assert_eq!(setup.signer.payloads().len(), sign_calls_before_saturation);

    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(saturated.clone()));
    let saturation_replay = setup
        .broker
        .wait_for_session_response(&saturated.request_id, 1);
    assert_eq!(
        saturation_replay.canonical_bytes(),
        saturation.canonical_bytes()
    );
    assert_eq!(
        event_count(&setup.events, "agent.interrupt"),
        ordinary_intent_capacity
    );

    let first_request = first_request.expect("first failed Interrupt is retained for retry");
    let first_response = first_response.expect("first failed response is retained for comparison");
    setup.broker.wait_for_session_request();
    setup.signer.fail_on_call(setup.signer.payloads().len());
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(first_request.clone()));
    let replay = setup
        .broker
        .wait_for_session_response(&first_request.request_id, 1);

    assert_eq!(replay.canonical_bytes(), first_response.canonical_bytes());
    assert_eq!(
        event_count(&setup.events, "agent.interrupt"),
        ordinary_intent_capacity,
        "an exact retry cannot repeat an already-completed Interrupt mechanic at the backlog bound"
    );

    setup.signer.fail_on_call(usize::MAX);
    setup.broker.wait_for_session_request();
    setup.broker.disconnect_session();
    setup.broker.wait_for_reconnect(0);
    let reconnect = setup.broker.reconnect(0);
    assert_eq!(reconnect.sequence, initial_head.sequence);
    assert_eq!(reconnect.receipt_digest, initial_head.digest);
    setup.broker.complete_reconnect(reconnect);
    let repaired_receipt_count = ordinary_intent_capacity + 1;
    for index in 0..repaired_receipt_count {
        setup.broker.wait_for_session_receipt(index);
        setup.broker.wait_for_session_request();
        setup
            .broker
            .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
                setup.broker.session_receipt_acknowledgement(index),
            ));
    }
    let repaired = request_supervisor_status(&setup, "status-after-pressure-backlog-repair");
    assert_eq!(repaired.pending_receipt_count, 0);
    assert_eq!(repaired.launcher_head, repaired.broker_head);
    assert_eq!(repaired.state, SessionState::Parked);
    assert_eq!(
        event_count(&setup.events, "agent.interrupt"),
        ordinary_intent_capacity
    );
    finish_session_relay(&setup, controller_input, relay_receiver, relay_worker);
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "One terminal disposal waits behind the full ordered receipt backlog scenario keeps its causal steps and assertions together."
)]
fn terminal_disposal_waits_behind_the_full_ordered_receipt_backlog() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    setup.platform.hold_relay_quiescence();
    let timer = Arc::new(FakeTimer::new(Arc::clone(&setup.events)));
    let timer_port: Arc<dyn SupervisorTimer> = timer.clone();
    let supervisor = setup.fresh_supervisor_with_timer(timer_port);
    let session = complete_launch_on(&setup, &supervisor);
    let initial_head = ReceiptHead {
        sequence: session.receipt().payload.sequence,
        digest: Digest::of(&session.receipt().canonical_bytes()).to_string(),
    };
    let (controller_input, relay_receiver, relay_worker) = begin_session_relay(session);

    let ordinary_intent_capacity =
        usize::try_from(MAX_PENDING_RECEIPTS).expect("protocol receipt bound fits usize") - 2;
    let mut interrupts = Vec::new();
    for index in 0..ordinary_intent_capacity {
        setup.broker.wait_for_session_request();
        let interrupt = interrupt_request(
            &setup,
            &format!("backlogged-interrupt-{index}"),
            SessionState::Running,
            initial_head.sequence,
        );
        if index == 0 {
            setup.signer.fail_on_call(setup.signer.payloads().len());
        }
        setup
            .broker
            .deliver_session_request(ProtocolMessage::Lifecycle(interrupt.clone()));
        let response = setup
            .broker
            .wait_for_session_response(&interrupt.request_id, 0);
        let ResponseResult::Error { error } = response.result else {
            panic!("backlogged Interrupt returns a typed signing error");
        };
        let expected = if index == 0 {
            ErrorCode::SigningUnavailable
        } else {
            ErrorCode::DurabilityUnavailable
        };
        assert_eq!(error.code, expected, "ordinary backlog entry {index}");
        interrupts.push(interrupt);
    }

    setup.broker.wait_for_session_request();
    let park = lifecycle_request(
        &setup,
        "reserved-backlog-park",
        "reserved-backlog-park-authorization",
        initial_head.sequence,
    );
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(park.clone()));
    let failed_park = setup.broker.wait_for_session_response(&park.request_id, 0);
    let ResponseResult::Error { error } = failed_park.result else {
        panic!("reserved Park returns a typed audit error");
    };
    assert_eq!(error.code, ErrorCode::DurabilityUnavailable);
    let parked = request_supervisor_status(&setup, "status-with-reserved-park-intent");
    assert_eq!(parked.state, SessionState::Parked);
    assert_eq!(parked.channel_state, ChannelState::Revoked);
    assert_eq!(parked.pending_receipt_count, MAX_PENDING_RECEIPTS - 1);
    assert_eq!(
        event_count(&setup.events, "agent.interrupt"),
        ordinary_intent_capacity
    );
    assert_eq!(event_count(&setup.events, "agent.park"), 1);

    let disposal = LifecycleRequest {
        schema: LIFECYCLE_REQUEST_SCHEMA.to_owned(),
        protocol_version: PROTOCOL_VERSION,
        request_id: "reserved-backlog-disposal".to_owned(),
        session_id: setup.request.session_id.clone(),
        run_id: setup.request.run_id.clone(),
        authorization_id: "reserved-backlog-disposal-authorization".to_owned(),
        action: LifecycleAction::Disposal,
        expected_state: SessionState::Parked,
        expected_receipt_sequence: Some(initial_head.sequence),
        envelope_revision: setup.request.envelope_revision,
    };
    setup.signer.fail_on_call(usize::MAX);
    let signs_before_disposal = setup.signer.payloads().len();
    setup.broker.hold_session_response(&disposal.request_id);
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(disposal.clone()));
    setup.platform.wait_for_relay_quiescence();
    let terminal = request_supervisor_status(&setup, "status-with-full-terminal-backlog");
    let signs_after_disposal = setup.signer.payloads().len();

    if signs_after_disposal != signs_before_disposal {
        setup.broker.wait_for_session_receipt(0);
        setup.broker.wait_for_session_request();
        setup
            .broker
            .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
                setup.broker.session_receipt_acknowledgement(0),
            ));
        setup
            .broker
            .wait_for_session_response(&disposal.request_id, 0);
        setup.broker.wait_for_held_session_response();
        setup.broker.release_session_response();
        setup.platform.complete_relay_quiescence();
        assert_eq!(
            relay_receiver
                .recv_timeout(CALLBACK_TIMEOUT)
                .expect("overtaking terminal receipt still completes its owner")
                .expect("terminal cleanup succeeds"),
            0,
        );
        drop(controller_input);
        relay_worker.join().expect("terminal relay worker finishes");
        assert_eq!(
            signs_after_disposal, signs_before_disposal,
            "terminal Disposal signed ahead of seven earlier mechanic-bearing intents"
        );
        return;
    }

    assert_eq!(terminal.state, SessionState::Terminal);
    assert_eq!(terminal.channel_state, ChannelState::Closed);
    assert_eq!(terminal.launcher_head.as_ref(), Some(&initial_head));
    assert_eq!(terminal.broker_head.as_ref(), Some(&initial_head));
    assert_eq!(terminal.pending_receipt_count, MAX_PENDING_RECEIPTS);
    assert_eq!(setup.broker.session_receipt_count(), 0);
    let disposal_failure = setup
        .broker
        .wait_for_session_response(&disposal.request_id, 0);
    let ResponseResult::Error { error } = &disposal_failure.result else {
        panic!("backlogged Disposal reports its deferred durability");
    };
    assert_eq!(error.code, ErrorCode::DurabilityUnavailable);
    setup.broker.wait_for_held_session_response();
    assert_eq!(event_count(&setup.events, "agent.dispose"), 1);
    assert_eq!(event_count(&setup.events, "identity.release"), 1);
    assert!(matches!(
        relay_receiver.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));

    setup.broker.wait_for_session_request();
    setup.broker.disconnect_session();
    setup.broker.wait_for_reconnect(0);
    let reconnect = setup.broker.reconnect(0);
    assert_eq!(reconnect.sequence, initial_head.sequence);
    assert_eq!(reconnect.receipt_digest, initial_head.digest);
    setup.broker.complete_reconnect(reconnect);

    let mut previous_head = initial_head;
    let expected_receipts =
        usize::try_from(MAX_PENDING_RECEIPTS).expect("protocol receipt bound fits usize");
    for index in 0..expected_receipts {
        setup.broker.wait_for_session_receipt(index);
        let bytes = setup.broker.session_receipt_bytes(index);
        let receipt =
            SignedReceipt::parse_canonical(&bytes).expect("drained backlog receipt is canonical");
        assert_eq!(receipt.payload.sequence, previous_head.sequence + 1);
        assert_eq!(
            receipt.payload.previous_receipt_digest.as_deref(),
            Some(previous_head.digest.as_str())
        );
        match index.cmp(&ordinary_intent_capacity) {
            std::cmp::Ordering::Less => {
                let ReceiptOutcome::Interrupt { authorization } = &receipt.payload.outcome else {
                    panic!("ordinary backlog entry {index} remains an Interrupt");
                };
                assert_eq!(
                    authorization.request_id,
                    interrupts
                        .get(index)
                        .expect("ordinary backlog entry has its source request")
                        .request_id,
                );
                assert_eq!(receipt.payload.resulting_state, SessionState::Running);
            }
            std::cmp::Ordering::Equal => {
                let ReceiptOutcome::Park {
                    authority: ReceiptAuthority::Authorized(authorization),
                } = &receipt.payload.outcome
                else {
                    panic!("the first reserved backlog entry remains its authorized Park");
                };
                assert_eq!(authorization.request_id, park.request_id);
                assert_eq!(receipt.payload.resulting_state, SessionState::Parked);
            }
            std::cmp::Ordering::Greater => {
                let ReceiptOutcome::Disposal {
                    authority: ReceiptAuthority::Authorized(authorization),
                } = &receipt.payload.outcome
                else {
                    panic!("the last reserved backlog entry remains its authorized Disposal");
                };
                assert_eq!(authorization.request_id, disposal.request_id);
                assert_eq!(receipt.payload.resulting_state, SessionState::Terminal);
            }
        }
        previous_head = ReceiptHead {
            sequence: receipt.payload.sequence,
            digest: Digest::of(&bytes).to_string(),
        };

        if index + 1 < expected_receipts {
            assert_eq!(
                setup
                    .broker
                    .session_response_count_for(&disposal.request_id),
                1,
                "terminal request gets no second response while its audit drains"
            );
            assert!(matches!(
                relay_receiver.try_recv(),
                Err(mpsc::TryRecvError::Empty)
            ));
        }
        setup.broker.wait_for_session_request();
        setup
            .broker
            .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
                setup.broker.session_receipt_acknowledgement(index),
            ));
    }

    assert!(
        matches!(relay_receiver.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "terminal owner must still await relay quiescence after backlog convergence; events: {:?}",
        event_snapshot(&setup.events),
    );
    assert!(
        !setup.broker.is_closed(),
        "terminal owner closed its broker before relay quiescence; events: {:?}",
        event_snapshot(&setup.events),
    );
    let converged = request_supervisor_status(&setup, "status-after-full-backlog-repair");
    assert_eq!(converged.state, SessionState::Terminal);
    assert_eq!(converged.channel_state, ChannelState::Closed);
    assert_eq!(converged.launcher_head.as_ref(), Some(&previous_head));
    assert_eq!(converged.broker_head.as_ref(), Some(&previous_head));
    assert_eq!(converged.pending_receipt_count, 0);
    assert_eq!(
        event_count(&setup.events, "agent.interrupt"),
        ordinary_intent_capacity
    );
    assert_eq!(event_count(&setup.events, "agent.park"), 1);
    assert_eq!(event_count(&setup.events, "agent.dispose"), 1);
    assert_eq!(event_count(&setup.events, "identity.release"), 1);
    assert_eq!(
        setup.signer.payloads().len(),
        signs_before_disposal + expected_receipts
    );
    assert!(matches!(
        relay_receiver.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));

    setup.platform.complete_relay_quiescence();
    setup.broker.release_session_response();
    let after_stale_response =
        request_supervisor_status(&setup, "status-after-stale-terminal-response");
    assert_eq!(after_stale_response.state, SessionState::Terminal);
    assert_eq!(after_stale_response.channel_state, ChannelState::Closed);
    assert_eq!(after_stale_response.pending_receipt_count, 0);
    assert!(matches!(
        relay_receiver.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    assert!(!setup.broker.is_closed());

    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(disposal.clone()));
    let replay = setup
        .broker
        .wait_for_session_response(&disposal.request_id, 1);
    assert_eq!(replay.canonical_bytes(), disposal_failure.canonical_bytes());
    setup.broker.wait_for_held_session_response();
    assert_eq!(event_count(&setup.events, "agent.dispose"), 1);
    assert_eq!(event_count(&setup.events, "identity.release"), 1);
    assert_eq!(
        setup.signer.payloads().len(),
        signs_before_disposal + expected_receipts
    );
    assert_eq!(setup.broker.session_receipt_count(), expected_receipts);

    let waiting_for_response =
        request_supervisor_status(&setup, "status-before-current-terminal-response");
    assert_eq!(waiting_for_response.state, SessionState::Terminal);
    assert_eq!(waiting_for_response.channel_state, ChannelState::Closed);
    assert_eq!(waiting_for_response.pending_receipt_count, 0);
    assert!(matches!(
        relay_receiver.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    assert!(!setup.broker.is_closed());

    setup.broker.release_session_response();
    assert_eq!(
        relay_receiver
            .recv_timeout(CALLBACK_TIMEOUT)
            .expect("terminal owner completes after its response send")
            .expect("terminal cleanup succeeds"),
        0,
    );
    assert!(setup.broker.is_closed());
    drop(controller_input);
    relay_worker.join().expect("terminal relay worker finishes");
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "One exact head reconnect drains an unsigned park intent once scenario keeps its causal steps and assertions together."
)]
fn exact_head_reconnect_drains_an_unsigned_park_intent_once() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let timer = Arc::new(FakeTimer::new(Arc::clone(&setup.events)));
    let timer_port: Arc<dyn SupervisorTimer> = timer.clone();
    let supervisor = setup.fresh_supervisor_with_timer(timer_port);
    let session = complete_launch_on(&setup, &supervisor);
    let initial_head = ReceiptHead {
        sequence: session.receipt().payload.sequence,
        digest: Digest::of(&session.receipt().canonical_bytes()).to_string(),
    };
    let (controller_input, relay_receiver, relay_worker) = begin_session_relay(session);

    setup.broker.wait_for_session_request();
    let park = lifecycle_request(
        &setup,
        "park-signing-failure-before-reconnect",
        "park-signing-failure-authorization",
        1,
    );
    setup.signer.fail_on_call(setup.signer.payloads().len());
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(park.clone()));
    let failed = setup.broker.wait_for_session_response(&park.request_id, 0);
    let ResponseResult::Error { error } = &failed.result else {
        panic!("failed Park signing returns a typed error");
    };
    assert_eq!(error.code, ErrorCode::SigningUnavailable);

    let parked = request_supervisor_status(&setup, "status-with-unsigned-park-intent");
    assert_eq!(parked.state, SessionState::Parked);
    assert_eq!(parked.channel_state, ChannelState::Revoked);
    assert_eq!(parked.launcher_head.as_ref(), Some(&initial_head));
    assert_eq!(parked.broker_head.as_ref(), Some(&initial_head));
    assert_eq!(parked.pending_receipt_count, 1);
    assert_eq!(setup.broker.session_receipt_count(), 0);
    assert_eq!(event_count(&setup.events, "agent.park"), 1);

    let resume = resume_request(&setup, "resume-while-park-audit-is-deferred");
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(resume.clone()));
    let blocked = setup
        .broker
        .wait_for_session_response(&resume.request_id, 0);
    let ResponseResult::Error { error } = blocked.result else {
        panic!("widening remains blocked behind the unsigned Park intent");
    };
    assert_eq!(error.code, ErrorCode::DurabilityUnavailable);
    assert_eq!(event_count(&setup.events, "agent.resume"), 0);

    let sign_calls_before_reconnect = setup.signer.payloads().len();
    setup.broker.wait_for_session_request();
    setup.broker.disconnect_session();
    setup.broker.wait_for_reconnect(0);
    let reconnect = setup.broker.reconnect(0);
    assert_eq!(reconnect.sequence, initial_head.sequence);
    assert_eq!(reconnect.receipt_digest, initial_head.digest);
    setup.broker.complete_reconnect(reconnect);
    let during_repair = request_supervisor_status(&setup, "status-during-unsigned-park-repair");
    let sign_calls_after_reconnect = setup.signer.payloads().len();

    if sign_calls_after_reconnect == sign_calls_before_reconnect {
        finish_session_relay(&setup, controller_input, relay_receiver, relay_worker);
        assert_eq!(
            sign_calls_after_reconnect,
            sign_calls_before_reconnect + 1,
            "exact-head reconnect did not resume signing the deferred Park intent"
        );
        return;
    }

    assert_eq!(during_repair.state, SessionState::Parked);
    assert_eq!(during_repair.channel_state, ChannelState::Revoked);
    setup.broker.wait_for_session_receipt(0);
    let park_bytes = setup.broker.session_receipt_bytes(0);
    let park_receipt =
        SignedReceipt::parse_canonical(&park_bytes).expect("recovered Park receipt is canonical");
    assert_eq!(park_receipt.payload.sequence, initial_head.sequence + 1);
    assert_eq!(
        park_receipt.payload.previous_receipt_digest,
        Some(initial_head.digest.clone())
    );
    let ReceiptOutcome::Park {
        authority: ReceiptAuthority::Authorized(authorization),
    } = &park_receipt.payload.outcome
    else {
        panic!("recovered Park receipt retains its broker authorization");
    };
    assert_eq!(authorization.request_id, park.request_id);
    assert_eq!(authorization.authorization_id, park.authorization_id);
    assert_eq!(event_count(&setup.events, "agent.park"), 1);
    assert_eq!(sign_calls_after_reconnect, sign_calls_before_reconnect + 1);

    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(0),
        ));
    let repaired = request_supervisor_status(&setup, "status-after-unsigned-park-repair");
    let repaired_head = ReceiptHead {
        sequence: park_receipt.payload.sequence,
        digest: Digest::of(&park_bytes).to_string(),
    };
    assert_eq!(repaired.state, SessionState::Parked);
    assert_eq!(repaired.channel_state, ChannelState::Revoked);
    assert_eq!(repaired.launcher_head.as_ref(), Some(&repaired_head));
    assert_eq!(repaired.broker_head.as_ref(), Some(&repaired_head));
    assert_eq!(repaired.pending_receipt_count, 0);

    let scheduled = timer.scheduled_count();
    for index in 0..scheduled {
        timer.fire(index);
    }
    let after_late_timers = request_supervisor_status(&setup, "status-after-late-repair-timers");
    assert_eq!(after_late_timers.state, SessionState::Parked);
    assert_eq!(
        after_late_timers.launcher_head.as_ref(),
        Some(&repaired_head)
    );
    assert_eq!(after_late_timers.broker_head.as_ref(), Some(&repaired_head));

    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(park.clone()));
    let replay = setup.broker.wait_for_session_response(&park.request_id, 1);
    assert_eq!(replay.canonical_bytes(), failed.canonical_bytes());
    assert_eq!(event_count(&setup.events, "agent.park"), 1);
    assert_eq!(setup.signer.payloads().len(), sign_calls_after_reconnect);
    assert_eq!(setup.broker.session_receipt_count(), 1);

    finish_session_relay(&setup, controller_input, relay_receiver, relay_worker);
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "One exact head reconnect freezes an unsigned running intent before repair scenario keeps its causal steps and assertions together."
)]
fn exact_head_reconnect_freezes_an_unsigned_running_intent_before_repair() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let timer = Arc::new(FakeTimer::new(Arc::clone(&setup.events)));
    let timer_port: Arc<dyn SupervisorTimer> = timer.clone();
    let supervisor = setup.fresh_supervisor_with_timer(timer_port);
    let session = complete_launch_on(&setup, &supervisor);
    let initial_head = ReceiptHead {
        sequence: session.receipt().payload.sequence,
        digest: Digest::of(&session.receipt().canonical_bytes()).to_string(),
    };
    let (controller_input, relay_receiver, relay_worker) = begin_session_relay(session);

    setup.broker.wait_for_session_request();
    let interrupt = interrupt_request(
        &setup,
        "unsigned-running-interrupt-before-reconnect",
        SessionState::Running,
        initial_head.sequence,
    );
    setup.signer.fail_on_call(setup.signer.payloads().len());
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(interrupt.clone()));
    let failed = setup
        .broker
        .wait_for_session_response(&interrupt.request_id, 0);
    let ResponseResult::Error { error } = failed.result else {
        panic!("failed Interrupt signing returns a typed error");
    };
    assert_eq!(error.code, ErrorCode::SigningUnavailable);
    let unsigned = request_supervisor_status(&setup, "status-with-unsigned-running-intent");
    assert_eq!(unsigned.state, SessionState::Running);
    assert_eq!(unsigned.channel_state, ChannelState::Revoked);
    assert_eq!(unsigned.pending_receipt_count, 1);
    assert_eq!(event_count(&setup.events, "agent.interrupt"), 1);
    assert_eq!(event_count(&setup.events, "agent.park"), 0);

    setup.broker.wait_for_session_request();
    setup.broker.disconnect_session();
    setup.broker.wait_for_reconnect(0);
    let reconnect = setup.broker.reconnect(0);
    assert_eq!(reconnect.sequence, initial_head.sequence);
    assert_eq!(reconnect.receipt_digest, initial_head.digest);
    setup.broker.complete_reconnect(reconnect);
    setup.broker.wait_for_session_receipt(0);

    let during = request_supervisor_status(&setup, "status-during-unsigned-running-repair");
    assert_eq!(
        during.state,
        SessionState::Parked,
        "reconciliation must freeze a Running tree even when its whole backlog is unsigned"
    );
    assert_eq!(during.broker_connection, BrokerConnection::Reconciling);
    assert_eq!(during.channel_state, ChannelState::Revoked);
    assert_eq!(during.pending_receipt_count, 2);
    assert_eq!(event_count(&setup.events, "agent.park"), 1);

    let interrupt_bytes = setup.broker.session_receipt_bytes(0);
    let interrupt_receipt = SignedReceipt::parse_canonical(&interrupt_bytes)
        .expect("rebased deferred Interrupt is canonical");
    assert_eq!(
        interrupt_receipt.payload.sequence,
        initial_head.sequence + 1
    );
    let ReceiptOutcome::Interrupt { authorization } = &interrupt_receipt.payload.outcome else {
        panic!("deferred Running intent remains an Interrupt");
    };
    assert_eq!(authorization.request_id, interrupt.request_id);
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(0),
        ));

    setup.broker.wait_for_session_receipt(1);
    let causal_park_bytes = setup.broker.session_receipt_bytes(1);
    let causal_park = SignedReceipt::parse_canonical(&causal_park_bytes)
        .expect("reconciliation freeze has a canonical causal Park");
    assert_eq!(
        causal_park.payload.outcome,
        ReceiptOutcome::Park {
            authority: ReceiptAuthority::Cause {
                cause: ReceiptCause::BrokerLost,
            },
        }
    );
    assert_eq!(
        causal_park.payload.previous_receipt_digest,
        Some(Digest::of(&interrupt_bytes).to_string())
    );
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(1),
        ));
    let repaired = request_supervisor_status(&setup, "status-after-unsigned-running-repair");
    assert_eq!(repaired.state, SessionState::Parked);
    assert_eq!(repaired.broker_connection, BrokerConnection::Connected);
    assert_eq!(repaired.channel_state, ChannelState::Revoked);
    assert_eq!(repaired.pending_receipt_count, 0);
    assert_eq!(repaired.launcher_head, repaired.broker_head);

    let mut resume = resume_request(&setup, "resume-after-unsigned-running-repair");
    resume.expected_receipt_sequence = Some(causal_park.payload.sequence);
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(resume.clone()));
    setup.broker.wait_for_session_receipt(2);
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(2),
        ));
    let response = setup
        .broker
        .wait_for_session_response(&resume.request_id, 0);
    assert!(matches!(response.result, ResponseResult::Receipt { .. }));
    let resumed = request_supervisor_status(&setup, "status-after-unsigned-running-resume");
    assert_eq!(resumed.state, SessionState::Running);
    assert_eq!(resumed.channel_state, ChannelState::Enabled);

    finish_session_relay(&setup, controller_input, relay_receiver, relay_worker);
}

#[test]
fn authorized_disposal_releases_everything_before_its_terminal_receipt() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let session = complete_launch(&setup);
    let (controller_input, relay_receiver, relay_worker) = begin_session_relay(session);
    setup.broker.wait_for_session_request();
    let disposal = disposal_request(&setup, "authorized-disposal");

    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(disposal.clone()));
    setup.broker.wait_for_session_receipt(0);
    let disposal_bytes = setup.broker.session_receipt_bytes(0);
    let disposal_receipt = SignedReceipt::parse_canonical(&disposal_bytes)
        .expect("authorized Disposal receipt is canonical");
    assert_eq!(
        disposal_receipt.payload.resulting_state,
        SessionState::Terminal
    );
    let ReceiptOutcome::Disposal {
        authority: ReceiptAuthority::Authorized(authorization),
    } = &disposal_receipt.payload.outcome
    else {
        panic!("authorized Disposal retains broker authority");
    };
    assert_eq!(authorization.authorization_id, disposal.authorization_id);
    assert_eq!(authorization.request_id, disposal.request_id);
    assert_eq!(authorization.request_digest, disposal.digest().to_string());

    let events = event_snapshot(&setup.events);
    let close = events
        .iter()
        .rposition(|event| event == "capability.close")
        .expect("Disposal closes capabilities");
    let dispose = events
        .iter()
        .rposition(|event| event == "agent.dispose")
        .expect("Disposal proves the process tree empty");
    let release = events
        .iter()
        .rposition(|event| event == "identity.release")
        .expect("Disposal releases the identity");
    let sign = events
        .iter()
        .rposition(|event| event == "signer.sign")
        .expect("Disposal signs its terminal truth");
    assert!(close < dispose && dispose < release && release < sign);
    assert_eq!(event_count(&setup.events, "agent.dispose"), 1);
    assert_eq!(event_count(&setup.events, "identity.release"), 1);

    let status = request_supervisor_status(&setup, "authorized-disposal-status");
    assert_eq!(status.state, SessionState::Terminal);
    assert_eq!(status.channel_state, ChannelState::Closed);
    assert_eq!(status.process_exit, None);
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(0),
        ));
    let response = setup
        .broker
        .wait_for_session_response(&disposal.request_id, 0);
    let ResponseResult::Receipt { receipt } = response.result else {
        panic!("authorized Disposal completes with its receipt");
    };
    assert_eq!(receipt.canonical_bytes(), disposal_bytes);
    assert_eq!(event_count(&setup.events, "agent.dispose"), 1);
    assert_eq!(event_count(&setup.events, "identity.release"), 1);

    finish_terminal_session_relay(controller_input, relay_receiver, relay_worker);
}

#[test]
fn authorized_disposal_waits_for_its_correlated_terminal_response_send() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let session = complete_launch(&setup);
    let (controller_input, relay_receiver, relay_worker) = begin_session_relay(session);
    setup.broker.wait_for_session_request();
    let disposal = disposal_request(&setup, "held-terminal-response");
    setup.broker.hold_session_response(&disposal.request_id);

    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(disposal.clone()));
    setup.broker.wait_for_session_receipt(0);
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(0),
        ));
    setup.broker.wait_for_held_session_response();

    let status_request = status_request(&setup, "status-during-terminal-response-send");
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Status(status_request.clone()));
    let status_response = setup
        .broker
        .wait_for_session_response(&status_request.request_id, 0);
    let ResponseResult::SupervisorStatus { status } = status_response.result else {
        panic!("the live owner reports Terminal while its response send is held");
    };
    assert_eq!(status.state, SessionState::Terminal);
    assert!(matches!(
        relay_receiver.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    assert!(!setup.broker.is_closed());

    setup.broker.release_session_response();
    assert_eq!(
        relay_receiver
            .recv_timeout(CALLBACK_TIMEOUT)
            .expect("terminal relay completes after its response send")
            .expect("terminal supervisor cleanup succeeds"),
        0,
    );
    assert!(setup.broker.is_closed());
    drop(controller_input);
    relay_worker.join().expect("terminal relay worker finishes");
}

#[test]
fn terminal_disposal_waits_for_relay_quiescence_and_ignores_late_relay_events() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    setup.platform.hold_relay_quiescence();
    let session = complete_launch(&setup);
    let (controller_input, relay_receiver, relay_worker) = begin_session_relay(session);
    setup.broker.wait_for_session_request();
    let disposal = disposal_request(&setup, "disposal-awaiting-relay-quiescence");

    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(disposal.clone()));
    setup.broker.wait_for_session_receipt(0);
    assert_eq!(
        event_count(&setup.events, "signer.complete"),
        0,
        "a pending terminal receipt retains signing authority"
    );
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(0),
        ));
    setup
        .broker
        .wait_for_session_response(&disposal.request_id, 0);
    setup.platform.wait_for_relay_quiescence();

    assert!(matches!(
        relay_receiver.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    assert!(!setup.broker.is_closed());
    assert_eq!(event_count(&setup.events, "signer.complete"), 0);

    setup.platform.complete_relay_quiescence();
    assert_eq!(
        relay_receiver
            .recv_timeout(CALLBACK_TIMEOUT)
            .expect("terminal relay completes after relay quiescence")
            .expect("terminal supervisor cleanup succeeds"),
        0,
    );
    assert!(setup.broker.is_closed());
    assert_eq!(event_count(&setup.events, "signer.complete"), 1);
    relay_worker.join().expect("terminal relay worker finishes");

    let receipts = setup.broker.session_receipt_count();
    let responses = setup.broker.session_response_count();
    let signs = setup.signer.payloads().len();
    let disposals = event_count(&setup.events, "agent.dispose");
    let releases = event_count(&setup.events, "identity.release");
    setup
        .platform
        .send_running_event(RunningAgentEvent::ProcessExited(
            ProcessExitClassification::Failure,
        ));
    assert_eq!(setup.broker.session_receipt_count(), receipts);
    assert_eq!(setup.broker.session_response_count(), responses);
    assert_eq!(setup.signer.payloads().len(), signs);
    assert_eq!(event_count(&setup.events, "agent.dispose"), disposals);
    assert_eq!(event_count(&setup.events, "identity.release"), releases);

    drop(controller_input);
}

#[test]
fn failed_key_completion_reports_maintenance_after_terminal_ack() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    setup.signer.fail_completion.store(true, Ordering::SeqCst);
    let session = complete_launch(&setup);
    let (input, receiver, worker) = begin_session_relay(session);
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(disposal_request(
            &setup,
            "cleanup-failure",
        )));
    setup.broker.wait_for_session_receipt(0);
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(0),
        ));
    let error = receiver
        .recv_timeout(CALLBACK_TIMEOUT)
        .unwrap()
        .unwrap_err();
    assert!(error.to_string().contains("signing-key cleanup"), "{error}");
    assert_eq!(event_count(&setup.events, "identity.release"), 1);
    assert_eq!(event_count(&setup.events, "signer.complete"), 1);
    drop(input);
    worker.join().unwrap();
}

#[test]
fn failed_authorized_disposal_retains_process_control_for_cleanup_retry() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior {
            dispose_fails: true,
            ..PlatformBehavior::default()
        },
        SUPERVISOR_TIMEOUT,
    );
    let session = complete_launch(&setup);
    let (controller_input, relay_receiver, relay_worker) = begin_session_relay(session);
    setup.broker.wait_for_session_request();
    let disposal = disposal_request(&setup, "failed-authorized-disposal");
    setup.broker.hold_session_response(&disposal.request_id);

    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(disposal.clone()));
    setup.broker.wait_for_held_session_response();
    let failed = setup
        .broker
        .wait_for_session_response(&disposal.request_id, 0);
    let ResponseResult::Error { error } = failed.result else {
        panic!("unproved Disposal returns a typed mechanic failure");
    };
    assert_eq!(error.code, ErrorCode::LifecycleMechanicUnavailable);

    let status = request_supervisor_status(&setup, "status-during-failed-disposal-response");
    assert_eq!(status.state, SessionState::Running);
    assert_eq!(status.channel_state, ChannelState::Revoked);
    assert_eq!(status.pending_receipt_count, 0);
    assert!(matches!(
        relay_receiver.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    assert!(!setup.broker.is_closed());

    setup.broker.release_session_response();
    assert_eq!(
        relay_receiver
            .recv_timeout(CALLBACK_TIMEOUT)
            .expect("failed terminal cleanup completes after its response send"),
        Err(SupervisorError::CleanupUnproven),
    );
    assert!(lock(&setup.platform.agent).dispose_attempts >= 2);
    assert_eq!(event_count(&setup.events, "identity.poison"), 1);
    assert_eq!(event_count(&setup.events, "identity.release"), 0);
    assert_eq!(setup.signer.payloads().len(), 2);
    assert_eq!(setup.broker.session_receipt_count(), 0);
    assert!(setup.broker.is_closed());
    drop(controller_input);
    relay_worker
        .join()
        .expect("failed terminal relay worker finishes");
}

#[test]
fn relay_failure_is_receipted_after_cleanup_and_waits_for_durable_ack() {
    for parked in [false, true] {
        relay_failure_after_cleanup(parked);
    }
}

fn relay_failure_after_cleanup(parked: bool) {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let session = complete_launch(&setup);
    let (controller_input, receiver, worker, previous) = if parked {
        park_launched_session(&setup, session)
    } else {
        let running = session.receipt().clone();
        let (input, receiver, worker) = begin_session_relay(session);
        (input, receiver, worker, running)
    };
    let receipt_index = setup.broker.session_receipt_count();
    setup.broker.wait_for_session_request();
    setup
        .platform
        .send_running_event(RunningAgentEvent::RelayFailed);
    setup.broker.wait_for_session_receipt(receipt_index);
    let receipt =
        SignedReceipt::parse_canonical(&setup.broker.session_receipt_bytes(receipt_index)).unwrap();
    assert_eq!(receipt.payload.resulting_state, SessionState::Terminal);
    assert_eq!(receipt.payload.sequence, previous.payload.sequence + 1);
    assert_eq!(
        receipt.payload.previous_receipt_digest,
        Some(previous.digest().to_string())
    );
    assert_eq!(
        serde_json::to_value(&receipt.payload.outcome).unwrap(),
        serde_json::json!({
            "action": "disposal", "authority": {"kind": "cause", "cause": "relay_failed"}
        })
    );
    let events = event_snapshot(&setup.events);
    let position = |name| events.iter().rposition(|event| event == name).unwrap();
    assert!(position("agent.event.relay_failed") < position("capability.close"));
    assert!(position("capability.close") < position("agent.dispose"));
    assert!(position("agent.dispose") < position("identity.release"));
    assert!(position("identity.release") < position("signer.sign"));
    let status = request_supervisor_status(&setup, "relay-failed-status");
    assert_eq!(status.state, SessionState::Terminal);
    assert_eq!(status.channel_state, ChannelState::Closed);
    assert_eq!(status.process_exit, None);
    for event in [
        RunningAgentEvent::RelayFailed,
        RunningAgentEvent::ControllerEof,
        RunningAgentEvent::ProcessExited(ProcessExitClassification::Success),
    ] {
        setup.platform.send_running_event(event);
    }
    let late = request_supervisor_status(&setup, "late-relay-events-are-inert");
    assert_eq!(late.state, SessionState::Terminal);
    assert_eq!(late.process_exit, None);
    assert!(matches!(
        receiver.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(receipt_index),
        ));
    assert_eq!(
        receiver.recv_timeout(CALLBACK_TIMEOUT).unwrap(),
        Err(SupervisorError::RelayFailed)
    );
    drop(controller_input);
    worker.join().unwrap();
    assert_eq!(event_count(&setup.events, "agent.dispose"), 1);
    assert_eq!(event_count(&setup.events, "identity.release"), 1);
}

#[test]
fn relay_failure_reconciles_its_failed_terminal_audit_before_returning_error() {
    for failure in [PendingAudit::FailedSigning, PendingAudit::RejectedAck] {
        let setup = setup(
            true,
            |_| {},
            AppendBehavior::Hold,
            PlatformBehavior::default(),
            SUPERVISOR_TIMEOUT,
        );
        let session = complete_launch(&setup);
        let head = session.receipt().clone();
        let (input, receiver, worker) = begin_session_relay(session);
        let sign_call = setup.signer.payloads().len();
        setup.signer.hold_on_call(sign_call);
        if failure == PendingAudit::FailedSigning {
            setup.signer.fail_on_call(sign_call);
        }
        setup.broker.wait_for_session_request();
        setup
            .platform
            .send_running_event(RunningAgentEvent::RelayFailed);
        setup.signer.wait_for_held_call();
        setup.signer.release_held_call();
        if failure == PendingAudit::RejectedAck {
            setup.broker.wait_for_session_receipt(0);
            let mut ack = setup.broker.session_receipt_acknowledgement(0);
            ack.disposition = ReceiptDisposition::Rejected;
            setup.broker.wait_for_session_request();
            setup
                .broker
                .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(ack));
        }
        let status = request_supervisor_status(&setup, "failed-terminal-audit");
        assert_eq!(status.pending_receipt_count, 1);
        assert_eq!(status.state, SessionState::Terminal);
        assert_eq!(status.channel_state, ChannelState::Closed);
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        setup.signer.fail_on_call(usize::MAX);
        setup.broker.wait_for_session_request();
        setup.broker.disconnect_session();
        setup.broker.wait_for_reconnect(0);
        let mut reconnect = setup.broker.reconnect(0);
        reconnect.sequence = head.payload.sequence;
        reconnect.receipt_digest = head.digest().to_string();
        setup.broker.complete_reconnect(reconnect);
        let index = usize::from(failure == PendingAudit::RejectedAck);
        setup.broker.wait_for_session_receipt(index);
        if failure == PendingAudit::RejectedAck {
            assert_eq!(
                setup.broker.session_receipt_bytes(1),
                setup.broker.session_receipt_bytes(0)
            );
        }
        let receipt =
            SignedReceipt::parse_canonical(&setup.broker.session_receipt_bytes(index)).unwrap();
        assert_eq!(receipt.payload.sequence, head.payload.sequence + 1);
        assert_eq!(
            receipt.payload.previous_receipt_digest,
            Some(head.digest().to_string())
        );
        assert_eq!(
            receipt.payload.outcome,
            ReceiptOutcome::Disposal {
                authority: ReceiptAuthority::Cause {
                    cause: ReceiptCause::RelayFailed
                }
            }
        );
        setup.broker.wait_for_session_request();
        setup
            .broker
            .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
                setup.broker.session_receipt_acknowledgement(index),
            ));
        assert_eq!(
            receiver.recv_timeout(CALLBACK_TIMEOUT).unwrap(),
            Err(SupervisorError::RelayFailed)
        );
        drop(input);
        worker.join().unwrap();
        assert_eq!(event_count(&setup.events, "agent.dispose"), 1);
        assert_eq!(event_count(&setup.events, "identity.release"), 1);
    }
}

#[test]
fn relay_failure_with_unproven_cleanup_never_signs_or_releases_identity() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior {
            dispose_fails: true,
            ..PlatformBehavior::default()
        },
        SUPERVISOR_TIMEOUT,
    );
    setup.platform.hold_relay_quiescence();
    let session = complete_launch(&setup);
    let (input, receiver, worker) = begin_session_relay(session);
    setup.broker.wait_for_session_request();
    setup
        .platform
        .send_running_event(RunningAgentEvent::RelayFailed);
    setup.platform.wait_for_relay_quiescence();
    let status = request_supervisor_status(&setup, "relay-cleanup-unproven");
    assert_eq!(status.state, SessionState::Running);
    assert_eq!(status.channel_state, ChannelState::Revoked);
    assert_eq!(status.process_exit, None);
    setup.platform.complete_relay_quiescence();
    assert_eq!(
        receiver.recv_timeout(CALLBACK_TIMEOUT).unwrap(),
        Err(SupervisorError::CleanupUnproven)
    );
    drop(input);
    worker.join().unwrap();
    assert_eq!(setup.signer.payloads().len(), 2);
    assert_eq!(setup.broker.session_receipt_count(), 0);
    assert_eq!(event_count(&setup.events, "identity.release"), 0);
    assert_eq!(event_count(&setup.events, "identity.poison"), 1);
}

#[test]
fn relay_failure_preserves_pending_interrupt_and_resume_receipts() {
    for action in [LifecycleAction::Interrupt, LifecycleAction::Resume] {
        for pending in [
            PendingAudit::Signing,
            PendingAudit::FailedSigning,
            PendingAudit::RejectedAck,
        ] {
            terminal_event_during_pending_receipt(action, pending, RunningAgentEvent::RelayFailed);
        }
    }
}

#[test]
fn failed_resume_signature_after_process_exit_preserves_terminal_audit() {
    for pending in [PendingAudit::FailedSigning, PendingAudit::RejectedAck] {
        terminal_event_during_pending_receipt(
            LifecycleAction::Resume,
            pending,
            RunningAgentEvent::ProcessExited(ProcessExitClassification::Success),
        );
    }
}

#[test]
fn agent_identity_loss_revokes_before_cleanup_and_cannot_be_revived_by_pending_audit() {
    for action in [LifecycleAction::Interrupt, LifecycleAction::Resume] {
        for pending in [
            PendingAudit::Signing,
            PendingAudit::FailedSigning,
            PendingAudit::RejectedAck,
        ] {
            terminal_event_during_pending_receipt(
                action,
                pending,
                RunningAgentEvent::AgentIdentityLost,
            );
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PendingAudit {
    Signing,
    FailedSigning,
    RejectedAck,
}

#[expect(
    clippy::too_many_lines,
    reason = "One ordered terminal race fixture preserves the prior mechanic and both successful/failed audit phases through reconnect."
)]
fn terminal_event_during_pending_receipt(
    action: LifecycleAction,
    pending: PendingAudit,
    event: RunningAgentEvent,
) {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let session = complete_launch(&setup);
    let (input, receiver, worker, head) = if action == LifecycleAction::Resume {
        park_launched_session(&setup, session)
    } else {
        let head = session.receipt().clone();
        let (input, receiver, worker) = begin_session_relay(session);
        (input, receiver, worker, head)
    };
    let receipt_index = setup.broker.session_receipt_count();
    let request = if action == LifecycleAction::Resume {
        resume_request(&setup, "resume-before-relay-failure")
    } else {
        interrupt_request(
            &setup,
            "interrupt-before-relay-failure",
            SessionState::Running,
            head.payload.sequence,
        )
    };
    let sign_call = setup.signer.payloads().len();
    setup.signer.hold_on_call(sign_call);
    if pending == PendingAudit::FailedSigning {
        setup.signer.fail_on_call(sign_call);
    }
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(request.clone()));
    setup.signer.wait_for_held_call();
    if pending == PendingAudit::RejectedAck {
        setup.signer.release_held_call();
        setup.broker.wait_for_session_receipt(receipt_index);
    }
    setup.platform.send_running_event(event);
    let terminal = request_supervisor_status(&setup, "queued-relay-failure");
    assert_eq!(terminal.state, SessionState::Terminal);
    assert_eq!(terminal.channel_state, ChannelState::Closed);
    let events = event_snapshot(&setup.events);
    let closed = events
        .iter()
        .rposition(|event| event == "capability.close")
        .unwrap();
    let disposed = events
        .iter()
        .rposition(|event| event == "agent.dispose")
        .unwrap();
    assert!(closed < disposed);
    let (authority, classification, finish) = match event {
        RunningAgentEvent::ProcessExited(classification) => (
            ReceiptAuthority::ProcessExited { classification },
            Some(classification),
            Ok(0),
        ),
        RunningAgentEvent::RelayFailed => (
            ReceiptAuthority::Cause {
                cause: ReceiptCause::RelayFailed,
            },
            None,
            Err(SupervisorError::RelayFailed),
        ),
        RunningAgentEvent::AgentIdentityLost => (
            ReceiptAuthority::Cause {
                cause: ReceiptCause::AgentIdentityLost,
            },
            None,
            Err(SupervisorError::AgentIdentityRejected),
        ),
        RunningAgentEvent::ControllerEof => panic!("only terminal events in this fixture"),
    };
    assert_eq!(terminal.process_exit, classification);
    if pending == PendingAudit::RejectedAck {
        let mut ack = setup.broker.session_receipt_acknowledgement(receipt_index);
        ack.disposition = ReceiptDisposition::Rejected;
        setup.broker.wait_for_session_request();
        setup
            .broker
            .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(ack));
    } else {
        setup.signer.release_held_call();
    }
    if pending != PendingAudit::Signing {
        let response = setup
            .broker
            .wait_for_session_response(&request.request_id, 0);
        let ResponseResult::Error { error } = response.result else {
            panic!("signing failed");
        };
        assert_eq!(
            error.code,
            if pending == PendingAudit::FailedSigning {
                ErrorCode::SigningUnavailable
            } else {
                ErrorCode::DurabilityUnavailable
            }
        );
        let status = request_supervisor_status(&setup, "relay-failure-retained-for-repair");
        assert_eq!(status.pending_receipt_count, 2);
        assert_eq!(status.channel_state, ChannelState::Closed);
        setup.signer.fail_on_call(usize::MAX);
        setup.broker.wait_for_session_request();
        setup.broker.disconnect_session();
        setup.broker.wait_for_reconnect(0);
        let mut reconnect = setup.broker.reconnect(0);
        reconnect.sequence = head.payload.sequence;
        reconnect.receipt_digest = head.digest().to_string();
        setup.broker.complete_reconnect(reconnect);
    }
    let receipt_index = receipt_index + usize::from(pending == PendingAudit::RejectedAck);
    setup.broker.wait_for_session_receipt(receipt_index);
    if pending == PendingAudit::RejectedAck {
        assert_eq!(
            setup.broker.session_receipt_bytes(receipt_index),
            setup.broker.session_receipt_bytes(receipt_index - 1),
            "replay exact signed bytes"
        );
    }
    let first =
        SignedReceipt::parse_canonical(&setup.broker.session_receipt_bytes(receipt_index)).unwrap();
    assert_eq!(
        first.payload.previous_receipt_digest,
        Some(head.digest().to_string())
    );
    assert!(matches!(
        (&first.payload.outcome, action),
        (ReceiptOutcome::Interrupt { .. }, LifecycleAction::Interrupt)
            | (ReceiptOutcome::Resume { .. }, LifecycleAction::Resume)
    ));
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(receipt_index),
        ));
    setup.broker.wait_for_session_receipt(receipt_index + 1);
    let terminal =
        SignedReceipt::parse_canonical(&setup.broker.session_receipt_bytes(receipt_index + 1))
            .unwrap();
    assert_eq!(
        terminal.payload.previous_receipt_digest,
        Some(first.digest().to_string())
    );
    assert_eq!(terminal.payload.sequence, first.payload.sequence + 1);
    assert_eq!(
        terminal.payload.outcome,
        ReceiptOutcome::Disposal { authority }
    );
    assert!(matches!(
        receiver.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup
                .broker
                .session_receipt_acknowledgement(receipt_index + 1),
        ));
    assert_eq!(receiver.recv_timeout(CALLBACK_TIMEOUT).unwrap(), finish);
    drop(input);
    worker.join().unwrap();
    assert_eq!(event_count(&setup.events, "agent.dispose"), 1);
    assert_eq!(event_count(&setup.events, "identity.release"), 1);
    assert_eq!(event_count(&setup.events, "capability.enable"), 1);
}

#[test]
fn natural_process_exit_is_sanitized_and_receipted_after_exactly_once_cleanup() {
    for classification in [
        ProcessExitClassification::Success,
        ProcessExitClassification::Failure,
        ProcessExitClassification::Signaled,
    ] {
        let setup = setup(
            true,
            |_| {},
            AppendBehavior::Hold,
            PlatformBehavior::default(),
            SUPERVISOR_TIMEOUT,
        );
        let session = complete_launch(&setup);
        let (controller_input, relay_receiver, relay_worker) = begin_session_relay(session);
        setup.broker.wait_for_session_request();

        setup
            .platform
            .send_running_event(RunningAgentEvent::ProcessExited(classification));
        setup.broker.wait_for_session_receipt(0);
        let receipt_bytes = setup.broker.session_receipt_bytes(0);
        let receipt = SignedReceipt::parse_canonical(&receipt_bytes)
            .expect("process exit Disposal receipt is canonical");
        assert_eq!(receipt.payload.resulting_state, SessionState::Terminal);
        assert_eq!(
            receipt.payload.outcome,
            ReceiptOutcome::Disposal {
                authority: ReceiptAuthority::ProcessExited { classification },
            }
        );
        let authority = serde_json::to_value(&receipt.payload.outcome)
            .expect("receipt outcome serializes")["authority"]
            .clone();
        let classification_name = match classification {
            ProcessExitClassification::Success => "success",
            ProcessExitClassification::Failure => "failure",
            ProcessExitClassification::Signaled => "signaled",
        };
        assert_eq!(
            authority,
            serde_json::json!({
                "kind": "process_exited",
                "classification": classification_name,
            }),
            "terminal receipt exposes no raw exit code, signal, or status"
        );

        let events = event_snapshot(&setup.events);
        let observed = events
            .iter()
            .rposition(|event| event.starts_with("agent.event.process_exited"))
            .expect("sanitized process exit reaches the owner");
        let close = events
            .iter()
            .rposition(|event| event == "capability.close")
            .expect("process exit closes capabilities");
        let dispose = events
            .iter()
            .rposition(|event| event == "agent.dispose")
            .expect("process exit proves the tree empty");
        let release = events
            .iter()
            .rposition(|event| event == "identity.release")
            .expect("process exit releases identity");
        let sign = events
            .iter()
            .rposition(|event| event == "signer.sign")
            .expect("process exit signs after cleanup");
        assert!(observed < close && close < dispose && dispose < release && release < sign);

        let status = request_supervisor_status(&setup, "process-exit-status");
        assert_eq!(status.state, SessionState::Terminal);
        assert_eq!(status.channel_state, ChannelState::Closed);
        assert_eq!(status.process_exit, Some(classification));
        assert_eq!(event_count(&setup.events, "agent.dispose"), 1);
        assert_eq!(event_count(&setup.events, "identity.release"), 1);
        setup.broker.wait_for_session_request();
        setup
            .broker
            .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
                setup.broker.session_receipt_acknowledgement(0),
            ));

        finish_terminal_session_relay(controller_input, relay_receiver, relay_worker);
    }
}

#[test]
fn authorized_disposal_wins_a_process_exit_race_exactly_once() {
    for event in [
        RunningAgentEvent::ProcessExited(ProcessExitClassification::Failure),
        RunningAgentEvent::RelayFailed,
    ] {
        authorized_disposal_wins_terminal_race(event);
    }
}

fn authorized_disposal_wins_terminal_race(event: RunningAgentEvent) {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let session = complete_launch(&setup);
    let (controller_input, relay_receiver, relay_worker) = begin_session_relay(session);
    setup.broker.wait_for_session_request();
    let disposal = disposal_request(&setup, "racing-disposal");
    setup.signer.hold_on_call(setup.signer.payloads().len());

    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(disposal.clone()));
    setup.signer.wait_for_held_call();
    setup.platform.send_running_event(event);
    setup.signer.release_held_call();
    setup.broker.wait_for_session_receipt(0);

    let receipt_bytes = setup.broker.session_receipt_bytes(0);
    let receipt = SignedReceipt::parse_canonical(&receipt_bytes)
        .expect("winning Disposal receipt is canonical");
    assert!(matches!(
        receipt.payload.outcome,
        ReceiptOutcome::Disposal {
            authority: ReceiptAuthority::Authorized(_),
        }
    ));
    assert_eq!(setup.broker.session_receipt_count(), 1);
    assert_eq!(setup.signer.payloads().len(), 3);
    assert_eq!(event_count(&setup.events, "agent.dispose"), 1);
    assert_eq!(event_count(&setup.events, "identity.release"), 1);
    let status = request_supervisor_status(&setup, "racing-disposal-status");
    assert_eq!(status.state, SessionState::Terminal);
    assert_eq!(status.process_exit, None);

    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(0),
        ));
    setup
        .broker
        .wait_for_session_response(&disposal.request_id, 0);
    assert_eq!(setup.broker.session_receipt_count(), 1);
    assert_eq!(event_count(&setup.events, "agent.dispose"), 1);
    assert_eq!(event_count(&setup.events, "identity.release"), 1);

    finish_terminal_session_relay(controller_input, relay_receiver, relay_worker);
}

#[test]
fn process_exit_waits_for_an_interrupt_signature_and_preserves_both_receipts() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let session = complete_launch(&setup);
    let (controller_input, relay_receiver, relay_worker) = begin_session_relay(session);
    setup.broker.wait_for_session_request();
    let interrupt = interrupt_request(
        &setup,
        "interrupt-before-exit-during-signing",
        SessionState::Running,
        1,
    );
    setup.signer.hold_on_call(setup.signer.payloads().len());
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(interrupt.clone()));
    setup.signer.wait_for_held_call();

    setup
        .platform
        .send_running_event(RunningAgentEvent::ProcessExited(
            ProcessExitClassification::Failure,
        ));
    let _ = request_supervisor_status(&setup, "exit-observed-during-interrupt-signing");
    setup.signer.release_held_call();
    setup.broker.wait_for_session_receipt(0);
    let interrupt_bytes = setup.broker.session_receipt_bytes(0);
    let first = SignedReceipt::parse_canonical(&interrupt_bytes)
        .expect("first serialized race receipt is canonical");
    let interrupt_won = matches!(first.payload.outcome, ReceiptOutcome::Interrupt { .. });
    if !interrupt_won {
        setup.broker.wait_for_session_request();
        setup
            .broker
            .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
                setup.broker.session_receipt_acknowledgement(0),
            ));
        finish_terminal_session_relay(controller_input, relay_receiver, relay_worker);
        assert!(
            interrupt_won,
            "process exit replaced an Interrupt whose mechanic had already run"
        );
        return;
    }

    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(0),
        ));
    setup
        .broker
        .wait_for_session_response(&interrupt.request_id, 0);
    setup.broker.wait_for_session_receipt(1);
    let terminal = SignedReceipt::parse_canonical(&setup.broker.session_receipt_bytes(1))
        .expect("queued process-exit receipt is canonical");
    assert_eq!(
        terminal.payload.outcome,
        ReceiptOutcome::Disposal {
            authority: ReceiptAuthority::ProcessExited {
                classification: ProcessExitClassification::Failure,
            },
        }
    );
    assert_eq!(
        terminal.payload.previous_receipt_digest,
        Some(Digest::of(&interrupt_bytes).to_string())
    );
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(1),
        ));
    finish_terminal_session_relay(controller_input, relay_receiver, relay_worker);
}

#[test]
fn failed_interrupt_signature_cannot_strand_a_queued_process_exit() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let session = complete_launch(&setup);
    let initial_head = ReceiptHead {
        sequence: session.receipt().payload.sequence,
        digest: Digest::of(&session.receipt().canonical_bytes()).to_string(),
    };
    let (controller_input, relay_receiver, relay_worker) = begin_session_relay(session);
    setup.broker.wait_for_session_request();
    let interrupt = interrupt_request(
        &setup,
        "failed-interrupt-before-process-exit",
        SessionState::Running,
        initial_head.sequence,
    );
    let interrupt_sign_call = setup.signer.payloads().len();
    setup.signer.fail_on_call(interrupt_sign_call);
    setup.signer.hold_on_call(interrupt_sign_call);
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(interrupt.clone()));
    setup.signer.wait_for_held_call();

    setup
        .platform
        .send_running_event(RunningAgentEvent::ProcessExited(
            ProcessExitClassification::Failure,
        ));
    let terminal = request_supervisor_status(&setup, "exit-queued-behind-failing-interrupt");
    assert_eq!(terminal.state, SessionState::Terminal);
    assert_eq!(
        terminal.process_exit,
        Some(ProcessExitClassification::Failure)
    );
    setup.signer.release_held_call();
    let failed = setup
        .broker
        .wait_for_session_response(&interrupt.request_id, 0);
    let ResponseResult::Error { error } = failed.result else {
        panic!("failed Interrupt returns its typed signing error");
    };
    assert_eq!(error.code, ErrorCode::SigningUnavailable);
    setup.signer.fail_on_call(usize::MAX);
    let awaiting_repair = request_supervisor_status(&setup, "terminal-exit-awaiting-audit-repair");
    assert_eq!(awaiting_repair.pending_receipt_count, 2);

    setup.broker.wait_for_session_request();
    setup.broker.disconnect_session();
    setup.broker.wait_for_reconnect(0);
    let reconnect = setup.broker.reconnect(0);
    assert_eq!(reconnect.sequence, initial_head.sequence);
    assert_eq!(reconnect.receipt_digest, initial_head.digest);
    setup.broker.complete_reconnect(reconnect);

    setup.broker.wait_for_session_receipt(0);
    let interrupt_bytes = setup.broker.session_receipt_bytes(0);
    let interrupt_receipt =
        SignedReceipt::parse_canonical(&interrupt_bytes).expect("deferred Interrupt is canonical");
    assert!(matches!(
        interrupt_receipt.payload.outcome,
        ReceiptOutcome::Interrupt { .. }
    ));
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(0),
        ));

    setup.broker.wait_for_session_receipt(1);
    let exit_receipt = SignedReceipt::parse_canonical(&setup.broker.session_receipt_bytes(1))
        .expect("queued process-exit Disposal is canonical");
    assert_eq!(
        exit_receipt.payload.outcome,
        ReceiptOutcome::Disposal {
            authority: ReceiptAuthority::ProcessExited {
                classification: ProcessExitClassification::Failure,
            },
        }
    );
    assert_eq!(
        exit_receipt.payload.previous_receipt_digest,
        Some(Digest::of(&interrupt_bytes).to_string())
    );
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(1),
        ));
    assert_eq!(
        relay_receiver
            .recv_timeout(CALLBACK_TIMEOUT)
            .expect("durable queued process exit completes its owner")
            .expect("terminal cleanup succeeds"),
        1,
    );
    drop(controller_input);
    relay_worker.join().expect("terminal relay worker finishes");
}

#[test]
fn process_exit_waits_for_an_interrupt_ack_and_preserves_both_receipts() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let session = complete_launch(&setup);
    let (controller_input, relay_receiver, relay_worker) = begin_session_relay(session);
    setup.broker.wait_for_session_request();
    let interrupt = interrupt_request(
        &setup,
        "interrupt-before-exit-during-ack",
        SessionState::Running,
        1,
    );
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(interrupt.clone()));
    setup.broker.wait_for_session_receipt(0);
    let interrupt_bytes = setup.broker.session_receipt_bytes(0);
    let first = SignedReceipt::parse_canonical(&interrupt_bytes)
        .expect("pending Interrupt receipt is canonical");
    assert!(matches!(
        first.payload.outcome,
        ReceiptOutcome::Interrupt { .. }
    ));
    let sign_calls_before_exit = setup.signer.payloads().len();

    setup
        .platform
        .send_running_event(RunningAgentEvent::ProcessExited(
            ProcessExitClassification::Signaled,
        ));
    let _ = request_supervisor_status(&setup, "exit-observed-during-interrupt-ack");
    let sign_calls_before_interrupt_ack = setup.signer.payloads().len();
    if sign_calls_before_interrupt_ack != sign_calls_before_exit {
        setup.broker.wait_for_session_receipt(1);
        setup.broker.wait_for_session_request();
        setup
            .broker
            .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
                setup.broker.session_receipt_acknowledgement(1),
            ));
        finish_terminal_session_relay(controller_input, relay_receiver, relay_worker);
        assert_eq!(
            sign_calls_before_interrupt_ack, sign_calls_before_exit,
            "process exit started a second receipt while the Interrupt still awaited its ACK"
        );
        return;
    }

    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(0),
        ));
    setup
        .broker
        .wait_for_session_response(&interrupt.request_id, 0);
    setup.broker.wait_for_session_receipt(1);
    let terminal = SignedReceipt::parse_canonical(&setup.broker.session_receipt_bytes(1))
        .expect("serialized process-exit receipt is canonical");
    assert_eq!(
        terminal.payload.outcome,
        ReceiptOutcome::Disposal {
            authority: ReceiptAuthority::ProcessExited {
                classification: ProcessExitClassification::Signaled,
            },
        }
    );
    assert_eq!(
        terminal.payload.previous_receipt_digest,
        Some(Digest::of(&interrupt_bytes).to_string())
    );
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(1),
        ));
    finish_terminal_session_relay(controller_input, relay_receiver, relay_worker);
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "One authenticated resume thaws once and enables only after its exact receipt ack scenario keeps its causal steps and assertions together."
)]
fn authenticated_resume_thaws_once_and_enables_only_after_its_exact_receipt_ack() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let session = complete_launch(&setup);
    let (controller_input, relay_receiver, relay_worker, park_receipt) =
        park_launched_session(&setup, session);
    let park_head = ReceiptHead {
        sequence: park_receipt.payload.sequence,
        digest: Digest::of(&park_receipt.canonical_bytes()).to_string(),
    };
    let resume_sign_call = setup.signer.payloads().len();
    setup.signer.hold_on_call(resume_sign_call);
    let resume = resume_request(&setup, "resume-request-1");

    setup.broker.wait_for_session_request();
    assert_eq!(event_count(&setup.events, "agent.resume"), 0);
    assert!(lock(&setup.platform.agent).parked);
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(resume.clone()));
    setup.signer.wait_for_held_call();

    assert_eq!(
        event_count(&setup.events, "broker.consume"),
        1,
        "the authenticated lifecycle request is itself durable Resume authorization"
    );
    assert_eq!(event_count(&setup.events, "agent.resume"), 1);
    assert!(lock(&setup.platform.agent).resumed);
    assert!(!lock(&setup.platform.agent).parked);
    assert_eq!(
        setup.broker.session_receipt_count(),
        1,
        "no Resume receipt exists until thaw has succeeded"
    );
    assert!(lock(&setup.platform.gate_state()).revoked);
    assert!(!lock(&setup.platform.gate_state()).enabled);
    assert_eq!(event_count(&setup.events, "capability.enable"), 1);
    assert_eq!(
        setup.broker.session_response_count_for(&resume.request_id),
        0
    );

    let events = event_snapshot(&setup.events);
    let thaw = events
        .iter()
        .rposition(|event| event == "agent.resume")
        .expect("Resume thaws the process tree");
    let sign = events
        .iter()
        .rposition(|event| event == "signer.sign")
        .expect("Resume starts signing after thaw");
    assert!(thaw < sign);

    setup.signer.release_held_call();
    setup.broker.wait_for_session_receipt(1);
    let resume_bytes = setup.broker.session_receipt_bytes(1);
    let resume_receipt =
        SignedReceipt::parse_canonical(&resume_bytes).expect("Resume receipt is canonical");
    let resume_head = ReceiptHead {
        sequence: resume_receipt.payload.sequence,
        digest: Digest::of(&resume_bytes).to_string(),
    };
    assert_eq!(resume_receipt.payload.sequence, 3);
    assert_eq!(
        resume_receipt.payload.previous_receipt_digest.as_deref(),
        Some(park_head.digest.as_str()),
    );
    assert_eq!(
        resume_receipt.payload.resulting_state,
        SessionState::Running
    );
    let ReceiptOutcome::Resume { authorization } = &resume_receipt.payload.outcome else {
        panic!("only a successful thaw produces a Resume receipt");
    };
    assert_eq!(authorization.authorization_id, resume.authorization_id);
    assert_eq!(authorization.request_id, resume.request_id);
    assert_eq!(authorization.request_digest, resume.digest().to_string());
    assert!(lock(&setup.platform.gate_state()).revoked);
    assert!(!lock(&setup.platform.gate_state()).enabled);
    assert_eq!(event_count(&setup.events, "capability.enable"), 1);

    let status = request_supervisor_status(&setup, "status-while-resume-awaits-ack");
    assert_eq!(status.state, SessionState::Running);
    assert_eq!(status.channel_state, ChannelState::Revoked);
    assert_eq!(status.launcher_head.as_ref(), Some(&resume_head));
    assert_eq!(status.broker_head.as_ref(), Some(&park_head));
    assert_eq!(status.pending_receipt_count, 1);
    let pending = status
        .pending_operation
        .as_ref()
        .expect("Resume completion waits for durable ACK");
    assert_eq!(pending.request_id, resume.request_id);
    assert_eq!(pending.action, PendingAction::Resume);
    assert_eq!(pending.phase, PendingPhase::AwaitingDurableAck);

    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(1),
        ));
    let response = setup
        .broker
        .wait_for_session_response(&resume.request_id, 0);
    let ResponseResult::Receipt { receipt } = response.result else {
        panic!("exact durable ACK completes Resume");
    };
    assert_eq!(receipt.canonical_bytes(), resume_bytes);
    assert_eq!(event_count(&setup.events, "agent.resume"), 1);
    assert_eq!(event_count(&setup.events, "capability.enable"), 2);
    assert!(lock(&setup.platform.gate_state()).enabled);
    assert!(!lock(&setup.platform.gate_state()).revoked);

    finish_session_relay(&setup, controller_input, relay_receiver, relay_worker);
}

#[derive(Clone, Copy, Debug)]
enum ResumeFailure {
    Thaw,
    Sign,
    Send,
    InvalidAck,
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "One resume failures never enable and audit post thaw failure before reparking scenario keeps its causal steps and assertions together."
)]
fn resume_failures_never_enable_and_audit_post_thaw_failure_before_reparking() {
    for failure in [
        ResumeFailure::Thaw,
        ResumeFailure::Sign,
        ResumeFailure::Send,
        ResumeFailure::InvalidAck,
    ] {
        let setup = setup(
            true,
            |_| {},
            AppendBehavior::Hold,
            PlatformBehavior {
                resume_fails: matches!(failure, ResumeFailure::Thaw),
                ..PlatformBehavior::default()
            },
            SUPERVISOR_TIMEOUT,
        );
        let session = complete_launch(&setup);
        let (controller_input, relay_receiver, relay_worker, park_receipt) =
            park_launched_session(&setup, session);
        let park_head = ReceiptHead {
            sequence: park_receipt.payload.sequence,
            digest: Digest::of(&park_receipt.canonical_bytes()).to_string(),
        };
        let resume_sign_call = setup.signer.payloads().len();
        if matches!(failure, ResumeFailure::Sign) {
            setup.signer.fail_on_call(resume_sign_call);
            setup.signer.hold_on_call(resume_sign_call);
        }
        if matches!(failure, ResumeFailure::Send) {
            setup
                .broker
                .set_session_receipt_send_behavior(SessionReceiptSendBehavior::Reject);
        }
        let resume = resume_request(&setup, "failing-resume");
        setup.broker.wait_for_session_request();
        setup
            .broker
            .deliver_session_request(ProtocolMessage::Lifecycle(resume.clone()));

        match failure {
            ResumeFailure::Thaw => {}
            ResumeFailure::Sign => {
                setup.signer.wait_for_held_call();
                assert_eq!(
                    setup.broker.session_receipt_count(),
                    1,
                    "a failed signature cannot create a Resume receipt"
                );
                setup.signer.release_held_call();
            }
            ResumeFailure::Send => setup.broker.wait_for_session_receipt(1),
            ResumeFailure::InvalidAck => {
                setup.broker.wait_for_session_receipt(1);
                let mut acknowledgement = setup.broker.session_receipt_acknowledgement(1);
                acknowledgement.receipt_digest = Digest::of(b"wrong-resume-receipt").to_string();
                setup.broker.wait_for_session_request();
                setup
                    .broker
                    .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
                        acknowledgement,
                    ));
            }
        }

        let response = setup
            .broker
            .wait_for_session_response(&resume.request_id, 0);
        let ResponseResult::Error { error } = response.result else {
            panic!("{failure:?}: failed Resume returns a typed error");
        };
        let expected_error = match failure {
            ResumeFailure::Thaw => ErrorCode::LifecycleMechanicUnavailable,
            ResumeFailure::Sign => ErrorCode::SigningUnavailable,
            ResumeFailure::Send => ErrorCode::DurabilityUnavailable,
            ResumeFailure::InvalidAck => ErrorCode::ReceiptChainInvalid,
        };
        assert_eq!(error.code, expected_error, "case {failure:?}");

        let status = request_supervisor_status(&setup, &format!("{failure:?}-status"));
        assert_eq!(status.state, SessionState::Parked, "case {failure:?}");
        assert_eq!(status.channel_state, ChannelState::Revoked);
        assert_eq!(status.broker_head.as_ref(), Some(&park_head));
        assert_eq!(event_count(&setup.events, "agent.resume"), 1);
        assert!(lock(&setup.platform.agent).parked);
        assert!(!lock(&setup.platform.gate_state()).enabled);
        assert_eq!(event_count(&setup.events, "capability.enable"), 1);

        if matches!(failure, ResumeFailure::Thaw) {
            assert_eq!(setup.broker.session_receipt_count(), 1);
            assert_eq!(setup.signer.payloads().len(), 3);
            assert_eq!(status.launcher_head.as_ref(), Some(&park_head));
            assert_eq!(status.pending_receipt_count, 0);
            assert_eq!(event_count(&setup.events, "agent.park"), 1);
            assert_eq!(event_count(&setup.events, "capability.revoke"), 1);
        } else {
            assert_eq!(
                status.pending_receipt_count, 2,
                "{failure:?}: Resume truth plus Park(AcknowledgementFailed) remain queued"
            );
            assert_eq!(event_count(&setup.events, "agent.park"), 2);
            assert_eq!(event_count(&setup.events, "capability.revoke"), 2);
            let events = event_snapshot(&setup.events);
            let thaw = events
                .iter()
                .rposition(|event| event == "agent.resume")
                .expect("Resume thaw was attempted");
            let revoke = events
                .iter()
                .rposition(|event| event == "capability.revoke")
                .expect("failed Resume revokes channels again");
            let repark = events
                .iter()
                .rposition(|event| event == "agent.park")
                .expect("failed Resume re-Parks the tree");
            assert!(thaw < revoke && revoke < repark, "case {failure:?}");
        }

        if matches!(failure, ResumeFailure::Thaw) {
            finish_session_relay(&setup, controller_input, relay_receiver, relay_worker);
        } else {
            finish_session_relay_after_reconciling_backlog(
                &setup,
                controller_input,
                relay_receiver,
                relay_worker,
                2,
            );
        }
    }
}

#[test]
fn exact_retry_after_failed_resume_replays_error_without_thawing_again() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior {
            resume_fails: true,
            ..PlatformBehavior::default()
        },
        SUPERVISOR_TIMEOUT,
    );
    let session = complete_launch(&setup);
    let (controller_input, relay_receiver, relay_worker, _) =
        park_launched_session(&setup, session);
    let resume = resume_request(&setup, "retry-failed-resume");

    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(resume.clone()));
    let first = setup
        .broker
        .wait_for_session_response(&resume.request_id, 0);
    let ResponseResult::Error { error } = &first.result else {
        panic!("failed Resume returns a typed error");
    };
    assert_eq!(error.code, ErrorCode::LifecycleMechanicUnavailable);
    assert_eq!(event_count(&setup.events, "agent.resume"), 1);

    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(resume.clone()));
    let replay = setup
        .broker
        .wait_for_session_response(&resume.request_id, 1);

    assert_eq!(replay.canonical_bytes(), first.canonical_bytes());
    assert_eq!(
        event_count(&setup.events, "agent.resume"),
        1,
        "an exact failed-request retry replays its error without another thaw"
    );
    assert_eq!(
        setup.broker.session_receipt_count(),
        1,
        "a failed thaw and its retry emit no Resume receipt"
    );

    finish_session_relay(&setup, controller_input, relay_receiver, relay_worker);
}

#[test]
fn rejected_exact_resume_ack_reparks_and_replays_failure_without_another_thaw() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let session = complete_launch(&setup);
    let (controller_input, relay_receiver, relay_worker, park_receipt) =
        park_launched_session(&setup, session);
    let park_head = ReceiptHead {
        sequence: park_receipt.payload.sequence,
        digest: Digest::of(&park_receipt.canonical_bytes()).to_string(),
    };
    let resume = resume_request(&setup, "rejected-ack-resume");

    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(resume.clone()));
    setup.broker.wait_for_session_receipt(1);
    let resume_head = ReceiptHead {
        sequence: 3,
        digest: Digest::of(&setup.broker.session_receipt_bytes(1)).to_string(),
    };
    let mut rejection = setup.broker.session_receipt_acknowledgement(1);
    rejection.disposition = ReceiptDisposition::Rejected;
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(rejection));

    let failed = setup
        .broker
        .wait_for_session_response(&resume.request_id, 0);
    let ResponseResult::Error { error } = &failed.result else {
        panic!("negative receipt acknowledgement fails Resume");
    };
    assert_eq!(error.code, ErrorCode::DurabilityUnavailable);
    let status = request_supervisor_status(&setup, "status-after-rejected-resume-ack");
    assert_eq!(status.state, SessionState::Parked);
    assert_eq!(status.channel_state, ChannelState::Revoked);
    assert_eq!(status.launcher_head.as_ref(), Some(&resume_head));
    assert_eq!(status.broker_head.as_ref(), Some(&park_head));
    assert_eq!(status.pending_receipt_count, 2);
    assert_eq!(event_count(&setup.events, "agent.resume"), 1);
    assert_eq!(event_count(&setup.events, "agent.park"), 2);
    assert_eq!(event_count(&setup.events, "capability.revoke"), 2);
    assert_eq!(event_count(&setup.events, "capability.enable"), 1);
    assert!(lock(&setup.platform.agent).parked);
    assert!(!lock(&setup.platform.gate_state()).enabled);

    let effects_before_retry = lifecycle_effect_counts(&setup.events);
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(resume.clone()));
    let replay = setup
        .broker
        .wait_for_session_response(&resume.request_id, 1);
    assert_eq!(replay.canonical_bytes(), failed.canonical_bytes());
    assert_eq!(lifecycle_effect_counts(&setup.events), effects_before_retry);
    assert_eq!(event_count(&setup.events, "agent.resume"), 1);
    assert_eq!(event_count(&setup.events, "capability.enable"), 1);
    assert_eq!(setup.broker.session_receipt_count(), 2);

    finish_session_relay_after_reconciling_backlog(
        &setup,
        controller_input,
        relay_receiver,
        relay_worker,
        2,
    );
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "One reconnect drains rejected resume truth before its causal park once scenario keeps its causal steps and assertions together."
)]
fn reconnect_drains_rejected_resume_truth_before_its_causal_park_once() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let timer = Arc::new(FakeTimer::new(Arc::clone(&setup.events)));
    let timer_port: Arc<dyn SupervisorTimer> = timer.clone();
    let supervisor = setup.fresh_supervisor_with_timer(timer_port);
    let session = complete_launch_on(&setup, &supervisor);
    let (controller_input, relay_receiver, relay_worker, park_receipt) =
        park_launched_session(&setup, session);
    let park_head = ReceiptHead {
        sequence: park_receipt.payload.sequence,
        digest: Digest::of(&park_receipt.canonical_bytes()).to_string(),
    };
    let resume = resume_request(&setup, "rejected-resume-before-reconnect");

    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(resume.clone()));
    setup.broker.wait_for_session_receipt(1);
    let resume_bytes = setup.broker.session_receipt_bytes(1);
    let resume_receipt = SignedReceipt::parse_canonical(&resume_bytes)
        .expect("truthful Resume receipt is canonical");
    let mut rejection = setup.broker.session_receipt_acknowledgement(1);
    rejection.disposition = ReceiptDisposition::Rejected;
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(rejection));
    let failed = setup
        .broker
        .wait_for_session_response(&resume.request_id, 0);
    let ResponseResult::Error { error } = &failed.result else {
        panic!("rejected Resume acknowledgement returns a typed error");
    };
    assert_eq!(error.code, ErrorCode::DurabilityUnavailable);
    let deferred = request_supervisor_status(&setup, "status-before-resume-backlog-repair");
    assert_eq!(deferred.state, SessionState::Parked);
    assert_eq!(deferred.channel_state, ChannelState::Revoked);
    assert_eq!(deferred.broker_head.as_ref(), Some(&park_head));
    assert_eq!(deferred.pending_receipt_count, 2);
    assert_eq!(event_count(&setup.events, "agent.resume"), 1);
    assert_eq!(event_count(&setup.events, "agent.park"), 2);
    assert_eq!(event_count(&setup.events, "capability.enable"), 1);

    let signs_before_reconnect = setup.signer.payloads().len();
    setup.broker.wait_for_session_request();
    setup.broker.disconnect_session();
    setup.broker.wait_for_reconnect(0);
    let mut reconnect = setup.broker.reconnect(0);
    reconnect.sequence = park_head.sequence;
    reconnect.receipt_digest = park_head.digest.clone();
    setup.broker.complete_reconnect(reconnect);

    setup.broker.wait_for_session_receipt(2);
    assert_eq!(setup.broker.session_receipt_bytes(2), resume_bytes);
    assert_eq!(
        setup.signer.payloads().len(),
        signs_before_reconnect,
        "the signed Resume suffix is replayed without signing again"
    );
    let repairing = request_supervisor_status(&setup, "status-replaying-resume-truth");
    assert_eq!(repairing.state, SessionState::Parked);
    assert_eq!(repairing.broker_connection, BrokerConnection::Reconciling);
    assert_eq!(repairing.channel_state, ChannelState::Revoked);
    assert_eq!(repairing.pending_receipt_count, 2);

    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(2),
        ));
    setup.broker.wait_for_session_receipt(3);
    let causal_park_bytes = setup.broker.session_receipt_bytes(3);
    let causal_park = SignedReceipt::parse_canonical(&causal_park_bytes)
        .expect("causal Park receipt is canonical");
    assert_eq!(
        causal_park.payload.sequence,
        resume_receipt.payload.sequence + 1
    );
    assert_eq!(
        causal_park.payload.previous_receipt_digest,
        Some(Digest::of(&resume_bytes).to_string())
    );
    assert_eq!(
        causal_park.payload.outcome,
        ReceiptOutcome::Park {
            authority: ReceiptAuthority::Cause {
                cause: ReceiptCause::AcknowledgementFailed,
            },
        }
    );
    assert_eq!(causal_park.payload.resulting_state, SessionState::Parked);
    assert_eq!(setup.signer.payloads().len(), signs_before_reconnect + 1);
    assert_eq!(event_count(&setup.events, "agent.resume"), 1);
    assert_eq!(event_count(&setup.events, "agent.park"), 2);
    assert_eq!(event_count(&setup.events, "capability.enable"), 1);

    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(3),
        ));
    let repaired = request_supervisor_status(&setup, "status-after-resume-backlog-repair");
    let repaired_head = ReceiptHead {
        sequence: causal_park.payload.sequence,
        digest: Digest::of(&causal_park_bytes).to_string(),
    };
    assert_eq!(repaired.state, SessionState::Parked);
    assert_eq!(repaired.broker_connection, BrokerConnection::Connected);
    assert_eq!(repaired.channel_state, ChannelState::Revoked);
    assert_eq!(repaired.launcher_head.as_ref(), Some(&repaired_head));
    assert_eq!(repaired.broker_head.as_ref(), Some(&repaired_head));
    assert_eq!(repaired.pending_receipt_count, 0);
    assert_eq!(event_count(&setup.events, "capability.enable"), 1);

    let scheduled = timer.scheduled_count();
    for index in 0..scheduled {
        timer.fire(index);
    }
    let after_late_timers = request_supervisor_status(&setup, "status-after-resume-repair-timers");
    assert_eq!(after_late_timers.state, SessionState::Parked);
    assert_eq!(
        after_late_timers.launcher_head.as_ref(),
        Some(&repaired_head)
    );
    assert_eq!(after_late_timers.broker_head.as_ref(), Some(&repaired_head));
    assert_eq!(event_count(&setup.events, "capability.enable"), 1);

    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(resume.clone()));
    let replay = setup
        .broker
        .wait_for_session_response(&resume.request_id, 1);
    assert_eq!(replay.canonical_bytes(), failed.canonical_bytes());
    assert_eq!(event_count(&setup.events, "agent.resume"), 1);
    assert_eq!(event_count(&setup.events, "agent.park"), 2);
    assert_eq!(setup.signer.payloads().len(), signs_before_reconnect + 1);
    assert_eq!(setup.broker.session_receipt_count(), 4);

    finish_session_relay(&setup, controller_input, relay_receiver, relay_worker);
}

#[test]
fn timed_out_held_resume_signature_is_inert_when_released_late() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let timer = Arc::new(FakeTimer::new(Arc::clone(&setup.events)));
    let timer_port: Arc<dyn SupervisorTimer> = timer.clone();
    let supervisor = setup.fresh_supervisor_with_timer(timer_port);
    let session = complete_launch_on(&setup, &supervisor);
    let (controller_input, relay_receiver, relay_worker, _) =
        park_launched_session(&setup, session);
    assert_eq!(timer.wait_for_schedule(0), SUPERVISOR_TIMEOUT);

    let resume_sign_call = setup.signer.payloads().len();
    setup.signer.hold_on_call(resume_sign_call);
    let resume = resume_request(&setup, "timed-out-resume-signature");
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(resume.clone()));
    setup.signer.wait_for_held_call();
    assert_eq!(timer.wait_for_schedule(1), SUPERVISOR_TIMEOUT);
    assert_eq!(event_count(&setup.events, "agent.resume"), 1);
    assert_eq!(setup.broker.session_receipt_count(), 1);
    assert_eq!(event_count(&setup.events, "capability.enable"), 1);

    timer.fire(1);
    let response = setup
        .broker
        .wait_for_session_response(&resume.request_id, 0);
    let ResponseResult::Error { error } = response.result else {
        panic!("Resume signature timeout returns a typed failure");
    };
    assert_eq!(error.code, ErrorCode::DurabilityUnavailable);
    let before_late_signature =
        request_supervisor_status(&setup, "status-before-late-resume-signature");
    assert_eq!(before_late_signature.state, SessionState::Parked);
    assert_eq!(before_late_signature.channel_state, ChannelState::Revoked);
    assert_eq!(before_late_signature.pending_receipt_count, 2);
    assert!(lock(&setup.platform.agent).parked);
    assert!(!lock(&setup.platform.gate_state()).enabled);
    assert_eq!(event_count(&setup.events, "agent.park"), 2);
    assert_eq!(event_count(&setup.events, "capability.revoke"), 2);

    let effects_before_release = lifecycle_effect_counts(&setup.events);
    let resume_calls_before_release = event_count(&setup.events, "agent.resume");
    let enables_before_release = event_count(&setup.events, "capability.enable");
    let receipts_before_release = setup.broker.session_receipt_count();
    let responses_before_release = setup.broker.session_response_count_for(&resume.request_id);
    setup.signer.release_held_call();
    let after_late_signature =
        request_supervisor_status(&setup, "status-after-late-resume-signature");

    assert_eq!(after_late_signature, before_late_signature);
    assert_eq!(
        lifecycle_effect_counts(&setup.events),
        effects_before_release
    );
    assert_eq!(
        event_count(&setup.events, "agent.resume"),
        resume_calls_before_release
    );
    assert_eq!(
        event_count(&setup.events, "capability.enable"),
        enables_before_release
    );
    assert_eq!(
        setup.broker.session_receipt_count(),
        receipts_before_release
    );
    assert_eq!(
        setup.broker.session_response_count_for(&resume.request_id),
        responses_before_release
    );

    finish_session_relay_after_reconciling_backlog(
        &setup,
        controller_input,
        relay_receiver,
        relay_worker,
        2,
    );
}

#[test]
fn lost_resume_ack_reparks_and_its_late_exact_ack_is_inert() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let timer = Arc::new(FakeTimer::new(Arc::clone(&setup.events)));
    let timer_port: Arc<dyn SupervisorTimer> = timer.clone();
    let supervisor = setup.fresh_supervisor_with_timer(timer_port);
    let session = complete_launch_on(&setup, &supervisor);
    let (controller_input, relay_receiver, relay_worker, park_receipt) =
        park_launched_session(&setup, session);
    let park_head = ReceiptHead {
        sequence: park_receipt.payload.sequence,
        digest: Digest::of(&park_receipt.canonical_bytes()).to_string(),
    };
    assert_eq!(timer.wait_for_schedule(0), SUPERVISOR_TIMEOUT);

    let resume = resume_request(&setup, "lost-ack-resume");
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(resume.clone()));
    setup.broker.wait_for_session_receipt(1);
    let resume_head = ReceiptHead {
        sequence: 3,
        digest: Digest::of(&setup.broker.session_receipt_bytes(1)).to_string(),
    };
    assert_eq!(timer.wait_for_schedule(1), SUPERVISOR_TIMEOUT);
    assert_eq!(event_count(&setup.events, "capability.enable"), 1);

    timer.fire(1);
    let response = setup
        .broker
        .wait_for_session_response(&resume.request_id, 0);
    let ResponseResult::Error { error } = response.result else {
        panic!("lost Resume ACK fails the pending request");
    };
    assert_eq!(error.code, ErrorCode::DurabilityUnavailable);
    let before_late_ack = request_supervisor_status(&setup, "status-after-lost-resume-ack");
    assert_eq!(before_late_ack.state, SessionState::Parked);
    assert_eq!(before_late_ack.channel_state, ChannelState::Revoked);
    assert_eq!(before_late_ack.launcher_head.as_ref(), Some(&resume_head));
    assert_eq!(before_late_ack.broker_head.as_ref(), Some(&park_head));
    assert_eq!(before_late_ack.pending_receipt_count, 2);
    assert!(lock(&setup.platform.agent).parked);
    assert!(!lock(&setup.platform.gate_state()).enabled);

    let effects_before_late_ack = lifecycle_effect_counts(&setup.events);
    let enables_before_late_ack = event_count(&setup.events, "capability.enable");
    let responses_before_late_ack = setup.broker.session_response_count_for(&resume.request_id);
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(1),
        ));
    let after_late_ack = request_supervisor_status(&setup, "status-after-late-resume-ack");
    assert_eq!(after_late_ack, before_late_ack);
    assert_eq!(
        lifecycle_effect_counts(&setup.events),
        effects_before_late_ack
    );
    assert_eq!(
        event_count(&setup.events, "capability.enable"),
        enables_before_late_ack
    );
    assert_eq!(
        setup.broker.session_response_count_for(&resume.request_id),
        responses_before_late_ack
    );

    finish_session_relay_after_reconciling_backlog(
        &setup,
        controller_input,
        relay_receiver,
        relay_worker,
        2,
    );
}

#[test]
fn equal_head_reconnect_within_one_signed_grace_restores_running_once() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let timer = Arc::new(FakeTimer::new(Arc::clone(&setup.events)));
    let timer_port: Arc<dyn SupervisorTimer> = timer.clone();
    let supervisor = setup.fresh_supervisor_with_timer(timer_port);
    let session = complete_launch_on(&setup, &supervisor);
    let head = ReceiptHead {
        sequence: session.receipt().payload.sequence,
        digest: Digest::of(&session.receipt().canonical_bytes()).to_string(),
    };
    let launch_receipt = SignedReceipt::parse_canonical(&setup.broker.receipt_bytes(0))
        .expect("sequence-zero receipt is canonical");
    let ReceiptOutcome::Launch { evidence, .. } = launch_receipt.payload.outcome else {
        panic!("sequence zero carries launch evidence");
    };
    assert_eq!(evidence.broker_loss_grace_ms, BROKER_LOSS_GRACE_MS);

    let (controller_input, relay_receiver, relay_worker) = begin_session_relay(session);
    setup.broker.wait_for_session_request();
    let old_status = status_request(&setup, "status-response-from-old-connection");
    setup.broker.hold_session_response(&old_status.request_id);
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Status(old_status.clone()));
    setup.broker.wait_for_held_session_response();
    setup.broker.wait_for_session_request();

    setup.broker.disconnect_session();
    setup.broker.wait_for_reconnect(0);
    assert_eq!(
        timer.wait_for_schedule(0),
        Duration::from_millis(u64::from(BROKER_LOSS_GRACE_MS))
    );
    assert!(lock(&setup.platform.gate_state()).revoked);
    assert!(!lock(&setup.platform.gate_state()).enabled);
    assert_eq!(event_count(&setup.events, "agent.park"), 0);
    let reconnect = setup.broker.reconnect(0);
    assert_eq!(reconnect.schema, BROKER_RECONNECT_SCHEMA);
    assert_eq!(reconnect.protocol_version, PROTOCOL_VERSION);
    assert_eq!(
        reconnect.session_id.as_str(),
        setup.request.session_id.as_str()
    );
    assert_eq!(reconnect.run_id.as_str(), setup.request.run_id.as_str());
    assert_eq!(reconnect.envelope_revision, setup.request.envelope_revision);
    assert_eq!(reconnect.sequence, head.sequence);
    assert_eq!(reconnect.receipt_digest.as_str(), head.digest.as_str());

    setup
        .broker
        .release_session_response_with(Err(SupervisorError::BrokerUnavailable));
    setup.broker.complete_reconnect(reconnect);
    let reconnected = request_supervisor_status(&setup, "status-after-equal-head-reconnect");
    assert_eq!(
        timer.scheduled_count(),
        2,
        "one grace deadline and one reconnect-attempt deadline are retained as stale callbacks"
    );
    assert_eq!(reconnected.state, SessionState::Running);
    assert_eq!(reconnected.broker_connection, BrokerConnection::Connected);
    assert_eq!(reconnected.channel_state, ChannelState::Enabled);
    assert_eq!(event_count(&setup.events, "agent.park"), 0);

    timer.fire(0);
    let after_stale_deadline =
        request_supervisor_status(&setup, "status-after-stale-broker-loss-deadline");
    assert_eq!(after_stale_deadline.state, SessionState::Running);
    assert_eq!(
        after_stale_deadline.broker_connection,
        BrokerConnection::Connected
    );
    assert_eq!(after_stale_deadline.channel_state, ChannelState::Enabled);
    assert_eq!(event_count(&setup.events, "agent.park"), 0);

    finish_session_relay(&setup, controller_input, relay_receiver, relay_worker);
}

#[test]
fn exact_pending_resume_head_reconnect_within_grace_enables_once() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let timer = Arc::new(FakeTimer::new(Arc::clone(&setup.events)));
    let timer_port: Arc<dyn SupervisorTimer> = timer.clone();
    let supervisor = setup.fresh_supervisor_with_timer(timer_port);
    let session = complete_launch_on(&setup, &supervisor);
    let (controller_input, relay_receiver, relay_worker, _) =
        park_launched_session(&setup, session);

    let resume = resume_request(&setup, "resume-with-ack-lost-to-broker-restart");
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(resume.clone()));
    setup.broker.wait_for_session_receipt(1);
    let resume_bytes = setup.broker.session_receipt_bytes(1);
    let resume_head = ReceiptHead {
        sequence: 3,
        digest: Digest::of(&resume_bytes).to_string(),
    };
    assert_eq!(event_count(&setup.events, "agent.resume"), 1);
    assert_eq!(event_count(&setup.events, "capability.enable"), 1);

    let grace_timer_index = timer.scheduled_count();
    setup.broker.wait_for_session_request();
    setup.broker.disconnect_session();
    setup.broker.wait_for_reconnect(0);
    assert_eq!(
        timer.wait_for_schedule(grace_timer_index),
        Duration::from_millis(u64::from(BROKER_LOSS_GRACE_MS))
    );
    let reconnect = setup.broker.reconnect(0);
    assert_eq!(reconnect.sequence, resume_head.sequence);
    assert_eq!(reconnect.receipt_digest, resume_head.digest);
    setup.broker.complete_reconnect(reconnect);

    let completed = setup
        .broker
        .wait_for_session_response(&resume.request_id, 0);
    let ResponseResult::Receipt { receipt } = completed.result else {
        panic!("the exact pending Resume head completes its original request");
    };
    assert_eq!(receipt.canonical_bytes(), resume_bytes);
    let restored = request_supervisor_status(&setup, "status-after-pending-resume-reconnect");
    assert_eq!(restored.state, SessionState::Running);
    assert_eq!(restored.broker_connection, BrokerConnection::Connected);
    assert_eq!(restored.channel_state, ChannelState::Enabled);
    assert_eq!(restored.launcher_head.as_ref(), Some(&resume_head));
    assert_eq!(restored.broker_head.as_ref(), Some(&resume_head));
    assert_eq!(restored.pending_receipt_count, 0);
    assert_eq!(event_count(&setup.events, "agent.resume"), 1);
    assert_eq!(
        event_count(&setup.events, "capability.enable"),
        2,
        "the initial launch and durable Resume each enable exactly once"
    );

    timer.fire(grace_timer_index);
    let after_stale_grace =
        request_supervisor_status(&setup, "status-after-pending-resume-stale-grace");
    assert_eq!(after_stale_grace, restored);
    assert_eq!(event_count(&setup.events, "capability.enable"), 2);

    finish_session_relay(&setup, controller_input, relay_receiver, relay_worker);
}

#[test]
fn old_receive_error_after_successful_reconnect_is_inert() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let timer = Arc::new(FakeTimer::new(Arc::clone(&setup.events)));
    let timer_port: Arc<dyn SupervisorTimer> = timer.clone();
    let supervisor = setup.fresh_supervisor_with_timer(timer_port);
    let session = complete_launch_on(&setup, &supervisor);
    let (controller_input, relay_receiver, relay_worker) = begin_session_relay(session);

    setup.broker.wait_for_session_request();
    let old_status = status_request(&setup, "status-whose-send-detects-broker-loss");
    setup.broker.hold_session_response(&old_status.request_id);
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Status(old_status.clone()));
    setup.broker.wait_for_held_session_response();
    setup.broker.wait_for_session_request();
    setup
        .broker
        .release_session_response_with(Err(SupervisorError::BrokerUnavailable));
    setup.broker.wait_for_reconnect(0);
    assert_eq!(
        timer.wait_for_schedule(0),
        Duration::from_millis(u64::from(BROKER_LOSS_GRACE_MS))
    );
    let reconnect = setup.broker.reconnect(0);

    setup.broker.detach_session_request_as_stale();
    setup.broker.complete_reconnect(reconnect);
    setup.broker.wait_for_session_request();
    setup.broker.fail_stale_session_request();

    let status = request_supervisor_status(&setup, "status-after-stale-old-channel-error");
    assert_eq!(status.state, SessionState::Running);
    assert_eq!(status.broker_connection, BrokerConnection::Connected);
    assert_eq!(status.channel_state, ChannelState::Enabled);
    assert_eq!(event_count(&setup.events, "broker.reconnect_session"), 1);
    assert_eq!(timer.scheduled_count(), 2);
    assert_eq!(event_count(&setup.events, "agent.park"), 0);

    finish_session_relay(&setup, controller_input, relay_receiver, relay_worker);
}

#[test]
fn transient_reconnect_failure_retries_within_the_original_grace_epoch() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let timer = Arc::new(FakeTimer::new(Arc::clone(&setup.events)));
    let timer_port: Arc<dyn SupervisorTimer> = timer.clone();
    let supervisor = setup.fresh_supervisor_with_timer(timer_port);
    let session = complete_launch_on(&setup, &supervisor);
    let head = ReceiptHead {
        sequence: session.receipt().payload.sequence,
        digest: Digest::of(&session.receipt().canonical_bytes()).to_string(),
    };
    let (controller_input, relay_receiver, relay_worker) = begin_session_relay(session);
    setup.broker.wait_for_session_request();

    setup.broker.disconnect_session();
    setup.broker.wait_for_reconnect(0);
    let grace = timer.wait_for_schedule(0);
    assert_eq!(
        grace,
        Duration::from_millis(u64::from(BROKER_LOSS_GRACE_MS))
    );
    let reconnect = setup.broker.reconnect(0);
    assert_eq!(reconnect.sequence, head.sequence);
    assert_eq!(reconnect.receipt_digest, head.digest);

    setup
        .broker
        .fail_reconnect(SupervisorError::BrokerUnavailable);
    assert_eq!(timer.wait_for_schedule(1), SUPERVISOR_TIMEOUT);
    let retry_delay = timer.wait_for_schedule(2);
    assert!(
        retry_delay < grace,
        "a transient reconnect retry must run before the immutable grace deadline"
    );
    assert_eq!(
        timer.scheduled_count(),
        3,
        "retry adds one timer beside the grace and stale attempt deadlines"
    );

    timer.fire(2);
    setup.broker.wait_for_reconnect(1);
    assert_eq!(
        setup.broker.reconnect(1),
        reconnect,
        "retry preserves the authenticated request and connection epoch"
    );
    setup.broker.complete_reconnect(reconnect);

    let restored = request_supervisor_status(&setup, "status-after-reconnect-retry");
    assert_eq!(restored.state, SessionState::Running);
    assert_eq!(restored.broker_connection, BrokerConnection::Connected);
    assert_eq!(restored.channel_state, ChannelState::Enabled);
    assert_eq!(event_count(&setup.events, "broker.reconnect_session"), 2);
    assert_eq!(event_count(&setup.events, "agent.park"), 0);
    assert_eq!(
        timer.scheduled_count(),
        4,
        "the retry owns its own bounded attempt without resetting grace"
    );

    timer.fire(0);
    let after_stale_deadline =
        request_supervisor_status(&setup, "status-after-retry-stale-grace-deadline");
    assert_eq!(after_stale_deadline, restored);
    assert_eq!(event_count(&setup.events, "broker.reconnect_session"), 2);
    assert_eq!(event_count(&setup.events, "agent.park"), 0);

    finish_session_relay(&setup, controller_input, relay_receiver, relay_worker);
}

#[test]
fn zero_grace_parks_before_reconnect_and_equal_head_never_auto_thaws() {
    let setup = setup(
        true,
        |authorization| authorization.broker_loss_grace_ms = 0,
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let timer = Arc::new(FakeTimer::new(Arc::clone(&setup.events)));
    let timer_port: Arc<dyn SupervisorTimer> = timer.clone();
    let supervisor = setup.fresh_supervisor_with_timer(timer_port);
    let session = complete_launch_on(&setup, &supervisor);
    let initial_head = ReceiptHead {
        sequence: session.receipt().payload.sequence,
        digest: Digest::of(&session.receipt().canonical_bytes()).to_string(),
    };
    let (controller_input, relay_receiver, relay_worker) = begin_session_relay(session);
    setup.broker.wait_for_session_request();

    setup.broker.disconnect_session();
    setup.broker.wait_for_reconnect(0);
    assert_eq!(
        timer.wait_for_schedule(0),
        SUPERVISOR_TIMEOUT,
        "zero grace schedules no grace timer; the first timer is the Park receipt deadline"
    );
    setup.broker.wait_for_session_receipt(0);
    let parked_bytes = setup.broker.session_receipt_bytes(0);
    let parked = SignedReceipt::parse_canonical(&parked_bytes)
        .expect("zero-grace Park receipt is canonical");
    assert_eq!(parked.payload.resulting_state, SessionState::Parked);
    assert_eq!(
        parked.payload.outcome,
        ReceiptOutcome::Park {
            authority: ReceiptAuthority::Cause {
                cause: ReceiptCause::BrokerLost,
            },
        }
    );
    let parked_head = ReceiptHead {
        sequence: parked.payload.sequence,
        digest: Digest::of(&parked_bytes).to_string(),
    };
    let reconnect = setup.broker.reconnect(0);
    assert_eq!(reconnect.sequence, initial_head.sequence);
    assert_eq!(reconnect.receipt_digest, initial_head.digest);

    setup.broker.complete_reconnect(reconnect);
    setup.broker.wait_for_session_receipt(1);
    assert_eq!(setup.broker.session_receipt_bytes(1), parked_bytes);
    let reconciling = request_supervisor_status(&setup, "status-during-zero-grace-repair");
    assert_eq!(reconciling.state, SessionState::Parked);
    assert_eq!(reconciling.broker_connection, BrokerConnection::Reconciling);
    assert_eq!(reconciling.channel_state, ChannelState::Revoked);
    assert_eq!(reconciling.launcher_head.as_ref(), Some(&parked_head));
    assert_eq!(reconciling.broker_head.as_ref(), Some(&initial_head));
    assert_eq!(reconciling.pending_receipt_count, 1);

    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(1),
        ));
    let repaired = request_supervisor_status(&setup, "status-after-zero-grace-repair");
    assert_eq!(repaired.state, SessionState::Parked);
    assert_eq!(repaired.broker_connection, BrokerConnection::Connected);
    assert_eq!(repaired.channel_state, ChannelState::Revoked);
    assert_eq!(repaired.launcher_head.as_ref(), Some(&parked_head));
    assert_eq!(repaired.broker_head.as_ref(), Some(&parked_head));
    assert_eq!(repaired.pending_receipt_count, 0);
    assert_eq!(event_count(&setup.events, "agent.resume"), 0);

    timer.fire(0);
    let after_stale_deadline =
        request_supervisor_status(&setup, "status-after-zero-grace-stale-park-deadline");
    assert_eq!(after_stale_deadline, repaired);

    finish_session_relay(&setup, controller_input, relay_receiver, relay_worker);
}

#[test]
fn broker_loss_grace_expiry_parks_before_receipting_and_reconnect_cannot_thaw() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let timer = Arc::new(FakeTimer::new(Arc::clone(&setup.events)));
    let timer_port: Arc<dyn SupervisorTimer> = timer.clone();
    let supervisor = setup.fresh_supervisor_with_timer(timer_port);
    let session = complete_launch_on(&setup, &supervisor);
    let (controller_input, relay_receiver, relay_worker) = begin_session_relay(session);
    setup.broker.wait_for_session_request();

    setup.broker.disconnect_session();
    setup.broker.wait_for_reconnect(0);
    assert_eq!(
        timer.wait_for_schedule(0),
        Duration::from_millis(u64::from(BROKER_LOSS_GRACE_MS))
    );
    timer.fire(0);
    setup.broker.wait_for_session_receipt(0);

    let receipt_bytes = setup.broker.session_receipt_bytes(0);
    let receipt = SignedReceipt::parse_canonical(&receipt_bytes)
        .expect("broker-loss Park receipt is canonical");
    assert_eq!(receipt.payload.resulting_state, SessionState::Parked);
    assert_eq!(
        receipt.payload.outcome,
        ReceiptOutcome::Park {
            authority: ReceiptAuthority::Cause {
                cause: ReceiptCause::BrokerLost,
            },
        }
    );
    let events = event_snapshot(&setup.events);
    let park = events
        .iter()
        .rposition(|event| event == "agent.park")
        .expect("grace expiry parks the process tree");
    let sign = events
        .iter()
        .rposition(|event| event == "signer.sign")
        .expect("grace expiry signs its Park truth");
    let send = events
        .iter()
        .rposition(|event| event == "broker.send_session_receipt")
        .expect("grace expiry sends its Park receipt");
    assert!(park < sign && sign < send);

    let parked_head = ReceiptHead {
        sequence: receipt.payload.sequence,
        digest: Digest::of(&receipt_bytes).to_string(),
    };
    let mut reconnected_head = setup.broker.reconnect(0);
    reconnected_head.sequence = parked_head.sequence;
    reconnected_head.receipt_digest = parked_head.digest.clone();
    setup.broker.complete_reconnect(reconnected_head);
    let status = request_supervisor_status(&setup, "status-after-expired-grace-reconnect");
    assert_eq!(status.state, SessionState::Parked);
    assert_eq!(status.broker_connection, BrokerConnection::Connected);
    assert_eq!(status.channel_state, ChannelState::Revoked);
    assert_eq!(status.launcher_head.as_ref(), Some(&parked_head));
    assert_eq!(status.broker_head.as_ref(), Some(&parked_head));
    assert_eq!(status.pending_receipt_count, 0);
    assert_eq!(event_count(&setup.events, "agent.resume"), 0);

    finish_session_relay(&setup, controller_input, relay_receiver, relay_worker);
}

#[test]
fn older_matching_broker_head_replays_exact_suffix_and_remains_parked() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let timer = Arc::new(FakeTimer::new(Arc::clone(&setup.events)));
    let timer_port: Arc<dyn SupervisorTimer> = timer.clone();
    let supervisor = setup.fresh_supervisor_with_timer(timer_port);
    let session = complete_launch_on(&setup, &supervisor);
    let broker_head = ReceiptHead {
        sequence: session.receipt().payload.sequence,
        digest: Digest::of(&session.receipt().canonical_bytes()).to_string(),
    };
    let (controller_input, relay_receiver, relay_worker) = begin_session_relay(session);
    setup.broker.wait_for_session_request();
    setup
        .broker
        .set_session_receipt_send_behavior(SessionReceiptSendBehavior::Reject);
    let park = lifecycle_request(&setup, "park-before-reconnect", "park-authorization", 1);
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(park.clone()));
    setup.broker.wait_for_session_receipt(0);
    let parked_bytes = setup.broker.session_receipt_bytes(0);
    let failed = setup.broker.wait_for_session_response(&park.request_id, 0);
    let ResponseResult::Error { error } = failed.result else {
        panic!("failed Park receipt send returns a typed error");
    };
    assert_eq!(error.code, ErrorCode::DurabilityUnavailable);
    setup
        .broker
        .set_session_receipt_send_behavior(SessionReceiptSendBehavior::Complete);

    setup.broker.wait_for_session_request();
    setup.broker.disconnect_session();
    setup.broker.wait_for_reconnect(0);
    let mut older_broker_head = setup.broker.reconnect(0);
    older_broker_head.sequence = broker_head.sequence;
    older_broker_head.receipt_digest = broker_head.digest.clone();
    setup.broker.complete_reconnect(older_broker_head);
    setup.broker.wait_for_session_receipt(1);
    assert_eq!(setup.broker.session_receipt_bytes(1), parked_bytes);
    assert_eq!(
        setup.signer.payloads().len(),
        3,
        "suffix replay never re-signs"
    );

    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(1),
        ));
    let status = request_supervisor_status(&setup, "status-after-suffix-repair");
    assert_eq!(status.state, SessionState::Parked);
    assert_eq!(status.broker_connection, BrokerConnection::Connected);
    assert_eq!(status.channel_state, ChannelState::Revoked);
    assert_eq!(status.launcher_head, status.broker_head);
    assert_eq!(status.pending_receipt_count, 0);
    assert_eq!(event_count(&setup.events, "agent.park"), 1);

    finish_session_relay(&setup, controller_input, relay_receiver, relay_worker);
}

#[derive(Clone, Copy, Debug)]
enum ReconnectConflict {
    BrokerAhead,
    UnknownSequence,
    CurrentHeadDigest,
    ForeignPrefix,
    Subject,
    Envelope,
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "One conflicting reconnect heads park revoke and report receipt chain invalid scenario keeps its causal steps and assertions together."
)]
fn conflicting_reconnect_heads_park_revoke_and_report_receipt_chain_invalid() {
    for conflict in [
        ReconnectConflict::BrokerAhead,
        ReconnectConflict::UnknownSequence,
        ReconnectConflict::CurrentHeadDigest,
        ReconnectConflict::ForeignPrefix,
        ReconnectConflict::Subject,
        ReconnectConflict::Envelope,
    ] {
        let setup = setup(
            true,
            |_| {},
            AppendBehavior::Hold,
            PlatformBehavior::default(),
            SUPERVISOR_TIMEOUT,
        );
        let timer = Arc::new(FakeTimer::new(Arc::clone(&setup.events)));
        let timer_port: Arc<dyn SupervisorTimer> = timer.clone();
        let supervisor = setup.fresh_supervisor_with_timer(timer_port);
        let session = complete_launch_on(&setup, &supervisor);
        let head = ReceiptHead {
            sequence: session.receipt().payload.sequence,
            digest: Digest::of(&session.receipt().canonical_bytes()).to_string(),
        };
        let (controller_input, relay_receiver, relay_worker) = begin_session_relay(session);
        setup.broker.wait_for_session_request();
        setup.broker.disconnect_session();
        setup.broker.wait_for_reconnect(0);
        assert_eq!(
            timer.wait_for_schedule(0),
            Duration::from_millis(u64::from(BROKER_LOSS_GRACE_MS))
        );

        let mut response = setup.broker.reconnect(0);
        match conflict {
            ReconnectConflict::BrokerAhead => {
                response.sequence = head.sequence + 1;
                response.receipt_digest = Digest::of(b"broker-ahead").to_string();
            }
            ReconnectConflict::UnknownSequence => {
                response.sequence = u64::MAX;
                response.receipt_digest = Digest::of(b"unknown-sequence").to_string();
            }
            ReconnectConflict::CurrentHeadDigest => {
                response.receipt_digest = Digest::of(b"conflicting-current-head").to_string();
            }
            ReconnectConflict::ForeignPrefix => {
                response.sequence = 0;
                response.receipt_digest = Digest::of(b"foreign-sequence-zero").to_string();
            }
            ReconnectConflict::Subject => {
                response.session_id = "another-session".to_owned();
            }
            ReconnectConflict::Envelope => {
                response.envelope_revision += 1;
            }
        }
        let enable_count = event_count(&setup.events, "capability.enable");
        setup.broker.complete_reconnect(response);

        let status = request_supervisor_status(
            &setup,
            &format!("status-after-{conflict:?}-reconnect-conflict"),
        );
        assert_eq!(status.state, SessionState::Parked, "case {conflict:?}");
        assert_eq!(
            status.broker_connection,
            BrokerConnection::Connected,
            "the authenticated candidate remains available for fail-closed control; case {conflict:?}"
        );
        assert_eq!(
            status.channel_state,
            ChannelState::Revoked,
            "case {conflict:?}"
        );
        assert_eq!(
            status.launcher_head.as_ref(),
            Some(&head),
            "case {conflict:?}"
        );
        assert_eq!(
            status.broker_head.as_ref(),
            Some(&head),
            "case {conflict:?}"
        );
        assert_eq!(
            status.pending_receipt_count, 1,
            "the mechanical BrokerLost Park remains an unsigned causal intent; case {conflict:?}"
        );
        assert_eq!(
            setup.broker.session_receipt_count(),
            0,
            "an untrusted head cannot receive a newly signed causal Park; case {conflict:?}"
        );
        assert_eq!(
            status.last_failure.as_ref().map(|failure| failure.code),
            Some(ErrorCode::ReceiptChainInvalid),
            "case {conflict:?}"
        );
        assert!(lock(&setup.platform.agent).parked, "case {conflict:?}");
        assert!(
            !lock(&setup.platform.gate_state()).enabled,
            "case {conflict:?}"
        );
        assert_eq!(
            event_count(&setup.events, "capability.enable"),
            enable_count,
            "a receipt conflict cannot widen channels; case {conflict:?}"
        );

        let mut resume = resume_request(&setup, &format!("resume-after-{conflict:?}"));
        resume.expected_receipt_sequence = Some(head.sequence);
        setup.broker.wait_for_session_request();
        setup
            .broker
            .deliver_session_request(ProtocolMessage::Lifecycle(resume.clone()));
        let blocked = setup
            .broker
            .wait_for_session_response(&resume.request_id, 0);
        let ResponseResult::Error { error } = blocked.result else {
            panic!("a receipt conflict permanently blocks widening; case {conflict:?}");
        };
        assert_eq!(error.code, ErrorCode::DurabilityUnavailable);
        assert_eq!(event_count(&setup.events, "agent.resume"), 0);
        assert_eq!(
            event_count(&setup.events, "capability.enable"),
            enable_count,
            "blocked Resume cannot widen channels; case {conflict:?}"
        );

        setup.broker.wait_for_session_request();
        setup.broker.disconnect_session();
        setup.broker.wait_for_reconnect(1);
        let recovery = setup.broker.reconnect(1);
        assert_eq!(recovery.sequence, head.sequence, "case {conflict:?}");
        assert_eq!(recovery.receipt_digest, head.digest, "case {conflict:?}");
        setup.broker.complete_reconnect(recovery);
        setup.broker.wait_for_session_receipt(0);
        let causal_park = SignedReceipt::parse_canonical(&setup.broker.session_receipt_bytes(0))
            .expect("repaired conflict emits a canonical causal Park");
        assert_eq!(
            causal_park.payload.outcome,
            ReceiptOutcome::Park {
                authority: ReceiptAuthority::Cause {
                    cause: ReceiptCause::BrokerLost,
                },
            },
            "case {conflict:?}"
        );
        setup.broker.wait_for_session_request();
        setup
            .broker
            .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
                setup.broker.session_receipt_acknowledgement(0),
            ));
        let repaired = request_supervisor_status(
            &setup,
            &format!("status-after-{conflict:?}-conflict-repair"),
        );
        assert_eq!(repaired.pending_receipt_count, 0, "case {conflict:?}");
        assert_eq!(
            repaired.launcher_head, repaired.broker_head,
            "case {conflict:?}"
        );

        finish_session_relay(&setup, controller_input, relay_receiver, relay_worker);
    }
}

#[test]
fn reconnect_conflict_with_failed_park_reports_mechanic_failure_and_allows_disposal() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior {
            park_fails: true,
            ..PlatformBehavior::default()
        },
        SUPERVISOR_TIMEOUT,
    );
    let timer = Arc::new(FakeTimer::new(Arc::clone(&setup.events)));
    let timer_port: Arc<dyn SupervisorTimer> = timer.clone();
    let supervisor = setup.fresh_supervisor_with_timer(timer_port);
    let session = complete_launch_on(&setup, &supervisor);
    let head = ReceiptHead {
        sequence: session.receipt().payload.sequence,
        digest: Digest::of(&session.receipt().canonical_bytes()).to_string(),
    };
    let (controller_input, relay_receiver, relay_worker) = begin_session_relay(session);
    setup.broker.wait_for_session_request();
    setup.broker.disconnect_session();
    setup.broker.wait_for_reconnect(0);
    assert_eq!(
        timer.wait_for_schedule(0),
        Duration::from_millis(u64::from(BROKER_LOSS_GRACE_MS))
    );

    let mut conflict = setup.broker.reconnect(0);
    conflict.sequence = head.sequence + 1;
    conflict.receipt_digest = Digest::of(b"broker-ahead").to_string();
    let enable_count = event_count(&setup.events, "capability.enable");
    setup.broker.complete_reconnect(conflict);

    let failed = request_supervisor_status(&setup, "status-after-failed-conflict-park");
    assert_eq!(failed.state, SessionState::Running);
    assert_eq!(failed.broker_connection, BrokerConnection::Connected);
    assert_eq!(failed.channel_state, ChannelState::Revoked);
    assert_eq!(failed.launcher_head.as_ref(), Some(&head));
    assert_eq!(failed.broker_head.as_ref(), Some(&head));
    assert_eq!(failed.pending_receipt_count, 0);
    assert_eq!(
        failed.last_failure.as_ref().map(|failure| failure.code),
        Some(ErrorCode::LifecycleMechanicUnavailable),
        "a receipt conflict cannot hide that the required fail-closed Park failed"
    );
    assert!(!lock(&setup.platform.agent).parked);
    assert_eq!(event_count(&setup.events, "agent.park"), 1);
    assert!(!lock(&setup.platform.gate_state()).enabled);
    assert_eq!(
        event_count(&setup.events, "capability.enable"),
        enable_count,
        "failed narrowing cannot widen the capability channel"
    );

    let mut resume = resume_request(&setup, "resume-after-failed-conflict-park");
    resume.expected_state = SessionState::Running;
    resume.expected_receipt_sequence = Some(head.sequence);
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(resume.clone()));
    let blocked = setup
        .broker
        .wait_for_session_response(&resume.request_id, 0);
    let ResponseResult::Error { error } = blocked.result else {
        panic!("failed reconciliation Park keeps widening blocked");
    };
    assert_eq!(error.code, ErrorCode::DurabilityUnavailable);
    assert_eq!(event_count(&setup.events, "agent.resume"), 0);
    assert_eq!(
        event_count(&setup.events, "capability.enable"),
        enable_count
    );

    let disposal = disposal_request(&setup, "dispose-after-failed-conflict-park");
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(disposal.clone()));
    setup.broker.wait_for_session_receipt(0);
    let receipt = SignedReceipt::parse_canonical(&setup.broker.session_receipt_bytes(0))
        .expect("authorized Disposal receipt is canonical");
    assert_eq!(receipt.payload.resulting_state, SessionState::Terminal);
    assert!(matches!(
        receipt.payload.outcome,
        ReceiptOutcome::Disposal {
            authority: ReceiptAuthority::Authorized(_),
        }
    ));
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(0),
        ));
    let response = setup
        .broker
        .wait_for_session_response(&disposal.request_id, 0);
    let ResponseResult::Receipt { receipt: response } = response.result else {
        panic!("authorized Disposal remains available after failed reconciliation Park");
    };
    assert_eq!(response, receipt);

    finish_terminal_session_relay(controller_input, relay_receiver, relay_worker);
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "One failed reconnect retry schedule fails closed and remains controllable scenario keeps its causal steps and assertions together."
)]
fn failed_reconnect_retry_schedule_fails_closed_and_remains_controllable() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let timer = Arc::new(FakeTimer::new(Arc::clone(&setup.events)));
    let timer_port: Arc<dyn SupervisorTimer> = timer.clone();
    let supervisor = setup.fresh_supervisor_with_timer(timer_port);
    let session = complete_launch_on(&setup, &supervisor);
    let broker_head = ReceiptHead {
        sequence: session.receipt().payload.sequence,
        digest: Digest::of(&session.receipt().canonical_bytes()).to_string(),
    };
    let (controller_input, relay_receiver, relay_worker) = begin_session_relay(session);

    setup
        .broker
        .set_session_receipt_send_behavior(SessionReceiptSendBehavior::Reject);
    setup.broker.wait_for_session_request();
    let park = lifecycle_request(
        &setup,
        "park-before-retry-schedule-failure",
        "park-authorization",
        broker_head.sequence,
    );
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(park.clone()));
    setup.broker.wait_for_session_receipt(0);
    let parked_bytes = setup.broker.session_receipt_bytes(0);
    let parked_head = ReceiptHead {
        sequence: broker_head.sequence + 1,
        digest: Digest::of(&parked_bytes).to_string(),
    };
    let failed = setup.broker.wait_for_session_response(&park.request_id, 0);
    let ResponseResult::Error { error } = failed.result else {
        panic!("failed Park receipt send returns a typed error");
    };
    assert_eq!(error.code, ErrorCode::DurabilityUnavailable);
    setup
        .broker
        .set_session_receipt_send_behavior(SessionReceiptSendBehavior::Complete);

    let grace_timer_index = timer.scheduled_count();
    setup.broker.wait_for_session_request();
    setup.broker.disconnect_session();
    setup.broker.wait_for_reconnect(0);
    assert_eq!(
        timer.wait_for_schedule(grace_timer_index),
        Duration::from_millis(u64::from(BROKER_LOSS_GRACE_MS))
    );
    assert_eq!(
        timer.wait_for_schedule(grace_timer_index + 1),
        SUPERVISOR_TIMEOUT,
    );
    let reconnect = setup.broker.reconnect(0);
    assert_eq!(reconnect.sequence, parked_head.sequence);
    assert_eq!(reconnect.receipt_digest, parked_head.digest);
    let schedules_before_failure = event_count(&setup.events, "timer.schedule");
    timer.fail_on_schedule_call(timer.scheduled_count());

    setup
        .broker
        .fail_reconnect(SupervisorError::BrokerUnavailable);
    setup.broker.wait_for_reconnect(1);
    assert_eq!(
        setup.broker.reconnect(1),
        reconnect,
        "a failed retry timer must fall back without losing the connection epoch"
    );
    assert_eq!(
        event_count(&setup.events, "timer.schedule"),
        schedules_before_failure + 2,
        "one failed retry schedule falls back to one bounded reconnect attempt"
    );

    let mut older_head = reconnect;
    older_head.sequence = broker_head.sequence;
    older_head.receipt_digest = broker_head.digest.clone();
    setup.broker.complete_reconnect(older_head);
    setup.broker.wait_for_session_receipt(1);
    assert_eq!(setup.broker.session_receipt_bytes(1), parked_bytes);

    let reconciling = request_supervisor_status(&setup, "status-after-retry-schedule-failure");
    assert_eq!(reconciling.state, SessionState::Parked);
    assert_eq!(reconciling.broker_connection, BrokerConnection::Reconciling);
    assert_eq!(reconciling.channel_state, ChannelState::Revoked);
    assert_eq!(reconciling.launcher_head.as_ref(), Some(&parked_head));
    assert_eq!(reconciling.broker_head.as_ref(), Some(&broker_head));
    assert_eq!(reconciling.pending_receipt_count, 1);
    assert_eq!(
        reconciling
            .last_failure
            .as_ref()
            .map(|failure| failure.code),
        Some(ErrorCode::BrokerUnavailable)
    );

    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(1),
        ));
    let repaired = request_supervisor_status(&setup, "status-after-retry-schedule-repair");
    assert_eq!(repaired.state, SessionState::Parked);
    assert_eq!(repaired.broker_connection, BrokerConnection::Connected);
    assert_eq!(repaired.channel_state, ChannelState::Revoked);
    assert_eq!(repaired.launcher_head, repaired.broker_head);
    assert_eq!(repaired.pending_receipt_count, 0);

    let resume = resume_request(&setup, "resume-after-retry-schedule-repair");
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(resume.clone()));
    setup.broker.wait_for_session_receipt(2);
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(2),
        ));
    let response = setup
        .broker
        .wait_for_session_response(&resume.request_id, 0);
    assert!(matches!(response.result, ResponseResult::Receipt { .. }));
    let running = request_supervisor_status(&setup, "status-after-retry-schedule-resume");
    assert_eq!(running.state, SessionState::Running);
    assert_eq!(running.broker_connection, BrokerConnection::Connected);
    assert_eq!(running.channel_state, ChannelState::Enabled);

    finish_session_relay(&setup, controller_input, relay_receiver, relay_worker);
}

#[test]
fn rejected_suffix_ack_remains_parked_and_reports_receipt_chain_invalid() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let timer = Arc::new(FakeTimer::new(Arc::clone(&setup.events)));
    let timer_port: Arc<dyn SupervisorTimer> = timer.clone();
    let supervisor = setup.fresh_supervisor_with_timer(timer_port);
    let session = complete_launch_on(&setup, &supervisor);
    let broker_head = ReceiptHead {
        sequence: session.receipt().payload.sequence,
        digest: Digest::of(&session.receipt().canonical_bytes()).to_string(),
    };
    let (controller_input, relay_receiver, relay_worker) = begin_session_relay(session);
    setup.broker.wait_for_session_request();
    setup
        .broker
        .set_session_receipt_send_behavior(SessionReceiptSendBehavior::Reject);
    let park = lifecycle_request(
        &setup,
        "park-before-invalid-suffix-ack",
        "park-authorization",
        1,
    );
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(park.clone()));
    setup.broker.wait_for_session_receipt(0);
    let parked_bytes = setup.broker.session_receipt_bytes(0);
    let parked_head = ReceiptHead {
        sequence: 2,
        digest: Digest::of(&parked_bytes).to_string(),
    };
    let failed = setup.broker.wait_for_session_response(&park.request_id, 0);
    let ResponseResult::Error { error } = failed.result else {
        panic!("failed Park receipt send returns a typed error");
    };
    assert_eq!(error.code, ErrorCode::DurabilityUnavailable);
    setup
        .broker
        .set_session_receipt_send_behavior(SessionReceiptSendBehavior::Complete);

    setup.broker.wait_for_session_request();
    setup.broker.disconnect_session();
    setup.broker.wait_for_reconnect(0);
    let mut older_head = setup.broker.reconnect(0);
    older_head.sequence = broker_head.sequence;
    older_head.receipt_digest = broker_head.digest.clone();
    setup.broker.complete_reconnect(older_head);
    setup.broker.wait_for_session_receipt(1);
    assert_eq!(setup.broker.session_receipt_bytes(1), parked_bytes);

    let reconciling = request_supervisor_status(&setup, "status-before-invalid-suffix-ack");
    assert_eq!(reconciling.state, SessionState::Parked);
    assert_eq!(reconciling.broker_connection, BrokerConnection::Reconciling);
    assert_eq!(reconciling.channel_state, ChannelState::Revoked);
    assert_eq!(reconciling.launcher_head.as_ref(), Some(&parked_head));
    assert_eq!(reconciling.broker_head.as_ref(), Some(&broker_head));
    assert_eq!(reconciling.pending_receipt_count, 1);

    let mut rejection = setup.broker.session_receipt_acknowledgement(1);
    rejection.disposition = ReceiptDisposition::Rejected;
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(rejection));
    let failed = request_supervisor_status(&setup, "status-after-invalid-suffix-ack");
    assert_eq!(failed.state, SessionState::Parked);
    assert_eq!(failed.broker_connection, BrokerConnection::Connected);
    assert_eq!(failed.channel_state, ChannelState::Revoked);
    assert_eq!(failed.launcher_head.as_ref(), Some(&parked_head));
    assert_eq!(failed.broker_head.as_ref(), Some(&broker_head));
    assert_eq!(failed.pending_receipt_count, 1);
    assert_eq!(
        failed.last_failure.as_ref().map(|failure| failure.code),
        Some(ErrorCode::ReceiptChainInvalid)
    );
    assert!(lock(&setup.platform.agent).parked);

    setup.broker.wait_for_session_request();
    setup.broker.disconnect_session();
    setup.broker.wait_for_reconnect(1);
    let mut retry = setup.broker.reconnect(1);
    retry.sequence = broker_head.sequence;
    retry.receipt_digest = broker_head.digest.clone();
    setup.broker.complete_reconnect(retry);
    setup.broker.wait_for_session_receipt(2);
    assert_eq!(setup.broker.session_receipt_bytes(2), parked_bytes);
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(2),
        ));
    let repaired = request_supervisor_status(&setup, "status-after-valid-suffix-retry");
    assert_eq!(repaired.pending_receipt_count, 0);
    assert_eq!(repaired.launcher_head, repaired.broker_head);

    finish_session_relay(&setup, controller_input, relay_receiver, relay_worker);
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "One grace expiry during mixed receipt repair never auto thaws and allows explicit resume scenario keeps its causal steps and assertions together."
)]
fn grace_expiry_during_mixed_receipt_repair_never_auto_thaws_and_allows_explicit_resume() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let timer = Arc::new(FakeTimer::new(Arc::clone(&setup.events)));
    let timer_port: Arc<dyn SupervisorTimer> = timer.clone();
    let supervisor = setup.fresh_supervisor_with_timer(timer_port);
    let session = complete_launch_on(&setup, &supervisor);
    let broker_head = ReceiptHead {
        sequence: session.receipt().payload.sequence,
        digest: Digest::of(&session.receipt().canonical_bytes()).to_string(),
    };
    let (controller_input, relay_receiver, relay_worker) = begin_session_relay(session);
    setup
        .broker
        .set_session_receipt_send_behavior(SessionReceiptSendBehavior::Reject);
    setup.broker.wait_for_session_request();
    let signed_interrupt = interrupt_request(
        &setup,
        "signed-interrupt-before-repair",
        SessionState::Running,
        broker_head.sequence,
    );
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(signed_interrupt.clone()));
    setup.broker.wait_for_session_receipt(0);
    let signed_interrupt_bytes = setup.broker.session_receipt_bytes(0);
    let failed = setup
        .broker
        .wait_for_session_response(&signed_interrupt.request_id, 0);
    let ResponseResult::Error { error } = failed.result else {
        panic!("failed signed Interrupt receipt send returns a typed error");
    };
    assert_eq!(error.code, ErrorCode::DurabilityUnavailable);

    setup.broker.wait_for_session_request();
    let deferred_interrupt = interrupt_request(
        &setup,
        "deferred-interrupt-before-repair",
        SessionState::Running,
        broker_head.sequence,
    );
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(deferred_interrupt.clone()));
    let failed = setup
        .broker
        .wait_for_session_response(&deferred_interrupt.request_id, 0);
    let ResponseResult::Error { error } = failed.result else {
        panic!("deferred Interrupt returns a typed durability error");
    };
    assert_eq!(error.code, ErrorCode::DurabilityUnavailable);
    assert_eq!(setup.broker.session_receipt_count(), 1);
    assert_eq!(event_count(&setup.events, "agent.interrupt"), 2);
    assert_eq!(
        setup.signer.payloads().len(),
        3,
        "the later Interrupt stays unsigned while an earlier signed receipt is pending"
    );
    setup
        .broker
        .set_session_receipt_send_behavior(SessionReceiptSendBehavior::Complete);
    let launcher_head = ReceiptHead {
        sequence: 2,
        digest: Digest::of(&signed_interrupt_bytes).to_string(),
    };
    let grace_timer_index = timer.scheduled_count();

    setup.broker.wait_for_session_request();
    setup.broker.disconnect_session();
    setup.broker.wait_for_reconnect(0);
    assert_eq!(
        timer.wait_for_schedule(grace_timer_index),
        Duration::from_millis(u64::from(BROKER_LOSS_GRACE_MS))
    );
    let mut older_head = setup.broker.reconnect(0);
    older_head.sequence = broker_head.sequence;
    older_head.receipt_digest = broker_head.digest.clone();
    setup.broker.complete_reconnect(older_head);
    setup.broker.wait_for_session_receipt(1);
    assert_eq!(
        setup.broker.session_receipt_bytes(1),
        signed_interrupt_bytes
    );
    assert_eq!(
        setup.signer.payloads().len(),
        3,
        "reconciliation replays exact bytes without signing"
    );

    let reconciling = request_supervisor_status(&setup, "status-during-mixed-receipt-repair");
    assert_eq!(reconciling.state, SessionState::Parked);
    assert_eq!(reconciling.broker_connection, BrokerConnection::Reconciling);
    assert_eq!(reconciling.channel_state, ChannelState::Revoked);
    assert_eq!(reconciling.launcher_head.as_ref(), Some(&launcher_head));
    assert_eq!(reconciling.broker_head.as_ref(), Some(&broker_head));
    assert_eq!(reconciling.pending_receipt_count, 3);

    timer.fire(grace_timer_index);
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(1),
        ));
    setup.broker.wait_for_session_receipt(2);
    let deferred_interrupt_bytes = setup.broker.session_receipt_bytes(2);
    let rebased_interrupt = SignedReceipt::parse_canonical(&deferred_interrupt_bytes)
        .expect("deferred Interrupt receipt is canonical after rebasing");
    assert_eq!(rebased_interrupt.payload.sequence, 3);
    assert_eq!(
        rebased_interrupt.payload.previous_receipt_digest,
        Some(Digest::of(&signed_interrupt_bytes).to_string())
    );
    assert_eq!(
        rebased_interrupt.payload.resulting_state,
        SessionState::Running
    );
    let ReceiptOutcome::Interrupt { authorization } = &rebased_interrupt.payload.outcome else {
        panic!("rebased deferred intent remains an Interrupt");
    };
    assert_eq!(
        authorization.authorization_id,
        deferred_interrupt.authorization_id
    );
    assert_eq!(authorization.request_id, deferred_interrupt.request_id);
    assert_eq!(
        authorization.request_digest,
        deferred_interrupt.digest().to_string()
    );
    assert_eq!(
        setup.signer.payloads().len(),
        4,
        "the deferred Interrupt is signed only after the exact suffix ACK"
    );
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(2),
        ));

    setup.broker.wait_for_session_receipt(3);
    let reconciliation_park_bytes = setup.broker.session_receipt_bytes(3);
    let reconciliation_park = SignedReceipt::parse_canonical(&reconciliation_park_bytes)
        .expect("reconciliation-induced Park receipt is canonical");
    assert_eq!(
        reconciliation_park.payload.outcome,
        ReceiptOutcome::Park {
            authority: ReceiptAuthority::Cause {
                cause: ReceiptCause::BrokerLost,
            },
        }
    );
    assert_eq!(
        reconciliation_park.payload.previous_receipt_digest,
        Some(Digest::of(&deferred_interrupt_bytes).to_string())
    );
    assert_eq!(
        reconciliation_park.payload.resulting_state,
        SessionState::Parked
    );
    assert_eq!(
        setup.signer.payloads().len(),
        5,
        "only the new causal Park is signed after exact suffix replay"
    );
    let parked_head = ReceiptHead {
        sequence: reconciliation_park.payload.sequence,
        digest: Digest::of(&reconciliation_park_bytes).to_string(),
    };
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(3),
        ));

    let repaired = request_supervisor_status(&setup, "status-after-mixed-receipt-repair");
    assert_eq!(repaired.state, SessionState::Parked);
    assert_eq!(repaired.broker_connection, BrokerConnection::Connected);
    assert_eq!(repaired.channel_state, ChannelState::Revoked);
    assert_eq!(repaired.launcher_head.as_ref(), Some(&parked_head));
    assert_eq!(repaired.broker_head.as_ref(), Some(&parked_head));
    assert_eq!(repaired.pending_receipt_count, 0);
    assert_eq!(event_count(&setup.events, "agent.interrupt"), 2);
    assert_eq!(event_count(&setup.events, "agent.park"), 1);
    assert_eq!(event_count(&setup.events, "agent.resume"), 0);

    let mut resume = resume_request(&setup, "resume-after-mixed-receipt-repair");
    resume.expected_receipt_sequence = Some(parked_head.sequence);
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(resume.clone()));
    setup.broker.wait_for_session_receipt(4);
    let resume_bytes = setup.broker.session_receipt_bytes(4);
    assert_eq!(event_count(&setup.events, "agent.resume"), 1);
    assert_eq!(
        event_count(&setup.events, "capability.enable"),
        1,
        "Resume cannot enable before exact ACK"
    );
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(4),
        ));
    let completed = setup
        .broker
        .wait_for_session_response(&resume.request_id, 0);
    let ResponseResult::Receipt {
        receipt: resume_receipt,
    } = completed.result
    else {
        panic!("explicit post-repair Resume returns its exact receipt");
    };
    assert_eq!(resume_receipt.canonical_bytes(), resume_bytes);
    let resumed = request_supervisor_status(&setup, "status-after-explicit-post-repair-resume");
    assert_eq!(resumed.state, SessionState::Running);
    assert_eq!(resumed.channel_state, ChannelState::Enabled);
    assert_eq!(resumed.pending_receipt_count, 0);
    assert_eq!(event_count(&setup.events, "capability.enable"), 2);

    let chain = [
        setup.broker.receipt_bytes(0),
        setup.broker.receipt_bytes(1),
        signed_interrupt_bytes,
        deferred_interrupt_bytes,
        reconciliation_park_bytes,
        resume_bytes,
    ]
    .map(|bytes| {
        SignedReceipt::parse_canonical(&bytes).expect("reconciled receipt chain is canonical")
    });
    verify_chain(
        &chain,
        &ChainAnchor {
            session_id: setup.request.session_id.clone(),
            run_id: setup.request.run_id.clone(),
            release_id: setup.signer.release_id().to_owned(),
            signing_key_id: setup.signer.signing_key_id().to_owned(),
        },
        |_, _, _| true,
    )
    .expect("causal Park makes the explicit post-repair Resume a valid transition");

    finish_session_relay(&setup, controller_input, relay_receiver, relay_worker);
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "One controller loss backlog starts settlement after its deferred park is durable scenario keeps its causal steps and assertions together."
)]
fn controller_loss_backlog_starts_settlement_after_its_deferred_park_is_durable() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let session = complete_launch(&setup);
    let initial_head = ReceiptHead {
        sequence: session.receipt().payload.sequence,
        digest: Digest::of(&session.receipt().canonical_bytes()).to_string(),
    };
    let (controller_input, relay_receiver, relay_worker) = begin_session_relay(session);

    setup.broker.wait_for_session_request();
    setup.signer.fail_on_call(setup.signer.payloads().len());
    let interrupt = interrupt_request(
        &setup,
        "interrupt-before-controller-loss",
        SessionState::Running,
        initial_head.sequence,
    );
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(interrupt.clone()));
    let failed = setup
        .broker
        .wait_for_session_response(&interrupt.request_id, 0);
    let ResponseResult::Error { error } = failed.result else {
        panic!("failed Interrupt reports its deferred receipt");
    };
    assert_eq!(error.code, ErrorCode::SigningUnavailable);
    setup.signer.fail_on_call(usize::MAX);

    setup.broker.wait_for_session_request();
    setup
        .platform
        .send_running_event(RunningAgentEvent::ControllerEof);
    let parked = request_supervisor_status(&setup, "status-with-controller-loss-backlog");
    assert_eq!(parked.state, SessionState::Parked);
    assert_eq!(parked.channel_state, ChannelState::Revoked);
    assert_eq!(parked.pending_receipt_count, 2);
    assert_eq!(
        event_count(&setup.events, "broker.settle_controller_loss"),
        0
    );

    setup.broker.wait_for_session_request();
    setup.broker.disconnect_session();
    setup.broker.wait_for_reconnect(0);
    let reconnect = setup.broker.reconnect(0);
    assert_eq!(reconnect.sequence, initial_head.sequence);
    assert_eq!(reconnect.receipt_digest, initial_head.digest);
    setup.broker.complete_reconnect(reconnect);

    setup.broker.wait_for_session_receipt(0);
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(0),
        ));
    setup.broker.wait_for_session_receipt(1);
    let park_bytes = setup.broker.session_receipt_bytes(1);
    let park_receipt = SignedReceipt::parse_canonical(&park_bytes)
        .expect("deferred controller-loss Park is canonical");
    assert_eq!(
        park_receipt.payload.outcome,
        ReceiptOutcome::Park {
            authority: ReceiptAuthority::Cause {
                cause: ReceiptCause::ControllerLost,
            },
        }
    );
    assert_eq!(
        event_count(&setup.events, "broker.settle_controller_loss"),
        0
    );

    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(1),
        ));
    setup.broker.wait_for_controller_loss_settlement(0);
    let settlement = setup.broker.controller_loss_settlement(0);
    assert_eq!(
        settlement.parked_head,
        ReceiptHead {
            sequence: park_receipt.payload.sequence,
            digest: Digest::of(&park_bytes).to_string(),
        }
    );

    setup
        .broker
        .complete_controller_loss_settlement(Ok(controller_loss_acknowledgement(
            &settlement,
            ControllerLossDisposition::Recoverable {
                acp_recovery_reference: "backlogged-controller-loss-recovery".to_owned(),
                attention_projection_id: "backlogged-controller-loss-attention".to_owned(),
            },
        )));
    setup.broker.wait_for_session_receipt(2);
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(2),
        ));
    assert_eq!(
        relay_receiver
            .recv_timeout(CALLBACK_TIMEOUT)
            .expect("durable controller-loss Disposal completes its owner")
            .expect("controller-loss cleanup succeeds"),
        0,
    );
    drop(controller_input);
    relay_worker.join().expect("terminal relay worker finishes");
}

#[test]
fn recoverable_controller_loss_waits_for_exact_durable_settlement_before_release() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let session = complete_launch(&setup);
    let (controller_input, relay_receiver, relay_worker, park_receipt, settlement) =
        begin_controller_loss_settlement(&setup, session);

    let waiting = request_supervisor_status(&setup, "status-awaiting-recovery-settlement");
    assert_eq!(waiting.state, SessionState::Parked);
    assert_eq!(waiting.channel_state, ChannelState::Revoked);
    assert_eq!(waiting.launcher_head, waiting.broker_head);
    assert_eq!(waiting.pending_receipt_count, 0);
    assert_eq!(event_count(&setup.events, "agent.dispose"), 0);
    assert_eq!(event_count(&setup.events, "identity.release"), 0);
    assert!(relay_receiver.try_recv().is_err());

    setup
        .broker
        .complete_controller_loss_settlement(Ok(controller_loss_acknowledgement(
            &settlement,
            ControllerLossDisposition::Recoverable {
                acp_recovery_reference: "acp-session-recovery-1".to_owned(),
                attention_projection_id: "attention-controller-loss-1".to_owned(),
            },
        )));
    setup.broker.wait_for_session_receipt(1);
    let disposal_receipt = SignedReceipt::parse_canonical(&setup.broker.session_receipt_bytes(1))
        .expect("post-settlement Disposal receipt is canonical");
    assert_eq!(
        disposal_receipt.payload.outcome,
        ReceiptOutcome::Disposal {
            authority: ReceiptAuthority::Cause {
                cause: ReceiptCause::ControllerLost,
            },
        }
    );
    assert_eq!(
        disposal_receipt.payload.resulting_state,
        SessionState::Terminal
    );
    assert_eq!(
        disposal_receipt.payload.previous_receipt_digest,
        Some(Digest::of(&park_receipt.canonical_bytes()).to_string())
    );
    let events = event_snapshot(&setup.events);
    let settlement_ack = events
        .iter()
        .rposition(|event| event == "broker.controller_loss_settlement.complete")
        .expect("broker supplied durable recovery settlement");
    let dispose = events
        .iter()
        .rposition(|event| event == "agent.dispose")
        .expect("old process tree is disposed");
    let release = events
        .iter()
        .rposition(|event| event == "identity.release")
        .expect("cold completion releases its identity");
    let sign = events
        .iter()
        .rposition(|event| event == "signer.sign")
        .expect("terminal Disposal is signed");
    assert!(settlement_ack < dispose && dispose < release && release < sign);
    assert!(relay_receiver.try_recv().is_err());

    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(1),
        ));
    assert!(
        relay_receiver
            .recv_timeout(CALLBACK_TIMEOUT)
            .expect("durable terminal receipt completes controller-loss handling")
            .is_ok()
    );
    drop(controller_input);
    relay_worker
        .join()
        .expect("supervised relay worker finishes");
    assert_eq!(event_count(&setup.events, "agent.dispose"), 1);
    assert_eq!(event_count(&setup.events, "identity.release"), 1);
}

#[test]
fn process_exit_invalidates_a_late_valid_controller_loss_settlement() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let session = complete_launch(&setup);
    let (controller_input, relay_receiver, relay_worker, _, settlement) =
        begin_controller_loss_settlement(&setup, session);

    setup
        .platform
        .send_running_event(RunningAgentEvent::ProcessExited(
            ProcessExitClassification::Failure,
        ));
    setup.broker.wait_for_session_receipt(1);
    let exit_receipt = SignedReceipt::parse_canonical(&setup.broker.session_receipt_bytes(1))
        .expect("process-exit Disposal is canonical");
    assert_eq!(
        exit_receipt.payload.outcome,
        ReceiptOutcome::Disposal {
            authority: ReceiptAuthority::ProcessExited {
                classification: ProcessExitClassification::Failure,
            },
        }
    );
    let before = request_supervisor_status(&setup, "status-before-late-controller-settlement");
    let effects_before = lifecycle_effect_counts(&setup.events);
    let releases_before = event_count(&setup.events, "identity.release");

    setup
        .broker
        .complete_controller_loss_settlement(Ok(controller_loss_acknowledgement(
            &settlement,
            ControllerLossDisposition::Recoverable {
                acp_recovery_reference: "late-controller-loss-recovery".to_owned(),
                attention_projection_id: "late-controller-loss-attention".to_owned(),
            },
        )));
    let after = request_supervisor_status(&setup, "status-after-late-controller-settlement");
    assert_eq!(after, before);
    assert_eq!(lifecycle_effect_counts(&setup.events), effects_before);
    assert_eq!(
        event_count(&setup.events, "identity.release"),
        releases_before
    );
    assert_eq!(setup.broker.session_receipt_count(), 2);

    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(1),
        ));
    assert_eq!(
        relay_receiver
            .recv_timeout(CALLBACK_TIMEOUT)
            .expect("process-exit receipt completes its owner")
            .expect("terminal cleanup succeeds"),
        1,
    );
    drop(controller_input);
    relay_worker.join().expect("terminal relay worker finishes");
}

#[test]
fn no_recovery_settlement_becomes_terminal_without_exposing_cold_park() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let session = complete_launch(&setup);
    let (controller_input, relay_receiver, relay_worker, _, settlement) =
        begin_controller_loss_settlement(&setup, session);
    let terminal_sign_call = setup.signer.payloads().len();
    setup.signer.hold_on_call(terminal_sign_call);

    setup
        .broker
        .complete_controller_loss_settlement(Ok(controller_loss_acknowledgement(
            &settlement,
            ControllerLossDisposition::NoRecovery {
                attention_projection_id: "attention-unrecoverable-loss-1".to_owned(),
            },
        )));
    setup.signer.wait_for_held_call();
    let terminal = request_supervisor_status(&setup, "status-after-no-recovery-settlement");
    assert_eq!(terminal.state, SessionState::Terminal);
    assert_eq!(terminal.channel_state, ChannelState::Closed);
    assert_eq!(terminal.process_exit, None);
    assert_eq!(event_count(&setup.events, "agent.dispose"), 1);
    assert_eq!(event_count(&setup.events, "identity.release"), 1);
    assert_eq!(
        setup.broker.session_receipt_count(),
        1,
        "terminal receipt is not observable before its signature exists"
    );

    setup.signer.release_held_call();
    setup.broker.wait_for_session_receipt(1);
    let receipt = SignedReceipt::parse_canonical(&setup.broker.session_receipt_bytes(1))
        .expect("no-recovery Disposal receipt is canonical");
    assert_eq!(
        receipt.payload.outcome,
        ReceiptOutcome::Disposal {
            authority: ReceiptAuthority::Cause {
                cause: ReceiptCause::ControllerLost,
            },
        }
    );
    assert_eq!(receipt.payload.resulting_state, SessionState::Terminal);
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(1),
        ));
    assert!(
        relay_receiver
            .recv_timeout(CALLBACK_TIMEOUT)
            .expect("durable no-recovery Disposal completes")
            .is_ok()
    );
    drop(controller_input);
    relay_worker
        .join()
        .expect("supervised relay worker finishes");
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "One invalid failed and late controller loss settlements retain the frozen lease scenario keeps its causal steps and assertions together."
)]
fn invalid_failed_and_late_controller_loss_settlements_retain_the_frozen_lease() {
    for callback_error in [false, true] {
        let setup = setup(
            true,
            |_| {},
            AppendBehavior::Hold,
            PlatformBehavior::default(),
            SUPERVISOR_TIMEOUT,
        );
        let session = complete_launch(&setup);
        let (controller_input, relay_receiver, relay_worker, park_receipt, settlement) =
            begin_controller_loss_settlement(&setup, session);
        if callback_error {
            setup
                .broker
                .complete_controller_loss_settlement(Err(SupervisorError::DurabilityUnavailable));
        } else {
            let mut mismatched = controller_loss_acknowledgement(
                &settlement,
                ControllerLossDisposition::Recoverable {
                    acp_recovery_reference: "acp-session-recovery-1".to_owned(),
                    attention_projection_id: "attention-controller-loss-1".to_owned(),
                },
            );
            mismatched.run_id = "another-run".to_owned();
            setup
                .broker
                .complete_controller_loss_settlement(Ok(mismatched));
        }

        let status = request_supervisor_status(
            &setup,
            if callback_error {
                "status-after-settlement-error"
            } else {
                "status-after-mismatched-settlement"
            },
        );
        assert_eq!(status.state, SessionState::Parked);
        assert_eq!(status.channel_state, ChannelState::Revoked);
        assert_eq!(status.launcher_head, status.broker_head);
        assert!(status.last_failure.is_some());
        assert_eq!(event_count(&setup.events, "agent.dispose"), 0);
        assert_eq!(event_count(&setup.events, "identity.release"), 0);
        assert_eq!(event_count(&setup.events, "capability.enable"), 1);
        assert!(relay_receiver.try_recv().is_err());

        let resume = resume_request(
            &setup,
            if callback_error {
                "resume-after-settlement-error"
            } else {
                "resume-after-mismatched-settlement"
            },
        );
        setup.broker.wait_for_session_request();
        setup
            .broker
            .deliver_session_request(ProtocolMessage::Lifecycle(resume.clone()));
        let response = setup
            .broker
            .wait_for_session_response(&resume.request_id, 0);
        assert!(matches!(response.result, ResponseResult::Error { .. }));
        assert_eq!(event_count(&setup.events, "agent.resume"), 0);
        assert_eq!(event_count(&setup.events, "capability.enable"), 1);

        dispose_retained_controller_loss_session(
            &setup,
            if callback_error {
                "dispose-after-settlement-error"
            } else {
                "dispose-after-mismatched-settlement"
            },
            park_receipt.payload.sequence,
            controller_input,
            relay_receiver,
            relay_worker,
        );
    }

    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let timer = Arc::new(FakeTimer::new(Arc::clone(&setup.events)));
    let timer_port: Arc<dyn SupervisorTimer> = timer.clone();
    let supervisor = setup.fresh_supervisor_with_timer(timer_port);
    let session = complete_launch_on(&setup, &supervisor);
    let (controller_input, relay_receiver, relay_worker, park_receipt, settlement) =
        begin_controller_loss_settlement(&setup, session);
    assert_eq!(timer.wait_for_schedule(0), SUPERVISOR_TIMEOUT);
    assert_eq!(timer.wait_for_schedule(1), SUPERVISOR_TIMEOUT);

    timer.fire(1);
    let before_late_ack = request_supervisor_status(&setup, "status-after-settlement-timeout");
    assert_eq!(before_late_ack.state, SessionState::Parked);
    assert_eq!(before_late_ack.channel_state, ChannelState::Revoked);
    assert_eq!(event_count(&setup.events, "agent.dispose"), 0);
    assert_eq!(event_count(&setup.events, "identity.release"), 0);
    let effects_before_late_ack = lifecycle_effect_counts(&setup.events);
    let signs_before_late_ack = setup.signer.payloads().len();

    setup
        .broker
        .complete_controller_loss_settlement(Ok(controller_loss_acknowledgement(
            &settlement,
            ControllerLossDisposition::Recoverable {
                acp_recovery_reference: "late-acp-session-recovery".to_owned(),
                attention_projection_id: "late-attention-projection".to_owned(),
            },
        )));
    let after_late_ack = request_supervisor_status(&setup, "status-after-late-settlement-ack");
    assert_eq!(after_late_ack, before_late_ack);
    assert_eq!(
        lifecycle_effect_counts(&setup.events),
        effects_before_late_ack
    );
    assert_eq!(setup.signer.payloads().len(), signs_before_late_ack);
    assert_eq!(event_count(&setup.events, "agent.dispose"), 0);
    assert_eq!(event_count(&setup.events, "identity.release"), 0);

    dispose_retained_controller_loss_session(
        &setup,
        "dispose-after-late-settlement",
        park_receipt.payload.sequence,
        controller_input,
        relay_receiver,
        relay_worker,
    );
}

#[test]
fn request_callback_from_a_finished_connection_epoch_is_inert() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let session = complete_launch(&setup);
    let (controller_input, relay_receiver, relay_worker) = begin_session_relay(session);
    setup.broker.wait_for_session_request();

    finish_session_relay(&setup, controller_input, relay_receiver, relay_worker);
    let effects_after_cleanup = lifecycle_effect_counts(&setup.events);
    let responses_after_cleanup = setup.broker.session_response_count();
    let releases_after_cleanup = event_count(&setup.events, "identity.release");

    // This one-shot callback was armed by the now-finished connection epoch.
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(lifecycle_request(
            &setup,
            "late-park",
            "late-authorization",
            1,
        )));

    assert_eq!(
        lifecycle_effect_counts(&setup.events),
        effects_after_cleanup,
        "a late callback cannot touch released Session mechanics"
    );
    assert_eq!(
        setup.broker.session_response_count(),
        responses_after_cleanup,
        "a late callback cannot complete a request in a dead epoch"
    );
    assert_eq!(
        event_count(&setup.events, "identity.release"),
        releases_after_cleanup,
        "a late callback cannot release identity twice"
    );
}

#[derive(Clone, Copy, Debug)]
enum StartTransitionFailure {
    Enable,
    Start,
}

#[test]
fn enable_and_start_failures_leave_only_the_durable_starting_receipt_and_clean_once() {
    for case in [
        StartTransitionFailure::Enable,
        StartTransitionFailure::Start,
    ] {
        let behavior = match case {
            StartTransitionFailure::Enable => PlatformBehavior {
                enable_fails: true,
                ..PlatformBehavior::default()
            },
            StartTransitionFailure::Start => PlatformBehavior {
                start_fails: true,
                ..PlatformBehavior::default()
            },
        };
        let expected = match case {
            StartTransitionFailure::Enable => SupervisorError::CapabilityUnavailable,
            StartTransitionFailure::Start => SupervisorError::SpawnFailed,
        };
        let setup = setup(
            true,
            |_| {},
            AppendBehavior::Hold,
            behavior,
            SUPERVISOR_TIMEOUT,
        );
        let (receiver, completion_count) = begin_launch(&setup, CONTROLLER_UID);
        setup.broker.wait_for_append(0);
        setup.broker.acknowledge();

        assert_eq!(
            receiver
                .recv_timeout(CALLBACK_TIMEOUT)
                .unwrap_or_else(|_| panic!("{case:?} failure did not complete"))
                .err()
                .expect("the Starting-to-Running transition must fail"),
            expected,
        );
        let receipts = setup.broker.receipts();
        assert_eq!(receipts.len(), 1, "case {case:?}");
        let receipt =
            SignedReceipt::parse_canonical(&receipts[0]).expect("Starting receipt is canonical");
        assert_eq!(receipt.payload.sequence, 0, "case {case:?}");
        assert_eq!(
            receipt.payload.resulting_state,
            SessionState::Starting,
            "case {case:?}",
        );
        assert_eq!(setup.signer.payloads().len(), 1, "case {case:?}");

        let events = event_snapshot(&setup.events);
        for event in [
            "capability.close",
            "agent.dispose",
            "identity.release",
            "broker.close",
            "complete",
        ] {
            assert_eq!(
                events
                    .iter()
                    .filter(|found| found.as_str() == event)
                    .count(),
                1,
                "case {case:?}: {events:?}",
            );
        }
        assert!(
            !events.iter().any(|event| {
                matches!(
                    event.as_str(),
                    "agent.prepared_dropped_without_disposal"
                        | "agent.running_dropped_without_disposal"
                        | "identity.dropped_without_release"
                )
            }),
            "case {case:?}: {events:?}",
        );
        assert_eq!(completion_count.load(Ordering::SeqCst), 1);
    }
}

#[derive(Clone, Copy, Debug)]
enum RunningReceiptFailure {
    Sign,
    Persist,
    Acknowledge,
}

#[test]
fn every_running_receipt_failure_disposes_the_started_tree_once() {
    for case in [
        RunningReceiptFailure::Sign,
        RunningReceiptFailure::Persist,
        RunningReceiptFailure::Acknowledge,
    ] {
        let setup = setup(
            true,
            |_| {},
            AppendBehavior::Hold,
            PlatformBehavior::default(),
            SUPERVISOR_TIMEOUT,
        );
        if matches!(case, RunningReceiptFailure::Sign) {
            setup.signer.fail_on_call(1);
        }
        let (receiver, completion_count) = begin_launch(&setup, CONTROLLER_UID);
        setup.broker.wait_for_append(0);
        if matches!(case, RunningReceiptFailure::Persist) {
            setup.broker.set_append_behavior(AppendBehavior::Reject);
        }
        setup.broker.acknowledge();
        if matches!(case, RunningReceiptFailure::Acknowledge) {
            setup.broker.wait_for_append(1);
            setup.broker.acknowledge_with(|acknowledgement| {
                acknowledgement.receipt_digest = Digest::of(b"different-running").to_string();
            });
        }

        let expected = match case {
            RunningReceiptFailure::Sign => SupervisorError::SigningUnavailable,
            RunningReceiptFailure::Persist => SupervisorError::DurabilityUnavailable,
            RunningReceiptFailure::Acknowledge => SupervisorError::AcknowledgementMismatch,
        };
        assert_eq!(
            receiver
                .recv_timeout(CALLBACK_TIMEOUT)
                .unwrap_or_else(|_| panic!("{case:?} failure did not complete"))
                .err()
                .expect("the Running receipt transaction must fail"),
            expected,
        );
        assert!(lock(&setup.platform.agent).started, "case {case:?}");
        assert!(lock(&setup.platform.agent).disposed, "case {case:?}");
        assert_eq!(setup.signer.payloads().len(), 2, "case {case:?}");
        let receipts = setup.broker.receipts();
        let expected_receipts = if matches!(case, RunningReceiptFailure::Acknowledge) {
            2
        } else {
            1
        };
        assert_eq!(receipts.len(), expected_receipts, "case {case:?}");
        assert_eq!(
            SignedReceipt::parse_canonical(&receipts[0])
                .expect("Starting receipt is canonical")
                .payload
                .resulting_state,
            SessionState::Starting,
        );
        if let Some(bytes) = receipts.get(1) {
            assert_eq!(
                SignedReceipt::parse_canonical(bytes)
                    .expect("Running receipt is canonical")
                    .payload
                    .resulting_state,
                SessionState::Running,
            );
        }

        let events = event_snapshot(&setup.events);
        for event in [
            "capability.close",
            "agent.dispose",
            "identity.release",
            "broker.close",
            "complete",
        ] {
            assert_eq!(
                events
                    .iter()
                    .filter(|found| found.as_str() == event)
                    .count(),
                1,
                "case {case:?}: {events:?}",
            );
        }
        assert!(
            !events
                .iter()
                .any(|event| event == "agent.running_dropped_without_disposal"),
            "case {case:?}: {events:?}",
        );
        assert_eq!(completion_count.load(Ordering::SeqCst), 1);
    }
}

#[derive(Clone, Copy, Debug)]
enum AuthorizationRejection {
    Missing,
    Expired,
    AuthorizationId,
    RequestId,
    RequestDigest,
    ControllerUid,
    Session,
    Run,
    EnvelopeRevision,
}

#[test]
fn every_missing_expired_or_mismatched_authorization_fails_before_root_mechanics() {
    for case in [
        AuthorizationRejection::Missing,
        AuthorizationRejection::Expired,
        AuthorizationRejection::AuthorizationId,
        AuthorizationRejection::RequestId,
        AuthorizationRejection::RequestDigest,
        AuthorizationRejection::ControllerUid,
        AuthorizationRejection::Session,
        AuthorizationRejection::Run,
        AuthorizationRejection::EnvelopeRevision,
    ] {
        let available = !matches!(case, AuthorizationRejection::Missing);
        let setup = setup(
            available,
            |authorization| match case {
                AuthorizationRejection::Missing => {}
                AuthorizationRejection::Expired => authorization.expires_at_ms = NOW_MS,
                AuthorizationRejection::AuthorizationId => {
                    authorization.authorization_id = "another-authorization".to_owned();
                }
                AuthorizationRejection::RequestId => {
                    authorization.request_id = "another-request".to_owned();
                }
                AuthorizationRejection::RequestDigest => {
                    authorization.request_digest = Digest::of(b"another-request").to_string();
                }
                AuthorizationRejection::ControllerUid => authorization.controller_uid += 1,
                AuthorizationRejection::Session => {
                    authorization.session_id = "another-session".to_owned();
                }
                AuthorizationRejection::Run => authorization.run_id = "another-run".to_owned(),
                AuthorizationRejection::EnvelopeRevision => authorization.envelope_revision += 1,
            },
            AppendBehavior::Hold,
            PlatformBehavior::default(),
            SUPERVISOR_TIMEOUT,
        );
        let (receiver, completion_count) = begin_launch(&setup, CONTROLLER_UID);
        assert_eq!(
            receiver
                .recv_timeout(CALLBACK_TIMEOUT)
                .unwrap_or_else(|_| panic!("{case:?} did not complete"))
                .err()
                .expect("authorization must fail"),
            SupervisorError::AuthorizationRejected,
            "case {case:?}",
        );
        assert_eq!(
            event_snapshot(&setup.events),
            ["broker.consume", "broker.close", "complete"]
        );
        assert_eq!(completion_count.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn consumed_authorization_is_single_use_on_a_real_second_launch_attempt() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let (first, first_count) = begin_launch(&setup, CONTROLLER_UID + 1);
    assert_eq!(
        first
            .recv_timeout(CALLBACK_TIMEOUT)
            .expect("first attempt completes")
            .err()
            .expect("the mismatched controller must fail"),
        SupervisorError::AuthorizationRejected,
    );
    assert_eq!(first_count.load(Ordering::SeqCst), 1);

    let second_supervisor = setup.fresh_supervisor();
    let (second, second_count) = begin_launch_on(
        &second_supervisor,
        &setup.request,
        &setup.events,
        CONTROLLER_UID,
    );
    assert_eq!(
        second
            .recv_timeout(CALLBACK_TIMEOUT)
            .expect("second attempt completes")
            .err()
            .expect("the consumed authorization cannot replay"),
        SupervisorError::AuthorizationRejected,
    );
    assert_eq!(second_count.load(Ordering::SeqCst), 1);
    assert_eq!(
        event_snapshot(&setup.events),
        [
            "broker.consume",
            "broker.close",
            "complete",
            "broker.consume",
            "broker.close",
            "complete",
        ],
    );
}

#[test]
fn authorization_that_expires_while_the_async_broker_callback_is_pending_is_rejected() {
    let setup = setup(
        true,
        |authorization| authorization.expires_at_ms = NOW_MS + 1,
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    setup.broker.hold_authorization();
    let (receiver, completion_count) = begin_launch(&setup, CONTROLLER_UID);
    setup.broker.wait_for_authorization();
    thread::sleep(Duration::from_millis(10));
    setup.broker.release_authorization();

    assert_eq!(
        receiver
            .recv_timeout(CALLBACK_TIMEOUT)
            .expect("expired async authorization completes")
            .err()
            .expect("authorization must be live when received"),
        SupervisorError::AuthorizationRejected,
    );
    assert_eq!(completion_count.load(Ordering::SeqCst), 1);
    assert_eq!(
        event_snapshot(&setup.events),
        ["broker.consume", "broker.close", "complete"],
    );
}

#[test]
fn authorization_timeout_closes_the_broker_without_entering_root_mechanics_or_recompleting() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        Duration::from_millis(25),
    );
    setup.broker.hold_authorization();
    let (receiver, completion_count) = begin_launch(&setup, CONTROLLER_UID);
    setup.broker.wait_for_authorization();

    assert_eq!(
        receiver
            .recv_timeout(CALLBACK_TIMEOUT)
            .expect("authorization timeout completes")
            .err()
            .expect("held authorization must time out"),
        SupervisorError::BrokerTimeout,
    );
    let events = event_snapshot(&setup.events);
    assert_eq!(completion_count.load(Ordering::SeqCst), 1);
    assert!(events.iter().any(|event| event == "broker.close"));
    for forbidden in [
        "identity.acquire",
        "capability.create",
        "platform.prepare",
        "capability.bind",
        "signer.sign",
        "broker.append",
        "capability.enable",
        "agent.start",
    ] {
        assert!(!events.iter().any(|event| event == forbidden), "{events:?}");
    }

    setup.broker.release_authorization();
    assert_eq!(completion_count.load(Ordering::SeqCst), 1);
    assert!(receiver.try_recv().is_err());
    assert_eq!(event_snapshot(&setup.events), events);
}

#[test]
fn broker_identity_exhaustion_propagates_typed_evidence_without_entering_root_mechanics() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let exhaustion = IdentityExhaustion::compose(vec![
        OccupiedSessionIdentity {
            session_id: "occupied-session-2".to_owned(),
            state: SessionState::Parked,
            slot: 7,
        },
        OccupiedSessionIdentity {
            session_id: "occupied-session-1".to_owned(),
            state: SessionState::Running,
            slot: 2,
        },
    ])
    .expect("fake broker composes valid operator evidence");
    let expected = SupervisorError::SessionIdentityExhausted(Box::new(exhaustion));
    assert_eq!(expected.to_string(), "no session identity is available");
    setup.broker.fail_authorization(expected.clone());

    let (receiver, completion_count) = begin_launch(&setup, CONTROLLER_UID);
    assert_eq!(
        receiver
            .recv_timeout(CALLBACK_TIMEOUT)
            .expect("identity exhaustion completes")
            .err()
            .expect("identity exhaustion must fail launch"),
        expected,
    );
    assert_eq!(completion_count.load(Ordering::SeqCst), 1);
    assert_eq!(
        event_snapshot(&setup.events),
        ["broker.consume", "broker.close", "complete"],
    );
}

#[test]
fn local_identity_unavailability_remains_distinct_from_broker_exhaustion() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior {
            identity_unavailable: true,
            ..PlatformBehavior::default()
        },
        SUPERVISOR_TIMEOUT,
    );
    let (receiver, _) = begin_launch(&setup, CONTROLLER_UID);
    assert_eq!(
        receiver
            .recv_timeout(CALLBACK_TIMEOUT)
            .expect("identity failure completes")
            .err()
            .expect("identity acquisition must fail"),
        SupervisorError::IdentityUnavailable,
    );
    assert_eq!(
        event_snapshot(&setup.events),
        [
            "broker.consume",
            "identity.acquire",
            "broker.close",
            "complete"
        ],
    );
}

#[test]
fn invalid_identity_assignment_stops_before_capability_or_spawn() {
    for (name, assigned, behavior) in [
        (
            "occupied-slot",
            None,
            PlatformBehavior {
                identity_occupied: true,
                ..PlatformBehavior::default()
            },
        ),
        (
            "poisoned-slot",
            None,
            PlatformBehavior {
                identity_poisoned: true,
                ..PlatformBehavior::default()
            },
        ),
        (
            "wrong-slot",
            Some((9_999, 200_003, 300_003)),
            PlatformBehavior::default(),
        ),
        (
            "wrong-uid",
            Some((3, 999_999, 300_003)),
            PlatformBehavior::default(),
        ),
        (
            "wrong-gid",
            Some((3, 200_003, 999_999)),
            PlatformBehavior::default(),
        ),
    ] {
        let setup = setup(
            true,
            |authorization| {
                if let Some((slot, uid, gid)) = assigned {
                    authorization.identity_slot = slot;
                    authorization.assigned_uid = uid;
                    authorization.assigned_gid = gid;
                }
            },
            AppendBehavior::Hold,
            behavior,
            SUPERVISOR_TIMEOUT,
        );
        let (receiver, _) = begin_launch(&setup, CONTROLLER_UID);
        assert_eq!(
            receiver
                .recv_timeout(CALLBACK_TIMEOUT)
                .expect("identity failure completes")
                .err()
                .expect("identity must fail"),
            SupervisorError::IdentityAssignmentInvalid,
            "case {name}",
        );
        assert_eq!(
            event_snapshot(&setup.events),
            [
                "broker.consume",
                "identity.acquire",
                "broker.close",
                "complete"
            ],
        );
    }
}

#[test]
fn mismatched_identity_guard_is_released_before_capability_or_spawn() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior {
            identity_guard_mismatch: true,
            ..PlatformBehavior::default()
        },
        SUPERVISOR_TIMEOUT,
    );
    let (receiver, _) = begin_launch(&setup, CONTROLLER_UID);
    assert_eq!(
        receiver
            .recv_timeout(CALLBACK_TIMEOUT)
            .expect("identity guard mismatch completes")
            .err()
            .expect("mismatched guard must fail"),
        SupervisorError::IdentityAssignmentInvalid,
    );
    assert_eq!(
        event_snapshot(&setup.events),
        [
            "broker.consume",
            "identity.acquire",
            "identity.release",
            "broker.close",
            "complete",
        ],
    );
}

#[test]
fn mismatched_receipt_ack_disposes_without_starting_or_enabling() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let (receiver, completion_count) = begin_launch(&setup, CONTROLLER_UID);
    setup.broker.wait_for_append(0);
    setup.broker.acknowledge_with(|acknowledgement| {
        acknowledgement.receipt_digest = Digest::of(b"different-receipt").to_string();
    });

    assert_eq!(
        receiver
            .recv_timeout(CALLBACK_TIMEOUT)
            .expect("mismatched ACK completes")
            .err()
            .expect("mismatched ACK must fail"),
        SupervisorError::AcknowledgementMismatch,
    );
    let events = event_snapshot(&setup.events);
    assert!(!events.iter().any(|event| event == "agent.start"));
    assert!(!events.iter().any(|event| event == "capability.enable"));
    assert_eq!(completion_count.load(Ordering::SeqCst), 1);
    assert!(
        events
            .windows(3)
            .any(|events| { events == ["capability.close", "agent.dispose", "identity.release"] })
    );
}

#[test]
fn spawn_failure_closes_capability_then_releases_identity() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior {
            prepare_fails: true,
            ..PlatformBehavior::default()
        },
        SUPERVISOR_TIMEOUT,
    );
    let (receiver, completion_count) = begin_launch(&setup, CONTROLLER_UID);
    assert_eq!(
        receiver
            .recv_timeout(CALLBACK_TIMEOUT)
            .expect("spawn failure completes")
            .err()
            .expect("spawn must fail"),
        SupervisorError::SpawnFailed,
    );
    assert_eq!(completion_count.load(Ordering::SeqCst), 1);
    assert_eq!(
        event_snapshot(&setup.events),
        [
            "broker.consume",
            "identity.acquire",
            "capability.create",
            "platform.prepare",
            "capability.close",
            "identity.release",
            "broker.close",
            "complete",
        ]
    );
}

#[test]
fn unverified_isolation_is_disposed_before_signing() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior {
            unverified_evidence: true,
            ..PlatformBehavior::default()
        },
        SUPERVISOR_TIMEOUT,
    );
    let (receiver, _) = begin_launch(&setup, CONTROLLER_UID);
    assert_eq!(
        receiver
            .recv_timeout(CALLBACK_TIMEOUT)
            .expect("evidence rejection completes")
            .err()
            .expect("unverified evidence must fail"),
        SupervisorError::IsolationRejected,
    );
    assert_eq!(
        event_snapshot(&setup.events),
        [
            "broker.consume",
            "identity.acquire",
            "capability.create",
            "platform.prepare",
            "capability.close",
            "agent.dispose",
            "identity.release",
            "broker.close",
            "complete",
        ]
    );
}

#[test]
fn receipt_rejection_and_timeout_dispose_before_releasing_identity_without_start_or_enable() {
    for (name, behavior, timeout, expected) in [
        (
            "rejected",
            AppendBehavior::Reject,
            SUPERVISOR_TIMEOUT,
            SupervisorError::DurabilityUnavailable,
        ),
        (
            "timeout",
            AppendBehavior::NeverComplete,
            Duration::from_millis(25),
            SupervisorError::BrokerTimeout,
        ),
    ] {
        let setup = setup(true, |_| {}, behavior, PlatformBehavior::default(), timeout);
        let (receiver, completion_count) = begin_launch(&setup, CONTROLLER_UID);
        assert_eq!(
            receiver
                .recv_timeout(CALLBACK_TIMEOUT)
                .unwrap_or_else(|_| panic!("durability {name} did not complete"))
                .err()
                .expect("durability must gate launch"),
            expected,
        );
        let events = event_snapshot(&setup.events);
        assert!(!events.iter().any(|event| event == "agent.start"));
        assert!(!events.iter().any(|event| event == "capability.enable"));
        let close = events
            .iter()
            .position(|event| event == "capability.close")
            .expect("capability closes");
        let dispose = events
            .iter()
            .position(|event| event == "agent.dispose")
            .expect("prepared tree is disposed");
        let release = events
            .iter()
            .position(|event| event == "identity.release")
            .expect("identity releases");
        assert!(close < dispose && dispose < release, "events: {events:?}");
        assert_eq!(completion_count.load(Ordering::SeqCst), 1);
        if matches!(behavior, AppendBehavior::NeverComplete) {
            setup.broker.acknowledge();
            assert_eq!(completion_count.load(Ordering::SeqCst), 1);
            let events = event_snapshot(&setup.events);
            assert!(!events.iter().any(|event| event == "agent.start"));
            assert!(!events.iter().any(|event| event == "capability.enable"));
        }
    }
}

#[test]
fn unproved_failure_cleanup_poisons_the_identity_instead_of_releasing_it() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Reject,
        PlatformBehavior {
            dispose_fails: true,
            ..PlatformBehavior::default()
        },
        SUPERVISOR_TIMEOUT,
    );
    let (receiver, completion_count) = begin_launch(&setup, CONTROLLER_UID);

    assert_eq!(
        receiver
            .recv_timeout(CALLBACK_TIMEOUT)
            .expect("failed cleanup completes")
            .err()
            .expect("unproved cleanup must fail"),
        SupervisorError::CleanupUnproven,
    );
    let events = event_snapshot(&setup.events);
    assert!(
        events
            .windows(3)
            .any(|events| { events == ["capability.close", "agent.dispose", "identity.poison"] })
    );
    assert!(!events.iter().any(|event| event == "identity.release"));
    assert!(
        !events
            .iter()
            .any(|event| event == "identity.dropped_without_release")
    );
    assert_eq!(completion_count.load(Ordering::SeqCst), 1);
}

#[test]
fn launch_frame_is_one_bounded_canonical_line_and_preserves_buffered_acp_bytes() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let trailing = vec![0x00, 0xff, b'{', b'}', b'\n'];
    let mut bytes = setup.request.canonical_bytes();
    bytes.push(b'\n');
    bytes.extend_from_slice(&trailing);
    let mut reader = BufReader::with_capacity(bytes.len(), Cursor::new(bytes));

    assert_eq!(
        read_launch_frame(&mut reader).expect("canonical launch line parses"),
        setup.request,
    );
    let mut remaining = Vec::new();
    reader
        .read_to_end(&mut remaining)
        .expect("buffered ACP bytes remain readable");
    assert_eq!(remaining, trailing);

    let canonical = setup.request.canonical_bytes();
    let mut noncanonical = vec![b' '];
    noncanonical.extend_from_slice(&canonical);
    noncanonical.push(b'\n');
    let mut unknown = String::from_utf8(canonical.clone())
        .expect("request is UTF-8")
        .replace(
            r#""envelope_revision":7"#,
            r#""envelope_revision":7,"command":"id""#,
        )
        .into_bytes();
    unknown.push(b'\n');
    let mut oversized = vec![b'x'; MAX_REQUEST_BYTES + 1];
    oversized.push(b'\n');

    for hostile in [Vec::new(), canonical, noncanonical, unknown, oversized] {
        assert!(
            read_launch_frame(&mut BufReader::new(Cursor::new(hostile))).is_err(),
            "hostile or unterminated frame must fail closed",
        );
    }
}

#[test]
fn parent_death_probe_helper() {
    let Some(ready) = env::var_os("LOUISELM_PARENT_DEATH_PROBE") else {
        return;
    };
    // This is a separate process, not a post-fork closure in the launcher.
    rustix::process::set_parent_process_death_signal(Some(rustix::process::Signal::KILL))
        .expect("the kernel arms the parent-thread death signal");
    let _ready = UnixStream::connect(ready).expect("report the armed signal before handoff");
    let error = Command::new("/bin/cat").stdout(Stdio::null()).exec();
    panic!("parent-death probe exec failed: {error}");
}

fn spawn_parent_death_probe(ready: &Path) -> Child {
    let listener = UnixListener::bind(ready).expect("probe readiness socket opens");
    listener.set_nonblocking(true).unwrap();
    let mut child = Command::new(env::current_exe().unwrap())
        .args(["--exact", "parent_death_probe_helper", "--nocapture"])
        .env("LOUISELM_PARENT_DEATH_PROBE", ready)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .expect("parent-death probe starts");
    let deadline = Instant::now() + CALLBACK_TIMEOUT;
    loop {
        match listener.accept() {
            Ok(_) => return child,
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(1));
            }
            Err(error) => {
                let _ = child.kill();
                child.wait().expect("failed probe is reaped");
                panic!("parent-death probe did not become ready: {error}");
            }
        }
    }
}

#[test]
fn launch_keeps_parent_death_bound_children_alive_until_lifecycle_cleanup() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let (sender, receiver) = mpsc::sync_channel(1);
    let probe_ready = setup.fixture.path("parent-death-ready.sock");
    setup
        .supervisor
        .launch(
            setup.request.clone(),
            CONTROLLER_UID,
            NOW_MS,
            Box::new(move |result| {
                // Ported from 7ccb05a: preserve both sides of the kernel lifetime
                // check without its now-forbidden unsafe pre_exec hook.
                let mut child = spawn_parent_death_probe(&probe_ready);
                let input = child.stdin.take().expect("probe stdin stays open");
                let (exit_sender, exit_receiver) = mpsc::sync_channel(1);
                let waiter = thread::spawn(move || {
                    exit_sender
                        .send(child.wait().expect("parent-death probe is reaped"))
                        .expect("test receives probe exit");
                });
                sender
                    .send((result, input, exit_receiver, waiter))
                    .expect("test receives launch completion");
            }),
        )
        .expect("launch registers");
    setup.broker.wait_for_append(0);
    setup.broker.acknowledge();
    setup.broker.wait_for_append(1);
    setup.broker.acknowledge();
    let (session, probe_input, probe_exit, probe_waiter) = receiver
        .recv_timeout(CALLBACK_TIMEOUT)
        .expect("launch completes");
    let (controller_input, relay_receiver, relay_worker) =
        begin_session_relay(session.expect("launch succeeds"));
    let early_exit = probe_exit.recv_timeout(Duration::from_millis(100));

    // Always settle the Session and reap the probe, including on regression failure.
    finish_session_relay(&setup, controller_input, relay_receiver, relay_worker);
    let terminal_exit = probe_exit.recv_timeout(CALLBACK_TIMEOUT);
    drop(probe_input);
    probe_waiter.join().expect("probe waiter finishes");

    assert!(
        matches!(early_exit, Err(mpsc::RecvTimeoutError::Timeout)),
        "launch completion killed the still-owned child: {early_exit:?}",
    );
    assert_eq!(
        terminal_exit
            .expect("the spawning coordinator exits after lifecycle cleanup")
            .signal(),
        Some(9),
        "the parent-death safety contract remains enabled",
    );
}

fn initial_user_namespace() -> bool {
    fs::read_to_string("/proc/self/uid_map")
        .ok()
        .and_then(|text| {
            text.lines()
                .map(|line| {
                    let values = line
                        .split_whitespace()
                        .map(str::parse)
                        .collect::<Result<Vec<u32>, _>>()
                        .map_err(|_| ())?;
                    values.try_into().map_err(|_| ())
                })
                .collect::<Result<Vec<[u32; 3]>, _>>()
                .ok()
        })
        .is_some_and(|rows| rows == [[0, 0, u32::MAX]])
}

#[test]
fn privileged_supervisor_launches_agent_under_the_assigned_outer_identity() {
    for ending in [
        PrivilegedEnding::NaturalExit,
        PrivilegedEnding::ControllerEof,
        PrivilegedEnding::RelayFailure,
    ] {
        privileged_supervisor_case(ending);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PrivilegedEnding {
    NaturalExit,
    ControllerEof,
    RelayFailure,
}

#[expect(
    clippy::too_many_lines,
    reason = "One privileged supervisor launches agent under the assigned outer identity scenario keeps its causal steps and assertions together."
)]
fn privileged_supervisor_case(ending: PrivilegedEnding) {
    let Some(assigned) = env::var_os("LOUISELM_TEST_HOST_ID") else {
        eprintln!("skipping: set LOUISELM_TEST_HOST_ID in the privileged fixture");
        return;
    };
    let assigned = assigned
        .to_string_lossy()
        .parse::<u32>()
        .expect("LOUISELM_TEST_HOST_ID is numeric");
    let operator_uid = env::var("LOUISELM_TEST_OPERATOR_UID")
        .expect("the privileged fixture names its non-root operator")
        .parse::<u32>()
        .expect("LOUISELM_TEST_OPERATOR_UID is numeric");
    assert_eq!(rustix::process::geteuid().as_raw(), 0);
    assert_ne!(operator_uid, 0, "the operator must remain unprivileged");
    assert_ne!(assigned, 0, "the Agent identity must be non-root");
    assert_ne!(assigned, operator_uid, "the Agent must not be the operator");
    assert!(
        env::var_os("LOUISELM_REQUIRE_INITIAL_HOST_IDENTITY").is_some(),
        "the privileged fixture must explicitly require the initial user namespace",
    );
    assert!(
        initial_user_namespace(),
        "outer credential acceptance must run in the initial user namespace",
    );

    let mut setup = setup(
        true,
        |authorization| {
            authorization.controller_uid = operator_uid;
            authorization.assigned_uid = assigned;
            authorization.assigned_gid = assigned;
        },
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        PRIVILEGED_TIMEOUT,
    );
    // Exercise the measured executable itself, not a script whose interpreter
    // or subsequent exec would name another principal. head exits after one
    // ACP line while cat remains alive for controller/relay failure cases.
    let (executable, arguments) = if ending == PrivilegedEnding::NaturalExit {
        ("/bin/head", vec!["-n", "1"])
    } else {
        ("/bin/cat", vec![])
    };
    fs::copy(executable, setup.fixture.path("runtime/bin/agent")).unwrap();
    write_registry(
        &setup.fixture.path("registry"),
        &setup.fixture.path("runtime"),
    );
    write_file(
        &setup.fixture.path("registry/agents.json"),
        &serde_json::json!({
            "schema": "louiselm.launch.registry/1",
            "entries": [{"id": "demo", "provider": "demo-provider", "runtime_id": "demo-runtime",
                "arguments": arguments, "environment": {}}]
        })
        .to_string(),
    );
    setup.registry = Arc::new(Registry::open(&setup.fixture.path("registry")).unwrap());
    for path in [
        setup.fixture.path(""),
        setup.fixture.path("runtime"),
        setup.fixture.path("runtime/bin"),
        setup.fixture.path("runtime/lib"),
    ] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))
            .expect("the assigned identity can traverse the runtime fixture");
    }
    fs::create_dir_all(&setup.sessions_root).expect("the Sessions root is creatable");
    fs::set_permissions(&setup.sessions_root, fs::Permissions::from_mode(0o711))
        .expect("the Sessions root has its fixed mode");

    let expected_identity = Identity {
        slot: 3,
        uid: assigned,
        gid: assigned,
    };
    let observation = Arc::new(Mutex::new(None));
    let bwrap = Path::new("/usr/bin/bwrap");
    let platform = Arc::new(BubblewrapLaunchPlatform {
        events: Arc::clone(&setup.events),
        expected_identity,
        backend: BubblewrapBackend::at(bwrap)
            .with_bootstrap(Path::new(env!("CARGO_BIN_EXE_louiselm-launch"))),
        backend_id: Digest::of(&fs::read(bwrap).expect("Bubblewrap is installed")).to_string(),
        capability_root: setup.fixture.path("real-capability"),
        observation: Arc::clone(&observation),
    });
    let supervisor = LaunchSupervisor::new(
        setup.broker.clone(),
        setup.signer.clone(),
        platform,
        Arc::clone(&setup.registry),
        setup.sessions_root.clone(),
        PRIVILEGED_TIMEOUT,
    );
    let (receiver, completion_count) =
        begin_launch_on(&supervisor, &setup.request, &setup.events, operator_uid);
    setup.broker.wait_for_append(0);
    setup.broker.acknowledge();
    setup.broker.wait_for_append(1);
    setup.broker.acknowledge();
    let session = receiver
        .recv_timeout(CALLBACK_TIMEOUT)
        .expect("the privileged launch completes")
        .expect("the real Bubblewrap Session launches");
    let agent_pid = session.capability_binding().agent_pid;
    let actual_agent =
        observe_sandbox_leader(agent_pid, lock(&observation).as_ref().unwrap().monitor_pid)
            .unwrap();
    assert!(actual_agent.uids.iter().all(|uid| *uid == assigned));
    assert!(actual_agent.gids.iter().all(|gid| *gid == assigned));
    assert!(actual_agent.groups.is_empty());
    assert_ne!(
        agent_pid,
        lock(&observation).as_ref().unwrap().sandbox_leader_pid
    );
    assert_eq!(rustix::process::geteuid().as_raw(), 0);

    let acp = b"composite ACP bytes\n".to_vec();
    let output = Arc::new(Mutex::new(Vec::new()));
    let (mut controller_input, supervisor_input) =
        UnixStream::pair().expect("privileged relay socket pair opens");
    let worker_output = Arc::clone(&output);
    let (mut output_reader, output_writer) = UnixStream::pair().unwrap();
    let output_fault = output_reader.try_clone().unwrap();
    let (relay_sender, relay_receiver) = mpsc::sync_channel(1);
    let relay_worker = thread::Builder::new()
        .name("privileged-launch-relay".to_owned())
        .spawn(move || {
            let output_worker = thread::spawn(move || {
                io::copy(&mut output_reader, &mut SharedWriter(worker_output)).unwrap();
            });
            let stdio = RelayStdio::new(
                BufReader::new(fs::File::from(OwnedFd::from(supervisor_input))),
                fs::File::from(OwnedFd::from(output_writer)),
            )
            .unwrap();
            let result = session.relay_stdio(stdio);
            output_worker.join().expect("output observer finishes");
            relay_sender
                .send(result)
                .expect("test receives privileged relay completion");
        })
        .expect("privileged relay worker starts");
    let controller_write = controller_input.write_all(&acp);
    let output_deadline = Instant::now() + PRIVILEGED_TIMEOUT;
    while *lock(&output) != acp && !relay_worker.is_finished() && Instant::now() < output_deadline {
        thread::sleep(Duration::from_millis(10));
    }
    let echoed_before_eof = *lock(&output) == acp;
    let mut receipts = Vec::new();
    let fault_write = if ending == PrivilegedEnding::RelayFailure {
        // The real relay now hits EPIPE on its next output write, while the
        // controller input stays open and the Agent remains alive in cat.
        output_fault.shutdown(Shutdown::Read).unwrap();
        Some(controller_input.write_all(b"trigger broken output\n"))
    } else {
        None
    };
    let ended_before_eof = if ending == PrivilegedEnding::ControllerEof {
        false
    } else {
        settle_privileged_relay(&setup, &relay_worker, &mut receipts);
        relay_worker.is_finished()
    };
    // Also settle cleanup on failure; keep the pre-EOF observations for verdicts.
    let _ = controller_input.shutdown(Shutdown::Write);
    settle_privileged_relay(&setup, &relay_worker, &mut receipts);
    drop(controller_input);
    let relay_result = relay_receiver
        .recv_timeout(PRIVILEGED_TIMEOUT)
        .unwrap_or_else(|error| {
            panic!(
                "terminal privileged relay failed: {error:?}; receipts: {receipts:?}; events: {:?}",
                event_snapshot(&setup.events),
            )
        });
    relay_worker
        .join()
        .expect("terminal privileged relay worker finishes");
    controller_write.expect("ACP bytes reached the real Agent");
    if let Some(result) = fault_write {
        result.expect("fault trigger reached the real Agent");
    }
    let running = SignedReceipt::parse_canonical(&setup.broker.receipts()[1]).unwrap();
    let mut previous = &running;
    for receipt in &receipts {
        assert_eq!(receipt.payload.sequence, previous.payload.sequence + 1);
        assert_eq!(
            receipt.payload.previous_receipt_digest,
            Some(previous.digest().to_string()),
        );
        previous = receipt;
    }
    let terminal = receipts
        .last()
        .expect("a terminal receipt was acknowledged");
    assert_eq!(terminal.payload.resulting_state, SessionState::Terminal);
    if ending == PrivilegedEnding::NaturalExit {
        assert!(
            ended_before_eof,
            "natural Agent exit must precede controller EOF"
        );
        assert_eq!(
            terminal.payload.outcome,
            ReceiptOutcome::Disposal {
                authority: ReceiptAuthority::ProcessExited {
                    classification: ProcessExitClassification::Success,
                },
            },
        );
    }
    if ending == PrivilegedEnding::RelayFailure {
        assert!(
            ended_before_eof,
            "broken output must terminate before controller EOF"
        );
        assert_eq!(
            terminal.payload.outcome,
            ReceiptOutcome::Disposal {
                authority: ReceiptAuthority::Cause {
                    cause: ReceiptCause::RelayFailed
                },
            }
        );
    }
    assert_eq!(
        relay_result,
        if ending == PrivilegedEnding::RelayFailure {
            Err(SupervisorError::RelayFailed)
        } else {
            Ok(0)
        },
        "unexpected production relay outcome; events: {:?}",
        event_snapshot(&setup.events),
    );
    assert!(
        echoed_before_eof,
        "the real Agent did not echo before controller EOF; events: {:?}",
        event_snapshot(&setup.events),
    );
    assert_eq!(*lock(&output), acp);
    assert_eq!(completion_count.load(Ordering::SeqCst), 1);

    let observed = lock(&observation)
        .take()
        .expect("the live Agent was observed through host /proc");
    assert_ne!(observed.sandbox_leader_pid, observed.monitor_pid);
    assert!(observed.uids.iter().all(|uid| *uid == assigned));
    assert!(observed.gids.iter().all(|gid| *gid == assigned));
    assert!(
        observed.groups.is_empty(),
        "the Agent inherited no supplementary host groups",
    );
}

fn settle_privileged_relay(
    setup: &Setup,
    worker: &thread::JoinHandle<()>,
    receipts: &mut Vec<SignedReceipt>,
) {
    let deadline = Instant::now() + PRIVILEGED_TIMEOUT;
    while !worker.is_finished() && Instant::now() < deadline {
        let (receipt_ready, settlement) = {
            let state = lock(&setup.broker.state);
            (
                state.session_receipts.len() > receipts.len()
                    && state.pending_session_request.is_some(),
                state
                    .pending_controller_loss_settlement
                    .as_ref()
                    .and_then(|_| state.controller_loss_settlements.last().cloned()),
            )
        };
        if receipt_ready {
            let index = receipts.len();
            receipts.push(
                SignedReceipt::parse_canonical(&setup.broker.session_receipt_bytes(index)).unwrap(),
            );
            setup
                .broker
                .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
                    setup.broker.session_receipt_acknowledgement(index),
                ));
        }
        if let Some(settlement) = settlement {
            setup
                .broker
                .complete_controller_loss_settlement(Ok(controller_loss_acknowledgement(
                    &settlement,
                    ControllerLossDisposition::Recoverable {
                        acp_recovery_reference: "privileged-controller-recovery".to_owned(),
                        attention_projection_id: "privileged-controller-attention".to_owned(),
                    },
                )));
        }
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn controller_eof_from_a_durably_parked_session_settles_the_existing_park_head() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let session = complete_launch(&setup);
    let (controller_input, relay_receiver, relay_worker, park_receipt) =
        park_launched_session(&setup, session);
    let park_bytes = setup.broker.session_receipt_bytes(0);
    let park_head = ReceiptHead {
        sequence: park_receipt.payload.sequence,
        digest: Digest::of(&park_bytes).to_string(),
    };

    setup
        .platform
        .send_running_event(RunningAgentEvent::ControllerEof);
    setup.broker.wait_for_controller_loss_settlement(0);
    let settlement = setup.broker.controller_loss_settlement(0);
    assert_eq!(settlement.parked_head, park_head);
    assert_eq!(
        setup.broker.session_receipt_count(),
        1,
        "ControllerEof from Parked must not emit an invalid Parked-to-Parked Park receipt",
    );
    assert_eq!(
        event_count(&setup.events, "agent.park"),
        1,
        "the already frozen process tree is not frozen again",
    );

    setup
        .broker
        .complete_controller_loss_settlement(Ok(controller_loss_acknowledgement(
            &settlement,
            ControllerLossDisposition::Recoverable {
                acp_recovery_reference: "parked-controller-recovery".to_owned(),
                attention_projection_id: "parked-controller-attention".to_owned(),
            },
        )));
    setup.broker.wait_for_session_receipt(1);
    let disposal_bytes = setup.broker.session_receipt_bytes(1);
    let disposal_receipt = SignedReceipt::parse_canonical(&disposal_bytes)
        .expect("controller-loss Disposal receipt is canonical");
    assert_eq!(
        disposal_receipt.payload.outcome,
        ReceiptOutcome::Disposal {
            authority: ReceiptAuthority::Cause {
                cause: ReceiptCause::ControllerLost,
            },
        },
    );
    let chain = [
        setup.broker.receipt_bytes(0),
        setup.broker.receipt_bytes(1),
        park_bytes,
        disposal_bytes,
    ]
    .map(|bytes| SignedReceipt::parse_canonical(&bytes).expect("receipt chain is canonical"));
    verify_chain(
        &chain,
        &ChainAnchor {
            session_id: setup.request.session_id.clone(),
            run_id: setup.request.run_id.clone(),
            release_id: setup.signer.release_id().to_owned(),
            signing_key_id: setup.signer.signing_key_id().to_owned(),
        },
        |_, _, _| true,
    )
    .expect("controller loss from Parked preserves a valid lifecycle chain");

    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(1),
        ));
    finish_terminal_session_relay(controller_input, relay_receiver, relay_worker);
}

#[derive(Clone, Copy, Debug)]
enum InterruptedReconciliation {
    SignedSuffix,
    DeferredReceipt,
}

#[expect(
    clippy::too_many_lines,
    reason = "One assert disconnect during reconciliation starts a new epoch scenario keeps its causal steps and assertions together."
)]
fn assert_disconnect_during_reconciliation_starts_a_new_epoch(case: InterruptedReconciliation) {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let timer = Arc::new(FakeTimer::new(Arc::clone(&setup.events)));
    let timer_port: Arc<dyn SupervisorTimer> = timer.clone();
    let supervisor = setup.fresh_supervisor_with_timer(timer_port);
    let session = complete_launch_on(&setup, &supervisor);
    let initial_head = ReceiptHead {
        sequence: session.receipt().payload.sequence,
        digest: Digest::of(&session.receipt().canonical_bytes()).to_string(),
    };
    let (controller_input, relay_receiver, relay_worker) = begin_session_relay(session);
    let park = lifecycle_request(
        &setup,
        &format!("park-before-interrupted-{case:?}"),
        &format!("park-before-interrupted-{case:?}-authorization"),
        initial_head.sequence,
    );

    setup.broker.wait_for_session_request();
    match case {
        InterruptedReconciliation::SignedSuffix => setup
            .broker
            .set_session_receipt_send_behavior(SessionReceiptSendBehavior::Reject),
        InterruptedReconciliation::DeferredReceipt => {
            setup.signer.fail_on_call(setup.signer.payloads().len());
        }
    }
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(park.clone()));
    if matches!(case, InterruptedReconciliation::SignedSuffix) {
        setup.broker.wait_for_session_receipt(0);
    }
    let failed = setup.broker.wait_for_session_response(&park.request_id, 0);
    assert!(matches!(failed.result, ResponseResult::Error { .. }));
    setup
        .broker
        .set_session_receipt_send_behavior(SessionReceiptSendBehavior::Complete);
    setup.signer.fail_on_call(usize::MAX);
    let first_repair_index = setup.broker.session_receipt_count();

    setup.broker.wait_for_session_request();
    setup.broker.disconnect_session();
    setup.broker.wait_for_reconnect(0);
    let mut first_reconnect = setup.broker.reconnect(0);
    first_reconnect.sequence = initial_head.sequence;
    first_reconnect
        .receipt_digest
        .clone_from(&initial_head.digest);
    setup.broker.complete_reconnect(first_reconnect);
    setup.broker.wait_for_session_receipt(first_repair_index);
    let first_repair_bytes = setup.broker.session_receipt_bytes(first_repair_index);
    let first_repair = SignedReceipt::parse_canonical(&first_repair_bytes)
        .expect("first repaired Park receipt is canonical");
    assert_eq!(first_repair.payload.sequence, initial_head.sequence + 1);
    assert!(matches!(
        first_repair.payload.outcome,
        ReceiptOutcome::Park {
            authority: ReceiptAuthority::Authorized(_),
        }
    ));
    let repairing =
        request_supervisor_status(&setup, &format!("status-before-second-{case:?}-disconnect"));
    assert_eq!(repairing.state, SessionState::Parked);
    assert_eq!(repairing.broker_connection, BrokerConnection::Reconciling);
    assert_eq!(repairing.pending_receipt_count, 1);

    setup.broker.wait_for_session_request();
    setup.broker.disconnect_session();
    setup.broker.wait_for_reconnect(1);
    let mut second_reconnect = setup.broker.reconnect(1);
    assert_eq!(second_reconnect.sequence, first_repair.payload.sequence);
    assert_eq!(
        second_reconnect.receipt_digest,
        Digest::of(&first_repair_bytes).to_string(),
    );
    second_reconnect.sequence = initial_head.sequence;
    second_reconnect
        .receipt_digest
        .clone_from(&initial_head.digest);
    setup.broker.complete_reconnect(second_reconnect);

    let second_repair_index = first_repair_index + 1;
    setup.broker.wait_for_session_receipt(second_repair_index);
    assert_eq!(
        setup.broker.session_receipt_bytes(second_repair_index),
        first_repair_bytes,
        "a new connection epoch replays the exact interrupted receipt",
    );
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup
                .broker
                .session_receipt_acknowledgement(second_repair_index),
        ));
    let repaired =
        request_supervisor_status(&setup, &format!("status-after-second-{case:?}-reconnect"));
    assert_eq!(repaired.state, SessionState::Parked);
    assert_eq!(repaired.broker_connection, BrokerConnection::Connected);
    assert_eq!(repaired.pending_receipt_count, 0);
    assert_eq!(repaired.launcher_head, repaired.broker_head);
    assert_eq!(event_count(&setup.events, "agent.park"), 1);

    finish_session_relay(&setup, controller_input, relay_receiver, relay_worker);
}

#[test]
fn disconnect_while_awaiting_a_suffix_ack_starts_a_new_reconnect_epoch() {
    assert_disconnect_during_reconciliation_starts_a_new_epoch(
        InterruptedReconciliation::SignedSuffix,
    );
}

#[test]
fn disconnect_while_awaiting_a_deferred_receipt_ack_starts_a_new_reconnect_epoch() {
    assert_disconnect_during_reconciliation_starts_a_new_epoch(
        InterruptedReconciliation::DeferredReceipt,
    );
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "One resume ack cannot reopen a session while its process exit receipt is signing scenario keeps its causal steps and assertions together."
)]
fn resume_ack_cannot_reopen_a_session_while_its_process_exit_receipt_is_signing() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let session = complete_launch(&setup);
    let (controller_input, relay_receiver, relay_worker, _) =
        park_launched_session(&setup, session);
    let resume = resume_request(&setup, "resume-before-process-exit");
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(resume.clone()));
    setup.broker.wait_for_session_receipt(1);
    let resume_bytes = setup.broker.session_receipt_bytes(1);
    let resume_receipt =
        SignedReceipt::parse_canonical(&resume_bytes).expect("pending Resume receipt is canonical");
    let resume_head = ReceiptHead {
        sequence: resume_receipt.payload.sequence,
        digest: Digest::of(&resume_bytes).to_string(),
    };

    setup.signer.hold_on_call(setup.signer.payloads().len());
    setup
        .platform
        .send_running_event(RunningAgentEvent::ProcessExited(
            ProcessExitClassification::Failure,
        ));
    let queued = request_supervisor_status(&setup, "status-with-exit-behind-resume-ack");
    assert_eq!(queued.state, SessionState::Terminal);
    assert_eq!(queued.channel_state, ChannelState::Closed);
    assert_eq!(
        queued.process_exit,
        Some(ProcessExitClassification::Failure)
    );

    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(1),
        ));
    let completed = setup
        .broker
        .wait_for_session_response(&resume.request_id, 0);
    let ResponseResult::Receipt { receipt } = completed.result else {
        panic!("the exact Resume ACK completes its original request");
    };
    assert_eq!(receipt.canonical_bytes(), resume_bytes);
    setup.signer.wait_for_held_call();

    let signing_exit = request_supervisor_status(&setup, "status-while-exit-receipt-signs");
    assert_eq!(signing_exit.state, SessionState::Terminal);
    assert_eq!(signing_exit.channel_state, ChannelState::Closed);
    assert_eq!(signing_exit.launcher_head.as_ref(), Some(&resume_head));
    assert_eq!(signing_exit.broker_head.as_ref(), Some(&resume_head));
    assert_eq!(signing_exit.pending_receipt_count, 1);
    assert_eq!(
        signing_exit.process_exit,
        Some(ProcessExitClassification::Failure)
    );
    let pending = signing_exit
        .pending_operation
        .expect("process-exit Disposal remains observable while signing");
    assert_eq!(pending.action, PendingAction::Disposal);
    assert_eq!(pending.phase, PendingPhase::Signing);
    assert_eq!(
        event_count(&setup.events, "capability.enable"),
        1,
        "a late Resume ACK cannot re-enable a terminal Session",
    );

    setup.signer.release_held_call();
    setup.broker.wait_for_session_receipt(2);
    let exit_receipt = SignedReceipt::parse_canonical(&setup.broker.session_receipt_bytes(2))
        .expect("serialized process-exit receipt is canonical");
    assert_eq!(
        exit_receipt.payload.outcome,
        ReceiptOutcome::Disposal {
            authority: ReceiptAuthority::ProcessExited {
                classification: ProcessExitClassification::Failure,
            },
        }
    );
    assert_eq!(
        exit_receipt.payload.previous_receipt_digest,
        Some(resume_head.digest),
    );
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(2),
        ));
    assert_eq!(
        relay_receiver
            .recv_timeout(CALLBACK_TIMEOUT)
            .expect("durable process-exit receipt completes its owner")
            .expect("terminal cleanup succeeds"),
        1,
    );
    drop(controller_input);
    relay_worker.join().expect("terminal relay worker finishes");
}

#[test]
fn failed_controller_loss_settlement_retries_after_exact_head_reconnect_without_widening() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let timer = Arc::new(FakeTimer::new(Arc::clone(&setup.events)));
    let timer_port: Arc<dyn SupervisorTimer> = timer.clone();
    let supervisor = setup.fresh_supervisor_with_timer(timer_port);
    let session = complete_launch_on(&setup, &supervisor);
    let (controller_input, relay_receiver, relay_worker, park_receipt, _) =
        begin_controller_loss_settlement(&setup, session);
    let park_bytes = setup.broker.session_receipt_bytes(0);
    let park_head = ReceiptHead {
        sequence: park_receipt.payload.sequence,
        digest: Digest::of(&park_bytes).to_string(),
    };

    setup
        .broker
        .complete_controller_loss_settlement(Err(SupervisorError::DurabilityUnavailable));
    let failed = request_supervisor_status(&setup, "status-after-controller-settlement-failure");
    assert_eq!(failed.state, SessionState::Parked);
    assert_eq!(failed.channel_state, ChannelState::Revoked);
    assert_eq!(
        event_count(&setup.events, "broker.settle_controller_loss"),
        1
    );

    setup.broker.wait_for_session_request();
    setup.broker.disconnect_session();
    setup.broker.wait_for_reconnect(0);
    let reconnect = setup.broker.reconnect(0);
    assert_eq!(reconnect.sequence, park_head.sequence);
    assert_eq!(reconnect.receipt_digest, park_head.digest);
    setup.broker.complete_reconnect(reconnect);

    setup.broker.wait_for_controller_loss_settlement(1);
    let retry = setup.broker.controller_loss_settlement(1);
    assert_eq!(retry.parked_head, park_head);
    let blocked = request_supervisor_status(&setup, "status-awaiting-retried-settlement");
    assert_eq!(blocked.state, SessionState::Parked);
    assert_eq!(blocked.channel_state, ChannelState::Revoked);

    let resume = resume_request(&setup, "resume-before-retried-settlement");
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(resume.clone()));
    let response = setup
        .broker
        .wait_for_session_response(&resume.request_id, 0);
    let ResponseResult::Error { error } = response.result else {
        panic!("widening stays blocked while controller-loss settlement is unresolved");
    };
    assert_eq!(error.code, ErrorCode::DurabilityUnavailable);
    assert_eq!(event_count(&setup.events, "agent.resume"), 0);
    assert_eq!(event_count(&setup.events, "capability.enable"), 1);

    setup
        .broker
        .complete_controller_loss_settlement(Ok(controller_loss_acknowledgement(
            &retry,
            ControllerLossDisposition::NoRecovery {
                attention_projection_id: "retry-controller-loss-attention".to_owned(),
            },
        )));
    setup.broker.wait_for_session_receipt(1);
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(1),
        ));
    finish_terminal_session_relay(controller_input, relay_receiver, relay_worker);
}

#[test]
fn interrupt_cannot_replace_an_active_controller_loss_settlement() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let session = complete_launch(&setup);
    let (controller_input, relay_receiver, relay_worker, park_receipt, settlement) =
        begin_controller_loss_settlement(&setup, session);
    let sign_count = setup.signer.payloads().len();
    setup.signer.fail_on_call(sign_count);
    let interrupt = interrupt_request(
        &setup,
        "interrupt-during-controller-loss-settlement",
        SessionState::Parked,
        park_receipt.payload.sequence,
    );

    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(interrupt.clone()));
    let response = setup
        .broker
        .wait_for_session_response(&interrupt.request_id, 0);
    let ResponseResult::Error { error } = response.result else {
        panic!("controller-loss settlement rejects competing lifecycle authority");
    };
    assert_eq!(error.code, ErrorCode::DurabilityUnavailable);
    assert_eq!(event_count(&setup.events, "agent.interrupt"), 0);
    assert_eq!(setup.signer.payloads().len(), sign_count);
    assert_eq!(setup.broker.session_receipt_count(), 1);
    let retained = request_supervisor_status(&setup, "status-after-blocked-settlement-interrupt");
    assert_eq!(retained.state, SessionState::Parked);
    assert_eq!(retained.channel_state, ChannelState::Revoked);
    assert_eq!(retained.pending_operation, None);
    assert_eq!(
        event_count(&setup.events, "broker.settle_controller_loss"),
        1
    );

    setup.signer.fail_on_call(usize::MAX);
    setup
        .broker
        .complete_controller_loss_settlement(Ok(controller_loss_acknowledgement(
            &settlement,
            ControllerLossDisposition::Recoverable {
                acp_recovery_reference: "retained-controller-recovery".to_owned(),
                attention_projection_id: "retained-controller-attention".to_owned(),
            },
        )));
    setup.broker.wait_for_session_receipt(1);
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(1),
        ));
    finish_terminal_session_relay(controller_input, relay_receiver, relay_worker);
}

#[test]
fn failed_terminal_response_send_finishes_only_after_exact_reconnect_retry() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let timer = Arc::new(FakeTimer::new(Arc::clone(&setup.events)));
    let timer_port: Arc<dyn SupervisorTimer> = timer.clone();
    let supervisor = setup.fresh_supervisor_with_timer(timer_port);
    let session = complete_launch_on(&setup, &supervisor);
    let (controller_input, relay_receiver, relay_worker) = begin_session_relay(session);
    let disposal = disposal_request(&setup, "retry-terminal-response-after-reconnect");
    setup.broker.hold_session_response(&disposal.request_id);

    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(disposal.clone()));
    setup.broker.wait_for_session_receipt(0);
    let disposal_bytes = setup.broker.session_receipt_bytes(0);
    let disposal_head = ReceiptHead {
        sequence: 2,
        digest: Digest::of(&disposal_bytes).to_string(),
    };
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(0),
        ));
    let first = setup
        .broker
        .wait_for_session_response(&disposal.request_id, 0);
    setup.broker.wait_for_held_session_response();
    let mechanics = (
        event_count(&setup.events, "agent.dispose"),
        event_count(&setup.events, "identity.release"),
        setup.signer.payloads().len(),
    );

    setup
        .broker
        .release_session_response_with(Err(SupervisorError::BrokerUnavailable));
    setup.broker.wait_for_reconnect(0);
    setup.broker.disconnect_session();
    let reconnect = setup.broker.reconnect(0);
    assert_eq!(reconnect.sequence, disposal_head.sequence);
    assert_eq!(reconnect.receipt_digest, disposal_head.digest);
    setup.broker.complete_reconnect(reconnect);

    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(disposal.clone()));
    let replay = setup
        .broker
        .wait_for_session_response(&disposal.request_id, 1);
    assert_eq!(replay.canonical_bytes(), first.canonical_bytes());
    setup.broker.wait_for_held_session_response();
    assert_eq!(
        (
            event_count(&setup.events, "agent.dispose"),
            event_count(&setup.events, "identity.release"),
            setup.signer.payloads().len(),
        ),
        mechanics,
        "exact retry cannot repeat terminal mechanics or signing",
    );
    assert_eq!(setup.broker.session_receipt_count(), 1);
    assert!(matches!(
        relay_receiver.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    assert!(!setup.broker.is_closed());

    setup.broker.release_session_response();
    assert_eq!(
        relay_receiver
            .recv_timeout(CALLBACK_TIMEOUT)
            .expect("current-epoch terminal response finishes its owner")
            .expect("terminal cleanup succeeds"),
        0,
    );
    assert!(setup.broker.is_closed());
    drop(controller_input);
    relay_worker.join().expect("terminal relay worker finishes");
}

#[test]
fn evicted_successful_request_id_remains_a_conflict_without_repeating_its_mechanic() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let session = complete_launch(&setup);
    let (controller_input, relay_receiver, relay_worker) = begin_session_relay(session);
    let mut first_request = None;
    let successful_requests = 65usize;

    for index in 0..successful_requests {
        let request = interrupt_request(
            &setup,
            &format!("successful-cache-pressure-{index}"),
            SessionState::Running,
            u64::try_from(index).expect("bounded test index fits u64") + 1,
        );
        if index == 0 {
            first_request = Some(request.clone());
        }
        setup.broker.wait_for_session_request();
        setup
            .broker
            .deliver_session_request(ProtocolMessage::Lifecycle(request.clone()));
        setup.broker.wait_for_session_receipt(index);
        setup.broker.wait_for_session_request();
        setup
            .broker
            .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
                setup.broker.session_receipt_acknowledgement(index),
            ));
        let response = setup
            .broker
            .wait_for_session_response(&request.request_id, 0);
        assert!(matches!(response.result, ResponseResult::Receipt { .. }));
    }

    let interrupt_count = event_count(&setup.events, "agent.interrupt");
    let sign_count = setup.signer.payloads().len();
    assert_eq!(interrupt_count, successful_requests);
    let mut conflict =
        first_request.expect("the first successful request was retained by the test");
    conflict.expected_receipt_sequence =
        Some(u64::try_from(successful_requests).expect("bounded test count fits u64") + 1);
    setup.signer.fail_on_call(sign_count);
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(conflict.clone()));
    let response = setup
        .broker
        .wait_for_session_response(&conflict.request_id, 1);
    let ResponseResult::Error { error } = response.result else {
        panic!("a reused successful request ID is permanently conflicting");
    };
    assert_eq!(error.code, ErrorCode::RequestIdConflict);
    assert_eq!(
        event_count(&setup.events, "agent.interrupt"),
        interrupt_count
    );
    assert_eq!(setup.signer.payloads().len(), sign_count);
    assert_eq!(setup.broker.session_receipt_count(), successful_requests,);

    let mut chain = vec![
        SignedReceipt::parse_canonical(&setup.broker.receipt_bytes(0))
            .expect("Starting receipt is canonical"),
        SignedReceipt::parse_canonical(&setup.broker.receipt_bytes(1))
            .expect("Running receipt is canonical"),
    ];
    chain.extend((0..successful_requests).map(|index| {
        SignedReceipt::parse_canonical(&setup.broker.session_receipt_bytes(index))
            .expect("Interrupt receipt is canonical")
    }));
    verify_chain(
        &chain,
        &ChainAnchor {
            session_id: setup.request.session_id.clone(),
            run_id: setup.request.run_id.clone(),
            release_id: setup.signer.release_id().to_owned(),
            signing_key_id: setup.signer.signing_key_id().to_owned(),
        },
        |_, _, _| true,
    )
    .expect("cache pressure preserves the complete receipt chain");

    setup.signer.fail_on_call(usize::MAX);
    finish_session_relay(&setup, controller_input, relay_receiver, relay_worker);
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "One controller settlement cleanup failure stays parked and blocks resume after reconnect scenario keeps its causal steps and assertions together."
)]
fn controller_settlement_cleanup_failure_stays_parked_and_blocks_resume_after_reconnect() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior {
            dispose_fails: true,
            ..PlatformBehavior::default()
        },
        SUPERVISOR_TIMEOUT,
    );
    let timer = Arc::new(FakeTimer::new(Arc::clone(&setup.events)));
    let timer_port: Arc<dyn SupervisorTimer> = timer.clone();
    let supervisor = setup.fresh_supervisor_with_timer(timer_port);
    let session = complete_launch_on(&setup, &supervisor);
    let (controller_input, relay_receiver, relay_worker, park_receipt, settlement) =
        begin_controller_loss_settlement(&setup, session);
    let park_bytes = setup.broker.session_receipt_bytes(0);
    let park_head = ReceiptHead {
        sequence: park_receipt.payload.sequence,
        digest: Digest::of(&park_bytes).to_string(),
    };

    setup
        .broker
        .complete_controller_loss_settlement(Ok(controller_loss_acknowledgement(
            &settlement,
            ControllerLossDisposition::Recoverable {
                acp_recovery_reference: "cleanup-failure-recovery".to_owned(),
                attention_projection_id: "cleanup-failure-attention".to_owned(),
            },
        )));
    let cleanup_failed =
        request_supervisor_status(&setup, "status-after-settlement-cleanup-failure");
    assert_eq!(cleanup_failed.state, SessionState::Parked);
    assert_eq!(cleanup_failed.channel_state, ChannelState::Revoked);
    assert_eq!(cleanup_failed.launcher_head.as_ref(), Some(&park_head));
    assert_eq!(cleanup_failed.broker_head.as_ref(), Some(&park_head));
    assert_eq!(cleanup_failed.pending_receipt_count, 0);
    assert_eq!(event_count(&setup.events, "identity.poison"), 1);
    assert_eq!(event_count(&setup.events, "identity.release"), 0);
    assert_eq!(setup.broker.session_receipt_count(), 1);

    setup.broker.wait_for_session_request();
    setup.broker.disconnect_session();
    setup.broker.wait_for_reconnect(0);
    let reconnect = setup.broker.reconnect(0);
    assert_eq!(reconnect.sequence, park_head.sequence);
    assert_eq!(reconnect.receipt_digest, park_head.digest);
    setup.broker.complete_reconnect(reconnect);
    let reconnected = request_supervisor_status(&setup, "status-after-cleanup-failure-reconnect");
    assert_eq!(reconnected.state, SessionState::Parked);
    assert_eq!(reconnected.channel_state, ChannelState::Revoked);

    let resume = resume_request(&setup, "resume-after-settlement-cleanup-failure");
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(resume.clone()));
    let blocked = setup
        .broker
        .wait_for_session_response(&resume.request_id, 0);
    let ResponseResult::Error { error } = blocked.result else {
        panic!("unproved controller-loss cleanup keeps widening blocked");
    };
    assert_eq!(error.code, ErrorCode::DurabilityUnavailable);
    assert_eq!(event_count(&setup.events, "agent.resume"), 0);
    assert_eq!(event_count(&setup.events, "capability.enable"), 1);
    assert_eq!(event_count(&setup.events, "identity.poison"), 1);

    let disposal = LifecycleRequest {
        schema: LIFECYCLE_REQUEST_SCHEMA.to_owned(),
        protocol_version: PROTOCOL_VERSION,
        request_id: "dispose-after-settlement-cleanup-failure".to_owned(),
        session_id: setup.request.session_id.clone(),
        run_id: setup.request.run_id.clone(),
        authorization_id: "dispose-after-settlement-cleanup-failure-authorization".to_owned(),
        action: LifecycleAction::Disposal,
        expected_state: SessionState::Parked,
        expected_receipt_sequence: Some(park_head.sequence),
        envelope_revision: setup.request.envelope_revision,
    };
    setup.broker.hold_session_response(&disposal.request_id);
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(disposal.clone()));
    setup.broker.wait_for_held_session_response();
    let failed = setup
        .broker
        .wait_for_session_response(&disposal.request_id, 0);
    let ResponseResult::Error { error } = failed.result else {
        panic!("explicit Disposal reports unproved cleanup");
    };
    assert_eq!(error.code, ErrorCode::LifecycleMechanicUnavailable);
    setup.broker.release_session_response();
    assert_eq!(
        relay_receiver
            .recv_timeout(CALLBACK_TIMEOUT)
            .expect("failed explicit Disposal completes its owner"),
        Err(SupervisorError::CleanupUnproven),
    );
    assert_eq!(event_count(&setup.events, "identity.poison"), 1);
    assert_eq!(event_count(&setup.events, "identity.release"), 0);
    drop(controller_input);
    relay_worker.join().expect("terminal relay worker finishes");
}

#[test]
fn completed_resume_retry_replays_while_controller_loss_blocks_new_widening() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let session = complete_launch(&setup);
    let (controller_input, relay_receiver, relay_worker, _) =
        park_launched_session(&setup, session);
    let resume = resume_request(&setup, "completed-resume-before-controller-loss");

    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(resume.clone()));
    setup.broker.wait_for_session_receipt(1);
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(1),
        ));
    let completed = setup
        .broker
        .wait_for_session_response(&resume.request_id, 0);

    setup.broker.wait_for_session_request();
    setup
        .platform
        .send_running_event(RunningAgentEvent::ControllerEof);
    setup.broker.wait_for_session_receipt(2);
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(2),
        ));
    setup.broker.wait_for_controller_loss_settlement(0);
    let settlement = setup.broker.controller_loss_settlement(0);
    let mechanics = lifecycle_effect_counts(&setup.events);

    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(resume.clone()));
    let replay = setup
        .broker
        .wait_for_session_response(&resume.request_id, 1);
    assert_eq!(replay.canonical_bytes(), completed.canonical_bytes());
    assert_eq!(
        lifecycle_effect_counts(&setup.events),
        mechanics,
        "a completed request replays before the unresolved-loss widening guard",
    );

    setup
        .broker
        .complete_controller_loss_settlement(Ok(controller_loss_acknowledgement(
            &settlement,
            ControllerLossDisposition::NoRecovery {
                attention_projection_id: "completed-replay-attention".to_owned(),
            },
        )));
    setup.broker.wait_for_session_receipt(3);
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(3),
        ));
    finish_terminal_session_relay(controller_input, relay_receiver, relay_worker);
}

#[test]
fn partial_park_with_a_frozen_process_blocks_resume_but_keeps_disposal_available() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior {
            revoke_fails: true,
            ..PlatformBehavior::default()
        },
        SUPERVISOR_TIMEOUT,
    );
    let session = complete_launch(&setup);
    let running_head = ReceiptHead {
        sequence: session.receipt().payload.sequence,
        digest: Digest::of(&session.receipt().canonical_bytes()).to_string(),
    };
    let (controller_input, relay_receiver, relay_worker) = begin_session_relay(session);
    let park = lifecycle_request(
        &setup,
        "partial-park-before-resume",
        "park-authorization",
        1,
    );

    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(park.clone()));
    let failed = setup.broker.wait_for_session_response(&park.request_id, 0);
    let ResponseResult::Error { error } = failed.result else {
        panic!("partial Park reports its failed capability revocation");
    };
    assert_eq!(error.code, ErrorCode::LifecycleMechanicUnavailable);
    assert_eq!(event_count(&setup.events, "agent.park"), 1);
    assert_eq!(setup.broker.session_receipt_count(), 0);
    let frozen = request_supervisor_status(&setup, "status-after-partial-park-before-resume");
    assert_eq!(frozen.state, SessionState::Parked);
    assert_eq!(frozen.channel_state, ChannelState::Revoked);
    assert_eq!(frozen.launcher_head.as_ref(), Some(&running_head));
    assert_eq!(frozen.broker_head.as_ref(), Some(&running_head));

    let resume = LifecycleRequest {
        expected_receipt_sequence: Some(running_head.sequence),
        ..resume_request(&setup, "resume-after-partial-park")
    };
    let thaw_count = event_count(&setup.events, "agent.resume");
    let sign_count = setup.signer.payloads().len();
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(resume.clone()));
    let blocked = setup
        .broker
        .wait_for_session_response(&resume.request_id, 0);
    let ResponseResult::Error { error } = blocked.result else {
        panic!("a partially completed Park cannot authorize widening");
    };
    assert_eq!(error.code, ErrorCode::DurabilityUnavailable);
    assert_eq!(event_count(&setup.events, "agent.resume"), thaw_count);
    assert_eq!(setup.signer.payloads().len(), sign_count);
    assert_eq!(event_count(&setup.events, "capability.enable"), 1);

    let disposal = LifecycleRequest {
        request_id: "dispose-after-partial-park".to_owned(),
        authorization_id: "dispose-after-partial-park-authorization".to_owned(),
        expected_state: SessionState::Parked,
        expected_receipt_sequence: Some(running_head.sequence),
        ..disposal_request(&setup, "dispose-after-partial-park")
    };
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(disposal.clone()));
    setup.broker.wait_for_session_receipt(0);
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(0),
        ));
    let disposed = setup
        .broker
        .wait_for_session_response(&disposal.request_id, 0);
    assert!(matches!(disposed.result, ResponseResult::Receipt { .. }));
    finish_terminal_session_relay(controller_input, relay_receiver, relay_worker);
}

#[test]
fn reconnect_callback_after_the_absolute_grace_deadline_cannot_restore_capability() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let timer = Arc::new(FakeTimer::new(Arc::clone(&setup.events)));
    let timer_port: Arc<dyn SupervisorTimer> = timer.clone();
    let supervisor = setup.fresh_supervisor_with_timer(timer_port);
    let session = complete_launch_on(&setup, &supervisor);
    let (controller_input, relay_receiver, relay_worker) = begin_session_relay(session);
    setup.broker.wait_for_session_request();

    setup.broker.disconnect_session();
    setup.broker.wait_for_reconnect(0);
    let grace = timer.wait_for_schedule(0);
    assert_eq!(
        grace,
        Duration::from_millis(u64::from(BROKER_LOSS_GRACE_MS))
    );
    assert_eq!(timer.wait_for_schedule(1), SUPERVISOR_TIMEOUT);
    let reconnect = setup.broker.reconnect(0);

    timer.advance(grace + Duration::from_millis(1));
    setup.broker.complete_reconnect(reconnect);
    setup.broker.wait_for_session_receipt(0);
    let receipt = SignedReceipt::parse_canonical(&setup.broker.session_receipt_bytes(0))
        .expect("elapsed grace produces a canonical causal Park");
    assert_eq!(
        receipt.payload.outcome,
        ReceiptOutcome::Park {
            authority: ReceiptAuthority::Cause {
                cause: ReceiptCause::BrokerLost,
            },
        }
    );
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(0),
        ));
    let parked = request_supervisor_status(&setup, "status-after-delayed-reconnect-callback");
    assert_eq!(parked.state, SessionState::Parked);
    assert_eq!(parked.channel_state, ChannelState::Revoked);
    assert_eq!(event_count(&setup.events, "agent.park"), 1);
    assert_eq!(event_count(&setup.events, "capability.enable"), 1);

    let schedules = timer.scheduled_count();
    timer.fire(0);
    let after_original_deadline =
        request_supervisor_status(&setup, "status-after-original-grace-callback");
    assert_eq!(after_original_deadline.state, SessionState::Parked);
    assert_eq!(after_original_deadline.channel_state, ChannelState::Revoked);
    assert_eq!(timer.scheduled_count(), schedules, "grace never resets");
    assert_eq!(event_count(&setup.events, "agent.park"), 1);
    assert_eq!(event_count(&setup.events, "capability.enable"), 1);

    finish_session_relay(&setup, controller_input, relay_receiver, relay_worker);
}

#[test]
fn held_reconnect_is_cancelled_and_retried_after_its_deadline() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let timer = Arc::new(FakeTimer::new(Arc::clone(&setup.events)));
    let timer_port: Arc<dyn SupervisorTimer> = timer.clone();
    let supervisor = setup.fresh_supervisor_with_timer(timer_port);
    let session = complete_launch_on(&setup, &supervisor);
    let initial_head = ReceiptHead {
        sequence: session.receipt().payload.sequence,
        digest: Digest::of(&session.receipt().canonical_bytes()).to_string(),
    };
    let (controller_input, relay_receiver, relay_worker) = begin_session_relay(session);
    setup.broker.wait_for_session_request();

    setup.broker.disconnect_session();
    setup.broker.wait_for_reconnect(0);
    let stale_response = setup.broker.reconnect(0);
    assert_eq!(
        timer.wait_for_schedule(0),
        Duration::from_millis(u64::from(BROKER_LOSS_GRACE_MS))
    );
    assert_eq!(timer.wait_for_schedule(1), SUPERVISOR_TIMEOUT);

    timer.fire(0);
    setup.broker.wait_for_session_receipt(0);
    timer.fire(1);
    assert!(timer.wait_for_schedule(3) < SUPERVISOR_TIMEOUT);
    assert_eq!(event_count(&setup.events, "broker.cancel_reconnect"), 1);
    timer.fire(3);
    setup.broker.wait_for_reconnect(1);

    let mechanics = lifecycle_effect_counts(&setup.events);
    setup
        .broker
        .complete_stale_reconnect(0, stale_response.clone());
    assert_eq!(setup.broker.reconnect(1), stale_response);
    assert_eq!(lifecycle_effect_counts(&setup.events), mechanics);

    let mut behind = setup.broker.reconnect(1);
    behind.sequence = initial_head.sequence;
    behind.receipt_digest = initial_head.digest;
    setup.broker.complete_reconnect(behind);
    setup.broker.wait_for_session_receipt(1);
    assert_eq!(
        setup.broker.session_receipt_bytes(1),
        setup.broker.session_receipt_bytes(0),
    );
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(1),
        ));
    let repaired = request_supervisor_status(&setup, "status-after-held-reconnect-repair");
    assert_eq!(repaired.state, SessionState::Parked);
    assert_eq!(repaired.broker_connection, BrokerConnection::Connected);
    assert_eq!(repaired.channel_state, ChannelState::Revoked);
    assert_eq!(repaired.launcher_head, repaired.broker_head);
    assert_eq!(event_count(&setup.events, "agent.park"), 1);
    assert_eq!(event_count(&setup.events, "capability.enable"), 1);

    finish_session_relay(&setup, controller_input, relay_receiver, relay_worker);
}

#[test]
fn withheld_suffix_ack_times_out_into_a_new_reconciliation_epoch() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let timer = Arc::new(FakeTimer::new(Arc::clone(&setup.events)));
    let timer_port: Arc<dyn SupervisorTimer> = timer.clone();
    let supervisor = setup.fresh_supervisor_with_timer(timer_port);
    let session = complete_launch_on(&setup, &supervisor);
    let broker_head = ReceiptHead {
        sequence: session.receipt().payload.sequence,
        digest: Digest::of(&session.receipt().canonical_bytes()).to_string(),
    };
    let (controller_input, relay_receiver, relay_worker) = begin_session_relay(session);
    setup
        .broker
        .set_session_receipt_send_behavior(SessionReceiptSendBehavior::Reject);
    setup.broker.wait_for_session_request();
    let park = lifecycle_request(
        &setup,
        "park-before-suffix-ack-timeout",
        "park-authorization",
        1,
    );
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(park.clone()));
    setup.broker.wait_for_session_receipt(0);
    let park_bytes = setup.broker.session_receipt_bytes(0);
    let failed = setup.broker.wait_for_session_response(&park.request_id, 0);
    assert!(matches!(failed.result, ResponseResult::Error { .. }));
    setup
        .broker
        .set_session_receipt_send_behavior(SessionReceiptSendBehavior::Complete);

    setup.broker.wait_for_session_request();
    setup.broker.disconnect_session();
    setup.broker.wait_for_reconnect(0);
    assert_eq!(
        timer.wait_for_schedule(1),
        Duration::from_millis(u64::from(BROKER_LOSS_GRACE_MS))
    );
    assert_eq!(timer.wait_for_schedule(2), SUPERVISOR_TIMEOUT);
    let mut behind = setup.broker.reconnect(0);
    behind.sequence = broker_head.sequence;
    behind.receipt_digest.clone_from(&broker_head.digest);
    setup.broker.complete_reconnect(behind);
    setup.broker.wait_for_session_receipt(1);
    assert_eq!(setup.broker.session_receipt_bytes(1), park_bytes);
    assert_eq!(timer.wait_for_schedule(3), SUPERVISOR_TIMEOUT);
    setup.broker.wait_for_session_request();
    setup.broker.detach_session_request_as_stale();

    timer.fire(3);
    setup.broker.wait_for_reconnect(1);
    assert_eq!(
        timer.wait_for_schedule(4),
        Duration::from_millis(u64::from(BROKER_LOSS_GRACE_MS))
    );
    assert_eq!(timer.wait_for_schedule(5), SUPERVISOR_TIMEOUT);
    let mut still_behind = setup.broker.reconnect(1);
    still_behind.sequence = broker_head.sequence;
    still_behind.receipt_digest = broker_head.digest;
    setup.broker.complete_reconnect(still_behind);
    setup.broker.wait_for_session_receipt(2);
    assert_eq!(setup.broker.session_receipt_bytes(2), park_bytes);
    setup.broker.wait_for_session_request();
    setup.broker.fail_stale_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(2),
        ));

    let repaired = request_supervisor_status(&setup, "status-after-suffix-ack-timeout-repair");
    assert_eq!(repaired.state, SessionState::Parked);
    assert_eq!(repaired.broker_connection, BrokerConnection::Connected);
    assert_eq!(repaired.channel_state, ChannelState::Revoked);
    assert_eq!(repaired.launcher_head, repaired.broker_head);
    assert_eq!(event_count(&setup.events, "agent.park"), 1);

    finish_session_relay(&setup, controller_input, relay_receiver, relay_worker);
}

#[test]
fn broker_loss_is_owned_between_launch_completion_and_stdio_attachment() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let timer = Arc::new(FakeTimer::new(Arc::clone(&setup.events)));
    let timer_port: Arc<dyn SupervisorTimer> = timer.clone();
    let supervisor = setup.fresh_supervisor_with_timer(timer_port);
    let session = complete_launch_on(&setup, &supervisor);
    let running_head = ReceiptHead {
        sequence: session.receipt().payload.sequence,
        digest: Digest::of(&session.receipt().canonical_bytes()).to_string(),
    };

    setup.broker.wait_for_session_request();
    setup.broker.disconnect_session();
    setup.broker.wait_for_reconnect(0);
    let grace = timer.wait_for_schedule(0);
    assert_eq!(
        grace,
        Duration::from_millis(u64::from(BROKER_LOSS_GRACE_MS))
    );
    assert_eq!(timer.wait_for_schedule(1), SUPERVISOR_TIMEOUT);
    assert!(lock(&setup.platform.gate_state()).revoked);
    assert!(!lock(&setup.platform.gate_state()).enabled);

    let mut reconnect = setup.broker.reconnect(0);
    reconnect.sequence = running_head.sequence;
    reconnect.receipt_digest = running_head.digest;
    timer.advance(grace + Duration::from_millis(1));
    setup.broker.complete_reconnect(reconnect);
    setup.broker.wait_for_session_receipt(0);
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(0),
        ));
    let parked = request_supervisor_status(&setup, "status-before-stdio-attachment");
    assert_eq!(parked.state, SessionState::Parked);
    assert_eq!(parked.channel_state, ChannelState::Revoked);
    assert_eq!(event_count(&setup.events, "agent.park"), 1);
    assert_eq!(event_count(&setup.events, "capability.enable"), 1);

    let (controller_input, relay_receiver, relay_worker) = begin_session_relay(session);
    finish_session_relay(&setup, controller_input, relay_receiver, relay_worker);
}

#[test]
fn ambiguous_resume_result_terminates_without_claiming_a_parked_state() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior {
            resume_ambiguous_after_running: true,
            ..PlatformBehavior::default()
        },
        SUPERVISOR_TIMEOUT,
    );
    let session = complete_launch(&setup);
    let (controller_input, relay_receiver, relay_worker, _) =
        park_launched_session(&setup, session);
    let resume = resume_request(&setup, "ambiguous-resume");

    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(resume.clone()));
    let response = setup
        .broker
        .wait_for_session_response(&resume.request_id, 0);
    let ResponseResult::Error { error } = response.result else {
        panic!("an ambiguous Resume result cannot produce a receipt");
    };
    assert_eq!(error.code, ErrorCode::LifecycleMechanicUnavailable);
    assert_ne!(error.current_state, Some(SessionState::Parked));
    setup.broker.wait_for_close();
    assert_eq!(setup.broker.session_receipt_count(), 1);
    assert_eq!(event_count(&setup.events, "agent.resume"), 1);
    assert_eq!(event_count(&setup.events, "capability.enable"), 1);
    assert_eq!(event_count(&setup.events, "agent.dispose"), 1);
    assert_eq!(event_count(&setup.events, "identity.release"), 1);
    let gate = setup.platform.gate_state();
    let gate = lock(&gate);
    assert!(gate.revoked);
    assert!(gate.closed);
    drop(gate);

    assert!(
        relay_receiver
            .recv_timeout(CALLBACK_TIMEOUT)
            .expect("ambiguous mechanic result closes the relay owner")
            .is_err(),
    );
    drop(controller_input);
    relay_worker
        .join()
        .expect("ambiguous relay worker finishes");
}

#[test]
fn known_parked_resume_failure_stays_parked_and_allows_authorized_disposal() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior {
            resume_fails: true,
            ..PlatformBehavior::default()
        },
        SUPERVISOR_TIMEOUT,
    );
    let session = complete_launch(&setup);
    let (controller_input, relay_receiver, relay_worker, park_receipt) =
        park_launched_session(&setup, session);
    let park_head = ReceiptHead {
        sequence: park_receipt.payload.sequence,
        digest: Digest::of(&park_receipt.canonical_bytes()).to_string(),
    };
    let resume = resume_request(&setup, "known-parked-resume-failure");

    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(resume.clone()));
    let response = setup
        .broker
        .wait_for_session_response(&resume.request_id, 0);
    let ResponseResult::Error { error } = response.result else {
        panic!("known Parked Resume failure returns a typed error");
    };
    assert_eq!(error.code, ErrorCode::LifecycleMechanicUnavailable);
    let parked = request_supervisor_status(&setup, "status-after-known-parked-resume-failure");
    assert_eq!(parked.state, SessionState::Parked);
    assert_eq!(parked.channel_state, ChannelState::Revoked);
    assert_eq!(parked.launcher_head.as_ref(), Some(&park_head));
    assert_eq!(parked.broker_head.as_ref(), Some(&park_head));
    assert_eq!(parked.pending_receipt_count, 0);
    assert_eq!(setup.broker.session_receipt_count(), 1);
    assert_eq!(event_count(&setup.events, "capability.enable"), 1);

    let disposal = LifecycleRequest {
        request_id: "dispose-after-known-parked-resume-failure".to_owned(),
        authorization_id: "dispose-after-known-parked-resume-failure-authorization".to_owned(),
        expected_state: SessionState::Parked,
        expected_receipt_sequence: Some(park_head.sequence),
        ..disposal_request(&setup, "dispose-after-known-parked-resume-failure")
    };
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(disposal.clone()));
    setup.broker.wait_for_session_receipt(1);
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(1),
        ));
    let disposed = setup
        .broker
        .wait_for_session_response(&disposal.request_id, 0);
    assert!(matches!(disposed.result, ResponseResult::Receipt { .. }));
    assert_eq!(event_count(&setup.events, "agent.dispose"), 1);
    finish_terminal_session_relay(controller_input, relay_receiver, relay_worker);
}

#[test]
fn unattached_drop_and_explicit_dispose_use_the_controller_loss_settlement_path() {
    for explicit_dispose in [false, true] {
        let setup = setup(
            true,
            |_| {},
            AppendBehavior::Hold,
            PlatformBehavior::default(),
            SUPERVISOR_TIMEOUT,
        );
        let session = complete_launch(&setup);
        setup.broker.wait_for_session_request();
        let (completion, worker) = if explicit_dispose {
            let (sender, receiver) = mpsc::sync_channel(1);
            let worker = thread::Builder::new()
                .name("explicit-unattached-session-disposal".to_owned())
                .spawn(move || {
                    sender
                        .send(session.dispose())
                        .expect("test receives explicit Disposal completion");
                })
                .expect("explicit Disposal worker starts");
            (Some(receiver), Some(worker))
        } else {
            drop(session);
            (None, None)
        };

        setup.broker.wait_for_session_receipt(0);
        let park = SignedReceipt::parse_canonical(&setup.broker.session_receipt_bytes(0))
            .expect("unattached controller loss Park is canonical");
        assert_eq!(
            park.payload.outcome,
            ReceiptOutcome::Park {
                authority: ReceiptAuthority::Cause {
                    cause: ReceiptCause::ControllerLost,
                },
            },
            "explicit_dispose={explicit_dispose}",
        );
        setup.broker.wait_for_session_request();
        setup
            .broker
            .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
                setup.broker.session_receipt_acknowledgement(0),
            ));
        setup.broker.wait_for_controller_loss_settlement(0);
        let settlement = setup.broker.controller_loss_settlement(0);
        setup
            .broker
            .complete_controller_loss_settlement(Ok(controller_loss_acknowledgement(
                &settlement,
                ControllerLossDisposition::NoRecovery {
                    attention_projection_id: format!(
                        "unattached-controller-loss-{explicit_dispose}"
                    ),
                },
            )));
        setup.broker.wait_for_session_receipt(1);
        setup.broker.wait_for_session_request();
        setup
            .broker
            .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
                setup.broker.session_receipt_acknowledgement(1),
            ));
        setup.broker.wait_for_close();
        assert_eq!(setup.broker.session_receipt_count(), 2);
        assert_eq!(event_count(&setup.events, "agent.park"), 1);
        assert_eq!(event_count(&setup.events, "agent.dispose"), 1);
        assert_eq!(event_count(&setup.events, "identity.release"), 1);

        if let Some(completion) = completion {
            completion
                .recv_timeout(CALLBACK_TIMEOUT)
                .expect("explicit Disposal returns after durable settlement")
                .expect("explicit Disposal cleanup succeeds");
        }
        if let Some(worker) = worker {
            worker.join().expect("explicit Disposal worker finishes");
        }
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "One terminal repark after resume audit failure preserves resume before exit scenario keeps its causal steps and assertions together."
)]
fn terminal_repark_after_resume_audit_failure_preserves_resume_before_exit() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior {
            repark_failure: Some(MechanicFailure::Terminal(
                ProcessExitClassification::Failure,
            )),
            ..PlatformBehavior::default()
        },
        SUPERVISOR_TIMEOUT,
    );
    let session = complete_launch(&setup);
    let (controller_input, relay_receiver, relay_worker, park_receipt) =
        park_launched_session(&setup, session);
    let park_head = ReceiptHead {
        sequence: park_receipt.payload.sequence,
        digest: Digest::of(&park_receipt.canonical_bytes()).to_string(),
    };
    let resume = resume_request(&setup, "resume-whose-repark-observes-exit");
    let resume_sign_call = setup.signer.payloads().len();
    setup.signer.fail_on_call(resume_sign_call);

    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(resume.clone()));
    let response = setup
        .broker
        .wait_for_session_response(&resume.request_id, 0);
    let ResponseResult::Error { error } = response.result else {
        panic!("failed Resume audit still returns its original error response");
    };
    assert_eq!(error.code, ErrorCode::SigningUnavailable);
    assert_eq!(
        setup.signer.payloads().len(),
        resume_sign_call + 1,
        "process-exit signing cannot overtake the deferred Resume truth",
    );
    assert_eq!(setup.broker.session_receipt_count(), 1);
    let terminal = request_supervisor_status(&setup, "terminal-repark-awaiting-ordered-repair");
    assert_eq!(terminal.state, SessionState::Terminal);
    assert_eq!(terminal.channel_state, ChannelState::Closed);
    assert_eq!(
        terminal.process_exit,
        Some(ProcessExitClassification::Failure)
    );
    assert_eq!(terminal.launcher_head.as_ref(), Some(&park_head));
    assert_eq!(terminal.broker_head.as_ref(), Some(&park_head));
    assert_eq!(terminal.pending_receipt_count, 2);
    assert!(matches!(
        relay_receiver.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));

    setup.signer.fail_on_call(usize::MAX);
    setup.broker.wait_for_session_request();
    setup.broker.disconnect_session();
    setup.broker.wait_for_reconnect(0);
    let reconnect = setup.broker.reconnect(0);
    assert_eq!(reconnect.sequence, park_head.sequence);
    assert_eq!(reconnect.receipt_digest, park_head.digest);
    setup.broker.complete_reconnect(reconnect);

    setup.broker.wait_for_session_receipt(1);
    let resume_bytes = setup.broker.session_receipt_bytes(1);
    let resume_receipt =
        SignedReceipt::parse_canonical(&resume_bytes).expect("deferred Resume truth is canonical");
    assert!(matches!(
        resume_receipt.payload.outcome,
        ReceiptOutcome::Resume { .. }
    ));
    assert_eq!(
        resume_receipt.payload.resulting_state,
        SessionState::Running
    );
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(1),
        ));

    setup.broker.wait_for_session_receipt(2);
    let exit_receipt = SignedReceipt::parse_canonical(&setup.broker.session_receipt_bytes(2))
        .expect("deferred process-exit truth is canonical");
    assert_eq!(
        exit_receipt.payload.outcome,
        ReceiptOutcome::Disposal {
            authority: ReceiptAuthority::ProcessExited {
                classification: ProcessExitClassification::Failure,
            },
        }
    );
    assert_eq!(
        exit_receipt.payload.previous_receipt_digest,
        Some(Digest::of(&resume_bytes).to_string()),
    );
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(2),
        ));
    assert_eq!(
        relay_receiver
            .recv_timeout(CALLBACK_TIMEOUT)
            .expect("ordered process-exit receipt finishes its owner")
            .expect("terminal cleanup succeeds"),
        1,
    );
    assert_eq!(
        setup.broker.session_response_count_for(&resume.request_id),
        1
    );
    drop(controller_input);
    relay_worker.join().expect("terminal relay worker finishes");
}

#[test]
fn ambiguous_repark_after_resume_audit_failure_waits_for_its_error_response() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior {
            repark_failure: Some(MechanicFailure::Ambiguous),
            ..PlatformBehavior::default()
        },
        SUPERVISOR_TIMEOUT,
    );
    setup.platform.hold_relay_quiescence();
    let session = complete_launch(&setup);
    let (controller_input, relay_receiver, relay_worker, _) =
        park_launched_session(&setup, session);
    let resume = resume_request(&setup, "resume-whose-repark-is-ambiguous");
    setup.signer.fail_on_call(setup.signer.payloads().len());
    setup.broker.hold_session_response(&resume.request_id);

    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(resume.clone()));
    setup.broker.wait_for_held_session_response();
    let response = setup
        .broker
        .wait_for_session_response(&resume.request_id, 0);
    let ResponseResult::Error { error } = response.result else {
        panic!("ambiguous re-Park retains the original Resume audit error");
    };
    assert_eq!(error.code, ErrorCode::LifecycleMechanicUnavailable);
    assert_eq!(event_count(&setup.events, "agent.resume"), 1);
    assert_eq!(event_count(&setup.events, "agent.park"), 2);
    assert_eq!(event_count(&setup.events, "agent.dispose"), 1);
    assert_eq!(event_count(&setup.events, "identity.release"), 1);
    assert_eq!(event_count(&setup.events, "capability.enable"), 1);
    assert_eq!(setup.broker.session_receipt_count(), 1);
    setup.platform.wait_for_relay_quiescence();
    setup.platform.complete_relay_quiescence();
    assert_eq!(
        relay_receiver.recv_timeout(Duration::from_millis(100)),
        Err(mpsc::RecvTimeoutError::Timeout),
        "owner exit is gated by the original error response callback",
    );
    assert!(!setup.broker.is_closed());

    setup.broker.release_session_response();
    assert!(
        relay_receiver
            .recv_timeout(CALLBACK_TIMEOUT)
            .expect("confirmed error response releases the quarantined owner")
            .is_err(),
    );
    setup.broker.wait_for_close();
    drop(controller_input);
    relay_worker
        .join()
        .expect("ambiguous re-Park relay worker finishes");
}
