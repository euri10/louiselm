//! Production adapters for the one-shot Launch supervisor.

#[cfg(test)]
#[path = "system_relay_tests.rs"]
mod relay_tests;

#[cfg(test)]
#[path = "tool_integration_tests.rs"]
mod tool_integration_tests;

#[cfg(test)]
#[path = "installed_tests.rs"]
mod installed_tests;

#[path = "system_command.rs"]
mod command_io;

#[cfg(test)]
#[path = "command_test_support.rs"]
pub(super) mod command_test_support;

#[cfg(test)]
#[path = "grant_test_support.rs"]
pub(super) mod grant_test_support;

use std::{
    fs, io,
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
        BrokerReconnect, ControllerLossAcknowledgement, ControllerLossSettlement,
        LaunchAuthorization, ProtocolMessage, ProtocolResponse, ReceiptAcknowledgement,
        ResponseResult,
    },
    launch_receipt::ProcessExitClassification,
    launch_transport::{
        AuthenticatedPacket, BoundSeqpacketListener, CredentialPin, KernelProcess, LauncherPacket,
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
    AgentAuthentication, CapabilityBinding, CapabilityGate, IdentityGuard, LaunchBroker,
    LaunchPlatform, LaunchSigner, MechanicFailure, PreparedAgent, ProcessMembership, RelayStdio,
    RunningAgent, RunningAgentEvents, SupervisorCompletion, SupervisorError, relay::RelayWorker,
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

const SIGNER_CLEANUP_MARGIN: Duration = Duration::from_millis(250);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn map_transport(_: TransportError) -> SupervisorError {
    SupervisorError::BrokerUnavailable
}

/// Connects to the install-pinned broker, directly or through its root service manager.
/// Listener credentials may name root; every packet must name the installed
/// non-root broker identity. Root listener ownership is not message authority.
///
/// # Errors
/// Returns `BrokerUnavailable` for transport/credential failures or `BrokerTimeout` when the bounded connection attempt expires.
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
        .connect_via_manager(
            &config.broker_socket_path,
            broker_pin.clone(),
            CredentialPin::Identity { uid: 0, gid: 0 },
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
        Self::transact_on(&self.current_channel()?, bytes, parse, complete)
    }

    fn transact_on<T>(
        channel: &SeqpacketChannel,
        bytes: Vec<u8>,
        parse: impl FnOnce(AuthenticatedPacket) -> Result<T, SupervisorError> + Send + 'static,
        complete: SupervisorCompletion<T>,
    ) -> Result<(), SupervisorError>
    where
        T: Send + 'static,
    {
        Self::transact_packets_on(channel, vec![bytes].into_iter(), parse, complete)
    }

    fn transact_packets_on<T>(
        channel: &SeqpacketChannel,
        mut packets: std::vec::IntoIter<Vec<u8>>,
        parse: impl FnOnce(AuthenticatedPacket) -> Result<T, SupervisorError> + Send + 'static,
        complete: SupervisorCompletion<T>,
    ) -> Result<(), SupervisorError>
    where
        T: Send + 'static,
    {
        let bytes = packets.next().ok_or(SupervisorError::ReceiptInvalid)?;
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
                    if packets.len() > 0 {
                        let callback_completion = Arc::clone(&send_completion);
                        let queued = Self::transact_packets_on(
                            &receive_channel,
                            packets,
                            parse,
                            Box::new(move |result| finish(&callback_completion, result)),
                        );
                        if let Err(error) = queued {
                            finish(&send_completion, Err(error));
                        }
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
        channel: &SeqpacketChannel,
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
                            &next_channel,
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
                    LauncherPacket::SignedReceipt(_)
                    | LauncherPacket::ConformanceUpdate(_)
                    | LauncherPacket::ConformanceReportChunk(_) => {
                        Err(SupervisorError::BrokerUnavailable)
                    }
                };
                if let Some(complete) = lock(&completion).take() {
                    complete(result);
                }
            }))
            .map_err(map_transport)
    }

    #[expect(
        clippy::expect_used,
        reason = "The same held mutex protects the generation check and taking its pending reconnect."
    )]
    fn finish_reconnect(
        state: &Arc<Mutex<SeqpacketLaunchBrokerState>>,
        generation: u64,
        result: Result<BrokerReconnect, SupervisorError>,
    ) {
        let mut replaced = None;
        let mut rejected = None;
        let (complete, result) = {
            let mut state = lock(state);
            if state
                .reconnect
                .as_ref()
                .is_none_or(|pending| pending.generation != generation)
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
    fn send_conformance(
        &self,
        update: crate::launch_protocol::ConformanceUpdate,
        complete: SupervisorCompletion<()>,
    ) -> Result<(), SupervisorError> {
        let bytes = update
            .canonical_bytes()
            .map_err(|_| SupervisorError::ConformanceUnavailable)?;
        self.current_channel()?
            .send(
                bytes,
                Box::new(move |result| complete(result.map_err(map_transport))),
            )
            .map_err(map_transport)
    }

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
        report_bytes: Option<Vec<u8>>,
        complete: SupervisorCompletion<ReceiptAcknowledgement>,
    ) -> Result<(), SupervisorError> {
        let mut packets = Vec::new();
        if let Some(report) = report_bytes {
            use crate::launch_protocol::{
                CONFORMANCE_REPORT_CHUNK_BYTES, CONFORMANCE_REPORT_CHUNK_SCHEMA,
                ConformanceReportChunk,
            };
            if report.is_empty() || report.len() > crate::conformance::MAX_REPORT_BYTES {
                return Err(SupervisorError::ReceiptInvalid);
            }
            let digest = Digest::of(&receipt_bytes).to_string();
            for (index, bytes) in report.chunks(CONFORMANCE_REPORT_CHUNK_BYTES).enumerate() {
                packets.push(
                    ConformanceReportChunk {
                        schema: CONFORMANCE_REPORT_CHUNK_SCHEMA.into(),
                        receipt_digest: digest.clone(),
                        offset: index * CONFORMANCE_REPORT_CHUNK_BYTES,
                        total_bytes: report.len(),
                        bytes: bytes.to_vec(),
                    }
                    .canonical_bytes()
                    .map_err(|_| SupervisorError::ReceiptInvalid)?,
                );
            }
        }
        packets.insert(0, receipt_bytes);
        Self::transact_packets_on(
            &self.current_channel()?,
            packets.into_iter(),
            |packet| match packet.packet {
                LauncherPacket::Request(ProtocolMessage::ReceiptAcknowledgement(
                    acknowledgement,
                )) => Ok(acknowledgement),
                _ => Err(SupervisorError::DurabilityUnavailable),
            },
            complete,
        )
    }

    #[expect(
        clippy::expect_used,
        reason = "Each connector or generation presence check and subsequent access share one uninterrupted mutex guard."
    )]
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
            let broker_pin = state.broker_pin.clone();
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
            let queued = connector.connect_via_manager(
                &socket_path,
                broker_pin,
                CredentialPin::Identity { uid: 0, gid: 0 },
                Box::new(move |connected| {
                    let Ok(candidate) = connected else {
                        Self::finish_reconnect(
                            &callback_state,
                            generation,
                            Err(SupervisorError::BrokerUnavailable),
                        );
                        return;
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
                        &candidate,
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
            &self.current_channel()?,
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

    fn send_command(
        &self,
        message: crate::launch_protocol::CommandMessage,
        complete: SupervisorCompletion<()>,
    ) -> Result<(), SupervisorError> {
        message
            .validate()
            .map_err(|_| SupervisorError::BrokerUnavailable)?;
        self.current_channel()?
            .send(
                message.canonical_bytes(),
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
    ///
    /// # Errors
    /// Returns `SigningUnavailable` if installed authority validation/opening fails or exceeds its deadline.
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
    fn complete_session(
        &self,
        terminal: crate::launch_receipt::ReceiptPayload,
        complete: SupervisorCompletion<()>,
    ) -> Result<(), SupervisorError> {
        let signer = Arc::clone(&self.signer);
        let deadline = Instant::now()
            .checked_add(self.timeout)
            .unwrap_or_else(Instant::now);
        thread::Builder::new()
            .name("louiselm-key-completion".into())
            .spawn(move || {
                complete(
                    signer
                        .complete_session(&terminal, deadline)
                        .map_err(|_| SupervisorError::KeyCleanupUnavailable),
                );
            })
            .map(drop)
            .map_err(|_| SupervisorError::KeyCleanupUnavailable)
    }

    fn check_authority(&self, complete: SupervisorCompletion<()>) -> Result<(), SupervisorError> {
        let signer = Arc::clone(&self.signer);
        let key_id = self.signing_key_id.clone();
        thread::Builder::new()
            .name("louiselm-key-authority".into())
            .spawn(move || {
                complete(
                    signer
                        .require_key_authority(&key_id)
                        .map_err(|_| SupervisorError::SigningUnavailable),
                );
            })
            .map(drop)
            .map_err(|_| SupervisorError::SigningUnavailable)
    }

    fn record_containment(
        &self,
        session_id: String,
        containment: crate::launcher_install::KeyContainment,
        complete: SupervisorCompletion<()>,
    ) -> Result<(), SupervisorError> {
        let signer = Arc::clone(&self.signer);
        let key_id = self.signing_key_id.clone();
        thread::Builder::new()
            .name("louiselm-key-containment".into())
            .spawn(move || {
                complete(
                    signer
                        .record_containment(&key_id, &session_id, containment)
                        .map_err(|_| SupervisorError::DurabilityUnavailable),
                );
            })
            .map(drop)
            .map_err(|_| SupervisorError::DurabilityUnavailable)
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
    registry_root: PathBuf,
    paths: LauncherPaths,
    config: LauncherConfig,
    backend: BubblewrapBackend,
    timeout: Duration,
}

impl SystemLaunchPlatform {
    fn prepare_tracker(
        &self,
        request: &LaunchRequest,
        plan: &mut ConfinementPlan,
    ) -> Result<(), SupervisorError> {
        super::beads_replica::prepare(
            &self
                .config
                .broker_socket_path
                .parent()
                .ok_or(SupervisorError::ResolutionFailed)?
                .join("beads-inputs"),
            (self.config.broker_uid, self.config.broker_gid),
            request,
            plan,
        )
    }
    fn prepare_workspace(
        &self,
        request: &LaunchRequest,
        plan: &ConfinementPlan,
    ) -> Result<super::workspace::SessionWorkspace, SupervisorError> {
        let registry = crate::registry::Registry::open_trusted(&self.registry_root)
            .map_err(|_| SupervisorError::ResolutionFailed)?;
        let inputs = super::workspace::load(
            &self
                .config
                .broker_socket_path
                .parent()
                .ok_or(SupervisorError::ResolutionFailed)?
                .join("workspace-inputs"),
            (self.config.broker_uid, self.config.broker_gid),
            request,
            &registry,
        )?;
        if plan.arguments != inputs.manifest.agent.arguments
            || plan.environment != inputs.manifest.agent.environment
        {
            return Err(SupervisorError::ResolutionFailed);
        }
        super::workspace::SessionWorkspace::prepare(&inputs, plan, request)
    }

    /// Creates the production platform after materializing only fixed roots.
    ///
    /// # Errors
    /// Returns root-directory/cgroup setup errors or `SpawnFailed` when the pinned Bubblewrap binary cannot be measured or queried.
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
            &paths
                .release_prefix
                .join("releases")
                .join(&config.release_id)
                .join("bin/louiselm-launch"),
            Path::new(SYSTEM_CGROUP_ROOT),
            backend_version,
        );
        Ok(Self {
            paths,
            registry_root: PathBuf::from(SYSTEM_REGISTRY_ROOT),
            config,
            backend,
            timeout,
        })
    }
}

impl LaunchPlatform for SystemLaunchPlatform {
    fn revalidate_conformance(
        &self,
        authorization: &LaunchAuthorization,
        now_ms: u64,
        deadline: Instant,
        complete: SupervisorCompletion<crate::launch_receipt::ConformanceEvidence>,
    ) -> Result<(), SupervisorError> {
        complete(
            super::conformance::inspect_current(
                &self.paths,
                &self.config,
                authorization,
                now_ms,
                deadline,
                false,
            )
            .map(|inspection| inspection.evidence),
        );
        Ok(())
    }

    fn inspect_conformance(
        &self,
        authorization: &LaunchAuthorization,
        now_ms: u64,
        complete: SupervisorCompletion<super::ConformanceAdmission>,
    ) -> Result<(), SupervisorError> {
        // The dedicated coordinator owns this bounded I/O. No detached worker
        // can outlive cancellation or the prepared process tree's cleanup.
        complete(super::conformance::inspect(
            &self.paths,
            &self.config,
            authorization,
            now_ms,
            Instant::now() + self.timeout,
        ));
        Ok(())
    }

    fn check_integration(
        &self,
        request: &LaunchRequest,
        complete: SupervisorCompletion<()>,
    ) -> Result<(), SupervisorError> {
        let result = (|| {
            let registry = crate::registry::Registry::open_trusted(&self.registry_root)
                .map_err(|_| SupervisorError::ToolIsolationUnproven)?;
            let agent = registry
                .agent(&request.agent_id)
                .map_err(|_| SupervisorError::ToolIsolationUnproven)?;
            super::tool_integration::validate_registration(&agent)
        })();
        complete(result);
        Ok(())
    }

    fn verify_tool_isolation(
        &self,
        request: &LaunchRequest,
        agent: &AgentAuthentication,
        complete: SupervisorCompletion<Digest>,
    ) -> Result<(), SupervisorError> {
        let result = (|| {
            let evidence = agent
                .tool_isolation
                .as_ref()
                .ok_or(SupervisorError::ToolIsolationUnproven)?;
            let registry = crate::registry::Registry::open_trusted(&self.registry_root)
                .map_err(|_| SupervisorError::ToolIsolationUnproven)?;
            evidence.verify(
                request,
                agent,
                &registry,
                &self.config.release_id,
                &self.config.bwrap_digest,
            )?;
            Ok(Digest::of(&evidence.canonical_bytes()))
        })();
        complete(result);
        Ok(())
    }

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
        Ok(Box::new(SystemIdentityGuard { lease }))
    }

    fn create_capability(
        &self,
        request: &LaunchRequest,
        assigned: Identity,
    ) -> Result<Box<dyn CapabilityGate>, SupervisorError> {
        SystemCapabilityGate::create(request, assigned)
            .map(|gate| Box::new(gate) as Box<dyn CapabilityGate>)
    }

    fn prepare(
        &self,
        request: &LaunchRequest,
        mut plan: ConfinementPlan,
    ) -> Result<Box<dyn PreparedAgent>, SupervisorError> {
        request
            .validate()
            .map_err(|_| SupervisorError::LaunchDocumentRejected)?;
        if request.session_id != plan.session_id || plan.home.parent() != plan.workspace.parent() {
            return Err(SupervisorError::ResolutionFailed);
        }
        let workspace = self.prepare_workspace(request, &plan)?;
        let result = (|| {
            require_measured_bwrap(&self.config).map_err(|_| SupervisorError::SpawnFailed)?;
            let release_root = self
                .paths
                .release_prefix
                .join("releases")
                .join(&self.config.release_id);
            let tool_isolation = super::ToolIsolationEvidence::measure(
                &plan,
                &release_root,
                &self.config.release_id,
                &self.config.bwrap_digest,
            )?;
            // Registration is validated first; this derived private path is a
            // launcher-owned environment addition, shared with confined tools.
            plan.environment.insert(
                "XDG_CACHE_HOME".into(),
                workspace.cache_path().display().to_string(),
            );
            plan.cache = Some(workspace.cache_path().to_owned());
            self.prepare_tracker(request, &mut plan)?;
            let mut tools = super::tool_execution::ToolExecutor::new(
                self.backend
                    .within_session(&plan.session_id)
                    .map_err(map_sandbox)?,
                &plan,
            )?;
            tools.measured_helper = match super::tool_helper::MeasuredHelper::measure(
                &release_root,
                &self.config.release_id,
            ) {
                Ok(helper) => Some(helper),
                Err(SupervisorError::ToolIsolationUnproven) => None,
                Err(error) => return Err(error),
            };
            let mut prepared = self.backend.prepare(&plan).map_err(map_sandbox)?;
            let directory = plan
                .home
                .parent()
                .ok_or(SupervisorError::ResolutionFailed)?;
            let recovery = if let Ok(storage) =
                super::recovery::SessionStorage::open(directory, request, &tool_isolation)
            {
                Arc::new(storage)
            } else {
                prepared
                    .dispose()
                    .map_err(|_| SupervisorError::CleanupUnproven)?;
                return Err(SupervisorError::DurabilityUnavailable);
            };
            let (verification_backend, verification_plan) = tools.verification_context();
            let verification = match super::verification::Storage::new(
                directory,
                request,
                crate::Digest::of(&tool_isolation.canonical_bytes()).to_string(),
                self.config
                    .broker_socket_path
                    .parent()
                    .ok_or(SupervisorError::ResolutionFailed)?
                    .join("verification-inputs"),
                (self.config.broker_uid, self.config.broker_gid),
                verification_backend,
                verification_plan,
            ) {
                Ok(storage) => Arc::new(storage),
                Err(error) => {
                    prepared
                        .dispose()
                        .map_err(|_| SupervisorError::CleanupUnproven)?;
                    return Err(error);
                }
            };
            Ok(Box::new(SystemPreparedAgent {
                prepared,
                backend_id: self.config.bwrap_digest.clone(),
                tool_isolation: Some(tool_isolation),
                tools: Some(tools),
                recovery,
                verification,
                workspace: None,
            }))
        })();
        match result {
            Ok(mut prepared) => {
                prepared.workspace = Some(workspace);
                Ok(prepared)
            }
            Err(error) => {
                workspace.seal()?;
                Err(error)
            }
        }
    }
}

struct SystemIdentityGuard {
    lease: IdentityLease,
}

impl IdentityGuard for SystemIdentityGuard {
    fn identity(&self) -> Identity {
        self.lease.identity()
    }

    fn release(self: Box<Self>) -> Result<(), SupervisorError> {
        self.lease
            .release()
            .map_err(|_| SupervisorError::CleanupUnproven)
    }

    fn poison(self: Box<Self>) -> Result<(), SupervisorError> {
        self.lease
            .poison()
            .map_err(|_| SupervisorError::CleanupUnproven)
    }
}

struct SystemPreparedAgent {
    prepared: PreparedSession,
    backend_id: String,
    tool_isolation: Option<super::ToolIsolationEvidence>,
    tools: Option<super::tool_execution::ToolExecutor>,
    recovery: Arc<super::recovery::SessionStorage>,
    verification: Arc<super::verification::Storage>,
    workspace: Option<super::workspace::SessionWorkspace>,
}

impl PreparedAgent for SystemPreparedAgent {
    fn evidence(&self) -> &crate::isolation::IsolationEvidence {
        self.prepared.evidence()
    }

    fn backend_id(&self) -> &str {
        &self.backend_id
    }

    fn sandbox_leader_pid(&self) -> Option<u32> {
        self.prepared.sandbox_leader_pid()
    }

    fn processes(&self) -> Result<Vec<u32>, SupervisorError> {
        self.prepared.processes().map_err(map_sandbox)
    }

    fn process_membership(&self) -> Arc<dyn ProcessMembership> {
        Arc::new(SystemProcessMembership {
            tree: self.prepared.process_tree(),
        })
    }

    fn start(self: Box<Self>) -> Result<Box<dyn RunningAgent>, SupervisorError> {
        let session = match self.prepared.start() {
            Ok(session) => session,
            Err(error) => {
                if let Some(workspace) = &self.workspace {
                    workspace.seal()?;
                }
                return Err(map_sandbox(error));
            }
        };
        let mut running = SystemRunningAgent::new(session);
        running.tools = self.tools;
        running.tool_isolation = self.tool_isolation;
        running.recovery = Some(self.recovery);
        running.verification_storage = Some(self.verification);
        running.workspace = self.workspace;
        Ok(Box::new(running))
    }

    fn dispose(&mut self) -> Result<(), SupervisorError> {
        self.prepared.dispose().map_err(map_sandbox)?;
        self.workspace
            .as_mut()
            .map_or(Ok(()), super::workspace::SessionWorkspace::disposed)
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

/// Production lifecycle/relay adapter for an already-confined running Session.
pub struct SystemRunningAgent {
    sender_guard: Option<super::sender_guard::SenderGuard>,
    cache_worker: Option<super::cache_download::Worker>,
    workspace: Option<super::workspace::SessionWorkspace>,
    tools: Option<super::tool_execution::ToolExecutor>,
    tool_isolation: Option<super::ToolIsolationEvidence>,
    session: Arc<Mutex<SandboxedSession>>,
    relay: Option<RelayWorker>,
    recovery: Option<Arc<super::recovery::SessionStorage>>,
    recovery_worker: Option<super::recovery_worker::Worker>,
    verification_storage: Option<Arc<super::verification::Storage>>,
    verification_worker: Option<super::verification::Worker>,
}

impl SystemRunningAgent {
    /// Takes exclusive lifecycle and stdio ownership of an already-started sandbox.
    ///
    /// This does not authorize a launch or establish Verified posture. The caller
    /// must obtain the Session through the receipt-gated launch workflow.
    #[must_use]
    pub fn new(session: SandboxedSession) -> Self {
        Self {
            sender_guard: None,
            cache_worker: None,
            workspace: None,
            session: Arc::new(Mutex::new(session)),
            relay: None,
            tools: None,
            tool_isolation: None,
            recovery: None,
            recovery_worker: None,
            verification_storage: None,
            verification_worker: None,
        }
    }

    /// Enrolls the measured runtime before releasing its exec stop and retains
    /// enforcement through process-tree disposal. Does not activate Brokered.
    /// Run only on the privileged launch worker. The authenticated handoff owner
    /// must close endpoint copies/leases before completing Session disposal.
    /// # Errors
    /// Returns startup or cleanup uncertainty; the outer identity owner must
    /// poison the lease on `CleanupUnproven`, as with ordinary sandbox disposal.
    pub fn start_guarded(
        prepared: PreparedSession,
        mut guard: super::sender_guard::SenderGuard,
    ) -> Result<Self, SupervisorError> {
        let session = guard.start(prepared).map_err(map_sandbox)?;
        let mut running = Self::new(session);
        running.sender_guard = Some(guard);
        Ok(running)
    }

    /// Guard mechanics for the authenticated handoff/request worker. The caller
    /// still owns broker admission; possession does not activate an endpoint.
    pub fn sender_guard(&mut self) -> Option<&mut super::sender_guard::SenderGuard> {
        self.sender_guard.as_mut()
    }

    fn join_recovery(&mut self) -> Result<(), SupervisorError> {
        self.recovery_worker
            .take()
            .map_or(Ok(()), super::recovery_worker::Worker::join)
    }

    fn cancel_verification(&mut self) -> Result<(), SupervisorError> {
        self.verification_worker
            .as_mut()
            .map_or(Ok(()), super::verification::Worker::cancel)
    }
}

pub(super) fn classify_exit(code: i32) -> ProcessExitClassification {
    match code {
        0 => ProcessExitClassification::Success,
        value if value < 0 => ProcessExitClassification::Signaled,
        _ => ProcessExitClassification::Failure,
    }
}

fn classify_mechanic_failure(
    target: Option<SandboxMechanicalState>,
    observed: &Result<SandboxMechanicalState, SandboxError>,
) -> Result<(), MechanicFailure> {
    match observed {
        Ok(state) if Some(*state) == target => Ok(()),
        Ok(SandboxMechanicalState::Running) => Err(MechanicFailure::Running),
        Ok(SandboxMechanicalState::Parked) => Err(MechanicFailure::Parked),
        Ok(SandboxMechanicalState::Exited(code)) => {
            Err(MechanicFailure::Terminal(classify_exit(*code)))
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
        Err(_) => classify_mechanic_failure(target, &session.mechanical_state()),
    }
}

impl RunningAgent for SystemRunningAgent {
    fn store_dependency(
        &mut self,
        digest: Digest,
        bytes: Vec<u8>,
        enforcer: Arc<super::command::CommandEnforcer>,
        expires_at_ms: u64,
        complete: SupervisorCompletion<String>,
    ) -> Result<(), SupervisorError> {
        self.cache_worker
            .as_mut()
            .map_or(Ok(()), super::cache_download::Worker::join)?;
        let writer = self
            .workspace
            .as_ref()
            .ok_or(SupervisorError::CapabilityUnavailable)?
            .cache_writer()?;
        self.cache_worker = Some(super::cache_download::spawn(
            writer,
            digest,
            bytes,
            enforcer,
            expires_at_ms,
            complete,
        )?);
        Ok(())
    }
    fn restore_recovery(
        &mut self,
        request: crate::launch_protocol::RecoveryRestoreRequest,
        complete: super::recovery::RestoreCompletion,
    ) -> Result<(), SupervisorError> {
        use super::recovery::RecoveryError;
        if self
            .recovery_worker
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
        {
            complete(Err(RecoveryError::NotParked));
            return Ok(());
        }
        self.join_recovery()?;
        let Some(storage) = self.recovery.clone() else {
            complete(Err(RecoveryError::Unsupported));
            return Ok(());
        };
        if !matches!(
            lock(&self.session).mechanical_state(),
            Ok(SandboxMechanicalState::Parked)
        ) {
            complete(Err(RecoveryError::NotParked));
            return Ok(());
        }
        self.cancel_tool()?;
        self.cancel_helper()?;
        let session = self.session.clone();
        self.recovery_worker = Some(super::recovery_worker::Worker::spawn(
            "louiselm-restore-recovery",
            move || {
                let mut session = lock(&session);
                if !matches!(
                    session.mechanical_state(),
                    Ok(SandboxMechanicalState::Parked)
                ) {
                    return Err(RecoveryError::NotParked);
                }
                storage.restore(&request, recovery_now_ms()?)?;
                Ok(request)
            },
            complete,
        )?);
        Ok(())
    }
    fn verification(
        &mut self,
        request: crate::launch_protocol::VerificationRequest,
        complete: SupervisorCompletion<ResponseResult>,
    ) -> Result<(), SupervisorError> {
        if self
            .verification_worker
            .as_ref()
            .is_some_and(|worker| !worker.finished())
            || self
                .recovery_worker
                .as_ref()
                .is_some_and(|worker| !worker.is_finished())
        {
            return Err(SupervisorError::WorkerUnavailable);
        }
        self.cancel_verification()?;
        self.join_recovery()?;
        let storage = self
            .verification_storage
            .clone()
            .ok_or(SupervisorError::ToolIsolationUnproven)?;
        self.verification_worker = Some(super::verification::Worker::spawn(
            storage,
            request,
            Arc::clone(&self.session),
            complete,
        )?);
        Ok(())
    }
    fn retain_recovery(
        &mut self,
        request: super::recovery::RetentionRequest,
        complete: super::recovery::RecoveryCompletion,
    ) -> Result<(), SupervisorError> {
        use super::recovery::RecoveryError;
        if self
            .recovery_worker
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
        {
            complete(Err(RecoveryError::NotParked));
            return Ok(());
        }
        self.join_recovery()?;
        let Some(storage) = self.recovery.clone() else {
            complete(Err(RecoveryError::Unsupported));
            return Ok(());
        };
        if !matches!(
            lock(&self.session).mechanical_state(),
            Ok(SandboxMechanicalState::Parked)
        ) {
            complete(Err(RecoveryError::NotParked));
            return Ok(());
        }
        self.cancel_tool()?;
        self.cancel_helper()?;
        let session = self.session.clone();
        self.recovery_worker = Some(super::recovery_worker::Worker::spawn(
            "louiselm-retain-recovery",
            move || {
                let mut session = lock(&session);
                if !matches!(
                    session.mechanical_state(),
                    Ok(SandboxMechanicalState::Parked)
                ) {
                    return Err(RecoveryError::NotParked);
                }
                let now = recovery_now_ms()?;
                let evidence = storage.retain(&request, now)?;
                if recovery_now_ms()? >= request.expires_at_ms {
                    return Err(RecoveryError::Expired);
                }
                Ok(evidence)
            },
            complete,
        )?);
        Ok(())
    }

    fn launch_helper(
        &mut self,
        request: crate::launch_protocol::CommandMessage,
        enforcer: Arc<super::command::CommandEnforcer>,
        complete: SupervisorCompletion<super::HelperPrincipal>,
    ) -> Result<(), SupervisorError> {
        self.join_recovery()?;
        self.tools
            .as_mut()
            .ok_or(SupervisorError::ToolIsolationUnproven)?
            .launch_helper(request, enforcer, complete)
    }

    fn cancel_helper(&mut self) -> Result<(), SupervisorError> {
        self.tools
            .as_mut()
            .map_or(Ok(()), super::tool_execution::ToolExecutor::cancel_helper)
    }
    fn execute_tool(
        &mut self,
        permit: super::command::CommandPermit,
        complete: SupervisorCompletion<crate::launch_protocol::ToolExecutionResult>,
    ) -> Result<(), SupervisorError> {
        self.join_recovery()?;
        self.tools
            .as_mut()
            .ok_or(SupervisorError::ToolIsolationUnproven)?
            .execute(permit, complete)
    }

    fn cancel_tool(&mut self) -> Result<(), SupervisorError> {
        self.cancel_verification()?;
        self.tools
            .as_mut()
            .map_or(Ok(()), super::tool_execution::ToolExecutor::cancel)
    }
    fn authentication(&self) -> Result<AgentAuthentication, SupervisorError> {
        let process = lock(&self.session)
            .agent_identity()
            .ok_or(SupervisorError::AgentIdentityRejected)?;
        Ok(AgentAuthentication {
            credentials: process.credentials(),
            process: Some(process),
            tool_isolation: self.tool_isolation.clone(),
        })
    }

    fn start_relay(
        &mut self,
        controller: mpsc::Receiver<RelayStdio>,
        events: RunningAgentEvents,
    ) -> Result<(), SupervisorError> {
        if self.relay.is_some() {
            return Err(SupervisorError::RelayFailed);
        }
        let (input, output, error) = {
            let mut session = lock(&self.session);
            (
                session.take_stdin().ok_or(SupervisorError::RelayFailed)?,
                session.take_stdout().ok_or(SupervisorError::RelayFailed)?,
                session.take_stderr().ok_or(SupervisorError::RelayFailed)?,
            )
        };
        let session = Arc::clone(&self.session);
        let mut identity_lost_at = None;
        self.relay = Some(RelayWorker::start(
            controller,
            input,
            output,
            error,
            move || {
                let mut session = lock(&session);
                let exited = session.try_wait().map_err(map_sandbox)?;
                if exited.is_some() {
                    return Ok(exited);
                }
                if let Some(process) = session.agent_identity()
                    && !process
                        .valid()
                        .map_err(|_| SupervisorError::AgentIdentityRejected)?
                {
                    // The lifetime pin already denies channel traffic. Allow
                    // the unchanged Bubblewrap reaper a bounded opportunity to
                    // report the actual exit status; no tool EOF is required.
                    let lost_at = identity_lost_at.get_or_insert_with(Instant::now);
                    if lost_at.elapsed() >= Duration::from_millis(100) {
                        return Err(SupervisorError::AgentIdentityRejected);
                    }
                }
                Ok(exited)
            },
            events,
        )?);
        Ok(())
    }

    fn quiesce_relay(&mut self, complete: SupervisorCompletion<()>) -> Result<(), SupervisorError> {
        complete(self.relay.as_mut().map_or(Ok(()), RelayWorker::stop));
        Ok(())
    }

    fn park(&mut self) -> Result<(), MechanicFailure> {
        let guard = self
            .sender_guard
            .as_mut()
            .map_or(Ok(()), super::sender_guard::SenderGuard::revoke);
        let cache = self
            .cache_worker
            .as_mut()
            .map_or(Ok(()), super::cache_download::Worker::join)
            .map_err(|_| MechanicFailure::Ambiguous);
        let verification = self
            .cancel_verification()
            .map_err(|_| MechanicFailure::Ambiguous);
        let parked = apply_mechanic(
            &self.session,
            Some(SandboxMechanicalState::Parked),
            SandboxedSession::park,
        );
        if guard.is_err() || cache.is_err() || verification.is_err() {
            Err(MechanicFailure::Ambiguous)
        } else {
            parked
        }
    }

    fn resume(&mut self) -> Result<(), MechanicFailure> {
        self.cancel_verification()
            .map_err(|_| MechanicFailure::Ambiguous)?;
        self.join_recovery()
            .map_err(|_| MechanicFailure::Ambiguous)?;
        apply_mechanic(
            &self.session,
            Some(SandboxMechanicalState::Running),
            SandboxedSession::resume,
        )
    }

    fn interrupt(&mut self) -> Result<(), MechanicFailure> {
        self.cancel_verification()
            .map_err(|_| MechanicFailure::Ambiguous)?;
        self.join_recovery()
            .map_err(|_| MechanicFailure::Ambiguous)?;
        self.tools
            .as_mut()
            .map_or(Ok(()), super::tool_execution::ToolExecutor::cancel)
            .map_err(|_| MechanicFailure::Ambiguous)?;
        apply_mechanic(&self.session, None, SandboxedSession::interrupt)
    }

    fn dispose(&mut self) -> Result<(), SupervisorError> {
        let guard = self
            .sender_guard
            .as_mut()
            .map_or(Ok(()), super::sender_guard::SenderGuard::revoke);
        let cache = self
            .cache_worker
            .as_mut()
            .map_or(Ok(()), super::cache_download::Worker::join);
        let recovery = self.join_recovery();
        let verification = self.cancel_verification();
        let relay = self.relay.as_mut().map_or(Ok(()), RelayWorker::stop);
        let tools = self
            .tools
            .as_mut()
            .map_or(Ok(()), super::tool_execution::ToolExecutor::dispose);
        let process = lock(&self.session)
            .dispose()
            .map(|_| ())
            .map_err(map_sandbox);
        let guard_closed = if process.is_ok() && tools.is_ok() && verification.is_ok() {
            self.sender_guard
                .as_mut()
                .map_or(Ok(()), super::sender_guard::SenderGuard::dispose)
        } else {
            Err(super::sender_guard::GuardError::Cleanup)
        };
        let sealed = if process.is_ok() && tools.is_ok() && verification.is_ok() {
            self.recovery
                .as_ref()
                .map_or(Ok(()), |storage| storage.seal())
        } else {
            Err(super::recovery::RecoveryError::NotParked)
        };
        if guard.is_err()
            || guard_closed.is_err()
            || cache.is_err()
            || verification.is_err()
            || recovery.is_err()
            || relay.is_err()
            || tools.is_err()
            || process.is_err()
            || sealed.is_err()
        {
            Err(SupervisorError::CleanupUnproven)
        } else {
            self.workspace
                .as_mut()
                .map_or(Ok(()), super::workspace::SessionWorkspace::disposed)
        }
    }
}

fn recovery_now_ms() -> Result<u64, super::recovery::RecoveryError> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| elapsed.as_millis().try_into().ok())
        .ok_or(super::recovery::RecoveryError::Invalid)
}

impl Drop for SystemRunningAgent {
    fn drop(&mut self) {
        let _ = self.cancel_verification();
        // No retention worker may outlive this owner. A panic cannot release a
        // host identity: the outer lifecycle owner requires successful dispose.
        let _ = self.join_recovery();
    }
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "Result::map_err transfers ownership of the error into this conversion."
)]
pub(super) fn map_sandbox(error: SandboxError) -> SupervisorError {
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
    process: Option<Arc<KernelProcess>>,
    commands: Option<Arc<super::command::CommandEnforcer>>,
    expected_session_id: String,
    expected_run_id: String,
    expected_envelope_revision: u64,
    expected_identity: Identity,
    accepted: Arc<Mutex<AcceptedCapability>>,
}

#[derive(Default)]
struct AcceptedCapability {
    closed: bool,
    generation: u64,
    channel: Option<SeqpacketChannel>,
    receiving: bool,
    pending: Option<SupervisorCompletion<ProtocolMessage>>,
}

fn accept_capability(
    result: Result<SeqpacketChannel, TransportError>,
    accepted: &Arc<Mutex<AcceptedCapability>>,
    process: &Arc<KernelProcess>,
    generation: u64,
) {
    let Ok(channel) = result else {
        return;
    };
    if channel.peer_credentials() != process.credentials() || process.valid().ok() != Some(true) {
        channel.close();
        return;
    }
    let mut state = lock(accepted);
    if state.closed || state.generation != generation {
        channel.close();
    } else {
        state.channel = Some(channel.clone());
        let pending = state.pending.take();
        drop(state);
        if let Some(complete) = pending {
            command_io::receive(
                &channel,
                Arc::clone(process),
                accepted,
                generation,
                complete,
            );
        }
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
        let Ok(metadata) = configured else {
            drop(bound);
            let _ = fs::remove_file(&path);
            return Err(SupervisorError::CapabilityUnavailable);
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
            process: None,
            commands: None,
            expected_session_id: request.session_id.clone(),
            expected_run_id: request.run_id.clone(),
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
        authentication: AgentAuthentication,
    ) -> Result<(), SupervisorError> {
        let process = authentication
            .process
            .ok_or(SupervisorError::AgentIdentityRejected)?;
        if binding.channel_id != self.channel.id()
            || binding.session_id != self.expected_session_id
            || binding.run_id != self.expected_run_id
            || binding.envelope_revision != self.expected_envelope_revision
            || binding.identity_slot != self.expected_identity.slot
            || binding.assigned_uid != self.expected_identity.uid
            || binding.assigned_gid != self.expected_identity.gid
            || self.binding.is_some()
            || self.process.is_some()
            || !matches!(self.state, Some(ListenerState::Bound(_)))
            || authentication.credentials != process.credentials()
            || binding.agent_pid != process.credentials().pid
            || binding.assigned_uid != process.credentials().uid
            || binding.assigned_gid != process.credentials().gid
            || !process
                .valid()
                .map_err(|_| SupervisorError::AgentIdentityRejected)?
        {
            return Err(SupervisorError::CapabilityUnavailable);
        }
        self.commands = Some(Arc::new(super::command::CommandEnforcer::new(
            binding.clone(),
            Arc::clone(&process),
        )?));
        self.binding = Some(binding);
        self.process = Some(process);
        Ok(())
    }

    fn enable(&mut self) -> Result<(), SupervisorError> {
        let process = Arc::clone(
            self.process
                .as_ref()
                .ok_or(SupervisorError::AgentIdentityRejected)?,
        );
        if !process
            .valid()
            .map_err(|_| SupervisorError::AgentIdentityRejected)?
        {
            self.close();
            return Err(SupervisorError::AgentIdentityRejected);
        }
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
        if listener
            .accept(
                CredentialPin::LiveProcess(Arc::clone(&process)),
                Box::new(move |result| {
                    accept_capability(result, &accepted, &process, generation);
                }),
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

    fn enable_after_resume(&mut self) -> Result<(), SupervisorError> {
        let commands = Arc::new(
            self.commands
                .as_ref()
                .ok_or(SupervisorError::CapabilityUnavailable)?
                .resume_agent()?,
        );
        self.enable()?;
        self.commands = Some(commands);
        Ok(())
    }

    fn revoke(&mut self) -> Result<(), SupervisorError> {
        let commands = self
            .commands
            .as_ref()
            .map_or(Ok(()), |commands| commands.revoke());
        let channel = {
            let mut accepted = lock(&self.accepted);
            accepted.closed = true;
            accepted.receiving = false;
            (accepted.channel.take(), accepted.pending.take())
        };
        if let Some(complete) = channel.1 {
            complete(Err(SupervisorError::CapabilityUnavailable));
        }
        if let Some(channel) = channel.0 {
            channel.close();
        }
        let result = match self.state.take() {
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
        };
        commands.and(result)
    }

    fn receive_command(
        &mut self,
        complete: SupervisorCompletion<ProtocolMessage>,
    ) -> Result<(), SupervisorError> {
        self.receive_agent_command(complete)
    }

    fn authorize_command(
        &self,
        request: &crate::launch_protocol::ToolExecutionRequest,
        decision: &crate::launch_protocol::CommandMessage,
        forwarded_at: Instant,
    ) -> Result<super::command::CommandPermit, SupervisorError> {
        let binding = self
            .binding
            .as_ref()
            .ok_or(SupervisorError::CapabilityUnavailable)?;
        self.commands
            .as_ref()
            .ok_or(SupervisorError::CapabilityUnavailable)?
            .admit(
                request,
                &crate::launch_protocol::CommandPrincipal {
                    channel_id: binding.channel_id.clone(),
                    pid: binding.agent_pid,
                    uid: binding.assigned_uid,
                    gid: binding.assigned_gid,
                },
                decision,
                forwarded_at,
            )
    }

    fn command_enforcer(&self) -> Result<Arc<super::command::CommandEnforcer>, SupervisorError> {
        self.commands
            .clone()
            .ok_or(SupervisorError::CapabilityUnavailable)
    }

    fn send_command(
        &self,
        message: crate::launch_protocol::CommandMessage,
        complete: SupervisorCompletion<()>,
    ) -> Result<(), SupervisorError> {
        message
            .validate()
            .map_err(|_| SupervisorError::CapabilityUnavailable)?;
        let accepted = lock(&self.accepted);
        if accepted.closed {
            return Err(SupervisorError::CapabilityUnavailable);
        }
        let channel = accepted
            .channel
            .clone()
            .ok_or(SupervisorError::CapabilityUnavailable)?;
        drop(accepted);
        channel
            .send(
                message.canonical_bytes(),
                Box::new(move |result| {
                    complete(result.map_err(|_| SupervisorError::CapabilityUnavailable));
                }),
            )
            .map_err(|_| SupervisorError::CapabilityUnavailable)
    }

    fn close(&mut self) {
        // Revocation error is handled by explicit lifecycle cleanup. Close never
        // releases a lease; a poisoned enforcement mutex is independently closed.
        let _ = self.commands.as_ref().map(|commands| commands.revoke());
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
            accepted.receiving = false;
            (accepted.channel.take(), accepted.pending.take())
        };
        if let Some(complete) = channel.1 {
            complete(Err(SupervisorError::CapabilityUnavailable));
        }
        if let Some(channel) = channel.0 {
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
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Test fixtures abort on setup failure and assert failures directly."
)]
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
                &Ok(SandboxMechanicalState::Parked),
            ),
            Ok(()),
        );
        assert_eq!(
            classify_mechanic_failure(
                Some(SandboxMechanicalState::Running),
                &Ok(SandboxMechanicalState::Running),
            ),
            Ok(()),
        );
    }

    #[test]
    fn mechanic_failure_preserves_known_terminal_and_ambiguous_state() {
        assert_eq!(
            classify_mechanic_failure(
                Some(SandboxMechanicalState::Parked),
                &Ok(SandboxMechanicalState::Running),
            ),
            Err(MechanicFailure::Running),
        );
        assert_eq!(
            classify_mechanic_failure(None, &Ok(SandboxMechanicalState::Parked)),
            Err(MechanicFailure::Parked),
        );
        assert_eq!(
            classify_mechanic_failure(None, &Ok(SandboxMechanicalState::Exited(-1))),
            Err(MechanicFailure::Terminal(
                ProcessExitClassification::Signaled,
            )),
        );
        assert_eq!(
            classify_mechanic_failure(None, &Ok(SandboxMechanicalState::Exited(0))),
            Err(MechanicFailure::Terminal(
                ProcessExitClassification::Success,
            )),
        );
        assert_eq!(
            classify_mechanic_failure(None, &Ok(SandboxMechanicalState::Exited(7))),
            Err(MechanicFailure::Terminal(
                ProcessExitClassification::Failure,
            )),
        );
        assert_eq!(
            classify_mechanic_failure(
                Some(SandboxMechanicalState::Running),
                &Err(SandboxError::NoCgroup("unavailable".to_owned())),
            ),
            Err(MechanicFailure::Ambiguous),
        );
    }

    fn pinned_test_process(pid: u32) -> Arc<KernelProcess> {
        let kernel_pid =
            rustix::process::Pid::from_raw(i32::try_from(pid).expect("fixture PID fits"))
                .expect("fixture PID is positive");
        Arc::new(
            KernelProcess::from_exec_stop(
                crate::launch_transport::KernelCredentials {
                    pid,
                    uid: rustix::process::getuid().as_raw(),
                    gid: rustix::process::getgid().as_raw(),
                },
                rustix::process::pidfd_open(kernel_pid, rustix::process::PidfdFlags::empty())
                    .expect("fixture lifetime pins"),
                &fs::File::open(format!("/proc/{pid}/exe")).expect("fixture executable opens"),
            )
            .expect("fixture identity constructs"),
        )
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
                pin.clone(),
                Box::new(move |result| {
                    server_sender.send(result).expect("server result received");
                }),
            )
            .expect("accept queues");
        let (client_sender, client_receiver) = mpsc::sync_channel(1);
        connector
            .connect(
                &path,
                pin.clone(),
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
        let accepted = accept_channel(&listener, pin.clone());
        let (connected, connection) = mpsc::sync_channel(1);
        connector
            .connect(
                &path,
                pin.clone(),
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
        let broker = SeqpacketLaunchBroker::new(connector, path, pin.clone(), client);
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

    #[expect(
        clippy::needless_pass_by_value,
        reason = "This terminal assertion consumes and releases the channel after proving disconnection."
    )]
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
    #[expect(
        clippy::too_many_lines,
        reason = "One rendezvous is followed from rejected bindings through valid activation, revocation, re-enable and owned-path cleanup."
    )]
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
            process: None,
            commands: None,
            expected_session_id: "session-1".to_owned(),
            expected_run_id: "run-1".to_owned(),
            expected_envelope_revision: 7,
            expected_identity: identity,
            accepted: Arc::new(Mutex::new(AcceptedCapability::default())),
        };
        let binding = CapabilityBinding {
            session_id: "session-1".to_owned(),
            run_id: "run-1".to_owned(),
            channel_id: "agent-capability".to_owned(),
            envelope_revision: 7,
            identity_slot: identity.slot,
            assigned_uid: identity.uid,
            assigned_gid: identity.gid,
            agent_pid: std::process::id(),
        };
        let process = pinned_test_process(std::process::id());
        let authentication = AgentAuthentication {
            credentials: process.credentials(),
            process: Some(process),
            tool_isolation: None,
        };
        assert_eq!(
            gate.bind(
                binding.clone(),
                AgentAuthentication {
                    process: None,
                    ..authentication.clone()
                }
            ),
            Err(SupervisorError::AgentIdentityRejected)
        );
        assert_eq!(
            gate.bind(
                CapabilityBinding {
                    run_id: "another-run".to_owned(),
                    ..binding.clone()
                },
                authentication.clone()
            ),
            Err(SupervisorError::CapabilityUnavailable)
        );
        assert_eq!(
            gate.bind(
                CapabilityBinding {
                    agent_pid: binding.agent_pid + 1,
                    ..binding.clone()
                },
                authentication.clone()
            ),
            Err(SupervisorError::CapabilityUnavailable)
        );
        gate.bind(binding, authentication)
            .expect("capability binding is valid");

        gate.enable().expect("capability enables");
        let first = connect_channel(&path, pin.clone());
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
        let second = connect_channel(&path, pin.clone());
        wait_for_accepted(&gate);
        let revoked_commands = gate
            .command_enforcer()
            .expect("original command generation");
        assert_eq!(
            revoked_commands.agent_valid(),
            Ok(false),
            "transport reconnect cannot restore command authority"
        );
        gate.revoke().expect("transport revokes again");
        assert_disconnected(second);
        gate.enable_after_resume()
            .expect("durable operator Resume enables a fresh generation");
        assert_eq!(revoked_commands.agent_valid(), Ok(false));
        assert_eq!(
            gate.command_enforcer()
                .expect("resumed commands")
                .agent_valid(),
            Ok(true)
        );
        let second = connect_channel(&path, pin.clone());
        wait_for_accepted(&gate);
        gate.close();
        assert_disconnected(second);
        assert!(!path.exists());
    }

    #[test]
    fn capability_accept_rejects_a_different_process_with_the_same_uid() {
        let (server, client) = channel_pair();
        let accepted = Arc::new(Mutex::new(AcceptedCapability::default()));
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("fixture starts");
        let process = pinned_test_process(child.id());
        accept_capability(Ok(server), &accepted, &process, 0);
        child.kill().expect("fixture killed");
        child.wait().expect("fixture reaped");

        assert!(lock(&accepted).channel.is_none());
        assert_disconnected(client);
    }

    #[test]
    fn capability_close_wins_a_pending_accept_callback() {
        let (server, client) = channel_pair();
        let accepted = Arc::new(Mutex::new(AcceptedCapability::default()));
        let (entered_sender, entered_receiver) = mpsc::sync_channel(1);
        let (release_sender, release_receiver) = mpsc::sync_channel(1);
        let process = pinned_test_process(std::process::id());
        let worker_accepted = Arc::clone(&accepted);
        let worker = thread::spawn(move || {
            entered_sender.send(()).expect("accept callback ready");
            release_receiver.recv().expect("accept callback releases");
            accept_capability(Ok(server), &worker_accepted, &process, 0);
        });
        entered_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("accept callback starts");
        lock(&accepted).closed = true;
        release_sender.send(()).expect("accept callback releases");
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
        let process = pinned_test_process(std::process::id());
        lock(&accepted).generation = 1;
        let worker_accepted = Arc::clone(&accepted);
        let worker = thread::spawn(move || {
            entered_sender.send(()).expect("accept callback ready");
            release_receiver.recv().expect("accept callback releases");
            accept_capability(Ok(server), &worker_accepted, &process, 1);
        });
        entered_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("accept callback starts");
        {
            let mut state = lock(&accepted);
            state.closed = true;
            state.generation = 2;
            state.closed = false;
        }
        release_sender.send(()).expect("accept callback releases");
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
        let (candidate, completed) =
            start_reconnect(&broker, &listener, pin.clone(), request.clone());

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
            start_reconnect(&broker, &listener, pin.clone(), request.clone());
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

        let (candidate, completed) =
            start_reconnect(&broker, &listener, pin.clone(), request.clone());
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
            let (candidate, completed) =
                start_reconnect(&broker, &listener, pin.clone(), request.clone());
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
            let (candidate, completed) =
                start_reconnect(&broker, &listener, pin.clone(), request.clone());
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
        let (candidate, completed) =
            start_reconnect(&broker, &listener, pin.clone(), request.clone());
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
