//! Production adapters for the one-shot Launch supervisor.

use std::{
    fs,
    io::{self, Read, Write},
    os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt, chown},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard, mpsc},
    thread,
    time::{Duration, Instant},
};

use crate::{
    Digest,
    launch::LaunchRequest,
    launch_protocol::{
        LaunchAuthorization, ProtocolMessage, ReceiptAcknowledgement, ResponseResult,
    },
    launch_transport::{
        AuthenticatedPacket, BoundSeqpacketListener, CredentialPin, LauncherPacket,
        SeqpacketChannel, SeqpacketConnector, SeqpacketListener, TransportError,
    },
    launcher_install::{
        Identity, IdentityLease, LauncherConfig, LauncherPaths, LauncherSigner,
        acquire_identity_with_deadline, bwrap_version_with_deadline, require_measured_bwrap,
    },
    sandbox::{
        BubblewrapBackend, Channel, ConfinementPlan, PreparedSession, ProcessTree, SandboxError,
        SandboxedSession,
    },
};

use super::{
    CapabilityBinding, CapabilityGate, IdentityGuard, LaunchBroker, LaunchPlatform, LaunchSigner,
    PreparedAgent, ProcessMembership, RunningAgent, SupervisorCompletion, SupervisorError,
};

/// Fixed root-owned launch registry read by the production entrypoint.
pub const SYSTEM_REGISTRY_ROOT: &str = "/var/lib/louiselm/registry";
/// Fixed root-owned parent of private Session homes and workspaces.
pub const SYSTEM_SESSIONS_ROOT: &str = "/var/lib/louiselm/sessions";
/// Fixed root-owned cgroup parent outside the invoking operator's delegation.
pub const SYSTEM_CGROUP_ROOT: &str = "/sys/fs/cgroup/louiselm-launch";
/// Fixed root-owned parent of per-Session capability rendezvous sockets.
pub const SYSTEM_CAPABILITY_ROOT: &str = "/run/louiselm-launch/sessions";
/// Fixed socket location visible inside each confined Session.
pub const SYSTEM_CAPABILITY_GUEST_PATH: &str = "/tmp/louiselm-capability.sock";

const RELAY_POLL_INTERVAL: Duration = Duration::from_millis(10);
const RELAY_EXIT_GRACE: Duration = Duration::from_secs(5);
const SIGNER_CLEANUP_MARGIN: Duration = Duration::from_millis(250);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn map_transport(_: TransportError) -> SupervisorError {
    SupervisorError::BrokerUnavailable
}

/// Connects only to the install-pinned broker rendezvous and kernel identity.
pub fn connect_control_broker(
    config: &LauncherConfig,
    timeout: Duration,
) -> Result<Arc<dyn LaunchBroker>, SupervisorError> {
    let connector = SeqpacketConnector::new().map_err(map_transport)?;
    let (sender, receiver) = mpsc::sync_channel(1);
    connector
        .connect(
            &config.broker_socket_path,
            CredentialPin::Identity {
                uid: config.broker_uid,
                gid: config.broker_gid,
            },
            Box::new(move |result| {
                let _ = sender.try_send(result);
            }),
        )
        .map_err(map_transport)?;
    let channel = receiver
        .recv_timeout(timeout)
        .map_err(|_| SupervisorError::BrokerTimeout)?
        .map_err(map_transport)?;
    drop(connector);
    Ok(Arc::new(SeqpacketLaunchBroker { channel }))
}

struct SeqpacketLaunchBroker {
    channel: SeqpacketChannel,
}

impl SeqpacketLaunchBroker {
    fn transact<T>(
        &self,
        bytes: Vec<u8>,
        parse: impl FnOnce(AuthenticatedPacket) -> Result<T, SupervisorError> + Send + 'static,
        complete: SupervisorCompletion<T>,
    ) -> Result<(), SupervisorError>
    where
        T: Send + 'static,
    {
        let completion = Arc::new(Mutex::new(Some(complete)));
        let finish = |completion: &Arc<Mutex<Option<SupervisorCompletion<T>>>>,
                      result: Result<T, SupervisorError>| {
            if let Some(complete) = lock(completion).take() {
                complete(result);
            }
        };
        let receive_channel = self.channel.clone();
        let send_completion = Arc::clone(&completion);
        self.channel
            .send(
                bytes,
                Box::new(move |sent| {
                    if sent.is_err() {
                        finish(&send_completion, Err(SupervisorError::BrokerUnavailable));
                        return;
                    }
                    let receive_completion = Arc::clone(&send_completion);
                    let callback_completion = Arc::clone(&send_completion);
                    let queued = receive_channel.receive(Box::new(move |received| {
                        let result = received.map_err(map_transport).and_then(parse);
                        finish(&callback_completion, result);
                    }));
                    if queued.is_err() {
                        finish(&receive_completion, Err(SupervisorError::BrokerUnavailable));
                    }
                }),
            )
            .map_err(map_transport)
    }
}

impl LaunchBroker for SeqpacketLaunchBroker {
    fn consume_authorization(
        &self,
        request: LaunchRequest,
        complete: SupervisorCompletion<LaunchAuthorization>,
    ) -> Result<(), SupervisorError> {
        let request_id = request.request_id.clone();
        self.transact(
            request.canonical_bytes(),
            move |packet| match packet.packet {
                LauncherPacket::Response(response) if response.request_id == request_id => {
                    match response.result {
                        ResponseResult::LaunchAuthorization { authorization } => Ok(authorization),
                        ResponseResult::Error { .. } => Err(SupervisorError::AuthorizationRejected),
                        _ => Err(SupervisorError::AuthorizationRejected),
                    }
                }
                _ => Err(SupervisorError::AuthorizationRejected),
            },
            complete,
        )
    }

    fn append_receipt(
        &self,
        receipt_bytes: Vec<u8>,
        complete: SupervisorCompletion<ReceiptAcknowledgement>,
    ) -> Result<(), SupervisorError> {
        self.transact(
            receipt_bytes,
            |packet| match packet.packet {
                LauncherPacket::Request(ProtocolMessage::ReceiptAcknowledgement(
                    acknowledgement,
                )) => Ok(acknowledgement),
                _ => Err(SupervisorError::DurabilityUnavailable),
            },
            complete,
        )
    }

    fn close(&self) {
        self.channel.close();
    }
}

/// Adapter around the validated installed root-owned launcher signer.
pub struct InstalledLaunchSigner {
    signer: Arc<LauncherSigner>,
    release_id: String,
    signing_key_id: String,
    timeout: Duration,
}

impl InstalledLaunchSigner {
    /// Opens the installed signing authority for a new receipt chain.
    pub fn open(paths: &LauncherPaths, timeout: Duration) -> Result<Self, SupervisorError> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .unwrap_or_else(Instant::now);
        let signer = LauncherSigner::open_with_deadline(paths, deadline)
            .map_err(|_| SupervisorError::SigningUnavailable)?;
        let release_id = signer.release_id().to_owned();
        let signing_key_id = signer.active_key_id().to_owned();
        Ok(Self {
            signer: Arc::new(signer),
            release_id,
            signing_key_id,
            timeout,
        })
    }
}

impl LaunchSigner for InstalledLaunchSigner {
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
        let signer = Arc::clone(&self.signer);
        let signing_key_id = self.signing_key_id.clone();
        // The supervisor has its own outer callback deadline. End the trusted
        // subprocess operation first so kill, wait, and pipe drainage finish
        // before that outer waiter is allowed to abandon the callback.
        let margin = (self.timeout / 10).min(SIGNER_CLEANUP_MARGIN);
        let deadline = Instant::now()
            .checked_add(self.timeout.saturating_sub(margin))
            .unwrap_or_else(Instant::now);
        thread::Builder::new()
            .name("louiselm-launch-sign".to_owned())
            .spawn(move || {
                let result = signer
                    .sign_receipt_with_deadline(&signing_key_id, &payload_bytes, deadline)
                    .map_err(|_| SupervisorError::SigningUnavailable);
                complete(result);
            })
            .map(drop)
            .map_err(|_| SupervisorError::SigningUnavailable)
    }
}

/// Root-backed process, identity, and capability implementation.
pub struct SystemLaunchPlatform {
    paths: LauncherPaths,
    config: LauncherConfig,
    backend: BubblewrapBackend,
    timeout: Duration,
}

impl SystemLaunchPlatform {
    /// Creates the production platform after materializing only fixed roots.
    pub fn new(
        paths: LauncherPaths,
        config: LauncherConfig,
        timeout: Duration,
    ) -> Result<Self, SupervisorError> {
        ensure_root_directory(Path::new("/var/lib/louiselm"), 0o711, false)?;
        ensure_root_directory(Path::new(SYSTEM_SESSIONS_ROOT), 0o711, true)?;
        ensure_root_directory(Path::new("/run/louiselm-launch"), 0o711, true)?;
        ensure_root_directory(Path::new(SYSTEM_CAPABILITY_ROOT), 0o711, true)?;
        ensure_system_cgroup_root()?;
        require_measured_bwrap(&config).map_err(|_| SupervisorError::SpawnFailed)?;
        let deadline = Instant::now()
            .checked_add(timeout)
            .unwrap_or_else(Instant::now);
        let backend_version = bwrap_version_with_deadline(&config, deadline)
            .map_err(|_| SupervisorError::SpawnFailed)?;
        let backend = BubblewrapBackend::for_launcher(
            &config.bwrap_path,
            Path::new(SYSTEM_CGROUP_ROOT),
            backend_version,
        );
        Ok(Self {
            paths,
            config,
            backend,
            timeout,
        })
    }
}

impl LaunchPlatform for SystemLaunchPlatform {
    fn acquire_identity(
        &self,
        assigned: Identity,
    ) -> Result<Box<dyn IdentityGuard>, SupervisorError> {
        let expected = self
            .config
            .pool
            .identity(assigned.slot)
            .map_err(|_| SupervisorError::IdentityUnavailable)?;
        if expected != assigned {
            return Err(SupervisorError::IdentityUnavailable);
        }
        let deadline = Instant::now()
            .checked_add(self.timeout)
            .unwrap_or_else(Instant::now);
        let lease = acquire_identity_with_deadline(&self.paths, assigned.slot, deadline)
            .map_err(|_| SupervisorError::IdentityUnavailable)?;
        if lease.identity() != assigned {
            return Err(SupervisorError::IdentityUnavailable);
        }
        Ok(Box::new(SystemIdentityGuard { lease: Some(lease) }))
    }

    fn create_capability(
        &self,
        request: &LaunchRequest,
        assigned: Identity,
    ) -> Result<Box<dyn CapabilityGate>, SupervisorError> {
        SystemCapabilityGate::create(request, assigned)
            .map(|gate| Box::new(gate) as Box<dyn CapabilityGate>)
    }

    fn prepare(&self, plan: ConfinementPlan) -> Result<Box<dyn PreparedAgent>, SupervisorError> {
        require_measured_bwrap(&self.config).map_err(|_| SupervisorError::SpawnFailed)?;
        let prepared = self.backend.prepare(&plan).map_err(map_sandbox)?;
        Ok(Box::new(SystemPreparedAgent {
            prepared: Some(prepared),
            backend_id: self.config.bwrap_digest.clone(),
        }))
    }
}

struct SystemIdentityGuard {
    lease: Option<IdentityLease>,
}

impl IdentityGuard for SystemIdentityGuard {
    fn identity(&self) -> Identity {
        self.lease
            .as_ref()
            .expect("an identity guard owns its lease")
            .identity()
    }

    fn release(mut self: Box<Self>) -> Result<(), SupervisorError> {
        self.lease
            .take()
            .expect("an identity guard owns its lease")
            .release()
            .map_err(|_| SupervisorError::CleanupUnproven)
    }

    fn poison(mut self: Box<Self>) -> Result<(), SupervisorError> {
        self.lease
            .take()
            .expect("an identity guard owns its lease")
            .poison()
            .map_err(|_| SupervisorError::CleanupUnproven)
    }
}

struct SystemPreparedAgent {
    prepared: Option<PreparedSession>,
    backend_id: String,
}

impl PreparedAgent for SystemPreparedAgent {
    fn evidence(&self) -> &crate::isolation::IsolationEvidence {
        self.prepared
            .as_ref()
            .expect("a prepared adapter owns its Session")
            .evidence()
    }

    fn backend_id(&self) -> &str {
        &self.backend_id
    }

    fn sandbox_leader_pid(&self) -> Option<u32> {
        self.prepared
            .as_ref()
            .expect("a prepared adapter owns its Session")
            .sandbox_leader_pid()
    }

    fn processes(&self) -> Result<Vec<u32>, SupervisorError> {
        self.prepared
            .as_ref()
            .expect("a prepared adapter owns its Session")
            .processes()
            .map_err(map_sandbox)
    }

    fn process_membership(&self) -> Arc<dyn ProcessMembership> {
        Arc::new(SystemProcessMembership {
            tree: self
                .prepared
                .as_ref()
                .expect("a prepared adapter owns its Session")
                .process_tree(),
        })
    }

    fn start(mut self: Box<Self>) -> Result<Box<dyn RunningAgent>, SupervisorError> {
        self.prepared
            .take()
            .expect("a prepared adapter owns its Session")
            .start()
            .map(|session| Box::new(SystemRunningAgent { session }) as Box<dyn RunningAgent>)
            .map_err(map_sandbox)
    }

    fn dispose(&mut self) -> Result<(), SupervisorError> {
        self.prepared
            .as_mut()
            .expect("a prepared adapter owns its Session")
            .dispose()
            .map(|_| ())
            .map_err(map_sandbox)
    }
}

struct SystemProcessMembership {
    tree: Option<ProcessTree>,
}

impl ProcessMembership for SystemProcessMembership {
    fn contains(&self, pid: u32) -> Result<bool, SupervisorError> {
        self.tree
            .as_ref()
            .ok_or(SupervisorError::IsolationRejected)?
            .contains(pid)
            .map_err(map_sandbox)
    }
}

struct SystemRunningAgent {
    session: SandboxedSession,
}

impl RunningAgent for SystemRunningAgent {
    fn relay(
        &mut self,
        mut input: Box<dyn Read + Send>,
        mut output: Box<dyn Write + Send>,
    ) -> Result<i32, SupervisorError> {
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
        let (input_sender, input_receiver) = mpsc::sync_channel(1);
        let input_worker = thread::Builder::new()
            .name("louiselm-launch-acp-input".to_owned())
            .spawn(move || {
                let result = io::copy(&mut input, &mut agent_input).map(|_| ());
                let _ = input_sender.try_send(result);
            })
            .map_err(|_| SupervisorError::RelayFailed)?;
        let error_worker = thread::Builder::new()
            .name("louiselm-launch-agent-stderr".to_owned())
            .spawn(move || io::copy(&mut agent_error, &mut io::sink()).map(|_| ()))
            .map_err(|_| SupervisorError::RelayFailed)?;
        let (output_sender, output_receiver) = mpsc::sync_channel(1);
        let output_worker = thread::Builder::new()
            .name("louiselm-launch-acp-output".to_owned())
            .spawn(move || {
                let result = io::copy(&mut agent_output, &mut output)
                    .and_then(|_| output.flush())
                    .map(|_| ());
                let _ = output_sender.try_send(result);
            })
            .map_err(|_| SupervisorError::RelayFailed)?;

        let exit = await_relay_completion(
            &input_receiver,
            &output_receiver,
            RELAY_POLL_INTERVAL,
            RELAY_EXIT_GRACE,
            || self.session.try_wait().map_err(map_sandbox),
        )?;
        // Controller stdin may remain open after the Agent exits. Neither
        // worker owns supervisor authority, so cleanup must not wait on an
        // uncancellable external reader or a descendant-held stderr pipe.
        drop(input_worker);
        drop(error_worker);
        drop(output_worker);
        Ok(exit)
    }

    fn dispose(&mut self) -> Result<(), SupervisorError> {
        self.session.dispose().map(|_| ()).map_err(map_sandbox)
    }
}

fn map_sandbox(error: SandboxError) -> SupervisorError {
    match error {
        SandboxError::CleanupUnproven { .. } | SandboxError::Survivors { .. } => {
            SupervisorError::CleanupUnproven
        }
        _ => SupervisorError::SpawnFailed,
    }
}

fn await_relay_completion(
    input: &mpsc::Receiver<io::Result<()>>,
    output: &mpsc::Receiver<io::Result<()>>,
    poll_interval: Duration,
    exit_grace: Duration,
    mut try_wait: impl FnMut() -> Result<Option<i32>, SupervisorError>,
) -> Result<i32, SupervisorError> {
    let mut input_done = false;
    let mut output_done = false;
    let mut exit = None;
    let mut deadline = None;

    loop {
        if !input_done {
            match input.try_recv() {
                Ok(result) => {
                    result.map_err(|_| SupervisorError::RelayFailed)?;
                    input_done = true;
                    deadline.get_or_insert_with(|| Instant::now() + exit_grace);
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(SupervisorError::RelayFailed);
                }
            }
        }

        if !output_done {
            match output.recv_timeout(poll_interval) {
                Ok(result) => {
                    result.map_err(|_| SupervisorError::RelayFailed)?;
                    output_done = true;
                    deadline.get_or_insert_with(|| Instant::now() + exit_grace);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(SupervisorError::RelayFailed);
                }
            }
        } else if !poll_interval.is_zero() {
            thread::sleep(poll_interval);
        }

        if exit.is_none() {
            exit = try_wait()?;
            if exit.is_some() {
                deadline.get_or_insert_with(|| Instant::now() + exit_grace);
            }
        }
        if let (Some(code), true) = (exit, output_done) {
            return Ok(code);
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(SupervisorError::RelayFailed);
        }
    }
}

enum ListenerState {
    Bound(BoundSeqpacketListener),
    Enabled(SeqpacketListener),
}

struct SystemCapabilityGate {
    state: Option<ListenerState>,
    path: PathBuf,
    device: u64,
    inode: u64,
    channel: Channel,
    binding: Option<CapabilityBinding>,
    membership: Option<Arc<dyn ProcessMembership>>,
    expected_session_id: String,
    expected_envelope_revision: u64,
    expected_identity: Identity,
    accepted: Arc<Mutex<AcceptedCapability>>,
}

#[derive(Default)]
struct AcceptedCapability {
    closed: bool,
    channel: Option<SeqpacketChannel>,
}

fn accept_capability(
    result: Result<SeqpacketChannel, TransportError>,
    accepted: Arc<Mutex<AcceptedCapability>>,
    membership: Arc<dyn ProcessMembership>,
) {
    let Ok(channel) = result else {
        return;
    };
    if membership.contains(channel.peer_credentials().pid) != Ok(true) {
        channel.close();
        return;
    }
    let mut accepted = lock(&accepted);
    if accepted.closed {
        channel.close();
    } else {
        accepted.channel = Some(channel);
    }
}

impl SystemCapabilityGate {
    fn create(request: &LaunchRequest, assigned: Identity) -> Result<Self, SupervisorError> {
        let socket_name = format!("{}.sock", Digest::of(request.session_id.as_bytes()).hex());
        let path = Path::new(SYSTEM_CAPABILITY_ROOT).join(socket_name);
        let bound = SeqpacketListener::bind_disabled(&path)
            .map_err(|_| SupervisorError::CapabilityUnavailable)?;
        let configured = fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .and_then(|()| chown(&path, Some(assigned.uid), Some(assigned.gid)))
            .and_then(|()| fs::symlink_metadata(&path));
        let metadata = match configured {
            Ok(metadata) => metadata,
            Err(_) => {
                drop(bound);
                let _ = fs::remove_file(&path);
                return Err(SupervisorError::CapabilityUnavailable);
            }
        };
        if !metadata.file_type().is_socket()
            || metadata.file_type().is_symlink()
            || metadata.uid() != assigned.uid
            || metadata.gid() != assigned.gid
            || metadata.mode() & 0o777 != 0o600
        {
            drop(bound);
            let _ = fs::remove_file(&path);
            return Err(SupervisorError::CapabilityUnavailable);
        }
        Ok(Self {
            state: Some(ListenerState::Bound(bound)),
            path: path.clone(),
            device: metadata.dev(),
            inode: metadata.ino(),
            channel: Channel::UnixSocket {
                id: "agent-capability".to_owned(),
                host_path: path,
                guest_path: PathBuf::from(SYSTEM_CAPABILITY_GUEST_PATH),
            },
            binding: None,
            membership: None,
            expected_session_id: request.session_id.clone(),
            expected_envelope_revision: request.envelope_revision,
            expected_identity: assigned,
            accepted: Arc::new(Mutex::new(AcceptedCapability::default())),
        })
    }

    fn remove_owned_path(&self) {
        let Ok(metadata) = fs::symlink_metadata(&self.path) else {
            return;
        };
        if metadata.file_type().is_socket()
            && !metadata.file_type().is_symlink()
            && metadata.dev() == self.device
            && metadata.ino() == self.inode
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

impl CapabilityGate for SystemCapabilityGate {
    fn channel(&self) -> Channel {
        self.channel.clone()
    }

    fn bind(
        &mut self,
        binding: CapabilityBinding,
        membership: Arc<dyn ProcessMembership>,
    ) -> Result<(), SupervisorError> {
        if binding.channel_id != self.channel.id()
            || binding.session_id != self.expected_session_id
            || binding.envelope_revision != self.expected_envelope_revision
            || binding.identity_slot != self.expected_identity.slot
            || binding.assigned_uid != self.expected_identity.uid
            || binding.assigned_gid != self.expected_identity.gid
            || self.binding.is_some()
            || self.membership.is_some()
            || !matches!(self.state, Some(ListenerState::Bound(_)))
            || membership.contains(binding.sandbox_leader_pid) != Ok(true)
        {
            return Err(SupervisorError::CapabilityUnavailable);
        }
        self.binding = Some(binding);
        self.membership = Some(membership);
        Ok(())
    }

    fn enable(&mut self) -> Result<(), SupervisorError> {
        if self.binding.is_none() {
            return Err(SupervisorError::CapabilityUnavailable);
        }
        let Some(ListenerState::Bound(bound)) = self.state.take() else {
            return Err(SupervisorError::CapabilityUnavailable);
        };
        let listener = bound
            .enable()
            .map_err(|_| SupervisorError::CapabilityUnavailable)?;
        let accepted = Arc::clone(&self.accepted);
        let membership = Arc::clone(
            self.membership
                .as_ref()
                .ok_or(SupervisorError::CapabilityUnavailable)?,
        );
        if listener
            .accept(
                CredentialPin::Identity {
                    uid: self.expected_identity.uid,
                    gid: self.expected_identity.gid,
                },
                Box::new(move |result| accept_capability(result, accepted, membership)),
            )
            .is_err()
        {
            listener.close();
            return Err(SupervisorError::CapabilityUnavailable);
        }
        self.state = Some(ListenerState::Enabled(listener));
        Ok(())
    }

    fn close(&mut self) {
        if let Some(ListenerState::Enabled(listener)) = self.state.take() {
            listener.close();
        }
        self.state = None;
        let channel = {
            let mut accepted = lock(&self.accepted);
            accepted.closed = true;
            accepted.channel.take()
        };
        if let Some(channel) = channel {
            channel.close();
        }
        self.remove_owned_path();
    }
}

impl Drop for SystemCapabilityGate {
    fn drop(&mut self) {
        self.close();
    }
}

fn ensure_root_directory(
    path: &Path,
    mode: u32,
    require_exact_mode: bool,
) -> Result<(), SupervisorError> {
    match fs::symlink_metadata(path) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir(path).map_err(|_| SupervisorError::CapabilityUnavailable)?;
            fs::set_permissions(path, fs::Permissions::from_mode(mode))
                .map_err(|_| SupervisorError::CapabilityUnavailable)?;
        }
        Err(_) => return Err(SupervisorError::CapabilityUnavailable),
    }
    let metadata =
        fs::symlink_metadata(path).map_err(|_| SupervisorError::CapabilityUnavailable)?;
    let actual_mode = metadata.mode() & 0o777;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != 0
        || actual_mode & 0o022 != 0
        || (require_exact_mode && actual_mode != mode)
    {
        return Err(SupervisorError::CapabilityUnavailable);
    }
    Ok(())
}

fn ensure_system_cgroup_root() -> Result<(), SupervisorError> {
    let cgroup_root = Path::new("/sys/fs/cgroup");
    let session_root = Path::new(SYSTEM_CGROUP_ROOT);
    if session_root.parent() != Some(cgroup_root) {
        return Err(SupervisorError::SpawnFailed);
    }
    let own_namespace =
        fs::metadata("/proc/self/ns/cgroup").map_err(|_| SupervisorError::SpawnFailed)?;
    let initial_namespace =
        fs::metadata("/proc/1/ns/cgroup").map_err(|_| SupervisorError::SpawnFailed)?;
    if own_namespace.dev() != initial_namespace.dev()
        || own_namespace.ino() != initial_namespace.ino()
    {
        return Err(SupervisorError::SpawnFailed);
    }
    let root_metadata =
        fs::symlink_metadata(cgroup_root).map_err(|_| SupervisorError::SpawnFailed)?;
    if !root_metadata.is_dir()
        || root_metadata.file_type().is_symlink()
        || root_metadata.uid() != 0
        || root_metadata.mode() & 0o022 != 0
        || !cgroup_root.join("cgroup.controllers").is_file()
    {
        return Err(SupervisorError::SpawnFailed);
    }
    match fs::symlink_metadata(session_root) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir(session_root).map_err(|_| SupervisorError::SpawnFailed)?;
            fs::set_permissions(session_root, fs::Permissions::from_mode(0o700))
                .map_err(|_| SupervisorError::SpawnFailed)?;
        }
        Err(_) => return Err(SupervisorError::SpawnFailed),
    }
    let metadata = fs::symlink_metadata(session_root).map_err(|_| SupervisorError::SpawnFailed)?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != 0
        || metadata.mode() & 0o777 != 0o700
    {
        return Err(SupervisorError::SpawnFailed);
    }
    let processes = fs::read_to_string(session_root.join("cgroup.procs"))
        .map_err(|_| SupervisorError::SpawnFailed)?;
    if !processes.trim().is_empty() {
        return Err(SupervisorError::SpawnFailed);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use tempfile::TempDir;

    use super::*;

    struct FixedMembership(bool);

    impl ProcessMembership for FixedMembership {
        fn contains(&self, _pid: u32) -> Result<bool, SupervisorError> {
            Ok(self.0)
        }
    }

    struct BlockingMembership {
        entered: mpsc::SyncSender<()>,
        release: Mutex<mpsc::Receiver<()>>,
    }

    impl ProcessMembership for BlockingMembership {
        fn contains(&self, _pid: u32) -> Result<bool, SupervisorError> {
            self.entered
                .send(())
                .map_err(|_| SupervisorError::CapabilityUnavailable)?;
            lock(&self.release)
                .recv()
                .map_err(|_| SupervisorError::CapabilityUnavailable)?;
            Ok(true)
        }
    }

    fn channel_pair() -> (SeqpacketChannel, SeqpacketChannel) {
        let directory = TempDir::new().expect("socket fixture opens");
        let path = directory.path().join("capability.sock");
        let listener = SeqpacketListener::bind(&path).expect("listener binds");
        let connector = SeqpacketConnector::new().expect("connector starts");
        let pin = CredentialPin::Identity {
            uid: rustix::process::getuid().as_raw(),
            gid: rustix::process::getgid().as_raw(),
        };
        let (server_sender, server_receiver) = mpsc::sync_channel(1);
        listener
            .accept(
                pin,
                Box::new(move |result| {
                    server_sender.send(result).expect("server result received");
                }),
            )
            .expect("accept queues");
        let (client_sender, client_receiver) = mpsc::sync_channel(1);
        connector
            .connect(
                &path,
                pin,
                Box::new(move |result| {
                    client_sender.send(result).expect("client result received");
                }),
            )
            .expect("connect queues");
        let client = client_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("client completes")
            .expect("client authenticates");
        let server = server_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("server completes")
            .expect("server authenticates");
        listener.close();
        drop(connector);
        (server, client)
    }

    fn assert_disconnected(channel: SeqpacketChannel) {
        let (sender, receiver) = mpsc::sync_channel(1);
        let queued = channel.receive(Box::new(move |result| {
            sender.send(result).expect("disconnect result received");
        }));
        if matches!(queued, Err(TransportError::Closed)) {
            return;
        }
        queued.expect("receive queues");
        let error = receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("disconnect completes")
            .expect_err("revoked peer cannot receive");
        assert!(matches!(
            error,
            TransportError::Disconnected | TransportError::Closed
        ));
    }

    #[test]
    fn capability_accept_rejects_a_peer_outside_the_session_process_tree() {
        let (server, client) = channel_pair();
        let accepted = Arc::new(Mutex::new(AcceptedCapability::default()));

        accept_capability(
            Ok(server),
            Arc::clone(&accepted),
            Arc::new(FixedMembership(false)),
        );

        assert!(lock(&accepted).channel.is_none());
        assert_disconnected(client);
    }

    #[test]
    fn capability_close_wins_an_accept_callback_already_checking_membership() {
        let (server, client) = channel_pair();
        let accepted = Arc::new(Mutex::new(AcceptedCapability::default()));
        let (entered_sender, entered_receiver) = mpsc::sync_channel(1);
        let (release_sender, release_receiver) = mpsc::sync_channel(1);
        let membership = Arc::new(BlockingMembership {
            entered: entered_sender,
            release: Mutex::new(release_receiver),
        });
        let worker_accepted = Arc::clone(&accepted);
        let worker = thread::spawn(move || {
            accept_capability(Ok(server), worker_accepted, membership);
        });
        entered_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("membership check starts");
        lock(&accepted).closed = true;
        release_sender.send(()).expect("membership check releases");
        worker.join().expect("accept callback finishes");

        assert!(lock(&accepted).channel.is_none());
        assert_disconnected(client);
    }

    #[test]
    fn relay_input_eof_has_a_bounded_exit_window() {
        let (input_sender, input_receiver) = mpsc::sync_channel(1);
        let (_output_sender, output_receiver) = mpsc::sync_channel(1);
        input_sender.send(Ok(())).expect("input EOF is observed");

        let result = await_relay_completion(
            &input_receiver,
            &output_receiver,
            Duration::ZERO,
            Duration::ZERO,
            || Ok(None),
        );

        assert_eq!(result, Err(SupervisorError::RelayFailed));
    }
}
