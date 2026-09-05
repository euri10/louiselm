//! Privileged launch and lifecycle orchestration for one receipt-gated Session.
//!
//! The supervisor owns the order at the privilege boundary: consume one
//! broker authorization, reserve its assigned host identity, prepare a
//! measured process tree behind a startup gate, sign and durably acknowledge
//! sequence zero as Starting, and only then let the capability listener and
//! workload run. A linked Running receipt must also become durable before the
//! launch completes.
//! The traits in this module are deliberately the actual I/O seams. Tests can
//! hold their callbacks; production adapters remain asynchronous too.

#![cfg(target_os = "linux")]

use std::{
    io::{self, BufRead, Read, Write},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    Digest,
    isolation::{CONTRACT_VERSION, IsolationEvidence},
    launch::{LaunchRequest, MAX_REQUEST_BYTES, resolve},
    launch_protocol::{
        BrokerReconnect, ControllerLossAcknowledgement, ControllerLossSettlement,
        IdentityExhaustion, LaunchAuthorization, ProtocolMessage, ProtocolResponse,
        ReceiptAcknowledgement, ReceiptDisposition,
    },
    launch_receipt::{
        Authorization, LaunchEvidence, ProcessExitClassification, RECEIPT_SCHEMA, ReceiptAuthority,
        ReceiptCause, ReceiptHead, ReceiptOutcome, ReceiptPayload, SIGNED_RECEIPT_SCHEMA,
        SessionState, SignedReceipt,
    },
    launcher_install::Identity,
    registry::Registry,
    sandbox::{Channel, ConfinementPlan},
};

mod lifecycle;
mod system;

pub use lifecycle::LaunchedSession;
pub use system::{
    InstalledLaunchSigner, SYSTEM_CAPABILITY_GUEST_PATH, SYSTEM_CAPABILITY_ROOT,
    SYSTEM_CGROUP_ROOT, SYSTEM_REGISTRY_ROOT, SYSTEM_SESSIONS_ROOT, SystemLaunchPlatform,
    connect_control_broker,
};

/// One exactly-once asynchronous launch completion.
pub type LaunchCompletion =
    Box<dyn FnOnce(Result<LaunchedSession, SupervisorError>) + Send + 'static>;

/// Completion used by broker and signer ports.
pub type SupervisorCompletion<T> = Box<dyn FnOnce(Result<T, SupervisorError>) + Send + 'static>;

/// Sanitized observations emitted by an Agent relay without lifecycle authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunningAgentEvent {
    /// The controller side of ACP stdin reached EOF.
    ControllerEof,
    /// The supervised process ended with a bounded public classification.
    ProcessExited(ProcessExitClassification),
    /// An opaque relay worker failed without exposing payload or process details.
    RelayFailed,
}

/// Sanitized process-tree observation after a lifecycle mechanic did not complete normally.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MechanicFailure {
    /// The live process tree is settled and able to execute.
    Running,
    /// The live process tree is settled and completely frozen.
    Parked,
    /// The supervised process exited while the mechanic was being applied.
    Terminal(ProcessExitClassification),
    /// No stable live or terminal state could be proved.
    Ambiguous,
}

/// Callback scheduler used for bounded lifecycle operations.
pub trait SupervisorTimer: Send + Sync {
    /// Returns the scheduler's monotonic time source.
    fn now(&self) -> Instant;

    /// Runs `complete` once after the monotonic `delay` has elapsed.
    fn schedule(
        &self,
        delay: Duration,
        complete: Box<dyn FnOnce() + Send + 'static>,
    ) -> Result<(), SupervisorError>;
}

struct ThreadSupervisorTimer;

impl SupervisorTimer for ThreadSupervisorTimer {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn schedule(
        &self,
        delay: Duration,
        complete: Box<dyn FnOnce() + Send + 'static>,
    ) -> Result<(), SupervisorError> {
        thread::Builder::new()
            .name("louiselm-launch-lifecycle-timer".to_owned())
            .spawn(move || {
                thread::sleep(delay);
                complete();
            })
            .map(drop)
            .map_err(|_| SupervisorError::WorkerUnavailable)
    }
}

/// Metadata fixed to the capability listener before it becomes reachable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityBinding {
    /// Session whose confined tree may use the listener.
    pub session_id: String,
    /// Channel identifier recorded in sequence zero.
    pub channel_id: String,
    /// Capability-envelope revision in force for the listener.
    pub envelope_revision: u64,
    /// Installed identity-pool slot held by this supervisor.
    pub identity_slot: u32,
    /// Host UID proven for the prepared process tree.
    pub assigned_uid: u32,
    /// Host GID proven for the prepared process tree.
    pub assigned_gid: u32,
    /// Bubblewrap's host-view PID-namespace leader.
    ///
    /// This is evidence and a cgroup anchor, not the identity expected on
    /// every Agent packet: the Agent itself is a descendant of this reaper.
    pub sandbox_leader_pid: u32,
}

/// Broker operations needed by the one-shot launch transaction.
pub trait LaunchBroker: Send + Sync {
    /// Atomically consumes the pending authorization for `request`.
    fn consume_authorization(
        &self,
        request: LaunchRequest,
        complete: SupervisorCompletion<LaunchAuthorization>,
    ) -> Result<(), SupervisorError>;

    /// Durably appends exact canonical signed-envelope bytes.
    fn append_receipt(
        &self,
        receipt_bytes: Vec<u8>,
        complete: SupervisorCompletion<ReceiptAcknowledgement>,
    ) -> Result<(), SupervisorError>;

    /// Reconnects to the install-pinned broker and exchanges exact receipt checkpoints.
    fn reconnect_session(
        &self,
        reconnect: BrokerReconnect,
        complete: SupervisorCompletion<BrokerReconnect>,
    ) -> Result<(), SupervisorError>;

    /// Cancels the current reconnect attempt so a later attempt can start.
    ///
    /// A pending completion receives [`SupervisorError::BrokerUnavailable`];
    /// cancellation is otherwise a no-op.
    fn cancel_reconnect(&self);

    /// Requests one durable broker decision for an exact controller-loss Park.
    fn settle_controller_loss(
        &self,
        settlement: ControllerLossSettlement,
        complete: SupervisorCompletion<ControllerLossAcknowledgement>,
    ) -> Result<(), SupervisorError>;

    /// Arms one receive for the next authenticated Session request or receipt ACK.
    ///
    /// Delivery of a lifecycle request certifies that the broker durably stored
    /// its authorization before sending it; the supervisor never treats a
    /// capability-channel or Agent message as equivalent authority.
    ///
    /// Exactly one receive may be outstanding. The supervisor assigns the
    /// connection epoch captured by `complete`; peers never supply epochs.
    fn receive_session_request(
        &self,
        complete: SupervisorCompletion<ProtocolMessage>,
    ) -> Result<(), SupervisorError>;

    /// Sends one exact signed lifecycle receipt without starting another receive.
    ///
    /// Its durable acknowledgement arrives through [`Self::receive_session_request`].
    fn send_session_receipt(
        &self,
        receipt_bytes: Vec<u8>,
        complete: SupervisorCompletion<()>,
    ) -> Result<(), SupervisorError>;

    /// Sends one correlated lifecycle or status response asynchronously.
    fn send_session_response(
        &self,
        response: ProtocolResponse,
        complete: SupervisorCompletion<()>,
    ) -> Result<(), SupervisorError>;

    /// Cancels outstanding broker I/O and closes the authenticated channel.
    fn close(&self);
}

/// Root-owned receipt signer used for a new chain.
pub trait LaunchSigner: Send + Sync {
    /// Trusted installed release digest.
    fn release_id(&self) -> &str;

    /// Active signing-key digest selected for a new chain.
    fn signing_key_id(&self) -> &str;

    /// Signs exact canonical [`ReceiptPayload`] bytes asynchronously.
    fn sign(
        &self,
        payload_bytes: Vec<u8>,
        complete: SupervisorCompletion<String>,
    ) -> Result<(), SupervisorError>;
}

/// Exclusive installed host-identity lease.
pub trait IdentityGuard: Send {
    /// Identity held until the process tree is proved empty.
    fn identity(&self) -> Identity;

    /// Clears the fail-closed lease marker after zero survivors are proved.
    fn release(self: Box<Self>) -> Result<(), SupervisorError>;

    /// Permanently prevents reuse after cleanup could not be proved.
    fn poison(self: Box<Self>) -> Result<(), SupervisorError>;
}

/// Dynamic proof that a host PID remains inside one Session's process tree.
pub trait ProcessMembership: Send + Sync {
    /// Re-reads the authoritative process boundary for `pid`.
    fn contains(&self, pid: u32) -> Result<bool, SupervisorError>;
}

/// Capability listener created unreachable and enabled only after receipt ACK.
pub trait CapabilityGate: Send {
    /// Mount declaration added to the resolved confinement plan.
    fn channel(&self) -> Channel;

    /// Fixes the verified process and envelope metadata for this listener.
    fn bind(
        &mut self,
        binding: CapabilityBinding,
        membership: Arc<dyn ProcessMembership>,
    ) -> Result<(), SupervisorError>;

    /// Makes the listener reachable after the exact receipt is durable.
    fn enable(&mut self) -> Result<(), SupervisorError>;

    /// Immediately revokes live and future capability use without destroying
    /// the rendezvous needed by a later authorized Resume.
    ///
    /// An error reports that the reversible revocation could not be fully
    /// proved, but implementations must still make existing and future uses
    /// unreachable before returning it.
    fn revoke(&mut self) -> Result<(), SupervisorError>;

    /// Revokes the listener and its owned rendezvous path. Idempotent.
    fn close(&mut self);
}

/// A confined Agent process tree still blocked before workload execution.
pub trait PreparedAgent: Send {
    /// Evidence established while the workload is blocked.
    fn evidence(&self) -> &IsolationEvidence;

    /// Digest identifying the measured sandbox helper used for this launch.
    fn backend_id(&self) -> &str;

    /// Bubblewrap's observed host-view PID-namespace leader.
    fn sandbox_leader_pid(&self) -> Option<u32>;

    /// Current host PIDs enclosed in this Session's cgroup.
    fn processes(&self) -> Result<Vec<u32>, SupervisorError>;

    /// Dynamic cgroup membership used when a capability peer connects later.
    fn process_membership(&self) -> Arc<dyn ProcessMembership>;

    /// Releases the startup gate.
    ///
    /// On failure, the implementation disposes the consumed prepared tree and
    /// returns [`SupervisorError::CleanupUnproven`] when zero survivors cannot
    /// be proved.
    fn start(self: Box<Self>) -> Result<Box<dyn RunningAgent>, SupervisorError>;

    /// Disposes the blocked tree and proves it empty.
    fn dispose(&mut self) -> Result<(), SupervisorError>;
}

/// A running Agent process tree with opaque ACP stdio.
pub trait RunningAgent: Send {
    /// Starts relaying ACP bytes without parsing, logging, or copying them into receipts.
    ///
    /// Implementations also drain Agent stderr without presenting its content
    /// and complete asynchronously so the lifecycle owner retains mechanics.
    fn start_relay(
        &mut self,
        input: Box<dyn Read + Send>,
        output: Box<dyn Write + Send>,
        events: Arc<dyn Fn(RunningAgentEvent) + Send + Sync>,
    ) -> Result<(), SupervisorError>;

    /// Revokes every relay worker's authority to publish further lifecycle events.
    ///
    /// Completion is asynchronous because production workers may be concurrently
    /// observing controller or process I/O when terminal cleanup begins.
    fn quiesce_relay(&mut self, complete: SupervisorCompletion<()>) -> Result<(), SupervisorError>;

    /// Freezes the whole process tree while retaining its identity lease.
    ///
    /// Failure reports the settled post-attempt state or that no state was provable.
    fn park(&mut self) -> Result<(), MechanicFailure>;

    /// Thaws the whole process tree after durable broker authorization.
    ///
    /// Failure reports the settled post-attempt state or that no state was provable.
    fn resume(&mut self) -> Result<(), MechanicFailure>;

    /// Interrupts the whole process tree and restores its prior freeze state.
    ///
    /// Failure reports the settled post-attempt state or that no state was provable.
    fn interrupt(&mut self) -> Result<(), MechanicFailure>;

    /// Disposes the whole process tree and proves it empty.
    fn dispose(&mut self) -> Result<(), SupervisorError>;
}

/// OS operations whose concrete implementation holds root authority.
pub trait LaunchPlatform: Send + Sync {
    /// Validates and exclusively leases the broker-assigned installed identity.
    fn acquire_identity(
        &self,
        assigned: Identity,
    ) -> Result<Box<dyn IdentityGuard>, SupervisorError>;

    /// Creates the Session capability socket in a disabled state.
    fn create_capability(
        &self,
        request: &LaunchRequest,
        assigned: Identity,
    ) -> Result<Box<dyn CapabilityGate>, SupervisorError>;

    /// Materializes the resolved confinement plan behind a startup gate.
    fn prepare(&self, plan: ConfinementPlan) -> Result<Box<dyn PreparedAgent>, SupervisorError>;
}

/// Stable launch failures. Variants deliberately carry no prompts, environment,
/// credentials, tool payloads, or external-process output.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum SupervisorError {
    /// The stdin launch frame was absent, malformed, noncanonical, or oversized.
    #[error("launch document rejected")]
    LaunchDocumentRejected,
    /// The broker did not return one matching live authorization.
    #[error("launch authorization rejected")]
    AuthorizationRejected,
    /// The authorization or acknowledgement broker operation failed.
    #[error("Control broker unavailable")]
    BrokerUnavailable,
    /// A broker operation did not complete within the fixed deadline.
    #[error("Control broker deadline expired")]
    BrokerTimeout,
    /// Local identity acquisition failed for a reason unrelated to broker assignment validity.
    #[error("Session identity unavailable")]
    IdentityUnavailable,
    /// The broker proved the installed identity pool is exhausted.
    #[error("no session identity is available")]
    SessionIdentityExhausted(Box<IdentityExhaustion>),
    /// The broker assigned an occupied, poisoned, or mismatched identity.
    #[error("broker-assigned session identity is invalid")]
    IdentityAssignmentInvalid,
    /// Trusted registry resolution failed before process creation.
    #[error("launch registry resolution failed")]
    ResolutionFailed,
    /// The disabled capability socket could not be created or bound.
    #[error("Agent capability channel unavailable")]
    CapabilityUnavailable,
    /// The process tree could not be prepared or started.
    #[error("confined Agent startup failed")]
    SpawnFailed,
    /// Prepared isolation evidence did not satisfy the complete contract.
    #[error("isolation evidence rejected")]
    IsolationRejected,
    /// A launch receipt signing operation failed or timed out.
    #[error("launcher receipt signing failed")]
    SigningUnavailable,
    /// Signed receipt construction produced an invalid envelope.
    #[error("launcher receipt invalid")]
    ReceiptInvalid,
    /// Exact receipt persistence failed.
    #[error("launcher receipt was not durably stored")]
    DurabilityUnavailable,
    /// Broker acknowledgement did not name the exact signed envelope.
    #[error("launcher receipt acknowledgement mismatched")]
    AcknowledgementMismatch,
    /// A lifecycle mechanic failed without a safe continuation state.
    #[error("Session lifecycle mechanic failed")]
    LifecycleMechanicUnavailable,
    /// Tree cleanup could not prove zero survivors; the identity is poisoned.
    #[error("Session cleanup could not be proved")]
    CleanupUnproven,
    /// Opaque ACP relay or process waiting failed.
    #[error("Agent stdio relay failed")]
    RelayFailed,
    /// This one-shot supervisor was already asked to launch.
    #[error("launch supervisor already used")]
    AlreadyLaunched,
    /// The fixed coordinator worker could not be started.
    #[error("launch supervisor worker unavailable")]
    WorkerUnavailable,
}

struct SupervisorInner {
    broker: Arc<dyn LaunchBroker>,
    signer: Arc<dyn LaunchSigner>,
    platform: Arc<dyn LaunchPlatform>,
    registry: Arc<Registry>,
    sessions_root: PathBuf,
    timeout: Duration,
    timer: Arc<dyn SupervisorTimer>,
    launched: AtomicBool,
}

/// One-shot authorized launch coordinator.
#[derive(Clone)]
pub struct LaunchSupervisor {
    inner: Arc<SupervisorInner>,
}

impl LaunchSupervisor {
    /// Creates a coordinator around already validated authorities and paths.
    pub fn new(
        broker: Arc<dyn LaunchBroker>,
        signer: Arc<dyn LaunchSigner>,
        platform: Arc<dyn LaunchPlatform>,
        registry: Arc<Registry>,
        sessions_root: PathBuf,
        timeout: Duration,
    ) -> Self {
        Self::new_with_timer(
            broker,
            signer,
            platform,
            registry,
            sessions_root,
            timeout,
            Arc::new(ThreadSupervisorTimer),
        )
    }

    /// Creates a coordinator with an explicit monotonic callback scheduler.
    ///
    /// This keeps lifecycle deadline tests deterministic without weakening the
    /// production constructor's fixed thread-backed clock.
    pub fn new_with_timer(
        broker: Arc<dyn LaunchBroker>,
        signer: Arc<dyn LaunchSigner>,
        platform: Arc<dyn LaunchPlatform>,
        registry: Arc<Registry>,
        sessions_root: PathBuf,
        timeout: Duration,
        timer: Arc<dyn SupervisorTimer>,
    ) -> Self {
        Self {
            inner: Arc::new(SupervisorInner {
                broker,
                signer,
                platform,
                registry,
                sessions_root,
                timeout,
                timer,
                launched: AtomicBool::new(false),
            }),
        }
    }

    /// Starts exactly one launch transaction on a dedicated coordinator.
    ///
    /// Registration errors are returned synchronously and do not invoke
    /// `complete`. Once accepted, `complete` runs exactly once on the
    /// coordinator thread, which remains alive until Session cleanup completes.
    pub fn launch(
        &self,
        request: LaunchRequest,
        controller_uid: u32,
        now_ms: u64,
        complete: LaunchCompletion,
    ) -> Result<(), SupervisorError> {
        if self
            .inner
            .launched
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(SupervisorError::AlreadyLaunched);
        }
        let validation_clock = Instant::now();
        let inner = Arc::clone(&self.inner);
        thread::Builder::new()
            .name("louiselm-launch-supervisor".to_owned())
            .spawn(move || {
                let broker = Arc::clone(&inner.broker);
                match run_launch(inner, request, controller_uid, now_ms, validation_clock) {
                    Ok((session, owner_finished)) => {
                        complete(Ok(session));
                        // Bubblewrap's parent-death signal follows its spawning thread,
                        // even after process ownership moves to the lifecycle worker.
                        let _ = owner_finished.recv();
                    }
                    Err(error) => {
                        broker.close();
                        complete(Err(error));
                    }
                }
            })
            .map(drop)
            .map_err(|_| SupervisorError::WorkerUnavailable)
    }
}

/// Reads one newline-delimited canonical launch document without consuming any
/// bytes already buffered after the delimiter.
pub fn read_launch_frame(
    reader: &mut (impl BufRead + ?Sized),
) -> Result<LaunchRequest, SupervisorError> {
    let mut frame = Vec::new();
    let mut bounded = reader.take((MAX_REQUEST_BYTES + 2) as u64);
    bounded
        .read_until(b'\n', &mut frame)
        .map_err(|_| SupervisorError::LaunchDocumentRejected)?;
    if frame.last() != Some(&b'\n') {
        return Err(SupervisorError::LaunchDocumentRejected);
    }
    frame.pop();
    if frame.len() > MAX_REQUEST_BYTES {
        return Err(SupervisorError::LaunchDocumentRejected);
    }
    LaunchRequest::parse_canonical(&frame).map_err(|_| SupervisorError::LaunchDocumentRejected)
}

fn run_launch(
    inner: Arc<SupervisorInner>,
    request: LaunchRequest,
    controller_uid: u32,
    now_ms: u64,
    validation_clock: Instant,
) -> Result<(LaunchedSession, mpsc::Receiver<()>), SupervisorError> {
    request
        .validate()
        .map_err(|_| SupervisorError::LaunchDocumentRejected)?;
    let authorization = await_broker(&inner, |complete| {
        inner
            .broker
            .consume_authorization(request.clone(), complete)
    })?;
    let elapsed_ms = u64::try_from(validation_clock.elapsed().as_millis()).unwrap_or(u64::MAX);
    authorization
        .validate_for(&request, controller_uid, now_ms.saturating_add(elapsed_ms))
        .map_err(|_| SupervisorError::AuthorizationRejected)?;
    let broker_loss_grace_ms = authorization.broker_loss_grace_ms;
    let assigned = Identity {
        slot: authorization.identity_slot,
        uid: authorization.assigned_uid,
        gid: authorization.assigned_gid,
    };

    let identity = inner.platform.acquire_identity(assigned)?;
    if identity.identity() != assigned {
        return Err(release_identity(
            identity,
            SupervisorError::IdentityAssignmentInvalid,
        ));
    }
    let mut capability = match inner.platform.create_capability(&request, assigned) {
        Ok(capability) => capability,
        Err(error) => return Err(release_identity(identity, error)),
    };
    let capability_channel = capability.channel();
    if !matches!(capability_channel, Channel::UnixSocket { .. }) {
        capability.close();
        return Err(release_identity(
            identity,
            SupervisorError::CapabilityUnavailable,
        ));
    }
    let mut resolution = match resolve(
        &request,
        &inner.registry,
        &inner.sessions_root,
        crate::sandbox::IdentityPlan::HostIdentity {
            uid: assigned.uid,
            gid: assigned.gid,
        },
    ) {
        Ok(resolution) => resolution,
        Err(_) => {
            capability.close();
            return Err(release_identity(
                identity,
                SupervisorError::ResolutionFailed,
            ));
        }
    };
    resolution.plan.channels.push(capability_channel.clone());
    let receipt_channels = resolution.plan.channels.clone();

    let prepared = match inner.platform.prepare(resolution.plan) {
        Ok(prepared) => prepared,
        Err(error) => {
            capability.close();
            if error == SupervisorError::CleanupUnproven {
                let _ = identity.poison();
                return Err(SupervisorError::CleanupUnproven);
            }
            return Err(release_identity(identity, error));
        }
    };
    if prepared.evidence().check().is_err() {
        return Err(cleanup_prepared(
            prepared,
            capability,
            identity,
            SupervisorError::IsolationRejected,
        ));
    }
    let leader_pid = match prepared.sandbox_leader_pid() {
        Some(pid) => pid,
        None => {
            return Err(cleanup_prepared(
                prepared,
                capability,
                identity,
                SupervisorError::IsolationRejected,
            ));
        }
    };
    match prepared.processes() {
        Ok(processes) if processes.contains(&leader_pid) => {}
        _ => {
            return Err(cleanup_prepared(
                prepared,
                capability,
                identity,
                SupervisorError::IsolationRejected,
            ));
        }
    }
    let membership = prepared.process_membership();
    if membership.contains(leader_pid) != Ok(true) {
        return Err(cleanup_prepared(
            prepared,
            capability,
            identity,
            SupervisorError::IsolationRejected,
        ));
    }
    let channel_id = capability_channel.id().to_owned();
    let binding = CapabilityBinding {
        session_id: request.session_id.clone(),
        channel_id,
        envelope_revision: request.envelope_revision,
        identity_slot: assigned.slot,
        assigned_uid: assigned.uid,
        assigned_gid: assigned.gid,
        sandbox_leader_pid: leader_pid,
    };
    if capability.bind(binding.clone(), membership).is_err() {
        return Err(cleanup_prepared(
            prepared,
            capability,
            identity,
            SupervisorError::CapabilityUnavailable,
        ));
    }

    let evidence = match launch_evidence(
        &request,
        &resolution.runtime,
        prepared.evidence(),
        prepared.backend_id(),
        &receipt_channels,
        broker_loss_grace_ms,
    ) {
        Ok(evidence) => evidence,
        Err(error) => {
            return Err(cleanup_prepared(prepared, capability, identity, error));
        }
    };
    let launch_payload = ReceiptPayload {
        schema: RECEIPT_SCHEMA.to_owned(),
        session_id: request.session_id.clone(),
        run_id: request.run_id.clone(),
        request_id: request.request_id.clone(),
        envelope_revision: request.envelope_revision,
        sequence: 0,
        previous_receipt_digest: None,
        release_id: inner.signer.release_id().to_owned(),
        signing_key_id: inner.signer.signing_key_id().to_owned(),
        outcome: ReceiptOutcome::Launch {
            authorization: Authorization {
                authorization_id: authorization.authorization_id,
                request_id: authorization.request_id,
                request_digest: authorization.request_digest,
            },
            evidence: Box::new(evidence),
        },
        resulting_state: SessionState::Starting,
    };
    let launch_receipt = match transact_receipt(&inner, launch_payload) {
        Ok(receipt) => receipt,
        Err(error) => {
            return Err(cleanup_prepared(prepared, capability, identity, error));
        }
    };

    if capability.enable().is_err() {
        return Err(cleanup_prepared(
            prepared,
            capability,
            identity,
            SupervisorError::CapabilityUnavailable,
        ));
    }
    let running = match prepared.start() {
        Ok(running) => running,
        Err(error) => {
            capability.close();
            if error == SupervisorError::CleanupUnproven {
                let _ = identity.poison();
                return Err(SupervisorError::CleanupUnproven);
            }
            return Err(release_identity(identity, error));
        }
    };

    let launch_digest = launch_receipt.digest();
    let start_payload = ReceiptPayload {
        schema: RECEIPT_SCHEMA.to_owned(),
        session_id: launch_receipt.payload.session_id.clone(),
        run_id: launch_receipt.payload.run_id.clone(),
        request_id: format!("start-{}", launch_digest.hex()),
        envelope_revision: launch_receipt.payload.envelope_revision,
        sequence: 1,
        previous_receipt_digest: Some(launch_digest.to_string()),
        release_id: launch_receipt.payload.release_id.clone(),
        signing_key_id: launch_receipt.payload.signing_key_id.clone(),
        outcome: ReceiptOutcome::Start {
            authority: ReceiptAuthority::Cause {
                cause: ReceiptCause::LaunchAcknowledged,
            },
        },
        resulting_state: SessionState::Running,
    };
    let receipt = match transact_receipt(&inner, start_payload) {
        Ok(receipt) => receipt,
        Err(error) => {
            return Err(cleanup_running(running, capability, identity, error));
        }
    };

    let resources =
        lifecycle::SessionResources::new(running, capability, identity, Arc::clone(&inner.broker));
    LaunchedSession::new(
        resources,
        Arc::clone(&inner.signer),
        vec![launch_receipt, receipt],
        binding,
        inner.timeout,
        Arc::clone(&inner.timer),
        Duration::from_millis(u64::from(broker_loss_grace_ms)),
    )
}

fn launch_evidence(
    request: &LaunchRequest,
    runtime: &crate::registry::RuntimeMeasurement,
    isolation: &IsolationEvidence,
    backend_id: &str,
    channels: &[Channel],
    broker_loss_grace_ms: u32,
) -> Result<LaunchEvidence, SupervisorError> {
    let runtime_bytes = serde_json::to_vec(runtime).map_err(|_| SupervisorError::ReceiptInvalid)?;
    let isolation_bytes =
        serde_json::to_vec(isolation).map_err(|_| SupervisorError::ReceiptInvalid)?;
    let kernel_bytes =
        serde_json::to_vec(&isolation.kernel).map_err(|_| SupervisorError::ReceiptInvalid)?;
    let mut channel_ids = channels
        .iter()
        .map(|channel| channel.id().to_owned())
        .collect::<Vec<_>>();
    channel_ids.sort();
    if channel_ids.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(SupervisorError::ReceiptInvalid);
    }
    let evidence = LaunchEvidence {
        launch_request_digest: request.digest().to_string(),
        runtime_measurement_digest: Digest::of(&runtime_bytes).to_string(),
        skill_generation_id: request.skill_generation_id.clone(),
        session_input_manifest_id: request.session_input_manifest_id.clone(),
        isolation_contract: CONTRACT_VERSION.to_owned(),
        isolation_backend_id: backend_id.to_owned(),
        kernel_identity: Digest::of(&kernel_bytes).to_string(),
        isolation_evidence_digest: Digest::of(&isolation_bytes).to_string(),
        broker_loss_grace_ms,
        capability_channel_ids: channel_ids,
    };
    let probe = ReceiptPayload {
        schema: RECEIPT_SCHEMA.to_owned(),
        session_id: request.session_id.clone(),
        run_id: request.run_id.clone(),
        request_id: request.request_id.clone(),
        envelope_revision: request.envelope_revision,
        sequence: 0,
        previous_receipt_digest: None,
        release_id: Digest::of(b"probe-release").to_string(),
        signing_key_id: Digest::of(b"probe-key").to_string(),
        outcome: ReceiptOutcome::Launch {
            authorization: Authorization {
                authorization_id: request.authorization_id.clone(),
                request_id: request.request_id.clone(),
                request_digest: request.digest().to_string(),
            },
            evidence: Box::new(evidence.clone()),
        },
        resulting_state: SessionState::Starting,
    };
    probe
        .validate()
        .map_err(|_| SupervisorError::ReceiptInvalid)?;
    Ok(evidence)
}

fn transact_receipt(
    inner: &SupervisorInner,
    payload: ReceiptPayload,
) -> Result<SignedReceipt, SupervisorError> {
    payload
        .validate()
        .map_err(|_| SupervisorError::ReceiptInvalid)?;
    let signature = await_signer(inner, payload.canonical_bytes())?;
    let receipt = SignedReceipt {
        schema: SIGNED_RECEIPT_SCHEMA.to_owned(),
        payload,
        signature,
    };
    receipt
        .validate()
        .map_err(|_| SupervisorError::ReceiptInvalid)?;
    let session_id = receipt.payload.session_id.clone();
    let run_id = receipt.payload.run_id.clone();
    let receipt_bytes = receipt.canonical_bytes();
    let head = ReceiptHead {
        sequence: receipt.payload.sequence,
        digest: Digest::of(&receipt_bytes).to_string(),
    };
    let acknowledgement = await_broker(inner, |complete| {
        inner.broker.append_receipt(receipt_bytes, complete)
    })
    .map_err(|error| match error {
        SupervisorError::BrokerTimeout => SupervisorError::BrokerTimeout,
        _ => SupervisorError::DurabilityUnavailable,
    })?;
    match acknowledgement.exact_disposition(&session_id, &run_id, &head) {
        Some(ReceiptDisposition::DurablyStored) => {}
        Some(ReceiptDisposition::Rejected) => return Err(SupervisorError::DurabilityUnavailable),
        None => return Err(SupervisorError::AcknowledgementMismatch),
    }
    Ok(receipt)
}

fn await_broker<T>(
    inner: &SupervisorInner,
    initiate: impl FnOnce(SupervisorCompletion<T>) -> Result<(), SupervisorError>,
) -> Result<T, SupervisorError>
where
    T: Send + 'static,
{
    let (sender, receiver) = mpsc::sync_channel(1);
    initiate(Box::new(move |result| {
        let _ = sender.try_send(result);
    }))?;
    match receiver.recv_timeout(inner.timeout) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            inner.broker.close();
            Err(SupervisorError::BrokerTimeout)
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            inner.broker.close();
            Err(SupervisorError::BrokerUnavailable)
        }
    }
}

fn await_signer(inner: &SupervisorInner, payload: Vec<u8>) -> Result<String, SupervisorError> {
    let (sender, receiver) = mpsc::sync_channel(1);
    inner.signer.sign(
        payload,
        Box::new(move |result| {
            let _ = sender.try_send(result);
        }),
    )?;
    match receiver.recv_timeout(inner.timeout) {
        Ok(result) => result,
        Err(_) => Err(SupervisorError::SigningUnavailable),
    }
}

fn cleanup_prepared(
    mut prepared: Box<dyn PreparedAgent>,
    mut capability: Box<dyn CapabilityGate>,
    identity: Box<dyn IdentityGuard>,
    original: SupervisorError,
) -> SupervisorError {
    capability.close();
    if prepared.dispose().is_ok() {
        release_identity(identity, original)
    } else {
        let _ = identity.poison();
        SupervisorError::CleanupUnproven
    }
}

fn cleanup_running(
    mut running: Box<dyn RunningAgent>,
    mut capability: Box<dyn CapabilityGate>,
    identity: Box<dyn IdentityGuard>,
    original: SupervisorError,
) -> SupervisorError {
    capability.close();
    if running.dispose().is_ok() {
        release_identity(identity, original)
    } else {
        let _ = identity.poison();
        SupervisorError::CleanupUnproven
    }
}

fn release_identity(
    identity: Box<dyn IdentityGuard>,
    original: SupervisorError,
) -> SupervisorError {
    if identity.release().is_ok() {
        original
    } else {
        SupervisorError::CleanupUnproven
    }
}

impl From<io::Error> for SupervisorError {
    fn from(_: io::Error) -> Self {
        Self::RelayFailed
    }
}
