//! Installed, unprivileged composition of launch policy, verification and transport.

use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use super::{
    AuditLog, AuthorizationStore, BrokerError, BrokerService, BrokerSession, GrantRequest,
    PendingAuthorization, ReceiptStore, SessionInspection, TrustedRelease,
    service::{receive, send},
};
use crate::{
    broker::lifecycle::LifecycleCaller,
    launch_protocol::LifecycleRequest,
    launch_receipt::SessionState,
    launch_receipt::SignedReceipt,
    launch_transport::{CredentialPin, LauncherPacket},
    launcher_install::{LauncherPaths, LauncherVerifier},
};

/// One dedicated broker process's installed launch service.
///
/// Construction requires the installed non-root UID/GID with no supplementary
/// groups. Public configuration and verification keys stay root-owned; only
/// private broker state and its rendezvous directory belong to the broker.
/// All methods perform blocking I/O on the explicitly owned broker worker.
pub struct InstalledBroker {
    service: BrokerService,
    verifier: LauncherVerifier,
}

impl InstalledBroker {
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
    ) -> Result<super::recovery::RecoveryReadiness, BrokerError> {
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
        endpoint: &super::attention::AttentionEndpoint,
    ) -> Result<bool, BrokerError> {
        self.service.attention.deliver_next(endpoint)
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
        let verifier =
            LauncherVerifier::open(paths, state).map_err(BrokerError::InstallationAuthority)?;
        let config = verifier.config();
        let uid = rustix::process::geteuid().as_raw();
        let gid = rustix::process::getegid().as_raw();
        if uid == 0
            || uid != config.broker_uid
            || gid != config.broker_gid
            || rustix::process::getuid().as_raw() != uid
            || rustix::process::getgid().as_raw() != gid
            || !rustix::process::getgroups()
                .map_err(|_| BrokerError::Installation)?
                .is_empty()
        {
            return Err(BrokerError::Installation);
        }
        private_directory(state, uid, gid)?;
        private_directory(
            config
                .broker_socket_path
                .parent()
                .ok_or(BrokerError::Installation)?,
            uid,
            gid,
        )?;
        let service = BrokerService::bind(
            &config.broker_socket_path,
            AuthorizationStore::open(&state.join("authorizations"), config.pool.clone())?,
            ReceiptStore::open(
                &state.join("receipts"),
                TrustedRelease {
                    release_id: config.release_id.clone(),
                    signing_key_id: verifier.active_key_id().to_owned(),
                },
            )?,
            AuditLog::open(&state.join("audit"))?,
            CredentialPin::Identity { uid: 0, gid: 0 },
        )?;
        Ok(Self { service, verifier })
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

    /// Processes one command or signed outcome on the retained supervisor channel.
    /// Returns true only after a terminal receipt became durable. This initial
    /// worker has no recovery/reconnect or controller-loss settlement authority.
    ///
    /// # Errors
    /// Closes the connection on protocol, verification, audit or transport failure;
    /// a closed channel never stands for confirmed process cleanup.
    pub fn step(&self, session: &mut BrokerSession) -> Result<bool, BrokerError> {
        let result = (|| {
            let packet = receive(session.channel())?;
            if let LauncherPacket::SignedReceipt(ref receipt) = packet.packet {
                self.service.lifecycle.check_receipt(receipt)?;
                let mut verification_failure = None;
                let stored = self.service.receipts().append(
                    session.authorization(),
                    &packet.bytes,
                    |key, payload, signature| match self.verifier.verify(key, payload, signature) {
                        Ok(()) => true,
                        Err(error) => {
                            verification_failure = Some(error);
                            false
                        }
                    },
                );
                if let Some(error) = verification_failure {
                    return Err(BrokerError::Verification(error));
                }
                let ack = stored?;
                send(session.channel(), ack.canonical_bytes())?;
                return Ok(receipt.payload.resulting_state == SessionState::Terminal);
            }
            session.handle_command(packet)?;
            Ok(false)
        })();
        if result.is_err() {
            session.close();
        }
        result
    }

    /// Reads durable initial launch evidence, without claiming current liveness.
    ///
    /// # Errors
    /// Returns unreadable or corrupt broker state.
    pub fn inspect(&self, session_id: &str) -> Result<Option<SessionInspection>, BrokerError> {
        self.service.inspect(session_id)
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
}

fn now_ms() -> Result<u64, BrokerError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .ok_or(BrokerError::InvalidGrant)
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
