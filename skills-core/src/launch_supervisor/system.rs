//! Production adapters for the one-shot Launch supervisor.

use std::{
    fs,
    io::{self, Read, Write},
    os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt, chown},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use crate::{
    Digest,
    launch::LaunchRequest,
    launch_protocol::{
        BrokerReconnect, ControllerLossAcknowledgement, ControllerLossSettlement,
        LaunchAuthorization, ProtocolMessage, ProtocolResponse, ReceiptAcknowledgement,
        ResponseResult,
    },
    launch_receipt::ProcessExitClassification,
    launch_transport::{
        AuthenticatedPacket, BoundSeqpacketListener, CredentialPin, LauncherPacket,
        SeqpacketChannel, SeqpacketConnector, SeqpacketListener, TransportError,
    },
    launcher_install::{
        Identity, IdentityLease, LauncherConfig, LauncherError, LauncherPaths, LauncherSigner,
        acquire_identity_with_deadline, bwrap_version_with_deadline, require_measured_bwrap,
    },
    sandbox::{
        BubblewrapBackend, Channel, ConfinementPlan, PreparedSession, ProcessTree, SandboxError,
        SandboxMechanicalState, SandboxedSession,
    },
};

use super::{
    CapabilityBinding, CapabilityGate, IdentityGuard, LaunchBroker, LaunchPlatform, LaunchSigner,
    MechanicFailure, PreparedAgent, ProcessMembership, RunningAgent, RunningAgentEvent,
    SupervisorCompletion, SupervisorError,
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

const PROCESS_EXIT_POLL_INTERVAL: Duration = Duration::from_millis(10);
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
    let broker_pin = CredentialPin::Identity {
        uid: config.broker_uid,
        gid: config.broker_gid,
    };
    let (sender, receiver) = mpsc::sync_channel(1);
    connector
        .connect(
            &config.broker_socket_path,
            broker_pin,
            Box::new(move |result| {
                let _ = sender.try_send(result);
            }),
        )
        .map_err(map_transport)?;
    let channel = receiver
        .recv_timeout(timeout)
        .map_err(|_| SupervisorError::BrokerTimeout)?
        .map_err(map_transport)?;
    Ok(Arc::new(SeqpacketLaunchBroker::new(
        connector,
        config.broker_socket_path.clone(),
        broker_pin,
        channel,
    )))
}

struct SeqpacketLaunchBroker {
    state: Arc<Mutex<SeqpacketLaunchBrokerState>>,
}

struct SeqpacketLaunchBrokerState {
    connector: Option<SeqpacketConnector>,
    socket_path: PathBuf,
    broker_pin: CredentialPin,
    channel: SeqpacketChannel,
    reconnect_generation: u64,
    reconnect: Option<PendingBrokerReconnect>,
    controller_loss_pending: Option<PendingControllerLossSettlement>,
}

struct PendingBrokerReconnect {
    generation: u64,
    candidate: Option<SeqpacketChannel>,
    complete: SupervisorCompletion<BrokerReconnect>,
}

struct PendingControllerLossSettlement {
    request: ControllerLossSettlement,
    complete: SupervisorCompletion<ControllerLossAcknowledgement>,
}

impl SeqpacketLaunchBroker {
    fn new(
        connector: SeqpacketConnector,
        socket_path: PathBuf,
        broker_pin: CredentialPin,
        channel: SeqpacketChannel,
    ) -> Self {
        Self {
            state: Arc::new(Mutex::new(SeqpacketLaunchBrokerState {
                connector: Some(connector),
                socket_path,
                broker_pin,
                channel,
                reconnect_generation: 0,
                reconnect: None,
                controller_loss_pending: None,
            })),
        }
    }

    fn current_channel(&self) -> Result<SeqpacketChannel, SupervisorError> {
        let state = lock(&self.state);
        state
            .connector
            .as_ref()
            .ok_or(SupervisorError::BrokerUnavailable)?;
        Ok(state.channel.clone())
    }

    fn transact<T>(
        &self,
        bytes: Vec<u8>,
        parse: impl FnOnce(AuthenticatedPacket) -> Result<T, SupervisorError> + Send + 'static,
        complete: SupervisorCompletion<T>,
    ) -> Result<(), SupervisorError>
    where
        T: Send + 'static,
    {
        Self::transact_on(self.current_channel()?, bytes, parse, complete)
    }

    fn transact_on<T>(
        channel: SeqpacketChannel,
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
        let receive_channel = channel.clone();
        let send_completion = Arc::clone(&completion);
        channel
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

    fn arm_session_receive(
        state: Arc<Mutex<SeqpacketLaunchBrokerState>>,
        channel: SeqpacketChannel,
        completion: Arc<Mutex<Option<SupervisorCompletion<ProtocolMessage>>>>,
    ) -> Result<(), SupervisorError> {
        let next_channel = channel.clone();
        channel
            .receive(Box::new(move |received| {
                let packet = match received.map_err(map_transport) {
                    Ok(packet) => packet,
                    Err(error) => {
                        let pending = lock(&state).controller_loss_pending.take();
                        if let Some(pending) = pending {
                            (pending.complete)(Err(error.clone()));
                        }
                        if let Some(complete) = lock(&completion).take() {
                            complete(Err(error));
                        }
                        return;
                    }
                };
                if let LauncherPacket::Response(response) = packet.packet {
                    let pending = lock(&state).controller_loss_pending.take();
                    if let Some(pending) = pending {
                        let result = match response.result {
                            ResponseResult::ControllerLossAcknowledgement { acknowledgement }
                                if response.request_id == pending.request.request_id =>
                            {
                                acknowledgement
                                    .validate_for(&pending.request)
                                    .map(|()| acknowledgement)
                                    .map_err(|_| SupervisorError::DurabilityUnavailable)
                            }
                            _ => Err(SupervisorError::DurabilityUnavailable),
                        };
                        (pending.complete)(result);
                        if let Err(error) = Self::arm_session_receive(
                            Arc::clone(&state),
                            next_channel,
                            Arc::clone(&completion),
                        ) && let Some(complete) = lock(&completion).take()
                        {
                            complete(Err(error));
                        }
                        return;
                    }
                    if let Some(complete) = lock(&completion).take() {
                        complete(Err(SupervisorError::BrokerUnavailable));
                    }
                    return;
                }
                let result = match packet.packet {
                    LauncherPacket::Request(message) => Ok(message),
                    LauncherPacket::Response(_) => unreachable!("response handled above"),
                    LauncherPacket::SignedReceipt(_) => Err(SupervisorError::BrokerUnavailable),
                };
                if let Some(complete) = lock(&completion).take() {
                    complete(result);
                }
            }))
            .map_err(map_transport)
    }

    fn finish_reconnect(
        state: &Arc<Mutex<SeqpacketLaunchBrokerState>>,
        generation: u64,
        result: Result<BrokerReconnect, SupervisorError>,
    ) {
        let mut replaced = None;
        let mut rejected = None;
        let (complete, result) = {
            let mut state = lock(state);
            if !state
                .reconnect
                .as_ref()
                .is_some_and(|pending| pending.generation == generation)
            {
                return;
            }
            let pending = state.reconnect.take().expect("generation was checked");
            let candidate = pending.candidate;
            match (result, candidate) {
                (Ok(response), Some(candidate)) if state.connector.is_some() => {
                    replaced = Some(std::mem::replace(&mut state.channel, candidate));
                    (pending.complete, Ok(response))
                }
                (Ok(_), candidate) => {
                    rejected = candidate;
                    (pending.complete, Err(SupervisorError::BrokerUnavailable))
                }
                (Err(error), candidate) => {
                    rejected = candidate;
                    (pending.complete, Err(error))
                }
            }
        };
        if let Some(channel) = replaced {
            channel.close();
        }
        if let Some(channel) = rejected {
            channel.close();
        }
        complete(result);
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
                        ResponseResult::IdentityExhaustion { exhaustion } => Err(
                            SupervisorError::SessionIdentityExhausted(Box::new(exhaustion)),
                        ),
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

    fn reconnect_session(
        &self,
        reconnect: BrokerReconnect,
        complete: SupervisorCompletion<BrokerReconnect>,
    ) -> Result<(), SupervisorError> {
        reconnect
            .validate()
            .map_err(|_| SupervisorError::BrokerUnavailable)?;
        let reconnect_bytes = reconnect.canonical_bytes();
        let callback_state = Arc::clone(&self.state);
        let (generation, queued) = {
            let mut state = lock(&self.state);
            if state.reconnect.is_some() {
                return Err(SupervisorError::BrokerUnavailable);
            }
            if state.connector.is_none() {
                return Err(SupervisorError::BrokerUnavailable);
            }
            let generation = state
                .reconnect_generation
                .checked_add(1)
                .ok_or(SupervisorError::BrokerUnavailable)?;
            let socket_path = state.socket_path.clone();
            let broker_pin = state.broker_pin;
            state.reconnect_generation = generation;
            state.reconnect = Some(PendingBrokerReconnect {
                generation,
                candidate: None,
                complete,
            });
            let connector = state
                .connector
                .as_ref()
                .expect("an open broker retains its connector");
            let queued = connector.connect(
                &socket_path,
                broker_pin,
                Box::new(move |connected| {
                    let candidate = match connected {
                        Ok(candidate) => candidate,
                        Err(_) => {
                            Self::finish_reconnect(
                                &callback_state,
                                generation,
                                Err(SupervisorError::BrokerUnavailable),
                            );
                            return;
                        }
                    };
                    {
                        let mut state = lock(&callback_state);
                        let current = state.connector.is_some()
                            && state
                                .reconnect
                                .as_ref()
                                .is_some_and(|pending| pending.generation == generation);
                        if !current {
                            drop(state);
                            candidate.close();
                            return;
                        }
                        state
                            .reconnect
                            .as_mut()
                            .expect("generation was checked")
                            .candidate = Some(candidate.clone());
                    }
                    let request_id = reconnect.request_id.clone();
                    let response_state = Arc::clone(&callback_state);
                    let enqueue_state = Arc::clone(&callback_state);
                    let queued = Self::transact_on(
                        candidate,
                        reconnect_bytes,
                        move |packet| match packet.packet {
                            LauncherPacket::Response(response)
                                if response.request_id == request_id =>
                            {
                                match response.result {
                                    ResponseResult::BrokerReconnect { reconnect } => Ok(reconnect),
                                    _ => Err(SupervisorError::BrokerUnavailable),
                                }
                            }
                            _ => Err(SupervisorError::BrokerUnavailable),
                        },
                        Box::new(move |result| {
                            Self::finish_reconnect(&response_state, generation, result);
                        }),
                    );
                    if let Err(error) = queued {
                        Self::finish_reconnect(&enqueue_state, generation, Err(error));
                    }
                }),
            );
            (generation, queued)
        };
        if let Err(error) = queued {
            let mut state = lock(&self.state);
            if state
                .reconnect
                .as_ref()
                .is_some_and(|pending| pending.generation == generation)
            {
                state.reconnect.take();
            }
            return Err(map_transport(error));
        }
        Ok(())
    }

    fn cancel_reconnect(&self) {
        let pending = lock(&self.state).reconnect.take();
        if let Some(pending) = pending {
            if let Some(candidate) = pending.candidate {
                candidate.close();
            }
            (pending.complete)(Err(SupervisorError::BrokerUnavailable));
        }
    }

    fn settle_controller_loss(
        &self,
        settlement: ControllerLossSettlement,
        complete: SupervisorCompletion<ControllerLossAcknowledgement>,
    ) -> Result<(), SupervisorError> {
        settlement
            .validate()
            .map_err(|_| SupervisorError::BrokerUnavailable)?;
        let bytes = settlement.canonical_bytes();
        let channel = {
            let mut state = lock(&self.state);
            if state.connector.is_none() || state.controller_loss_pending.is_some() {
                return Err(SupervisorError::BrokerUnavailable);
            }
            state.controller_loss_pending = Some(PendingControllerLossSettlement {
                request: settlement,
                complete,
            });
            state.channel.clone()
        };
        let callback_state = Arc::clone(&self.state);
        let queued = channel.send(
            bytes,
            Box::new(move |result| {
                if result.is_err() {
                    let pending = lock(&callback_state).controller_loss_pending.take();
                    if let Some(pending) = pending {
                        (pending.complete)(Err(SupervisorError::BrokerUnavailable));
                    }
                }
            }),
        );
        if let Err(error) = queued {
            lock(&self.state).controller_loss_pending.take();
            return Err(map_transport(error));
        }
        Ok(())
    }

    fn receive_session_request(
        &self,
        complete: SupervisorCompletion<ProtocolMessage>,
    ) -> Result<(), SupervisorError> {
        Self::arm_session_receive(
            Arc::clone(&self.state),
            self.current_channel()?,
            Arc::new(Mutex::new(Some(complete))),
        )
    }

    fn send_session_receipt(
        &self,
        receipt_bytes: Vec<u8>,
        complete: SupervisorCompletion<()>,
    ) -> Result<(), SupervisorError> {
        self.current_channel()?
            .send(
                receipt_bytes,
                Box::new(move |result| complete(result.map_err(map_transport))),
            )
            .map_err(map_transport)
    }

    fn send_session_response(
        &self,
        response: ProtocolResponse,
        complete: SupervisorCompletion<()>,
    ) -> Result<(), SupervisorError> {
        response
            .validate()
            .map_err(|_| SupervisorError::BrokerUnavailable)?;
        self.current_channel()?
            .send(
                response.canonical_bytes(),
                Box::new(move |result| complete(result.map_err(map_transport))),
            )
            .map_err(map_transport)
    }

    fn close(&self) {
        let (connector, channel, reconnect, controller_loss_pending) = {
            let mut state = lock(&self.state);
            (
                state.connector.take(),
                state.channel.clone(),
                state.reconnect.take(),
                state.controller_loss_pending.take(),
            )
        };
        channel.close();
        if let Some(pending) = reconnect {
            if let Some(candidate) = pending.candidate {
                candidate.close();
            }
            (pending.complete)(Err(SupervisorError::BrokerUnavailable));
        }
        drop(connector);
        if let Some(pending) = controller_loss_pending {
            (pending.complete)(Err(SupervisorError::BrokerUnavailable));
        }
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
            .map_err(|_| SupervisorError::IdentityAssignmentInvalid)?;
        if expected != assigned {
            return Err(SupervisorError::IdentityAssignmentInvalid);
        }
        let deadline = Instant::now()
            .checked_add(self.timeout)
            .unwrap_or_else(Instant::now);
        let lease = acquire_identity_with_deadline(&self.paths, assigned.slot, deadline).map_err(
            |error| match error {
                LauncherError::Occupied { .. } | LauncherError::Poisoned { .. } => {
                    SupervisorError::IdentityAssignmentInvalid
                }
                _ => SupervisorError::IdentityUnavailable,
            },
        )?;
        if lease.identity() != assigned {
            let _ = lease.poison();
            return Err(SupervisorError::IdentityAssignmentInvalid);
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
            .map(|session| {
                Box::new(SystemRunningAgent {
                    session: Arc::new(Mutex::new(session)),
                    relay_events: None,
                }) as Box<dyn RunningAgent>
            })
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
    session: Arc<Mutex<SandboxedSession>>,
    relay_events: Option<Arc<RelayEventGate>>,
}

struct RelayEventGate {
    active: AtomicBool,
    events: Arc<dyn Fn(RunningAgentEvent) + Send + Sync>,
}

fn classify_exit(code: i32) -> ProcessExitClassification {
    match code {
        0 => ProcessExitClassification::Success,
        value if value < 0 => ProcessExitClassification::Signaled,
        _ => ProcessExitClassification::Failure,
    }
}

fn classify_mechanic_failure(
    target: Option<SandboxMechanicalState>,
    observed: Result<SandboxMechanicalState, SandboxError>,
) -> Result<(), MechanicFailure> {
    match observed {
        Ok(state) if Some(state) == target => Ok(()),
        Ok(SandboxMechanicalState::Running) => Err(MechanicFailure::Running),
        Ok(SandboxMechanicalState::Parked) => Err(MechanicFailure::Parked),
        Ok(SandboxMechanicalState::Exited(code)) => {
            Err(MechanicFailure::Terminal(classify_exit(code)))
        }
        Err(_) => Err(MechanicFailure::Ambiguous),
    }
}

fn apply_mechanic<T>(
    session: &Arc<Mutex<SandboxedSession>>,
    target: Option<SandboxMechanicalState>,
    apply: impl FnOnce(&mut SandboxedSession) -> Result<T, SandboxError>,
) -> Result<(), MechanicFailure> {
    let mut session = lock(session);
    match apply(&mut session) {
        Ok(_) => Ok(()),
        Err(_) => classify_mechanic_failure(target, session.mechanical_state()),
    }
}

impl RelayEventGate {
    fn new(events: Arc<dyn Fn(RunningAgentEvent) + Send + Sync>) -> Self {
        Self {
            active: AtomicBool::new(true),
            events,
        }
    }

    fn emit(&self, event: RunningAgentEvent) {
        if self.active.load(Ordering::Acquire) {
            (self.events)(event);
        }
    }

    fn quiesce(&self) {
        self.active.store(false, Ordering::Release);
    }
}

impl RunningAgent for SystemRunningAgent {
    fn start_relay(
        &mut self,
        input: Box<dyn Read + Send>,
        output: Box<dyn Write + Send>,
        events: Arc<dyn Fn(RunningAgentEvent) + Send + Sync>,
    ) -> Result<(), SupervisorError> {
        if self.relay_events.is_some() {
            return Err(SupervisorError::RelayFailed);
        }
        let (agent_input, agent_output, agent_error) = {
            let mut session = lock(&self.session);
            (
                session.take_stdin().ok_or(SupervisorError::RelayFailed)?,
                session.take_stdout().ok_or(SupervisorError::RelayFailed)?,
                session.take_stderr().ok_or(SupervisorError::RelayFailed)?,
            )
        };
        let events = Arc::new(RelayEventGate::new(events));
        self.relay_events = Some(Arc::clone(&events));
        start_relay_workers(
            Arc::clone(&self.session),
            input,
            output,
            agent_input,
            agent_output,
            agent_error,
            events,
        )
    }

    fn quiesce_relay(&mut self, complete: SupervisorCompletion<()>) -> Result<(), SupervisorError> {
        if let Some(events) = self.relay_events.take() {
            events.quiesce();
        }
        thread::Builder::new()
            .name("louiselm-launch-relay-quiescence".to_owned())
            .spawn(move || complete(Ok(())))
            .map(drop)
            .map_err(|_| SupervisorError::RelayFailed)
    }

    fn park(&mut self) -> Result<(), MechanicFailure> {
        apply_mechanic(
            &self.session,
            Some(SandboxMechanicalState::Parked),
            SandboxedSession::park,
        )
    }

    fn resume(&mut self) -> Result<(), MechanicFailure> {
        apply_mechanic(
            &self.session,
            Some(SandboxMechanicalState::Running),
            SandboxedSession::resume,
        )
    }

    fn interrupt(&mut self) -> Result<(), MechanicFailure> {
        apply_mechanic(&self.session, None, SandboxedSession::interrupt)
    }

    fn dispose(&mut self) -> Result<(), SupervisorError> {
        lock(&self.session)
            .dispose()
            .map(|_| ())
            .map_err(map_sandbox)
    }
}

fn start_relay_workers(
    session: Arc<Mutex<SandboxedSession>>,
    mut input: Box<dyn Read + Send>,
    mut output: Box<dyn Write + Send>,
    mut agent_input: std::process::ChildStdin,
    mut agent_output: std::process::ChildStdout,
    mut agent_error: std::process::ChildStderr,
    events: Arc<RelayEventGate>,
) -> Result<(), SupervisorError> {
    let input_events = Arc::clone(&events);
    thread::Builder::new()
        .name("louiselm-launch-acp-input".to_owned())
        .spawn(move || {
            let mut buffer = [0_u8; 8 * 1024];
            loop {
                match input.read(&mut buffer) {
                    Ok(0) => {
                        input_events.emit(RunningAgentEvent::ControllerEof);
                        break;
                    }
                    Ok(read) if agent_input.write_all(&buffer[..read]).is_ok() => {}
                    Ok(_) => {
                        input_events.emit(RunningAgentEvent::RelayFailed);
                        break;
                    }
                    Err(_) => {
                        input_events.emit(RunningAgentEvent::RelayFailed);
                        break;
                    }
                }
            }
        })
        .map(drop)
        .map_err(|_| SupervisorError::RelayFailed)?;

    let error_events = Arc::clone(&events);
    thread::Builder::new()
        .name("louiselm-launch-agent-stderr".to_owned())
        .spawn(move || {
            if io::copy(&mut agent_error, &mut io::sink()).is_err() {
                error_events.emit(RunningAgentEvent::RelayFailed);
            }
        })
        .map(drop)
        .map_err(|_| SupervisorError::RelayFailed)?;

    let output_events = Arc::clone(&events);
    thread::Builder::new()
        .name("louiselm-launch-acp-output".to_owned())
        .spawn(move || {
            if io::copy(&mut agent_output, &mut output)
                .and_then(|_| output.flush())
                .is_err()
            {
                output_events.emit(RunningAgentEvent::RelayFailed);
            }
        })
        .map(drop)
        .map_err(|_| SupervisorError::RelayFailed)?;

    thread::Builder::new()
        .name("louiselm-launch-process-exit".to_owned())
        .spawn(move || {
            loop {
                match lock(&session).try_wait().map_err(map_sandbox) {
                    Ok(Some(code)) => {
                        let classification = classify_exit(code);
                        events.emit(RunningAgentEvent::ProcessExited(classification));
                        break;
                    }
                    Ok(None) => thread::sleep(PROCESS_EXIT_POLL_INTERVAL),
                    Err(_) => {
                        events.emit(RunningAgentEvent::RelayFailed);
                        break;
                    }
                }
            }
        })
        .map(drop)
        .map_err(|_| SupervisorError::RelayFailed)
}

fn map_sandbox(error: SandboxError) -> SupervisorError {
    match error {
        SandboxError::CleanupUnproven { .. } | SandboxError::Survivors { .. } => {
            SupervisorError::CleanupUnproven
        }
        _ => SupervisorError::SpawnFailed,
    }
}

#[cfg(test)]
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
    Revoked(SeqpacketListener),
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
    generation: u64,
    channel: Option<SeqpacketChannel>,
}

fn accept_capability(
    result: Result<SeqpacketChannel, TransportError>,
    accepted: Arc<Mutex<AcceptedCapability>>,
    membership: Arc<dyn ProcessMembership>,
    generation: u64,
) {
    let Ok(channel) = result else {
        return;
    };
    if membership.contains(channel.peer_credentials().pid) != Ok(true) {
        channel.close();
        return;
    }
    let mut accepted = lock(&accepted);
    if accepted.closed || accepted.generation != generation {
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

    fn set_owned_mode(&self, mode: u32) -> Result<(), SupervisorError> {
        let metadata =
            fs::symlink_metadata(&self.path).map_err(|_| SupervisorError::CapabilityUnavailable)?;
        if !metadata.file_type().is_socket()
            || metadata.file_type().is_symlink()
            || metadata.dev() != self.device
            || metadata.ino() != self.inode
        {
            return Err(SupervisorError::CapabilityUnavailable);
        }
        fs::set_permissions(&self.path, fs::Permissions::from_mode(mode))
            .map_err(|_| SupervisorError::CapabilityUnavailable)
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
        let (listener, was_revoked) = match self.state.take() {
            Some(ListenerState::Bound(bound)) => (
                bound
                    .enable()
                    .map_err(|_| SupervisorError::CapabilityUnavailable)?,
                false,
            ),
            Some(ListenerState::Revoked(listener)) => {
                if let Err(error) = self.set_owned_mode(0o600) {
                    self.state = Some(ListenerState::Revoked(listener));
                    return Err(error);
                }
                (listener, true)
            }
            Some(ListenerState::Enabled(listener)) => {
                self.state = Some(ListenerState::Enabled(listener));
                return Ok(());
            }
            None => return Err(SupervisorError::CapabilityUnavailable),
        };
        let generation = {
            let mut accepted = lock(&self.accepted);
            let Some(generation) = accepted.generation.checked_add(1) else {
                accepted.closed = true;
                if was_revoked {
                    let _ = self.set_owned_mode(0o000);
                    self.state = Some(ListenerState::Revoked(listener));
                } else {
                    listener.close();
                }
                return Err(SupervisorError::CapabilityUnavailable);
            };
            accepted.generation = generation;
            accepted.closed = false;
            generation
        };
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
                Box::new(move |result| accept_capability(result, accepted, membership, generation)),
            )
            .is_err()
        {
            lock(&self.accepted).closed = true;
            if was_revoked {
                let _ = self.set_owned_mode(0o000);
                self.state = Some(ListenerState::Revoked(listener));
            } else {
                listener.close();
            }
            return Err(SupervisorError::CapabilityUnavailable);
        }
        self.state = Some(ListenerState::Enabled(listener));
        Ok(())
    }

    fn revoke(&mut self) -> Result<(), SupervisorError> {
        let channel = {
            let mut accepted = lock(&self.accepted);
            accepted.closed = true;
            accepted.channel.take()
        };
        if let Some(channel) = channel {
            channel.close();
        }
        match self.state.take() {
            Some(ListenerState::Enabled(listener)) => {
                let result = self.set_owned_mode(0o000);
                self.state = Some(ListenerState::Revoked(listener));
                result
            }
            Some(ListenerState::Revoked(listener)) => {
                self.state = Some(ListenerState::Revoked(listener));
                Ok(())
            }
            Some(ListenerState::Bound(bound)) => {
                self.state = Some(ListenerState::Bound(bound));
                Ok(())
            }
            None => Err(SupervisorError::CapabilityUnavailable),
        }
    }

    fn close(&mut self) {
        match self.state.take() {
            Some(ListenerState::Enabled(listener) | ListenerState::Revoked(listener)) => {
                listener.close();
            }
            Some(ListenerState::Bound(_)) | None => {}
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

    use crate::launch_protocol::{
        BROKER_RECONNECT_SCHEMA, CONTROLLER_LOSS_ACK_SCHEMA, CONTROLLER_LOSS_SETTLEMENT_SCHEMA,
        ControllerLossDisposition, ErrorCode, PROTOCOL_VERSION, ProtocolError, RESPONSE_SCHEMA,
        STATUS_REQUEST_SCHEMA, StatusRequest,
    };
    use crate::launch_receipt::ReceiptHead;

    use super::*;

    #[test]
    fn mechanic_failure_normalizes_a_proven_park_or_resume_target() {
        assert_eq!(
            classify_mechanic_failure(
                Some(SandboxMechanicalState::Parked),
                Ok(SandboxMechanicalState::Parked),
            ),
            Ok(()),
        );
        assert_eq!(
            classify_mechanic_failure(
                Some(SandboxMechanicalState::Running),
                Ok(SandboxMechanicalState::Running),
            ),
            Ok(()),
        );
    }

    #[test]
    fn mechanic_failure_preserves_known_terminal_and_ambiguous_state() {
        assert_eq!(
            classify_mechanic_failure(
                Some(SandboxMechanicalState::Parked),
                Ok(SandboxMechanicalState::Running),
            ),
            Err(MechanicFailure::Running),
        );
        assert_eq!(
            classify_mechanic_failure(None, Ok(SandboxMechanicalState::Parked)),
            Err(MechanicFailure::Parked),
        );
        assert_eq!(
            classify_mechanic_failure(None, Ok(SandboxMechanicalState::Exited(-1))),
            Err(MechanicFailure::Terminal(
                ProcessExitClassification::Signaled,
            )),
        );
        assert_eq!(
            classify_mechanic_failure(None, Ok(SandboxMechanicalState::Exited(0))),
            Err(MechanicFailure::Terminal(
                ProcessExitClassification::Success,
            )),
        );
        assert_eq!(
            classify_mechanic_failure(None, Ok(SandboxMechanicalState::Exited(7))),
            Err(MechanicFailure::Terminal(
                ProcessExitClassification::Failure,
            )),
        );
        assert_eq!(
            classify_mechanic_failure(
                Some(SandboxMechanicalState::Running),
                Err(SandboxError::NoCgroup("unavailable".to_owned())),
            ),
            Err(MechanicFailure::Ambiguous),
        );
    }

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

    fn broker_fixture() -> (
        TempDir,
        SeqpacketListener,
        SeqpacketLaunchBroker,
        SeqpacketChannel,
        CredentialPin,
    ) {
        let directory = TempDir::new().expect("socket fixture opens");
        let path = directory.path().join("broker.sock");
        let listener = SeqpacketListener::bind(&path).expect("listener binds");
        let connector = SeqpacketConnector::new().expect("connector starts");
        let pin = CredentialPin::Identity {
            uid: rustix::process::getuid().as_raw(),
            gid: rustix::process::getgid().as_raw(),
        };
        let accepted = accept_channel(&listener, pin);
        let (connected, connection) = mpsc::sync_channel(1);
        connector
            .connect(
                &path,
                pin,
                Box::new(move |result| {
                    connected.send(result).expect("connect result received");
                }),
            )
            .expect("connect queues");
        let client = connection
            .recv_timeout(Duration::from_secs(2))
            .expect("connect completes")
            .expect("client authenticates");
        let server = accepted
            .recv_timeout(Duration::from_secs(2))
            .expect("accept completes")
            .expect("server authenticates");
        let broker = SeqpacketLaunchBroker::new(connector, path, pin, client);
        (directory, listener, broker, server, pin)
    }

    fn accept_channel(
        listener: &SeqpacketListener,
        pin: CredentialPin,
    ) -> mpsc::Receiver<Result<SeqpacketChannel, TransportError>> {
        let (sender, receiver) = mpsc::sync_channel(1);
        listener
            .accept(
                pin,
                Box::new(move |result| {
                    sender.send(result).expect("accept result received");
                }),
            )
            .expect("accept queues");
        receiver
    }

    fn receive_packet(channel: &SeqpacketChannel) -> AuthenticatedPacket {
        let (sender, receiver) = mpsc::sync_channel(1);
        channel
            .receive(Box::new(move |result| {
                sender.send(result).expect("receive result received");
            }))
            .expect("receive queues");
        receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("receive completes")
            .expect("packet authenticates")
    }

    fn send_packet(channel: &SeqpacketChannel, bytes: Vec<u8>) {
        let (sender, receiver) = mpsc::sync_channel(1);
        channel
            .send(
                bytes,
                Box::new(move |result| {
                    sender.send(result).expect("send result received");
                }),
            )
            .expect("send queues");
        receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("send completes")
            .expect("packet sends");
    }

    fn reconnect_request() -> BrokerReconnect {
        BrokerReconnect {
            schema: BROKER_RECONNECT_SCHEMA.to_owned(),
            protocol_version: PROTOCOL_VERSION,
            request_id: "reconnect-1".to_owned(),
            session_id: "session-1".to_owned(),
            run_id: "run-1".to_owned(),
            envelope_revision: 7,
            sequence: 3,
            receipt_digest: Digest::of(b"launcher-head").to_string(),
        }
    }

    fn status_request(request_id: &str) -> StatusRequest {
        StatusRequest {
            schema: STATUS_REQUEST_SCHEMA.to_owned(),
            protocol_version: PROTOCOL_VERSION,
            request_id: request_id.to_owned(),
            session_id: "session-1".to_owned(),
            run_id: "run-1".to_owned(),
        }
    }

    fn controller_loss_settlement() -> ControllerLossSettlement {
        ControllerLossSettlement {
            schema: CONTROLLER_LOSS_SETTLEMENT_SCHEMA.to_owned(),
            protocol_version: PROTOCOL_VERSION,
            request_id: "controller-loss-settlement-1".to_owned(),
            session_id: "session-1".to_owned(),
            run_id: "run-1".to_owned(),
            envelope_revision: 7,
            parked_head: ReceiptHead {
                sequence: 4,
                digest: Digest::of(b"parked-head").to_string(),
            },
        }
    }

    fn arm_broker_receive(
        broker: &SeqpacketLaunchBroker,
    ) -> mpsc::Receiver<Result<ProtocolMessage, SupervisorError>> {
        let (sender, receiver) = mpsc::sync_channel(1);
        broker
            .receive_session_request(Box::new(move |result| {
                sender.send(result).expect("broker result received");
            }))
            .expect("broker receive queues");
        receiver
    }

    fn start_reconnect(
        broker: &SeqpacketLaunchBroker,
        listener: &SeqpacketListener,
        pin: CredentialPin,
        reconnect: BrokerReconnect,
    ) -> (
        SeqpacketChannel,
        mpsc::Receiver<Result<BrokerReconnect, SupervisorError>>,
    ) {
        let accepted = accept_channel(listener, pin);
        let (sender, receiver) = mpsc::sync_channel(1);
        broker
            .reconnect_session(
                reconnect,
                Box::new(move |result| {
                    sender.send(result).expect("reconnect result received");
                }),
            )
            .expect("reconnect queues");
        let candidate = accepted
            .recv_timeout(Duration::from_secs(2))
            .expect("candidate accept completes")
            .expect("candidate authenticates");
        (candidate, receiver)
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

    fn connect_channel(path: &Path, pin: CredentialPin) -> SeqpacketChannel {
        let connector = SeqpacketConnector::new().expect("connector starts");
        let (sender, receiver) = mpsc::sync_channel(1);
        connector
            .connect(
                path,
                pin,
                Box::new(move |result| sender.send(result).expect("connect result received")),
            )
            .expect("connect queues");
        receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("connect completes")
            .expect("client authenticates")
    }

    fn wait_for_accepted(gate: &SystemCapabilityGate) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while lock(&gate.accepted).channel.is_none() {
            assert!(
                Instant::now() < deadline,
                "capability connection was never accepted"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn capability_revoke_disconnects_and_preserves_a_reenableable_rendezvous() {
        let directory = TempDir::new().expect("socket fixture opens");
        let path = directory.path().join("capability.sock");
        let bound = SeqpacketListener::bind_disabled(&path).expect("listener binds disabled");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .expect("capability mode is fixed");
        let metadata = fs::symlink_metadata(&path).expect("capability metadata reads");
        let identity = Identity {
            slot: 3,
            uid: rustix::process::getuid().as_raw(),
            gid: rustix::process::getgid().as_raw(),
        };
        let pin = CredentialPin::Identity {
            uid: identity.uid,
            gid: identity.gid,
        };
        let mut gate = SystemCapabilityGate {
            state: Some(ListenerState::Bound(bound)),
            path: path.clone(),
            device: metadata.dev(),
            inode: metadata.ino(),
            channel: Channel::UnixSocket {
                id: "agent-capability".to_owned(),
                host_path: path.clone(),
                guest_path: PathBuf::from(SYSTEM_CAPABILITY_GUEST_PATH),
            },
            binding: None,
            membership: None,
            expected_session_id: "session-1".to_owned(),
            expected_envelope_revision: 7,
            expected_identity: identity,
            accepted: Arc::new(Mutex::new(AcceptedCapability::default())),
        };
        gate.bind(
            CapabilityBinding {
                session_id: "session-1".to_owned(),
                channel_id: "agent-capability".to_owned(),
                envelope_revision: 7,
                identity_slot: identity.slot,
                assigned_uid: identity.uid,
                assigned_gid: identity.gid,
                sandbox_leader_pid: 42,
            },
            Arc::new(FixedMembership(true)),
        )
        .expect("capability binding is valid");

        gate.enable().expect("capability enables");
        let first = connect_channel(&path, pin);
        wait_for_accepted(&gate);
        gate.revoke().expect("capability revokes");
        assert_eq!(
            fs::symlink_metadata(&path)
                .expect("revoked path remains")
                .mode()
                & 0o777,
            0,
        );
        assert!(matches!(gate.state, Some(ListenerState::Revoked(_))));
        assert_disconnected(first);

        gate.enable().expect("revoked capability re-enables");
        assert_eq!(
            fs::symlink_metadata(&path)
                .expect("re-enabled path remains")
                .mode()
                & 0o777,
            0o600,
        );
        let second = connect_channel(&path, pin);
        wait_for_accepted(&gate);
        gate.close();
        assert_disconnected(second);
        assert!(!path.exists());
    }

    #[test]
    fn capability_accept_rejects_a_peer_outside_the_session_process_tree() {
        let (server, client) = channel_pair();
        let accepted = Arc::new(Mutex::new(AcceptedCapability::default()));

        accept_capability(
            Ok(server),
            Arc::clone(&accepted),
            Arc::new(FixedMembership(false)),
            0,
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
            accept_capability(Ok(server), worker_accepted, membership, 0);
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
    fn capability_reenable_rejects_an_accept_from_the_revoked_generation() {
        let (server, client) = channel_pair();
        let accepted = Arc::new(Mutex::new(AcceptedCapability::default()));
        let (entered_sender, entered_receiver) = mpsc::sync_channel(1);
        let (release_sender, release_receiver) = mpsc::sync_channel(1);
        let membership = Arc::new(BlockingMembership {
            entered: entered_sender,
            release: Mutex::new(release_receiver),
        });
        lock(&accepted).generation = 1;
        let worker_accepted = Arc::clone(&accepted);
        let worker = thread::spawn(move || {
            accept_capability(Ok(server), worker_accepted, membership, 1);
        });
        entered_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("membership check starts");
        {
            let mut state = lock(&accepted);
            state.closed = true;
            state.generation = 2;
            state.closed = false;
        }
        release_sender.send(()).expect("membership check releases");
        worker.join().expect("accept callback finishes");

        assert!(
            lock(&accepted).channel.is_none(),
            "re-enable cannot revive an accept authorized before revoke"
        );
        assert_disconnected(client);
    }

    #[test]
    fn broker_reconnect_swaps_only_after_a_correlated_head_response() {
        let (_directory, listener, broker, old_server, pin) = broker_fixture();
        let request = reconnect_request();
        let (candidate, completed) = start_reconnect(&broker, &listener, pin, request.clone());

        let offered = receive_packet(&candidate);
        assert_eq!(offered.bytes, request.canonical_bytes());
        assert_eq!(
            offered.packet,
            LauncherPacket::Request(ProtocolMessage::BrokerReconnect(request.clone()))
        );

        let old_receive = arm_broker_receive(&broker);
        let before_swap = status_request("before-swap");
        send_packet(&old_server, before_swap.canonical_bytes());
        assert_eq!(
            old_receive
                .recv_timeout(Duration::from_secs(2))
                .expect("old channel receive completes")
                .expect("old channel remains active until validation"),
            ProtocolMessage::Status(before_swap),
        );

        let mut broker_head = request.clone();
        broker_head.sequence = 1;
        broker_head.receipt_digest = Digest::of(b"broker-head").to_string();
        let response = ProtocolResponse {
            schema: RESPONSE_SCHEMA.to_owned(),
            protocol_version: PROTOCOL_VERSION,
            request_id: request.request_id.clone(),
            result: ResponseResult::BrokerReconnect {
                reconnect: broker_head.clone(),
            },
        };
        send_packet(&candidate, response.canonical_bytes());
        assert_eq!(
            completed
                .recv_timeout(Duration::from_secs(2))
                .expect("reconnect completes")
                .expect("correlated broker head is accepted"),
            broker_head,
        );

        let new_receive = arm_broker_receive(&broker);
        let after_swap = status_request("after-swap");
        send_packet(&candidate, after_swap.canonical_bytes());
        assert_eq!(
            new_receive
                .recv_timeout(Duration::from_secs(2))
                .expect("new channel receive completes")
                .expect("new channel owns later traffic"),
            ProtocolMessage::Status(after_swap),
        );
        assert_disconnected(old_server);
    }

    #[test]
    fn cancelled_reconnect_cannot_clear_or_replace_a_newer_attempt() {
        let (_directory, listener, broker, old_server, pin) = broker_fixture();
        let request = reconnect_request();
        let (stale_candidate, stale_completed) =
            start_reconnect(&broker, &listener, pin, request.clone());
        assert_eq!(
            receive_packet(&stale_candidate).bytes,
            request.canonical_bytes()
        );

        broker.cancel_reconnect();
        assert_eq!(
            stale_completed
                .recv_timeout(Duration::from_secs(2))
                .expect("cancelled reconnect completes"),
            Err(SupervisorError::BrokerUnavailable),
        );

        let (candidate, completed) = start_reconnect(&broker, &listener, pin, request.clone());
        assert_eq!(receive_packet(&candidate).bytes, request.canonical_bytes());
        assert_disconnected(stale_candidate);

        let response = ProtocolResponse {
            schema: RESPONSE_SCHEMA.to_owned(),
            protocol_version: PROTOCOL_VERSION,
            request_id: request.request_id.clone(),
            result: ResponseResult::BrokerReconnect {
                reconnect: request.clone(),
            },
        };
        send_packet(&candidate, response.canonical_bytes());
        assert_eq!(
            completed
                .recv_timeout(Duration::from_secs(2))
                .expect("newer reconnect completes"),
            Ok(request),
        );

        broker.cancel_reconnect();
        let new_receive = arm_broker_receive(&broker);
        let after_swap = status_request("after-cancelled-reconnect-retry");
        send_packet(&candidate, after_swap.canonical_bytes());
        assert_eq!(
            new_receive
                .recv_timeout(Duration::from_secs(2))
                .expect("new channel receive completes")
                .expect("stale reconnect callback cannot replace the new channel"),
            ProtocolMessage::Status(after_swap),
        );
        assert_disconnected(old_server);
    }

    #[test]
    fn broker_reconnect_rejects_wrong_outer_correlation_or_result_without_replacing_the_channel() {
        let (_directory, listener, broker, old_server, pin) = broker_fixture();
        let request = reconnect_request();
        let mut wrong_request = request.clone();
        wrong_request.request_id = "another-request".to_owned();
        let responses = [
            ProtocolResponse {
                schema: RESPONSE_SCHEMA.to_owned(),
                protocol_version: PROTOCOL_VERSION,
                request_id: wrong_request.request_id.clone(),
                result: ResponseResult::BrokerReconnect {
                    reconnect: wrong_request,
                },
            },
            ProtocolResponse {
                schema: RESPONSE_SCHEMA.to_owned(),
                protocol_version: PROTOCOL_VERSION,
                request_id: request.request_id.clone(),
                result: ResponseResult::Error {
                    error: ProtocolError::new(ErrorCode::BrokerUnavailable, None, None),
                },
            },
        ];

        for response in responses {
            let (candidate, completed) = start_reconnect(&broker, &listener, pin, request.clone());
            let offered = receive_packet(&candidate);
            assert_eq!(offered.bytes, request.canonical_bytes());
            send_packet(&candidate, response.canonical_bytes());
            assert_eq!(
                completed
                    .recv_timeout(Duration::from_secs(2))
                    .expect("reconnect rejection completes"),
                Err(SupervisorError::BrokerUnavailable),
            );
            assert_disconnected(candidate);
        }

        let old_receive = arm_broker_receive(&broker);
        let retained = status_request("retained-old-channel");
        send_packet(&old_server, retained.canonical_bytes());
        assert_eq!(
            old_receive
                .recv_timeout(Duration::from_secs(2))
                .expect("retained channel receive completes")
                .expect("invalid reconnect never replaces the channel"),
            ProtocolMessage::Status(retained),
        );
    }

    #[test]
    fn broker_reconnect_forwards_nested_subject_and_envelope_for_owner_classification() {
        let request = reconnect_request();
        let mut wrong_subject = request.clone();
        wrong_subject.session_id = "another-session".to_owned();
        let mut wrong_envelope = request.clone();
        wrong_envelope.envelope_revision += 1;

        for broker_head in [wrong_subject, wrong_envelope] {
            let (_directory, listener, broker, old_server, pin) = broker_fixture();
            let (candidate, completed) = start_reconnect(&broker, &listener, pin, request.clone());
            assert_eq!(receive_packet(&candidate).bytes, request.canonical_bytes());
            let response = ProtocolResponse {
                schema: RESPONSE_SCHEMA.to_owned(),
                protocol_version: PROTOCOL_VERSION,
                request_id: request.request_id.clone(),
                result: ResponseResult::BrokerReconnect {
                    reconnect: broker_head.clone(),
                },
            };
            send_packet(&candidate, response.canonical_bytes());
            assert_eq!(
                completed
                    .recv_timeout(Duration::from_secs(2))
                    .expect("reconnect classification reaches its owner")
                    .expect("transport accepts a structurally valid nested checkpoint"),
                broker_head,
            );

            let new_receive = arm_broker_receive(&broker);
            let after_swap = status_request("nested-conflict-reaches-owner");
            send_packet(&candidate, after_swap.canonical_bytes());
            assert_eq!(
                new_receive
                    .recv_timeout(Duration::from_secs(2))
                    .expect("candidate receive completes")
                    .expect("candidate becomes the authenticated current channel"),
                ProtocolMessage::Status(after_swap),
            );
            assert_disconnected(old_server);
        }
    }

    #[test]
    fn controller_loss_ack_is_demultiplexed_without_stealing_session_requests() {
        let (_directory, _listener, broker, server, _pin) = broker_fixture();
        let first_receive = arm_broker_receive(&broker);
        let settlement = controller_loss_settlement();
        let (settled_sender, settled_receiver) = mpsc::sync_channel(1);
        broker
            .settle_controller_loss(
                settlement.clone(),
                Box::new(move |result| {
                    settled_sender
                        .send(result)
                        .expect("settlement result received");
                }),
            )
            .expect("settlement send queues");
        let offered = receive_packet(&server);
        assert_eq!(offered.bytes, settlement.canonical_bytes());

        let before = status_request("status-before-settlement-ack");
        send_packet(&server, before.canonical_bytes());
        assert_eq!(
            first_receive
                .recv_timeout(Duration::from_secs(2))
                .expect("ordinary request remains deliverable")
                .expect("ordinary request authenticates"),
            ProtocolMessage::Status(before),
        );

        let second_receive = arm_broker_receive(&broker);
        let acknowledgement = ControllerLossAcknowledgement {
            schema: CONTROLLER_LOSS_ACK_SCHEMA.to_owned(),
            protocol_version: PROTOCOL_VERSION,
            request_id: settlement.request_id.clone(),
            session_id: settlement.session_id.clone(),
            run_id: settlement.run_id.clone(),
            envelope_revision: settlement.envelope_revision,
            parked_head: settlement.parked_head.clone(),
            disposition: ControllerLossDisposition::Recoverable {
                acp_recovery_reference: "acp-recovery-1".to_owned(),
                attention_projection_id: "attention-1".to_owned(),
            },
        };
        let response = ProtocolResponse {
            schema: RESPONSE_SCHEMA.to_owned(),
            protocol_version: PROTOCOL_VERSION,
            request_id: settlement.request_id.clone(),
            result: ResponseResult::ControllerLossAcknowledgement {
                acknowledgement: acknowledgement.clone(),
            },
        };
        send_packet(&server, response.canonical_bytes());
        assert_eq!(
            settled_receiver
                .recv_timeout(Duration::from_secs(2))
                .expect("settlement completes"),
            Ok(acknowledgement),
        );
        assert!(second_receive.try_recv().is_err());

        let after = status_request("status-after-settlement-ack");
        send_packet(&server, after.canonical_bytes());
        assert_eq!(
            second_receive
                .recv_timeout(Duration::from_secs(2))
                .expect("logical receive remains armed after demultiplexing")
                .expect("later request authenticates"),
            ProtocolMessage::Status(after),
        );
    }

    #[test]
    fn closing_the_broker_cancels_a_pending_reconnect_channel() {
        let (_directory, listener, broker, old_server, pin) = broker_fixture();
        let request = reconnect_request();
        let (candidate, completed) = start_reconnect(&broker, &listener, pin, request.clone());
        assert_eq!(receive_packet(&candidate).bytes, request.canonical_bytes());

        broker.close();

        assert_eq!(
            completed
                .recv_timeout(Duration::from_secs(2))
                .expect("reconnect cancellation completes"),
            Err(SupervisorError::BrokerUnavailable),
        );
        assert_disconnected(candidate);
        assert_disconnected(old_server);
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
