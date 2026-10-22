//! Explicit abort-only recovery of consumed launches and orphaned receipt chains.
//! Arming fences the assigned slot and records its actual kernel boot. Only a
//! subsequent boot permits cleanup proof. Exact outcomes survive lost ACKs;
//! retry never touches an identity after its authenticated recovery is durable.

mod storage;
use storage::seal_storage;

use super::{
    LauncherConfig, LauncherError, LauncherPaths, LauncherSigner, LauncherVerifier, identity,
    io_error, read_required_json, require_root_metadata, sync_dir, write_json_atomic,
};
use crate::{
    Digest,
    broker::{
        consumed_recovery::{Binding, Snapshot},
        operator::{
            InspectError,
            consumed_recovery::{self as protocol, Request, Response},
        },
    },
    launch_recovery::{Acknowledgement, Payload, Proof, SCHEMA, SIGNED_SCHEMA, valid_boot_id},
};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use thiserror::Error;

/// Stable recovery refusal, preserving installation and transport causes internally.
#[derive(Debug, Error)]
pub enum RecoveryError {
    /// No trustworthy arm record exists for this consumption.
    #[error("arm this exact consumed authorization before rebooting")]
    NotArmed,
    /// The old launcher could still execute in the current kernel boot.
    #[error("reboot required after arming; the reservation remains held")]
    RebootRequired,
    /// Existing bytes or scope disagree; no state may be reconstructed.
    #[error("recovery binding conflicts; inspect preserved evidence")]
    Conflict,
    /// A real current owner retains this operation or identity.
    #[error("recovery owner or identity is busy; reservation retained")]
    Busy,
    /// Protected installation, filesystem, signing or cleanup failed.
    #[error("recovery installation or durability unavailable: {0}")]
    Installation(#[from] LauncherError),
    /// No exact authenticated broker response was obtained.
    #[error("recovery broker exchange failed: {0:?}")]
    Broker(InspectError),
}

/// Exact immutable operator intent; no cleanup is claimed by this document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Arm {
    /// Closed root-owned record schema.
    pub schema: String,
    /// Original consumption, operator, Run and identity scope.
    pub binding: Binding,
    /// Actual kernel boot when the identity was fenced.
    pub armed_boot_id: String,
    /// Any original root registration, retained exactly rather than repaired.
    pub receipt_binding_digest: Option<String>,
}

impl Arm {
    /// Digest of the exact canonical arm document used by the signed outcome.
    /// # Errors
    /// Returns malformed-record failure if serialization fails.
    pub fn digest(&self) -> Result<String, RecoveryError> {
        Ok(Digest::of(&serde_json::to_vec(self).map_err(|_| RecoveryError::Conflict)?).to_string())
    }
}

struct Operation {
    directory: PathBuf,
    lock: File,
}
impl Drop for Operation {
    fn drop(&mut self) {
        // No helper inherits this lock. Failure cannot grant cleanup authority;
        // closing our only descriptor releases it after this operation ends.
        let _ = self.lock.unlock();
    }
}

fn context(
    paths: &LauncherPaths,
    operator: u32,
    id: &str,
) -> Result<(LauncherConfig, Operation), RecoveryError> {
    if !rustix::process::geteuid().is_root() {
        return Err(RecoveryError::Conflict);
    }
    crate::broker::operator::validate_subject(id).map_err(|_| RecoveryError::Conflict)?;
    super::require_system_install_context(paths)?;
    let config =
        super::runtime_config_with_deadline(paths, Instant::now() + Duration::from_secs(5))?;
    if operator == 0
        || config.operator_uid != operator
        || super::public_runtime_config(paths)? != config
    {
        return Err(RecoveryError::Conflict);
    }
    let parent = paths.state_root.join("consumed-recovery");
    directory(&parent)?;
    sync_dir(&paths.state_root)?;
    let directory_path = parent.join(Digest::of(id.as_bytes()).hex());
    directory(&directory_path)?;
    sync_dir(&parent)?;
    let lock_path = directory_path.join("operation.lock");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(
            rustix::fs::OFlags::NOFOLLOW.bits().cast_signed()
                | rustix::fs::OFlags::NONBLOCK.bits().cast_signed(),
        )
        .open(&lock_path)
        .map_err(|error| io_error("recovery operation lock", error))?;
    require_root_metadata(&lock_path, 0o600, false, "recovery operation lock")?;
    lock.try_lock().map_err(|_| RecoveryError::Busy)?;
    Ok((
        config,
        Operation {
            directory: directory_path,
            lock,
        },
    ))
}

fn directory(path: &Path) -> Result<(), RecoveryError> {
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(io_error("recovery directory", error).into()),
    }
    require_root_metadata(path, 0o700, true, "recovery directory")?;
    Ok(())
}

fn read<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Option<T>, RecoveryError> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error("recovery record", error).into()),
        Ok(_) => {}
    }
    require_root_metadata(path, 0o600, false, "recovery record")?;
    let value = read_required_json(path)?;
    // A previous rename may have reached disk before its fsync failed.
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|error| io_error("recovery record durability", error))?;
    sync_dir(path.parent().ok_or(RecoveryError::Conflict)?)?;
    Ok(Some(value))
}

fn boot_id() -> Result<String, RecoveryError> {
    let text = fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .map_err(|error| io_error("kernel boot identity", error))?;
    let boot = text.trim_end_matches('\n');
    if !valid_boot_id(boot) {
        return Err(RecoveryError::Conflict);
    }
    Ok(boot.into())
}

fn acquire(paths: &LauncherPaths, slot: u32) -> Result<super::IdentityLease, RecoveryError> {
    identity::acquire_for_recovery(paths, slot).map_err(|error| match error {
        LauncherError::Occupied { .. } => RecoveryError::Busy,
        other => RecoveryError::Installation(other),
    })
}

fn snapshot(config: &LauncherConfig, endpoint: &Path, id: &str) -> Result<Snapshot, RecoveryError> {
    let response = protocol::exchange(
        endpoint,
        config.broker_uid,
        &Request::Prepare {
            authorization_id: id.into(),
        },
    )
    .map_err(RecoveryError::Broker)?;
    let Response::Snapshot { snapshot } = response else {
        return Err(RecoveryError::Conflict);
    };
    let authorization = &snapshot.binding.authorization;
    snapshot
        .binding
        .validate()
        .map_err(|_| RecoveryError::Conflict)?;
    let identity = config.pool.identity(authorization.identity_slot)?;
    if authorization.authorization_id != id
        || authorization.controller_uid != config.operator_uid
        || identity.uid != authorization.assigned_uid
        || identity.gid != authorization.assigned_gid
    {
        return Err(RecoveryError::Conflict);
    }
    Ok(*snapshot)
}

/// Fence one exact consumed launch and durably arm recovery in the current boot.
/// Never releases an identity or claims cleanup. Invoke only as the installed
/// operator through the fixed root entrypoint. Blocking, bounded helper I/O.
/// # Errors
/// Refuses foreign authority, live locks, conflicting state or unavailable durability.
pub fn arm(paths: &LauncherPaths, operator: u32, id: &str) -> Result<Arm, RecoveryError> {
    arm_at(paths, operator, id, Path::new(protocol::SOCKET))
}

pub(crate) fn arm_at(
    paths: &LauncherPaths,
    operator: u32,
    id: &str,
    endpoint: &Path,
) -> Result<Arm, RecoveryError> {
    let (config, operation) = context(paths, operator, id)?;
    let snapshot = snapshot(&config, endpoint, id)?;
    let receipt_binding_digest =
        super::history::recovery_binding(paths, &snapshot.binding.authorization)?
            .map(|(digest, _)| digest);
    let path = operation.directory.join("arm.json");
    if let Some(existing) = read::<Arm>(&path)? {
        if existing.schema != "louiselm.consumed-recovery.arm/1"
            || existing.binding != snapshot.binding
            || !valid_boot_id(&existing.armed_boot_id)
            || existing.receipt_binding_digest != receipt_binding_digest
        {
            return Err(RecoveryError::Conflict);
        }
        return Ok(existing);
    }
    if snapshot.recovered.is_some() {
        return Err(RecoveryError::Conflict);
    }
    let armed_boot_id = boot_id()?;
    let lease = acquire(paths, snapshot.binding.authorization.identity_slot)?;
    // Acquisition fsyncs the poison marker while excluding every live owner.
    // Never undo this fence, including when writing the intent fails.
    lease.poison()?;
    let record = Arm {
        schema: "louiselm.consumed-recovery.arm/1".into(),
        binding: snapshot.binding,
        armed_boot_id,
        receipt_binding_digest,
    };
    write_json_atomic(&path, &record, 0o600)?;
    Ok(record)
}

/// Complete an explicitly armed recovery only after the arming kernel ended.
/// Retries submit exact saved proof without repeating identity mechanics.
/// # Errors
/// Refuses same-boot, unarmed, live, conflicting or uncertain state. Lost replies
/// preserve the same independent signed proof for retry; no receipt is invented.
pub fn complete(paths: &LauncherPaths, operator: u32, id: &str) -> Result<Proof, RecoveryError> {
    complete_at(
        paths,
        operator,
        id,
        Path::new(protocol::SOCKET),
        Path::new(crate::launch_supervisor::SYSTEM_SESSIONS_ROOT),
    )
}

pub(crate) fn complete_at(
    paths: &LauncherPaths,
    operator: u32,
    id: &str,
    endpoint: &Path,
    sessions_root: &Path,
) -> Result<Proof, RecoveryError> {
    let (config, operation) = context(paths, operator, id)?;
    let current = snapshot(&config, endpoint, id)?;
    let arm: Arm = read(&operation.directory.join("arm.json"))?.ok_or(RecoveryError::NotArmed)?;
    if arm.schema != "louiselm.consumed-recovery.arm/1"
        || arm.binding != current.binding
        || !valid_boot_id(&arm.armed_boot_id)
        || super::history::recovery_binding(paths, &arm.binding.authorization)?
            .map(|(digest, _)| digest)
            != arm.receipt_binding_digest
    {
        return Err(RecoveryError::Conflict);
    }
    let outcome_path = operation.directory.join("outcome.json");
    let proof = if let Some(saved) = read::<Proof>(&outcome_path)? {
        saved
    } else {
        if current.recovered.is_some() {
            return Err(RecoveryError::Conflict);
        }
        let signer = LauncherSigner::open(paths)?;
        let payload_path = operation.directory.join("payload.json");
        let payload = if let Some(saved) = read::<Payload>(&payload_path)? {
            saved
        } else {
            let boot = boot_id()?;
            if boot == arm.armed_boot_id {
                return Err(RecoveryError::RebootRequired);
            }
            let lease = acquire(paths, arm.binding.authorization.identity_slot)?;
            // Arming durably fenced this identity. The original kernel ended,
            // consumption cannot replay, and the exclusive lease excludes new work.
            // Dropping the lease on any storage failure preserves its poison.
            let retained_storage =
                seal_storage(sessions_root, &arm.binding.authorization.session_id)?;
            lease.release()?;
            let payload = Payload {
                schema: SCHEMA.into(),
                binding: arm.binding.clone(),
                arm_digest: arm.digest()?,
                receipt_binding_digest: arm.receipt_binding_digest.clone(),
                armed_boot_id: arm.armed_boot_id.clone(),
                recovered_boot_id: boot,
                retained_storage,
                release_id: signer.release_id().into(),
                signing_key_id: signer.active_key_id().into(),
            };
            payload.validate().map_err(|_| RecoveryError::Conflict)?;
            write_json_atomic(&payload_path, &payload, 0o600)?;
            payload
        };
        check_payload(&payload, &arm)?;
        let signature = signer.sign_recovery(&payload)?;
        let proof = Proof {
            schema: SIGNED_SCHEMA.into(),
            payload,
            signature,
        };
        write_json_atomic(&outcome_path, &proof, 0o600)?;
        proof
    };
    check_payload(&proof.payload, &arm)?;
    let verifier = LauncherVerifier::open(paths, &operation.directory)?;
    verifier.verify_recovery(&proof)?;
    let response = protocol::exchange(
        endpoint,
        config.broker_uid,
        &Request::Commit {
            proof: Box::new(proof.clone()),
        },
    )
    .map_err(RecoveryError::Broker)?;
    let Response::Acknowledged { acknowledgement } = response else {
        return Err(RecoveryError::Conflict);
    };
    if acknowledgement != Acknowledgement::for_proof(&proof) {
        return Err(RecoveryError::Conflict);
    }
    super::key_cleanup::complete_recovery(paths, &proof)?;
    Ok(proof)
}

fn check_payload(payload: &Payload, arm: &Arm) -> Result<(), RecoveryError> {
    payload.validate().map_err(|_| RecoveryError::Conflict)?;
    if payload.binding != arm.binding
        || payload.arm_digest != arm.digest()?
        || payload.armed_boot_id != arm.armed_boot_id
        || payload.receipt_binding_digest != arm.receipt_binding_digest
    {
        return Err(RecoveryError::Conflict);
    }
    Ok(())
}
