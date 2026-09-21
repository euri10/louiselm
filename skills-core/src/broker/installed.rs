//! Installed, unprivileged composition of launch policy, verification and transport.

use std::{fs, os::unix::fs::MetadataExt, path::Path, sync::Arc};

#[path = "promotion/installed.rs"]
mod promotion;
#[path = "verification/installed.rs"]
mod verification;

mod beads;

use super::{
    AuditLog, AuthorizationStore, BrokerError, BrokerService, BrokerSession, GrantRequest,
    PendingAuthorization, ReceiptStore, SessionInspection, now_ms,
};
use crate::{
    broker::lifecycle::LifecycleCaller,
    launch_protocol::LifecycleRequest,
    launch_receipt::SignedReceipt,
    launch_transport::{CredentialPin, SeqpacketChannel, SeqpacketListener},
    launcher_install::{LauncherConfig, LauncherPaths, LauncherVerifier},
};

/// One dedicated broker process's installed launch service.
///
/// Construction requires the installed non-root UID/GID with no additional
/// group authority. Public configuration and verification keys stay root-owned;
/// only private broker state and its rendezvous directory belong to the broker.
/// All methods perform blocking I/O on the explicitly owned broker worker.
pub struct InstalledBroker {
    pub(in crate::broker) service: BrokerService,
    pub(in crate::broker) verifier: Arc<LauncherVerifier>,
    pub(in crate::broker) provider_credentials:
        super::provider_credentials::ProviderCredentialStore,
    // Drop last, keeping adoption out through destruction of the owned service.
    _state_lock: fs::File,
}

impl InstalledBroker {
    /// Processes an authenticated operator conformance-waiver request on its Session worker.
    /// # Errors
    /// Refuses invalid policy, unavailable state and unacknowledged supervisor changes.
    pub fn waiver_control(
        &self,
        session: &mut BrokerSession,
        uid: u32,
        request: &super::waiver::Request,
    ) -> Result<super::waiver::Outcome, BrokerError> {
        let mut failure = None;
        let result = self.service.waiver_control(
            session,
            uid,
            request,
            now_ms()?,
            |key, payload, signature| match self.verifier.verify(key, payload, signature) {
                Ok(()) => true,
                Err(error) => {
                    failure = Some(error);
                    false
                }
            },
        );
        match failure {
            Some(error) => Err(BrokerError::Verification(error)),
            None => result,
        }
    }

    /// Inspect durable waiver outcomes without a live supervisor.
    /// # Errors
    /// Refuses foreign callers, mutations and unavailable canonical history.
    pub fn waiver_history(
        &self,
        uid: u32,
        id: &str,
        request: &super::waiver::Request,
    ) -> Result<super::waiver::Outcome, BrokerError> {
        self.service.waiver_history(uid, id, request, now_ms()?)
    }
    /// Returns a secret-free reference to a configured Provider, not permission to call it.
    ///
    /// # Errors
    /// Returns the existing typed protocol failure for unknown or invalid Provider ids.
    pub fn provider_credential(
        &self,
        provider: &str,
    ) -> Result<super::provider_credentials::CredentialHandle, crate::launch_protocol::ProtocolError>
    {
        self.provider_credentials.handle(provider)
    }

    /// Inspects or approves an exact dependency batch for the authenticated operator.
    /// Unattended Runs cannot gain approvals after start; this never starts a fetch.
    /// # Errors
    /// Refuses invalid identities, expired scope, unknown candidates or storage failure.
    pub fn dependency_control(
        &self,
        operator_uid: u32,
        session_id: &str,
        candidates: Option<&[String]>,
    ) -> Result<super::DependencyInspection, BrokerError> {
        self.service
            .dependency_control(operator_uid, session_id, candidates, now_ms()?)
    }
    /// Explicitly adopts a valid mismatched marker under the installed broker identity.
    ///
    /// `operator_uid` must come from the trusted sudo invocation, never Agent
    /// input. The CLI requires explicit confirmation before calling this API.
    /// Directory ownership must already match the systemd-provisioned identity.
    /// Blocks on filesystem I/O; refuses an active broker without waiting.
    /// Returns `true` when adopted, or `false` for an unchanged matching marker.
    /// Receipts and authorization records are never changed or repaired.
    ///
    /// # Errors
    /// Refuses foreign operators, invalid installed identity/authority/directories,
    /// busy state, missing/corrupt markers or failed audit/marker durability.
    pub fn adopt_state(
        paths: &LauncherPaths,
        state: &Path,
        operator_uid: u32,
    ) -> Result<bool, BrokerError> {
        let verifier =
            LauncherVerifier::open(paths, state).map_err(BrokerError::InstallationAuthority)?;
        let config = verifier.config();
        installed_identity(config)?;
        if operator_uid != config.operator_uid {
            return Err(BrokerError::ControllerMismatch);
        }
        private_directory(state, config.broker_uid, config.broker_gid)?;
        super::state_identity::adopt(
            state,
            config.broker_uid,
            config.broker_gid,
            operator_uid,
            now_ms()?,
        )
    }

    /// Reports a revoked Session's trusted binding and local containment observation.
    /// Receipt bytes remain untouched and untrusted. This inspection never grants authority.
    /// # Errors
    /// Refuses unknown Sessions or unavailable authority/reporting storage.
    pub fn key_revocation(
        &self,
        session_id: &str,
    ) -> Result<Option<crate::launcher_install::SessionKeyRevocation>, BrokerError> {
        self.service
            .authorizations()
            .consumed_for_session(session_id)?
            .ok_or(BrokerError::UnknownAuthorization)?;
        let report = self
            .verifier
            .key_revocation(session_id)
            .map_err(BrokerError::Verification)?;
        if report.is_some() {
            match self.service.check_history(session_id) {
                Err(BrokerError::Policy(error))
                    if error.code == crate::launch_protocol::ErrorCode::SigningKeyRevoked => {}
                Err(error) => return Err(error),
                Ok(()) => return Err(BrokerError::ReceiptUnauthorized),
            }
        }
        Ok(report)
    }
    /// Retains exact launch-bound supply facts produced on the trusted I/O worker.
    /// This updates status evidence only; it does not grant Session authority.
    /// Missing vendor discovery integrations must supply no native proof.
    ///
    /// # Errors
    /// Refuses foreign launch evidence, stale/future observations or unreadable state.
    pub fn retain_supply_posture(
        &self,
        session: &mut BrokerSession,
        evidence: crate::supply_posture::SupplyEvidence,
    ) -> Result<(), BrokerError> {
        self.service
            .retain_supply_posture(session, evidence, now_ms()?)
    }

    /// Reattaches an existing supervisor without reconstructing command authority.
    /// Run on the broker I/O worker; verifies the stored prefix and every appended
    /// suffix using the installed signer. Reattachment never Resumes a Park.
    /// # Errors
    /// Refuses invalid binding, receipt divergence, signature or durability failure.
    pub fn serve_reconnect(&self) -> Result<BrokerSession, BrokerError> {
        let mut verification_failure = None;
        let result = self
            .service
            .serve_reconnect(now_ms()?, |key, payload, signature| {
                match self.verifier.verify(key, payload, signature) {
                    Ok(()) => true,
                    Err(error) => {
                        verification_failure = Some(error);
                        false
                    }
                }
            });
        match verification_failure {
            Some(error) => Err(BrokerError::Verification(error)),
            None => result,
        }
    }

    /// Registers observed ACP metadata from the launch-authorized controller.
    /// Execute on the Session worker; transport completions remain asynchronous.
    /// The caller is the existing authenticated local controller boundary, not
    /// a deserialized Agent/helper role. This never Parks or Resumes a Session.
    /// # Errors
    /// Refuses wrong controller/binding, stale state, failed retention or persistence.
    pub fn register_recovery(
        &self,
        session: &mut BrokerSession,
        caller: &LifecycleCaller,
        request: &crate::launch_protocol::RecoveryRequest,
    ) -> Result<crate::launch_protocol::RetentionEvidence, BrokerError> {
        let mut verification_failure = None;
        let result = self.service.register_recovery(
            session,
            caller,
            request,
            now_ms()?,
            |key, payload, signature| match self.verifier.verify(key, payload, signature) {
                Ok(()) => true,
                Err(error) => {
                    verification_failure = Some(error);
                    false
                }
            },
        );
        match verification_failure {
            Some(error) => Err(BrokerError::Verification(error)),
            None => result,
        }
    }

    /// Checks the persisted Run requirement before the controller dispatches work.
    /// Does not grant capabilities or implicitly Resume a Parked Session.
    /// # Errors
    /// Refuses required recovery without current broker-owned evidence.
    pub fn admit_recovery(&self, session_id: &str) -> Result<(), BrokerError> {
        self.service.admit_recovery(session_id, now_ms()?)
    }

    /// Returns broker-owned recovery readiness for the canonical status consumer.
    /// # Errors
    /// Refuses unknown Sessions or unreadable/invalid durable evidence.
    pub fn recovery_readiness(
        &self,
        session_id: &str,
    ) -> Result<crate::launch_protocol::RecoveryReadiness, BrokerError> {
        self.service.recovery_readiness(session_id, now_ms()?)
    }
    /// Delivers one pending normalized Attention entry independently of Neovim.
    /// Run on the broker's delivery worker. Failure retains the durable entry
    /// and never changes Session authorization or launcher receipts.
    ///
    /// # Errors
    /// Returns projection transport/authentication failure or unavailable outbox state.
    pub fn deliver_attention(
        &self,
        endpoint: Option<&super::attention::AttentionEndpoint>,
    ) -> Result<bool, BrokerError> {
        let reconciled = self.reconcile_admissions(endpoint);
        // A Run observation outage must not suppress already-durable projections.
        let delivered = self
            .service
            .attention
            .deliver_next(endpoint.ok_or(BrokerError::InvalidGrant)?)?;
        reconciled?;
        Ok(delivered)
    }

    /// Reads or settles an exact request for the authenticated operator.
    /// # Errors
    /// Refuses foreign scope, conflicting terminal decisions or unavailable storage.
    pub fn skill_request_control(
        &self,
        operator_uid: u32,
        operation_id: &str,
        outcome: Option<crate::skill_request::SkillRequestOutcome>,
    ) -> Result<crate::skill_request::SkillRequestStatus, BrokerError> {
        self.service
            .skill_request_control(operator_uid, operation_id, outcome)
    }

    /// Inspects or settles one Beads operation for its authenticated operator.
    /// The original mutation outcome and spent budget never change.
    /// # Errors
    /// Refuses foreign scope, conflicting decisions or unavailable durable state.
    pub fn beads_mutation_control(
        &self,
        operator_uid: u32,
        operation_id: &str,
        decision: Option<&crate::beads_mutation::BeadsControlDecision>,
    ) -> Result<crate::beads_mutation::BeadsInspection, BrokerError> {
        self.service
            .beads_mutation_control(operator_uid, operation_id, decision)
    }

    fn reconcile_admissions(
        &self,
        endpoint: Option<&super::attention::AttentionEndpoint>,
    ) -> Result<(), BrokerError> {
        let source =
            super::admission_source::AdmissionSource::installed().map_err(BrokerError::Storage)?;
        if source.as_ref().is_some_and(|source| {
            source.operator_uid != self.verifier.config().operator_uid
                || source.broker_uid != self.verifier.config().broker_uid
        }) {
            return Err(BrokerError::ControllerMismatch);
        }
        self.service.reconcile_skill_admissions(
            endpoint,
            source.as_ref(),
            |key, payload, signature| self.verifier.verify(key, payload, signature).is_ok(),
        )
    }

    /// Applies emergency quarantine with installed receipt verification.
    /// Broker approval revocation precedes the supervisor Park request.
    ///
    /// # Errors
    /// Refuses unauthorized requests, failed revocation, invalid signatures or unavailable storage/transport.
    pub fn quarantine(
        &self,
        session: &mut BrokerSession,
        caller: &LifecycleCaller,
        request: &LifecycleRequest,
    ) -> Result<SignedReceipt, BrokerError> {
        let mut verification_failure = None;
        let result = self.service.quarantine(
            session,
            caller,
            request,
            now_ms()?,
            |key, payload, signature| match self.verifier.verify(key, payload, signature) {
                Ok(()) => true,
                Err(error) => {
                    verification_failure = Some(error);
                    false
                }
            },
        );
        match verification_failure {
            Some(error) => Err(BrokerError::Verification(error)),
            None => result,
        }
    }
    /// Runs one authorized lifecycle operation using the installed signature verifier.
    /// Caller identity and coordinator scope must come from the trusted control boundary.
    /// This blocks the owning Session worker while asynchronous transport completes.
    ///
    /// # Errors
    /// Returns typed policy/CAS, installed-verification, storage or transport failure.
    pub fn request_lifecycle(
        &self,
        session: &mut BrokerSession,
        caller: &LifecycleCaller,
        request: &LifecycleRequest,
    ) -> Result<SignedReceipt, BrokerError> {
        let mut verification_failure = None;
        let result = self.service.request_lifecycle(
            session,
            caller,
            request,
            now_ms()?,
            |key, payload, signature| match self.verifier.verify(key, payload, signature) {
                Ok(()) => true,
                Err(error) => {
                    verification_failure = Some(error);
                    false
                }
            },
        );
        match verification_failure {
            Some(error) => Err(BrokerError::Verification(error)),
            None => result,
        }
    }
    /// Opens installed public authority and binds its exact local rendezvous.
    /// Provision the state/rendezvous directories under the installed broker
    /// identity first. An existing socket is never replaced.
    ///
    /// # Errors
    /// Refuses wrong identity, writable authority, insecure directories, changed
    /// release/tool bytes, occupied rendezvous or unavailable durable storage.
    pub fn bind(paths: &LauncherPaths, state: &Path) -> Result<Self, BrokerError> {
        Self::open(paths, state, None)
    }

    /// Opens installed authority over a service-manager-owned listener.
    /// The listener must name the configured rendezvous. Identity and directory
    /// checks are identical to [`Self::bind`]; this never binds or replaces a path.
    ///
    /// # Errors
    /// Refuses wrong identity, untrusted installation, mismatched rendezvous or
    /// unavailable durable state.
    pub fn over(
        paths: &LauncherPaths,
        state: &Path,
        listener: SeqpacketListener,
    ) -> Result<Self, BrokerError> {
        Self::open(paths, state, Some(listener))
    }

    fn open(
        paths: &LauncherPaths,
        state: &Path,
        listener: Option<SeqpacketListener>,
    ) -> Result<Self, BrokerError> {
        let verifier = Arc::new(
            LauncherVerifier::open(paths, state).map_err(BrokerError::InstallationAuthority)?,
        );
        let config = verifier.config();
        installed_identity(config)?;
        let (uid, gid) = (config.broker_uid, config.broker_gid);
        private_directory(state, uid, gid)?;
        let state_lock = super::state_identity::exclusive(state)?;
        private_directory(
            config
                .broker_socket_path
                .parent()
                .ok_or(BrokerError::Installation)?,
            uid,
            gid,
        )?;
        if listener
            .as_ref()
            .is_some_and(|listener| listener.path() != Some(config.broker_socket_path.as_path()))
        {
            return Err(BrokerError::Installation);
        }
        super::state_identity::check(state, uid, gid)?;
        let provider_credentials =
            super::provider_credentials::ProviderCredentialStore::installed(state, uid, gid)?;
        let listener = match listener {
            Some(listener) => listener,
            None => SeqpacketListener::bind(&config.broker_socket_path)
                .map_err(BrokerError::Transport)?,
        };
        let mut service = BrokerService::over(
            listener,
            &config.broker_socket_path,
            AuthorizationStore::open(&state.join("authorizations"), config.pool.clone())?,
            ReceiptStore::installed(&state.join("receipts"), Arc::clone(&verifier))?,
            AuditLog::open(&state.join("audit"))?,
            CredentialPin::Identity { uid: 0, gid: 0 },
        )?;
        beads::configure(&mut service, config)?;
        service.enable_beads_replicas(
            Path::new(crate::launch_supervisor::SYSTEM_SESSIONS_ROOT),
            config.broker_uid,
            config.broker_gid,
        )?;
        Ok(Self {
            provider_credentials,
            _state_lock: state_lock,
            service,
            verifier,
        })
    }

    /// Persists exact authority already approved by the trusted operator/controller.
    /// This Rust API is not an Agent-facing approval endpoint.
    ///
    /// # Errors
    /// Refuses a foreign controller, malformed/expired approval, exhausted pool or storage failure.
    pub fn authorize(&self, grant: &GrantRequest) -> Result<PendingAuthorization, BrokerError> {
        if grant.controller_uid != self.verifier.config().operator_uid {
            return Err(BrokerError::ControllerMismatch);
        }
        if let Some(permission) = &grant.beads_mutations {
            let tracker = self
                .service
                .tracker
                .as_ref()
                .ok_or(BrokerError::InvalidGrant)?;
            if permission.project_digest != tracker.project_digest() {
                return Err(BrokerError::InvalidGrant);
            }
        }
        self.service.authorizations().authorize(grant, now_ms()?)
    }

    /// Consumes one pending launch and verifies both exact receipts with installed keys.
    ///
    /// # Errors
    /// Returns authentication, policy, signature, durability or transport failure.
    pub fn serve_launch(&self) -> Result<BrokerSession, BrokerError> {
        let mut verification_failure = None;
        let result = self
            .service
            .serve_launch(now_ms()?, |key, payload, signature| {
                match self.verifier.verify(key, payload, signature) {
                    Ok(()) => true,
                    Err(error) => {
                        verification_failure = Some(error);
                        false
                    }
                }
            });
        match verification_failure {
            Some(error) => Err(BrokerError::Verification(error)),
            None => result,
        }
    }

    /// Accepts one supervisor and routes it by the connection's first packet.
    ///
    /// A running broker serves new launches and post-restart reattachments on
    /// one rendezvous and cannot know which is arriving, so the first packet
    /// decides. This is the entry point a long-running broker loops on.
    ///
    /// # Errors
    /// Returns the same failures as [`Self::serve_launch`] and
    /// [`Self::serve_reconnect`], according to which the peer asked for.
    pub fn serve_connection(&self) -> Result<BrokerSession, BrokerError> {
        let mut verification_failure = None;
        let result = self
            .service
            .serve_connection(now_ms()?, |key, payload, signature| {
                match self.verifier.verify(key, payload, signature) {
                    Ok(()) => true,
                    Err(error) => {
                        verification_failure = Some(error);
                        false
                    }
                }
            });
        match verification_failure {
            Some(error) => Err(BrokerError::Verification(error)),
            None => result,
        }
    }

    /// Waits for one authenticated supervisor without starting its handshake.
    /// The daemon gives each accepted connection its own worker.
    /// # Errors
    /// Returns listener, peer authentication or transport setup failure.
    pub fn accept_connection(&self) -> Result<SeqpacketChannel, BrokerError> {
        self.service.accept_connection()
    }

    /// Runs an accepted connection's launch/reconnect using installed verification.
    /// Blocks only this connection's worker; failures close its channel.
    /// # Errors
    /// Returns authentication, policy, signature, durability or transport failure.
    pub fn serve_accepted(&self, channel: SeqpacketChannel) -> Result<BrokerSession, BrokerError> {
        let mut verification_failure = None;
        let result = self
            .service
            .serve_accepted(channel, now_ms()?, |key, payload, signature| {
                match self.verifier.verify(key, payload, signature) {
                    Ok(()) => true,
                    Err(error) => {
                        verification_failure = Some(error);
                        false
                    }
                }
            });
        match verification_failure {
            Some(error) => Err(BrokerError::Verification(error)),
            None => result,
        }
    }

    /// Processes a command, signed outcome or durable controller-loss settlement.
    /// Returns true only after a terminal receipt became durable. Local decision
    /// and Attention enqueue precede settlement; remote delivery grants no authority.
    /// Waiting for the next packet is idle time, without an operation deadline;
    /// closing the Session channel interrupts that wait.
    ///
    /// # Errors
    /// Closes the connection on protocol, verification, audit or transport failure;
    /// a closed channel never stands for confirmed process cleanup.
    pub fn step(&self, session: &mut BrokerSession) -> Result<bool, BrokerError> {
        // Missing/untrusted configuration disables Run requests, not Session control.
        let endpoint = super::attention::AttentionEndpoint::installed();
        let mut verification_failure = None;
        let result = self.service.step(
            session,
            now_ms()?,
            endpoint.as_ref().ok(),
            |key, payload, signature| match self.verifier.verify(key, payload, signature) {
                Ok(()) => true,
                Err(error) => {
                    verification_failure = Some(error);
                    false
                }
            },
        );
        match verification_failure {
            Some(error) => Err(BrokerError::Verification(error)),
            None => result,
        }
    }

    /// Reads durable initial launch evidence, without claiming current liveness.
    ///
    /// # Errors
    /// Returns a typed receipt-chain refusal for quarantined history, invalid
    /// shared authority, or unavailable durable state. Failed Session history
    /// is preserved and quarantined without disabling unrelated Sessions.
    pub fn inspect(&self, session_id: &str) -> Result<Option<SessionInspection>, BrokerError> {
        self.service.inspect(session_id)
    }

    /// Read exact retained conformance evidence for an authenticated operator.
    /// No live supervisor is required; historical evidence grants no authority.
    /// # Errors
    /// Refuses unverified, quarantined, corrupt or unavailable history.
    pub fn inspect_conformance(
        &self,
        session_id: &str,
    ) -> Result<Option<super::conformance_inspection::ConformanceInspection>, BrokerError> {
        self.service.inspect_conformance(session_id)
    }

    /// Inspects durable evidence together with this worker's acknowledged channel.
    ///
    /// # Errors
    /// Refuses foreign Sessions or unavailable durable evidence.
    pub fn inspect_active(
        &self,
        session: &BrokerSession,
    ) -> Result<SessionInspection, BrokerError> {
        self.service.inspect_active(session)
    }

    /// Serves one read-only self-status request from the Agent capability channel.
    ///
    /// Enforces self-scope and answers with no lifecycle actions, because an
    /// Agent channel has none. Reading grants nothing.
    ///
    /// # Errors
    /// Returns transport/verification failure, or the typed refusal sent for a
    /// foreign subject.
    pub fn serve_agent_status(
        &self,
        session: &mut BrokerSession,
    ) -> Result<crate::launch_protocol::SessionStatus, BrokerError> {
        let mut verification_failure = None;
        let result = self.service.serve_agent_status(
            session,
            now_ms()?,
            |key, payload, signature| match self.verifier.verify(key, payload, signature) {
                Ok(()) => true,
                Err(error) => {
                    verification_failure = Some(error);
                    false
                }
            },
        );
        match verification_failure {
            Some(error) => Err(BrokerError::Verification(error)),
            None => result,
        }
    }

    /// Reads canonical Session status with installed receipt verification.
    ///
    /// The single status answer both the operator surface and the scoped Agent
    /// read, so neither can observe a different Session than the other. Caller
    /// identity comes from the trusted control boundary, never a wire role.
    /// Run on the Session's broker worker; this authorizes nothing.
    ///
    /// # Errors
    /// Returns supervisor transport/verification failure, unreadable quarantine
    /// state, or a composition the canonical status schema rejects.
    pub fn session_status(
        &self,
        session: &mut BrokerSession,
        caller: &LifecycleCaller,
    ) -> Result<crate::launch_protocol::SessionStatus, BrokerError> {
        let mut verification_failure = None;
        let result =
            self.service
                .session_status(
                    session,
                    caller,
                    now_ms()?,
                    |key, payload, signature| match self.verifier.verify(key, payload, signature) {
                        Ok(()) => true,
                        Err(error) => {
                            verification_failure = Some(error);
                            false
                        }
                    },
                );
        match verification_failure {
            Some(error) => Err(BrokerError::Verification(error)),
            None => result,
        }
    }
}

fn installed_identity(config: &LauncherConfig) -> Result<(), BrokerError> {
    let uid = rustix::process::geteuid().as_raw();
    let gid = rustix::process::getegid().as_raw();
    if uid == 0
        || uid != config.broker_uid
        || gid != config.broker_gid
        || rustix::process::getuid().as_raw() != uid
        || rustix::process::getgid().as_raw() != gid
        // systemd repeats the primary GID; distinct supplementary groups add authority.
        || rustix::process::getgroups()
            .map_err(|_| BrokerError::Installation)?
            .iter().any(|group| group.as_raw() != gid)
    {
        return Err(BrokerError::Installation);
    }
    Ok(())
}

fn private_directory(path: &Path, uid: u32, gid: u32) -> Result<(), BrokerError> {
    if !path.is_absolute() {
        return Err(BrokerError::Installation);
    }
    for (depth, parent) in path.ancestors().enumerate() {
        let metadata = fs::symlink_metadata(parent).map_err(BrokerError::Storage)?;
        let trusted = if depth == 0 {
            metadata.uid() == uid && metadata.gid() == gid && metadata.mode() & 0o777 == 0o700
        } else {
            metadata.uid() == 0 && metadata.mode() & 0o022 == 0
        };
        if !metadata.is_dir() || !trusted {
            return Err(BrokerError::Installation);
        }
    }
    Ok(())
}
