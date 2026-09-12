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
    io::{self, BufRead, Read},
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
    launch_transport::{KernelCredentials, KernelProcess},
    launcher_install::Identity,
    registry::Registry,
    sandbox::{Channel, ConfinementPlan},
};

pub mod command;
mod lifecycle;
pub mod recovery;
mod tool_execution;
mod tool_helper;
mod tool_integration;
pub use tool_helper::HelperPrincipal;
pub use tool_integration::ToolIsolationEvidence;
mod relay;
mod stdio;
mod system;

pub use lifecycle::LaunchedSession;
pub use stdio::RelayStdio;
pub use system::{
    InstalledLaunchSigner, SYSTEM_CAPABILITY_GUEST_PATH, SYSTEM_CAPABILITY_ROOT,
    SYSTEM_CGROUP_ROOT, SYSTEM_REGISTRY_ROOT, SYSTEM_SESSIONS_ROOT, SystemLaunchPlatform,
    SystemRunningAgent, connect_control_broker,
};

/// One exactly-once asynchronous launch completion.
pub type LaunchCompletion =
    Box<dyn FnOnce(Result<LaunchedSession, SupervisorError>) + Send + 'static>;

/// Completion used by broker and signer ports.
pub type SupervisorCompletion<T> = Box<dyn FnOnce(Result<T, SupervisorError>) + Send + 'static>;

/// Nonblocking relay-event delivery: `false` means retry while the relay is active.
///
/// Callbacks must not block. A worker retains an undelivered event until accepted
/// or terminal cancellation, so a full owner queue cannot prevent its shutdown.
pub type RunningAgentEvents = Arc<dyn Fn(RunningAgentEvent) -> bool + Send + Sync>;

/// Sanitized observations emitted by an Agent relay without lifecycle authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunningAgentEvent {
    /// The controller side of ACP stdin reached EOF.
    ControllerEof,
    /// The supervised process ended with a bounded public classification.
    ProcessExited(ProcessExitClassification),
    /// Authenticated Agent lifetime or executable proof ended without an exit status.
    AgentIdentityLost,
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
    ///
    /// # Errors
    /// Returns a scheduler/worker admission error if the callback cannot be registered.
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
    /// Session whose authenticated Agent may use the listener.
    pub session_id: String,
    /// Run that authorized this Agent lifetime.
    pub run_id: String,
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
    /// Authenticated workload's host PID, never the namespace reaper.
    pub agent_pid: u32,
}

/// Identity supplied by the trusted launch platform after restricted startup.
#[derive(Clone, Debug)]
pub struct AgentAuthentication {
    /// Kernel credentials authenticated by the platform, not a peer claim.
    pub credentials: KernelCredentials,
    /// Actual kernel lifetime proof. Production gates refuse an absent proof;
    /// deterministic platform doubles may model it without an OS process.
    pub process: Option<Arc<KernelProcess>>,
    /// Exact integration verified from trusted launch inputs before startup.
    /// Production refuses missing evidence; platform doubles may omit it.
    pub tool_isolation: Option<ToolIsolationEvidence>,
}

/// Broker operations needed by the one-shot launch transaction.
pub trait LaunchBroker: Send + Sync {
    /// Atomically consumes the pending authorization for `request`.
    ///
    /// # Errors
    /// Returns broker admission/unavailability errors; authorization/refusal and transport failures after admission arrive through `complete`.
    fn consume_authorization(
        &self,
        request: LaunchRequest,
        complete: SupervisorCompletion<LaunchAuthorization>,
    ) -> Result<(), SupervisorError>;

    /// Durably appends exact canonical signed-envelope bytes.
    ///
    /// # Errors
    /// Returns broker admission/unavailability errors; durable append failures after admission arrive through `complete`.
    fn append_receipt(
        &self,
        receipt_bytes: Vec<u8>,
        complete: SupervisorCompletion<ReceiptAcknowledgement>,
    ) -> Result<(), SupervisorError>;

    /// Reconnects to the install-pinned broker and exchanges exact receipt checkpoints.
    ///
    /// # Errors
    /// Returns broker admission/unavailability errors; connection and checkpoint failures after admission arrive through `complete`.
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
    ///
    /// # Errors
    /// Returns broker admission/unavailability errors; settlement refusal or transport failures after admission arrive through `complete`.
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
    ///
    /// # Errors
    /// Returns admission errors for unavailable/busy broker I/O; authenticated receive failures after admission arrive through `complete`.
    fn receive_session_request(
        &self,
        complete: SupervisorCompletion<ProtocolMessage>,
    ) -> Result<(), SupervisorError>;

    /// Sends one exact signed lifecycle receipt without starting another receive.
    ///
    /// Its durable acknowledgement arrives through [`Self::receive_session_request`].
    ///
    /// # Errors
    /// Returns broker admission/unavailability errors; send failures after admission arrive through `complete`.
    fn send_session_receipt(
        &self,
        receipt_bytes: Vec<u8>,
        complete: SupervisorCompletion<()>,
    ) -> Result<(), SupervisorError>;

    /// Sends one correlated lifecycle or status response asynchronously.
    ///
    /// # Errors
    /// Returns broker admission/unavailability errors; send failures after admission arrive through `complete`.
    fn send_session_response(
        &self,
        response: ProtocolResponse,
        complete: SupervisorCompletion<()>,
    ) -> Result<(), SupervisorError>;

    /// Sends a command attribution or outcome on the retained authenticated channel.
    /// Replies arrive through the single Session receive, never a competing reader.
    ///
    /// # Errors
    /// Reports invalid records or unavailable transport; later failures use `complete`.
    fn send_command(
        &self,
        message: crate::launch_protocol::CommandMessage,
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
    ///
    /// # Errors
    /// Returns signing-worker admission errors; signing failures after admission arrive through `complete`.
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
    ///
    /// # Errors
    /// Returns an identity-release failure; the identity must not become reusable without a proved release.
    fn release(self: Box<Self>) -> Result<(), SupervisorError>;

    /// Permanently prevents reuse after cleanup could not be proved.
    ///
    /// # Errors
    /// Returns a persistence failure if permanently withholding the identity cannot be recorded.
    fn poison(self: Box<Self>) -> Result<(), SupervisorError>;
}

/// Dynamic proof that a host PID remains inside one Session's process tree.
pub trait ProcessMembership: Send + Sync {
    /// Re-reads the authoritative process boundary for `pid`.
    ///
    /// # Errors
    /// Returns a process-boundary read/validation error when membership cannot be established.
    fn contains(&self, pid: u32) -> Result<bool, SupervisorError>;
}

/// Capability listener created unreachable and enabled only after receipt ACK.
pub trait CapabilityGate: Send {
    /// Mount declaration added to the resolved confinement plan.
    fn channel(&self) -> Channel;

    /// Fixes the verified process and envelope metadata for this listener.
    ///
    /// # Errors
    /// Returns an invalid/inconsistent binding or capability-listener setup error.
    fn bind(
        &mut self,
        binding: CapabilityBinding,
        authentication: AgentAuthentication,
    ) -> Result<(), SupervisorError>;

    /// Makes the listener reachable after the exact receipt is durable.
    ///
    /// # Errors
    /// Returns an unbound/closed gate or listener activation error.
    fn enable(&mut self) -> Result<(), SupervisorError>;

    /// Receives one request after validating connected and per-packet Agent identity.
    /// May be armed before the Agent connects; at most one receive is outstanding.
    ///
    /// # Errors
    /// Refuses a closed/busy gate. Authentication or decoding failures use `complete`.
    fn receive_command(
        &mut self,
        complete: SupervisorCompletion<ProtocolMessage>,
    ) -> Result<(), SupervisorError>;

    /// Rechecks the original packet binding and the exact broker decision locally.
    ///
    /// # Errors
    /// Refuses replay, expiry, revocation, changed scope or a lost Agent lifetime.
    fn authorize_command(
        &self,
        request: &crate::launch_protocol::ToolExecutionRequest,
        decision: &crate::launch_protocol::CommandMessage,
        forwarded_at: std::time::Instant,
    ) -> Result<command::CommandPermit, SupervisorError>;

    /// Shared local enforcement owner, retained through every delegated lifetime.
    ///
    /// # Errors
    /// Refuses an unbound or unavailable Agent authority owner.
    fn command_enforcer(&self) -> Result<Arc<command::CommandEnforcer>, SupervisorError>;

    /// Sends a correlated result to the same authenticated Agent connection.
    ///
    /// # Errors
    /// Refuses an invalid result or unavailable connection; late errors use `complete`.
    fn send_command(
        &self,
        message: crate::launch_protocol::CommandMessage,
        complete: SupervisorCompletion<()>,
    ) -> Result<(), SupervisorError>;

    /// Immediately revokes live and future capability use without destroying
    /// the rendezvous needed by a later authorized Resume.
    ///
    /// An error reports that the reversible revocation could not be fully
    /// proved, but implementations must still make existing and future uses
    /// unreachable before returning it.
    ///
    /// # Errors
    /// Returns an error if reversible revocation cannot be proved, without retaining live capability reachability.
    fn revoke(&mut self) -> Result<(), SupervisorError>;

    /// Enables a fresh command generation only after durable operator Resume.
    /// Transport-only reattachment must continue using `enable`, so reconnect
    /// cannot restore revoked command authority. Old permits/grants stay revoked.
    /// # Errors
    /// Refuses a dead/replaced Agent or an unavailable capability generation.
    fn enable_after_resume(&mut self) -> Result<(), SupervisorError> {
        self.enable()
    }

    /// Revokes the listener and its owned rendezvous path. Idempotent.
    fn close(&mut self);
}

/// A confined Agent process tree still blocked before workload execution.
/// Preparation and start stay on one thread: Linux startup tracing is thread-owned.
pub trait PreparedAgent {
    /// Evidence established while the workload is blocked.
    fn evidence(&self) -> &IsolationEvidence;

    /// Digest identifying the measured sandbox helper used for this launch.
    fn backend_id(&self) -> &str;

    /// Bubblewrap's observed host-view PID-namespace leader.
    fn sandbox_leader_pid(&self) -> Option<u32>;

    /// Current host PIDs enclosed in this Session's cgroup.
    ///
    /// # Errors
    /// Returns a process-boundary read error when the enclosed process set cannot be established.
    fn processes(&self) -> Result<Vec<u32>, SupervisorError>;

    /// Containment proof, never a source of Agent channel authority.
    fn process_membership(&self) -> Arc<dyn ProcessMembership>;

    /// Releases the startup gate.
    ///
    /// On failure, the implementation disposes the consumed prepared tree and
    /// returns [`SupervisorError::CleanupUnproven`] when zero survivors cannot
    /// be proved.
    ///
    /// # Errors
    /// Returns a startup-gate failure, or `CleanupUnproven` if the failed prepared tree cannot be proved empty.
    fn start(self: Box<Self>) -> Result<Box<dyn RunningAgent>, SupervisorError>;

    /// Disposes the blocked tree and proves it empty.
    ///
    /// # Errors
    /// Returns cleanup failure unless the owned process tree is proved empty.
    fn dispose(&mut self) -> Result<(), SupervisorError>;
}

/// A running Agent process tree with opaque ACP stdio.
pub trait RunningAgent: Send {
    /// Retains a verified recovery point while the tree is frozen.
    ///
    /// Production performs I/O on an owned worker; disposal joins that worker
    /// before identity release. Unsupported adapters return no evidence.
    /// Callbacks must not block or synchronously call back into the adapter.
    ///
    /// # Errors
    /// Returns worker-admission errors. Layout, state and persistence failures
    /// arrive through `complete`; this never authorizes disposal or Resume.
    fn retain_recovery(
        &mut self,
        _request: recovery::RetentionRequest,
        complete: recovery::RecoveryCompletion,
    ) -> Result<(), SupervisorError> {
        complete(Err(recovery::RecoveryError::Unsupported));
        Ok(())
    }

    /// Starts the fixed measured isolated helper, without granting command authority.
    /// The callback returns its independently authenticated process and channel.
    /// The running Agent retains the helper tree and joins it on disposal.
    ///
    /// # Errors
    /// Refuses missing measured integration, an existing helper or worker failure.
    fn launch_helper(
        &mut self,
        request: crate::launch_protocol::CommandMessage,
        enforcer: Arc<command::CommandEnforcer>,
        complete: SupervisorCompletion<HelperPrincipal>,
    ) -> Result<(), SupervisorError>;

    /// Stops the owned helper tree without cancelling an unrelated Agent command.
    ///
    /// # Errors
    /// Returns `CleanupUnproven` unless the helper tree has terminated.
    fn cancel_helper(&mut self) -> Result<(), SupervisorError>;

    /// Executes one broker-authorized command asynchronously without Agent authority.
    ///
    /// Disposal cancels and joins execution before releasing the Session identity.
    /// Completion must not block; output remains untrusted data.
    ///
    /// # Errors
    /// Refuses unsupported integration, concurrent execution, invalid requests or startup failure.
    fn execute_tool(
        &mut self,
        permit: command::CommandPermit,
        complete: SupervisorCompletion<crate::launch_protocol::ToolExecutionResult>,
    ) -> Result<(), SupervisorError>;

    /// Cancels and joins the command tree before acknowledging policy revocation.
    ///
    /// # Errors
    /// Returns `CleanupUnproven` unless the owned tool tree is proved terminated.
    fn cancel_tool(&mut self) -> Result<(), SupervisorError>;
    /// Returns the cached identity established during restricted startup.
    ///
    /// # Errors
    /// Returns a refusal when the actual workload could not be authenticated.
    fn authentication(&self) -> Result<AgentAuthentication, SupervisorError>;

    /// Starts relaying ACP bytes without parsing, logging, or copying them into receipts.
    ///
    /// Implementations also drain Agent stderr without presenting its content
    /// and complete asynchronously so the lifecycle owner retains mechanics.
    ///
    /// # Errors
    /// Returns relay setup/worker-start errors. Later I/O and process outcomes arrive through `events`.
    fn start_relay(
        &mut self,
        controller: mpsc::Receiver<RelayStdio>,
        events: RunningAgentEvents,
    ) -> Result<(), SupervisorError>;

    /// Cancels and joins every relay worker, closing its owned I/O before completion.
    ///
    /// The callback-shaped boundary permits asynchronous cleanup. Completion may
    /// be immediate after a bounded join; callers must not block in the callback.
    ///
    /// # Errors
    /// Returns registration errors; cleanup-proof failures arrive through `complete`.
    fn quiesce_relay(&mut self, complete: SupervisorCompletion<()>) -> Result<(), SupervisorError>;

    /// Freezes the whole process tree while retaining its identity lease.
    ///
    /// Failure reports the settled post-attempt state or that no state was provable.
    ///
    /// # Errors
    /// Returns the proved post-attempt mechanical state, or `Ambiguous` if freeze completion cannot be proved.
    fn park(&mut self) -> Result<(), MechanicFailure>;

    /// Thaws the whole process tree after durable broker authorization.
    ///
    /// Failure reports the settled post-attempt state or that no state was provable.
    ///
    /// # Errors
    /// Returns the proved post-attempt mechanical state, or `Ambiguous` if thaw completion cannot be proved.
    fn resume(&mut self) -> Result<(), MechanicFailure>;

    /// Interrupts the whole process tree and restores its prior freeze state.
    ///
    /// Failure reports the settled post-attempt state or that no state was provable.
    ///
    /// # Errors
    /// Returns the proved post-attempt state, or `Ambiguous` if signalling and prior-state restoration cannot be proved.
    fn interrupt(&mut self) -> Result<(), MechanicFailure>;

    /// Cancels/joins all relay workers and disposes the whole process tree.
    ///
    /// # Errors
    /// Returns cleanup failure unless both relay quiescence and zero descendants are proved.
    fn dispose(&mut self) -> Result<(), SupervisorError>;
}

/// OS operations whose concrete implementation holds root authority.
pub trait LaunchPlatform: Send + Sync {
    /// Rejects known unsupported registrations before privileged resource acquisition.
    /// This prerequisite is rechecked against the actual runtime after restricted start.
    ///
    /// # Errors
    /// Returns `ToolIsolationUnproven` for missing or unsupported integration.
    fn check_integration(
        &self,
        request: &LaunchRequest,
        complete: SupervisorCompletion<()>,
    ) -> Result<(), SupervisorError>;

    /// Verifies enforced separation between this Agent and its tools before
    /// brokered effects are enabled. Missing integration must fail closed.
    ///
    /// # Errors
    /// Returns admission errors; verification results arrive through `complete`.
    fn verify_tool_isolation(
        &self,
        request: &LaunchRequest,
        agent: &AgentAuthentication,
        complete: SupervisorCompletion<Digest>,
    ) -> Result<(), SupervisorError>;

    /// Validates and exclusively leases the broker-assigned installed identity.
    ///
    /// # Errors
    /// Returns invalid-assignment, lease contention/poisoning, or lease-persistence errors.
    fn acquire_identity(
        &self,
        assigned: Identity,
    ) -> Result<Box<dyn IdentityGuard>, SupervisorError>;

    /// Creates the Session capability socket in a disabled state.
    ///
    /// # Errors
    /// Returns capability-path, permission, or disabled-listener setup errors.
    fn create_capability(
        &self,
        request: &LaunchRequest,
        assigned: Identity,
    ) -> Result<Box<dyn CapabilityGate>, SupervisorError>;

    /// Materializes the resolved confinement plan behind a startup gate.
    /// The coordinator supplies the already-authorized request used to resolve
    /// this plan, so retained storage stays bound to that exact launch.
    ///
    /// # Errors
    /// Returns confinement validation, resource setup, spawn, or host-identity verification errors.
    fn prepare(
        &self,
        request: &LaunchRequest,
        plan: ConfinementPlan,
    ) -> Result<Box<dyn PreparedAgent>, SupervisorError>;
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
    /// The actual Agent process identity could not be authenticated.
    #[error("Agent process identity rejected")]
    AgentIdentityRejected,
    /// No enforced Agent/tool separation was proved for this integration.
    #[error("Agent/tool isolation unproven")]
    ToolIsolationUnproven,
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
    ///
    /// # Errors
    /// Returns `AlreadyLaunched` or `WorkerUnavailable` before admission. Launch-transaction failures are delivered to `complete`.
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
                match run_launch(&inner, &request, controller_uid, now_ms, validation_clock) {
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
///
/// # Errors
/// Returns `LaunchDocumentRejected` for read failures, missing delimiters, oversized frames, or noncanonical/invalid launch requests.
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

#[expect(
    clippy::too_many_lines,
    reason = "Startup is one ordered authorization, resource-acquisition and receipt transaction with rollback at each boundary."
)]
fn run_launch(
    inner: &SupervisorInner,
    request: &LaunchRequest,
    controller_uid: u32,
    now_ms: u64,
    validation_clock: Instant,
) -> Result<(LaunchedSession, mpsc::Receiver<()>), SupervisorError> {
    request
        .validate()
        .map_err(|_| SupervisorError::LaunchDocumentRejected)?;
    let (sender, receiver) = mpsc::sync_channel(1);
    inner.platform.check_integration(
        request,
        Box::new(move |result| {
            let _ = sender.try_send(result);
        }),
    )?;
    receiver
        .recv_timeout(inner.timeout)
        .map_err(|_| SupervisorError::ToolIsolationUnproven)??;
    let authorization = await_broker(inner, |complete| {
        inner
            .broker
            .consume_authorization(request.clone(), complete)
    })?;
    let elapsed_ms = u64::try_from(validation_clock.elapsed().as_millis()).unwrap_or(u64::MAX);
    authorization
        .validate_for(request, controller_uid, now_ms.saturating_add(elapsed_ms))
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
    let mut capability = match inner.platform.create_capability(request, assigned) {
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
    let Ok(mut resolution) = resolve(
        request,
        &inner.registry,
        &inner.sessions_root,
        crate::sandbox::IdentityPlan::HostIdentity {
            uid: assigned.uid,
            gid: assigned.gid,
        },
    ) else {
        capability.close();
        return Err(release_identity(
            identity,
            SupervisorError::ResolutionFailed,
        ));
    };
    resolution.plan.channels.push(capability_channel.clone());
    let receipt_channels = resolution.plan.channels.clone();

    let prepared = match inner.platform.prepare(request, resolution.plan) {
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
    let Some(leader_pid) = prepared.sandbox_leader_pid() else {
        return Err(cleanup_prepared(
            prepared,
            capability,
            identity,
            SupervisorError::IsolationRejected,
        ));
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
    let evidence = match launch_evidence(
        request,
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
    let launch_receipt = match transact_receipt(inner, launch_payload) {
        Ok(receipt) => receipt,
        Err(error) => {
            return Err(cleanup_prepared(prepared, capability, identity, error));
        }
    };

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

    let authentication = match running.authentication() {
        Ok(proof)
            if proof.credentials.pid != leader_pid
                && proof.credentials.pid != 0
                && proof.credentials.uid == assigned.uid
                && proof.credentials.gid == assigned.gid
                && membership.contains(proof.credentials.pid) == Ok(true) =>
        {
            proof
        }
        _ => {
            return Err(cleanup_running(
                running,
                capability,
                identity,
                SupervisorError::AgentIdentityRejected,
            ));
        }
    };
    let (sender, receiver) = mpsc::sync_channel(1);
    let isolated = inner
        .platform
        .verify_tool_isolation(
            request,
            &authentication,
            Box::new(move |result| {
                let _ = sender.try_send(result);
            }),
        )
        .and_then(|()| {
            receiver
                .recv_timeout(inner.timeout)
                .map_err(|_| SupervisorError::ToolIsolationUnproven)?
        });
    let tool_isolation_digest = match isolated {
        Ok(digest) => digest.to_string(),
        Err(error) => return Err(cleanup_running(running, capability, identity, error)),
    };
    let binding = CapabilityBinding {
        session_id: request.session_id.clone(),
        run_id: request.run_id.clone(),
        channel_id,
        envelope_revision: request.envelope_revision,
        identity_slot: assigned.slot,
        assigned_uid: assigned.uid,
        assigned_gid: assigned.gid,
        agent_pid: authentication.credentials.pid,
    };
    let agent_process = authentication.process.clone();
    if let Err(error) = capability
        .bind(binding.clone(), authentication)
        .and_then(|()| capability.enable())
    {
        return Err(cleanup_running(running, capability, identity, error));
    }

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
            evidence: crate::launch_receipt::StartEvidence {
                agent_pid: binding.agent_pid,
                assigned_uid: assigned.uid,
                assigned_gid: assigned.gid,
                tool_isolation_digest,
            },
        },
        resulting_state: SessionState::Running,
    };
    let receipt = match transact_receipt(inner, start_payload) {
        Ok(receipt) => receipt,
        Err(error) => {
            return Err(cleanup_running(running, capability, identity, error));
        }
    };

    if let Some(process) = agent_process
        && process.valid().ok() != Some(true)
    {
        return Err(cleanup_running(
            running,
            capability,
            identity,
            SupervisorError::AgentIdentityRejected,
        ));
    }
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
