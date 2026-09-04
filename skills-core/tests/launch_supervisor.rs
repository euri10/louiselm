#![cfg(target_os = "linux")]

//! One authorized launch through fake broker, signer, and privileged platform ports.

mod support;

use std::{
    env, fs,
    io::{self, BufReader, Cursor, Read, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex, MutexGuard,
        atomic::{AtomicUsize, Ordering},
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
        LAUNCH_AUTHORIZATION_SCHEMA, LaunchAuthorization, RECEIPT_ACK_SCHEMA,
        ReceiptAcknowledgement,
    },
    launch_receipt::{ReceiptAuthority, ReceiptCause, ReceiptOutcome, SessionState, SignedReceipt},
    launch_supervisor::{
        CapabilityBinding, CapabilityGate, IdentityGuard, LaunchBroker, LaunchPlatform,
        LaunchSigner, LaunchSupervisor, LaunchedSession, PreparedAgent, ProcessMembership,
        RunningAgent, SupervisorCompletion, SupervisorError, read_launch_frame,
    },
    launcher_install::Identity,
    registry::Registry,
    sandbox::{
        BubblewrapBackend, Channel, ConfinementPlan, IdentityPlan, PreparedSession, ProcessTree,
        SandboxError, SandboxedSession,
    },
};
use support::{Fixture, write_file, write_registry};

const CONTROLLER_UID: u32 = 1_000;
const NOW_MS: u64 = 1_000;
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(2);
const SUPERVISOR_TIMEOUT: Duration = Duration::from_millis(500);

type Events = Arc<Mutex<Vec<String>>>;

fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
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

struct BrokerState {
    authorization: Option<LaunchAuthorization>,
    hold_authorization: bool,
    pending_authorization: Option<(
        Option<LaunchAuthorization>,
        SupervisorCompletion<LaunchAuthorization>,
    )>,
    append_behavior: AppendBehavior,
    receipts: Vec<Vec<u8>>,
    pending_append: Option<(usize, SupervisorCompletion<ReceiptAcknowledgement>)>,
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
                hold_authorization: false,
                pending_authorization: None,
                append_behavior,
                receipts: Vec::new(),
                pending_append: None,
            }),
            changed: Condvar::new(),
            receipt_root,
        }
    }

    fn hold_authorization(&self) {
        lock(&self.state).hold_authorization = true;
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
        };
        edit(&mut acknowledgement);
        thread::Builder::new()
            .name("fake-broker-receipt-ack".to_owned())
            .spawn(move || complete(Ok(acknowledgement)))
            .expect("receipt ACK callback worker starts")
            .join()
            .expect("receipt ACK callback worker finishes");
    }
}

impl LaunchBroker for FakeBroker {
    fn consume_authorization(
        &self,
        _request: LaunchRequest,
        complete: SupervisorCompletion<LaunchAuthorization>,
    ) -> Result<(), SupervisorError> {
        record(&self.events, "broker.consume");
        let authorization = {
            let mut state = lock(&self.state);
            let authorization = state.authorization.take();
            if state.hold_authorization {
                state.pending_authorization = Some((authorization, complete));
                self.changed.notify_all();
                return Ok(());
            }
            authorization
        };
        thread::Builder::new()
            .name("fake-broker-authorization".to_owned())
            .spawn(move || {
                complete(authorization.ok_or(SupervisorError::AuthorizationRejected));
            })
            .map(drop)
            .map_err(|_| SupervisorError::BrokerUnavailable)
    }

    fn append_receipt(
        &self,
        receipt_bytes: Vec<u8>,
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
        state.pending_append = Some((index, complete));
        self.changed.notify_all();
        Ok(())
    }

    fn close(&self) {
        record(&self.events, "broker.close");
    }
}

struct FakeSigner {
    events: Events,
    release_id: String,
    signing_key_id: String,
    payloads: Mutex<Vec<Vec<u8>>>,
    fail_on_call: Mutex<Option<usize>>,
}

impl FakeSigner {
    fn new(events: Events) -> Self {
        Self {
            events,
            release_id: Digest::of(b"release").to_string(),
            signing_key_id: Digest::of(b"signing-key").to_string(),
            payloads: Mutex::new(Vec::new()),
            fail_on_call: Mutex::new(None),
        }
    }

    fn fail_on_call(&self, index: usize) {
        *lock(&self.fail_on_call) = Some(index);
    }

    fn payloads(&self) -> Vec<Vec<u8>> {
        lock(&self.payloads).clone()
    }
}

impl LaunchSigner for FakeSigner {
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
        thread::Builder::new()
            .name("fake-launch-signer".to_owned())
            .spawn(move || complete(result))
            .map(drop)
            .map_err(|_| SupervisorError::SigningUnavailable)
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
    closed: bool,
}

struct FakeCapabilityGate {
    events: Events,
    state: Arc<Mutex<GateState>>,
    channel: Channel,
    enable_fails: bool,
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
    fn channel(&self) -> Channel {
        self.channel.clone()
    }

    fn bind(
        &mut self,
        binding: CapabilityBinding,
        membership: Arc<dyn ProcessMembership>,
    ) -> Result<(), SupervisorError> {
        record(&self.events, "capability.bind");
        if !membership.contains(binding.sandbox_leader_pid)? {
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
        lock(&self.state).enabled = true;
        Ok(())
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
struct AgentState {
    started: bool,
    disposed: bool,
    relayed_input: Vec<u8>,
}

struct FakePreparedAgent {
    events: Events,
    state: Arc<Mutex<AgentState>>,
    evidence: IsolationEvidence,
    backend_id: String,
    sandbox_leader_pid: Option<u32>,
    membership: Arc<dyn ProcessMembership>,
    output: Vec<u8>,
    dispose_fails: bool,
    start_fails: bool,
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
            events: Arc::clone(&self.events),
            state: Arc::clone(&self.state),
            output: self.output.clone(),
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

struct FakeRunningAgent {
    events: Events,
    state: Arc<Mutex<AgentState>>,
    output: Vec<u8>,
    active: bool,
}

impl RunningAgent for FakeRunningAgent {
    fn relay(
        &mut self,
        mut input: Box<dyn Read + Send>,
        mut output: Box<dyn Write + Send>,
    ) -> Result<i32, SupervisorError> {
        record(&self.events, "agent.relay");
        input
            .read_to_end(&mut lock(&self.state).relayed_input)
            .map_err(|_| SupervisorError::RelayFailed)?;
        output
            .write_all(&self.output)
            .map_err(|_| SupervisorError::RelayFailed)?;
        Ok(0)
    }

    fn dispose(&mut self) -> Result<(), SupervisorError> {
        if self.active {
            record(&self.events, "agent.dispose");
            lock(&self.state).disposed = true;
            self.active = false;
        }
        Ok(())
    }
}

impl Drop for FakeRunningAgent {
    fn drop(&mut self) {
        if self.active {
            record(&self.events, "agent.running_dropped_without_disposal");
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct OuterProcessObservation {
    agent_pid: u32,
    monitor_pid: u32,
    sandbox_leader_pid: u32,
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
    observation: Arc<Mutex<Option<OuterProcessObservation>>>,
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
        let monitor_pid = prepared.monitor_pid();
        let sandbox_leader_pid = prepared
            .sandbox_leader_pid()
            .ok_or(SupervisorError::IsolationRejected)?;
        let session = prepared.start().map_err(map_test_sandbox)?;
        Ok(Box::new(BubblewrapRunningAgent {
            session,
            tree: self.tree.clone(),
            observation: Arc::clone(&self.observation),
            monitor_pid,
            sandbox_leader_pid,
        }))
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

struct BubblewrapRunningAgent {
    session: SandboxedSession,
    tree: ProcessTree,
    observation: Arc<Mutex<Option<OuterProcessObservation>>>,
    monitor_pid: u32,
    sandbox_leader_pid: u32,
}

impl RunningAgent for BubblewrapRunningAgent {
    fn relay(
        &mut self,
        mut input: Box<dyn Read + Send>,
        mut output: Box<dyn Write + Send>,
    ) -> Result<i32, SupervisorError> {
        let observation =
            observe_agent_process(&self.tree, self.monitor_pid, self.sandbox_leader_pid)?;
        *lock(&self.observation) = Some(observation);

        let mut agent_input = self
            .session
            .take_stdin()
            .ok_or(SupervisorError::RelayFailed)?;
        let mut agent_output = self
            .session
            .take_stdout()
            .ok_or(SupervisorError::RelayFailed)?;
        let mut agent_error = self
            .session
            .take_stderr()
            .ok_or(SupervisorError::RelayFailed)?;
        let error_worker = thread::Builder::new()
            .name("louiselm-test-agent-stderr".to_owned())
            .spawn(move || io::copy(&mut agent_error, &mut io::sink()))
            .map_err(|_| SupervisorError::RelayFailed)?;

        io::copy(&mut input, &mut agent_input)?;
        agent_input.flush()?;
        drop(agent_input);
        io::copy(&mut agent_output, &mut output)?;
        output.flush()?;
        let exit = self.session.wait().map_err(map_test_sandbox)?;
        error_worker
            .join()
            .map_err(|_| SupervisorError::RelayFailed)?
            .map_err(|_| SupervisorError::RelayFailed)?;
        Ok(exit)
    }

    fn dispose(&mut self) -> Result<(), SupervisorError> {
        self.session.dispose().map(|_| ()).map_err(map_test_sandbox)
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
        }))
    }

    fn prepare(&self, plan: ConfinementPlan) -> Result<Box<dyn PreparedAgent>, SupervisorError> {
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
        Ok(Box::new(BubblewrapPreparedAgent {
            prepared: Some(prepared),
            backend_id: self.backend_id.clone(),
            tree,
            observation: Arc::clone(&self.observation),
        }))
    }
}

fn map_test_sandbox(error: SandboxError) -> SupervisorError {
    match error {
        SandboxError::CleanupUnproven { .. } | SandboxError::Survivors { .. } => {
            SupervisorError::CleanupUnproven
        }
        _ => SupervisorError::SpawnFailed,
    }
}

fn observe_agent_process(
    tree: &ProcessTree,
    monitor_pid: u32,
    sandbox_leader_pid: u32,
) -> Result<OuterProcessObservation, SupervisorError> {
    let deadline = Instant::now() + CALLBACK_TIMEOUT;
    loop {
        for pid in tree.processes().map_err(map_test_sandbox)? {
            if pid == monitor_pid || pid == sandbox_leader_pid {
                continue;
            }
            if fs::read_to_string(format!("/proc/{pid}/comm"))
                .is_ok_and(|name| name.trim() == "cat")
                && let (Some(uids), Some(gids), Some(groups)) = (
                    process_status_values(pid, "Uid:"),
                    process_status_values(pid, "Gid:"),
                    process_status_values(pid, "Groups:"),
                )
            {
                return Ok(OuterProcessObservation {
                    agent_pid: pid,
                    monitor_pid,
                    sandbox_leader_pid,
                    uids,
                    gids,
                    groups,
                });
            }
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
struct PlatformBehavior {
    identity_unavailable: bool,
    prepare_fails: bool,
    unverified_evidence: bool,
    dispose_fails: bool,
    enable_fails: bool,
    start_fails: bool,
}

#[derive(Default)]
struct PlatformState {
    plan: Option<ConfinementPlan>,
    gate: Option<Arc<Mutex<GateState>>>,
}

struct FakePlatform {
    events: Events,
    expected_identity: Identity,
    behavior: PlatformBehavior,
    state: Mutex<PlatformState>,
    agent: Arc<Mutex<AgentState>>,
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
}

impl LaunchPlatform for FakePlatform {
    fn acquire_identity(
        &self,
        assigned: Identity,
    ) -> Result<Box<dyn IdentityGuard>, SupervisorError> {
        record(&self.events, "identity.acquire");
        if self.behavior.identity_unavailable || assigned != self.expected_identity {
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
        }))
    }

    fn prepare(&self, plan: ConfinementPlan) -> Result<Box<dyn PreparedAgent>, SupervisorError> {
        record(&self.events, "platform.prepare");
        lock(&self.state).plan = Some(plan);
        if self.behavior.prepare_fails {
            return Err(SupervisorError::SpawnFailed);
        }
        Ok(Box::new(FakePreparedAgent {
            events: Arc::clone(&self.events),
            state: Arc::clone(&self.agent),
            evidence: isolation_evidence(!self.behavior.unverified_evidence),
            backend_id: Digest::of(b"bubblewrap-binary").to_string(),
            sandbox_leader_pid: Some(42_424),
            membership: Arc::new(FakeProcessMembership {
                members: vec![42_424],
            }),
            output: self.agent_output.clone(),
            dispose_fails: self.behavior.dispose_fails,
            start_fails: self.behavior.start_fails,
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
    _fixture: Fixture,
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
        _fixture: fixture,
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

#[test]
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
            "capability.bind",
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
    assert_eq!(
        evidence.launch_request_digest,
        setup.request.digest().to_string()
    );
    assert_eq!(evidence.capability_channel_ids, ["acp", "broker"]);
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
            channel_id: "broker".to_owned(),
            envelope_revision: 7,
            identity_slot: 3,
            assigned_uid: 200_003,
            assigned_gid: 300_003,
            sandbox_leader_pid: 42_424,
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
            "capability.bind",
            "signer.sign",
            "broker.append",
            "broker.fsync",
            "broker.ack",
            "capability.enable",
            "agent.start",
            "signer.sign",
            "broker.append",
            "broker.fsync",
            "broker.ack",
            "complete",
        ]
    );

    let agent_input = vec![
        0xff, 0x00, b't', b'o', b'o', b'l', b'-', b'p', b'a', b'y', b'l', b'o', b'a', b'd', b'\n',
    ];
    let controller_output = Arc::new(Mutex::new(Vec::new()));
    assert_eq!(
        session
            .relay_stdio(
                Box::new(Cursor::new(agent_input.clone())),
                Box::new(SharedWriter(Arc::clone(&controller_output))),
            )
            .expect("opaque relay and cleanup succeed"),
        0,
    );
    assert_eq!(lock(&setup.platform.agent).relayed_input, agent_input);
    assert_eq!(
        *lock(&controller_output),
        vec![0x00, 0xff, b'o', b'u', b't', b'\n']
    );
    assert_eq!(completion_count.load(Ordering::SeqCst), 1);
    assert_eq!(
        &event_snapshot(&setup.events)[16..],
        [
            "agent.relay",
            "capability.close",
            "broker.close",
            "agent.dispose",
            "identity.release",
        ]
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
                    authorization.authorization_id = "another-authorization".to_owned()
                }
                AuthorizationRejection::RequestId => {
                    authorization.request_id = "another-request".to_owned()
                }
                AuthorizationRejection::RequestDigest => {
                    authorization.request_digest = Digest::of(b"another-request").to_string()
                }
                AuthorizationRejection::ControllerUid => authorization.controller_uid += 1,
                AuthorizationRejection::Session => {
                    authorization.session_id = "another-session".to_owned()
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
fn identity_exhaustion_and_out_of_pool_assignment_stop_before_capability_or_spawn() {
    for (name, assigned, behavior) in [
        (
            "exhausted",
            None,
            PlatformBehavior {
                identity_unavailable: true,
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
            SupervisorError::IdentityUnavailable,
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

    let setup = setup(
        true,
        |authorization| {
            authorization.controller_uid = operator_uid;
            authorization.assigned_uid = assigned;
            authorization.assigned_gid = assigned;
        },
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        Duration::from_secs(5),
    );
    for path in [
        setup._fixture.path(""),
        setup._fixture.path("runtime"),
        setup._fixture.path("runtime/bin"),
        setup._fixture.path("runtime/lib"),
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
        backend: BubblewrapBackend::at(bwrap),
        backend_id: Digest::of(&fs::read(bwrap).expect("Bubblewrap is installed")).to_string(),
        capability_root: setup._fixture.path("real-capability"),
        observation: Arc::clone(&observation),
    });
    let supervisor = LaunchSupervisor::new(
        setup.broker.clone(),
        setup.signer.clone(),
        platform,
        Arc::clone(&setup.registry),
        setup.sessions_root.clone(),
        Duration::from_secs(5),
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
    assert_eq!(rustix::process::geteuid().as_raw(), 0);

    let acp = b"composite ACP bytes\n".to_vec();
    let output = Arc::new(Mutex::new(Vec::new()));
    assert_eq!(
        session
            .relay_stdio(
                Box::new(Cursor::new(acp.clone())),
                Box::new(SharedWriter(Arc::clone(&output))),
            )
            .expect("the deterministic fake Agent relays and exits"),
        0,
    );
    assert_eq!(*lock(&output), acp);
    assert_eq!(completion_count.load(Ordering::SeqCst), 1);

    let observed = lock(&observation)
        .take()
        .expect("the live Agent was observed through host /proc");
    assert_ne!(observed.agent_pid, observed.monitor_pid);
    assert_ne!(observed.agent_pid, observed.sandbox_leader_pid);
    assert!(observed.uids.iter().all(|uid| *uid == assigned));
    assert!(observed.gids.iter().all(|gid| *gid == assigned));
    assert!(
        observed.groups.is_empty(),
        "the Agent inherited no supplementary host groups",
    );
}
