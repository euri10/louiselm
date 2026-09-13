//! Durable single-use launch authorizations.
//!
//! An authorization is written before a privileged launch runs and consumed
//! exactly once when the supervisor presents its exact request. Consumption is
//! a compare-and-swap against the filesystem, so restart, replay, and a
//! concurrent second consumer all fail rather than producing a second launch.

use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

use crate::{
    Digest,
    broker::{
        BrokerError, CONSUMED_DIRECTORY, PENDING_DIRECTORY, lock, read_record, record_name,
        sync_directory, write_new_record,
    },
    launch::{LaunchRequest, PROTOCOL_VERSION},
    launch_protocol::{
        IdentityExhaustion, LAUNCH_AUTHORIZATION_SCHEMA, LaunchAuthorization,
        OccupiedSessionIdentity,
    },
    launch_receipt::SessionState,
    launcher_install::{Identity, IdentityPool},
};

/// Exact operator-approved command scope persisted before launch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovedCommands {
    /// Canonical digest of the exact shell input, never the input itself.
    pub command_digest: String,
    /// Maximum execution duration, bounded by the command protocol.
    pub timeout_ms: u32,
    /// Optional non-refundable invocation limit (1..=64); omission is uncapped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uses: Option<u32>,
    /// Whether the existing operator approval explicitly permits delegation.
    pub allow_delegation: bool,
    /// Exclusive absolute expiry; reconstruction never renews this authority.
    pub expires_at_ms: u64,
}

impl ApprovedCommands {
    pub(super) fn policy(
        &self,
        authorization_id: &str,
        now_ms: u64,
    ) -> Result<super::delegation::DelegationPolicy, BrokerError> {
        let digest = Digest::parse(&self.command_digest).map_err(|_| BrokerError::InvalidGrant)?;
        let scope = super::delegation::CommandScope {
            command_digest: digest.clone(),
            timeout_ms: self.timeout_ms,
            uses: self.uses,
        };
        if digest.to_string() != self.command_digest || !scope.valid() {
            return Err(BrokerError::InvalidGrant);
        }
        let remaining = self
            .expires_at_ms
            .checked_sub(now_ms)
            .filter(|ms| *ms > 0)
            .ok_or(BrokerError::Expired)?;
        let expires_at = Instant::now()
            .checked_add(Duration::from_millis(remaining))
            .ok_or(BrokerError::InvalidGrant)?;
        Ok(super::delegation::DelegationPolicy {
            authorization_id: authorization_id.to_owned(),
            scope,
            allow_delegation: self.allow_delegation,
            expires_at,
        })
    }
}

/// A launch the operator's controller has authorized but not yet started.
#[derive(Clone, Debug)]
pub struct GrantRequest {
    /// Trusted Run policy: no governed work until durable cold recovery is ready.
    pub require_cold_recovery: bool,
    /// The exact canonical request the controller will hand the launcher.
    pub request: LaunchRequest,
    /// Unprivileged controller identity permitted to spend this authorization.
    pub controller_uid: u32,
    /// Exclusive millisecond expiry; `now >= expires_at_ms` is expired.
    pub expires_at_ms: u64,
    /// Signed fail-closed interval allowed for authenticated broker reattachment.
    pub broker_loss_grace_ms: u32,
    /// Explicit approved effects; absence grants no command authority.
    pub commands: Option<ApprovedCommands>,
}

/// One durable single-use authorization awaiting its supervisor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingAuthorization {
    /// Exact recovery requirement fixed by the trusted Run controller.
    pub require_cold_recovery: bool,
    /// Single-use authorization identity, also its durable record name.
    pub authorization_id: String,
    /// Idempotency identity of the authorized request.
    pub request_id: String,
    /// Digest of the exact canonical launch request this authorization binds.
    pub request_digest: String,
    /// Controller identity permitted to spend the authorization.
    pub controller_uid: u32,
    /// Authorized Session.
    pub session_id: String,
    /// Authorized Run.
    pub run_id: String,
    /// Authorized capability-envelope revision.
    pub envelope_revision: u64,
    /// Host identity leased from the installed pool for this Session.
    pub identity: Identity,
    /// Exclusive millisecond expiry.
    pub expires_at_ms: u64,
    /// Fail-closed interval allowed for authenticated broker reattachment.
    pub broker_loss_grace_ms: u32,
    /// Exact effect approval bound to this single-use launch.
    pub commands: Option<ApprovedCommands>,
}

/// Durable evidence that one authorization was spent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConsumedAuthorization {
    /// The record as it stood when it was consumed.
    authorization: PendingAuthorization,
    /// Broker clock reading at consumption.
    consumed_at_ms: u64,
}

impl PendingAuthorization {
    pub(super) fn launch_authorization(&self) -> LaunchAuthorization {
        LaunchAuthorization {
            schema: LAUNCH_AUTHORIZATION_SCHEMA.to_owned(),
            protocol_version: PROTOCOL_VERSION,
            authorization_id: self.authorization_id.clone(),
            request_id: self.request_id.clone(),
            request_digest: self.request_digest.clone(),
            controller_uid: self.controller_uid,
            session_id: self.session_id.clone(),
            run_id: self.run_id.clone(),
            envelope_revision: self.envelope_revision,
            identity_slot: self.identity.slot,
            assigned_uid: self.identity.uid,
            assigned_gid: self.identity.gid,
            expires_at_ms: self.expires_at_ms,
            broker_loss_grace_ms: self.broker_loss_grace_ms,
        }
    }
}

/// The broker's durable single-use authorization store.
///
/// Consumption is a compare-and-swap against the filesystem: the caller that
/// removes the pending record wins, and only that caller receives an
/// authorization. Restart re-reads the same directories, so a consumed
/// authorization stays consumed.
#[derive(Debug)]
pub struct AuthorizationStore {
    pub(super) root: PathBuf,
    pool: IdentityPool,
    /// Serializes slot assignment so two grants cannot pick the same slot.
    assignment: Mutex<()>,
}

impl AuthorizationStore {
    /// Opens or creates the durable store under `root`.
    ///
    /// # Errors
    /// Returns [`BrokerError::InvalidPool`] for an unusable installed pool and
    /// [`BrokerError::Storage`] when the state directories cannot be created.
    pub fn open(root: &Path, pool: IdentityPool) -> Result<Self, BrokerError> {
        pool.identity(0).map_err(|_| BrokerError::InvalidPool)?;
        for directory in [PENDING_DIRECTORY, CONSUMED_DIRECTORY] {
            fs::create_dir_all(root.join(directory)).map_err(BrokerError::Storage)?;
        }
        sync_directory(root)?;
        Ok(Self {
            root: root.to_owned(),
            pool,
            assignment: Mutex::new(()),
        })
    }

    /// Durably records one short-lived single-use authorization.
    ///
    /// The record binds the exact canonical request digest, the controller, the
    /// Session, the Run, the envelope revision, the authorization identity, and
    /// one slot from the installed pool. It is durable before this returns.
    ///
    /// # Errors
    /// Returns [`BrokerError::InvalidGrant`] for a malformed request or expiry,
    /// [`BrokerError::DuplicateAuthorization`] when the identity is already in
    /// use, [`BrokerError::IdentityExhausted`] when no slot is free, and
    /// [`BrokerError::Storage`] when the record cannot be made durable.
    pub fn authorize(
        &self,
        grant: &GrantRequest,
        now_ms: u64,
    ) -> Result<PendingAuthorization, BrokerError> {
        grant
            .request
            .validate()
            .map_err(|_| BrokerError::InvalidGrant)?;
        if grant.expires_at_ms <= now_ms || grant.controller_uid == 0 {
            return Err(BrokerError::InvalidGrant);
        }
        if let Some(commands) = &grant.commands {
            commands.policy(&grant.request.authorization_id, now_ms)?;
        }
        let record_name = record_name(&grant.request.authorization_id)?;

        let assignment = lock(&self.assignment);
        if self.pending_path(&record_name).exists() || self.consumed_path(&record_name).exists() {
            return Err(BrokerError::DuplicateAuthorization);
        }
        let identity = self.assign_identity(now_ms)?;
        let pending = PendingAuthorization {
            require_cold_recovery: grant.require_cold_recovery,
            authorization_id: grant.request.authorization_id.clone(),
            request_id: grant.request.request_id.clone(),
            request_digest: grant.request.digest().to_string(),
            controller_uid: grant.controller_uid,
            session_id: grant.request.session_id.clone(),
            run_id: grant.request.run_id.clone(),
            envelope_revision: grant.request.envelope_revision,
            identity,
            expires_at_ms: grant.expires_at_ms,
            broker_loss_grace_ms: grant.broker_loss_grace_ms,
            commands: grant.commands.clone(),
        };
        write_new_record(&self.pending_path(&record_name), &pending)?;
        drop(assignment);
        Ok(pending)
    }

    /// Atomically consumes the authorization bound to `request`.
    ///
    /// Exactly one caller can consume a given authorization: restart, replay,
    /// expiry, a mismatched request or controller, and a concurrent second
    /// consumer all fail here rather than producing a second launch.
    ///
    /// # Errors
    /// Returns [`BrokerError::UnknownAuthorization`], [`BrokerError::Expired`],
    /// [`BrokerError::RequestMismatch`], [`BrokerError::ControllerMismatch`],
    /// or [`BrokerError::Storage`] when consumption cannot be made durable.
    pub fn consume(
        &self,
        request: &LaunchRequest,
        controller_uid: u32,
        now_ms: u64,
    ) -> Result<LaunchAuthorization, BrokerError> {
        request.validate().map_err(|_| BrokerError::InvalidGrant)?;
        let record_name = record_name(&request.authorization_id)?;
        let path = self.pending_path(&record_name);
        let pending: PendingAuthorization = match read_record(&path) {
            Ok(Some(pending)) => pending,
            Ok(None) => return Err(BrokerError::UnknownAuthorization),
            Err(error) => return Err(error),
        };

        if pending.request_digest != request.digest().to_string()
            || pending.request_id != request.request_id
            || pending.session_id != request.session_id
            || pending.run_id != request.run_id
            || pending.envelope_revision != request.envelope_revision
        {
            return Err(BrokerError::RequestMismatch);
        }
        if pending.controller_uid != controller_uid {
            return Err(BrokerError::ControllerMismatch);
        }
        if now_ms >= pending.expires_at_ms {
            return Err(BrokerError::Expired);
        }

        // Removing the pending record is the single-winner compare-and-swap.
        // A caller that loses the race, or that arrives after a restart, finds
        // nothing to remove and never reaches the authorization below.
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(BrokerError::UnknownAuthorization);
            }
            Err(error) => return Err(BrokerError::Storage(error)),
        }
        sync_directory(&self.root.join(PENDING_DIRECTORY))?;
        write_new_record(
            &self.consumed_path(&record_name),
            &ConsumedAuthorization {
                authorization: pending.clone(),
                consumed_at_ms: now_ms,
            },
        )?;

        let authorization = pending.launch_authorization();
        authorization
            .validate()
            .map_err(|_| BrokerError::InvalidGrant)?;
        Ok(authorization)
    }

    /// Consumes on behalf of the trusted launcher, which cannot state the
    /// controller it serves on this closed schema.
    ///
    /// The controller binding is still enforced, just at the other end: the
    /// supervisor checks the returned record with
    /// [`LaunchAuthorization::validate_for`] against the controller that
    /// actually invoked it, and ends the launch when they differ. Delegating it
    /// this way can only narrow the launch, never widen it.
    ///
    /// # Errors
    /// Returns the same failures as [`Self::consume`].
    pub fn consume_for_launcher(
        &self,
        request: &LaunchRequest,
        now_ms: u64,
    ) -> Result<LaunchAuthorization, BrokerError> {
        let record_name = record_name(&request.authorization_id)?;
        let pending: PendingAuthorization = read_record(&self.pending_path(&record_name))?
            .ok_or(BrokerError::UnknownAuthorization)?;
        self.consume(request, pending.controller_uid, now_ms)
    }

    /// Returns the consumed authorization that launched `session_id`.
    ///
    /// # Errors
    /// Returns [`BrokerError::Storage`] when durable state cannot be read.
    pub fn consumed_for_session(
        &self,
        session_id: &str,
    ) -> Result<Option<PendingAuthorization>, BrokerError> {
        for entry in
            fs::read_dir(self.root.join(CONSUMED_DIRECTORY)).map_err(BrokerError::Storage)?
        {
            let entry = entry.map_err(BrokerError::Storage)?;
            let consumed: Option<ConsumedAuthorization> = read_record(&entry.path())?;
            if let Some(consumed) = consumed
                && consumed.authorization.session_id == session_id
            {
                return Ok(Some(consumed.authorization));
            }
        }
        Ok(None)
    }

    /// Assigns the lowest installed slot no live Session holds.
    ///
    /// Expired pending authorizations release their slot because no Session was
    /// ever created under them. A consumed authorization keeps its slot: its
    /// Session owns that identity until lifecycle reconciliation releases it.
    fn assign_identity(&self, now_ms: u64) -> Result<Identity, BrokerError> {
        let occupants = self.occupants(now_ms)?;
        let free = (0..self.pool.slots)
            .find(|slot| !occupants.iter().any(|occupant| occupant.slot == *slot));
        if let Some(slot) = free {
            return self
                .pool
                .identity(slot)
                .map_err(|_| BrokerError::InvalidPool);
        }
        let exhaustion =
            IdentityExhaustion::compose(occupants).map_err(|_| BrokerError::InvalidPool)?;
        Err(BrokerError::IdentityExhausted(Box::new(exhaustion)))
    }

    /// Collects the live occupancy of the installed pool from durable state.
    fn occupants(&self, now_ms: u64) -> Result<Vec<OccupiedSessionIdentity>, BrokerError> {
        let mut occupants = Vec::new();
        for (directory, live_only) in [(PENDING_DIRECTORY, true), (CONSUMED_DIRECTORY, false)] {
            for entry in fs::read_dir(self.root.join(directory)).map_err(BrokerError::Storage)? {
                let entry = entry.map_err(BrokerError::Storage)?;
                let held = if live_only {
                    read_record::<PendingAuthorization>(&entry.path())?
                } else {
                    read_record::<ConsumedAuthorization>(&entry.path())?
                        .map(|consumed| consumed.authorization)
                };
                let Some(held) = held else { continue };
                if live_only && now_ms >= held.expires_at_ms {
                    continue;
                }
                occupants.push(OccupiedSessionIdentity {
                    session_id: held.session_id,
                    state: SessionState::Starting,
                    slot: held.identity.slot,
                });
            }
        }
        Ok(occupants)
    }

    fn pending_path(&self, record_name: &str) -> PathBuf {
        self.root.join(PENDING_DIRECTORY).join(record_name)
    }

    fn consumed_path(&self, record_name: &str) -> PathBuf {
        self.root.join(CONSUMED_DIRECTORY).join(record_name)
    }
}
