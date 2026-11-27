//! Fixed first-store transaction for managed and personal receivers.

use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{self, Write},
    os::unix::{fs::MetadataExt, io::AsRawFd},
    path::PathBuf,
};

use rustix::fs::{
    Gid, Mode, OFlags, RenameFlags, Uid, fchmod, fchown, mkdirat, openat, renameat_with,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    ActionEvidenceError, ActionEvidenceKind, ActionEvidenceStore, ReceiverGuard, Seal, Snapshot,
};
use crate::{
    hex::lower_hex,
    phone_control::{
        authority::random_hex,
        barrier::{
            self, Barrier, BarrierBinding,
            namespace_exclusion::{Registrar, Startup},
        },
        barrier_log, fixed,
    },
    phone_lock::FileIdentity,
};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FirstStoresReceipt {
    v: u8,
    receiver_identity: String,
    generation: String,
    barrier: BarrierBinding,
    owner_guard_sha256: String,
    archive_guard_sha256: String,
    owner_snapshot_sha256: String,
    archive_snapshot_sha256: String,
    owner_lock: FileIdentity,
    archive_lock: FileIdentity,
    owner_guard_lock: FileIdentity,
    archive_guard_lock: FileIdentity,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FirstBarrierReceipt {
    v: u8,
    receiver_identity: String,
    generation: String,
    barrier: BarrierBinding,
}

#[derive(Clone, Copy)]
pub(crate) struct StoreOwner {
    pub(crate) uid: u32,
    pub(crate) gid: u32,
}

pub(crate) struct FirstStoreInput<'a> {
    pub(crate) parent: &'a File,
    pub(crate) capture: &'a File,
    pub(crate) private_state: &'a File,
    pub(crate) parent_stage: &'a File,
    pub(crate) capture_stage: &'a File,
    pub(crate) receiver: &'a str,
    pub(crate) generation: &'a str,
    pub(crate) owner: StoreOwner,
    /// The personal installation may already have a trusted receiver key.
    pub(crate) existing_identity: bool,
    pub(crate) _startup: &'a Startup,
    pub(crate) _registrar: &'a Registrar,
}

// The caller must authenticate these descriptors, retain the matching M/R
// leases and independently prove first origin. This function proves neither.
pub(crate) fn initialize_first_stores(
    input: &FirstStoreInput<'_>,
    persist_barrier: impl FnOnce(&FirstBarrierReceipt) -> io::Result<()>,
    commit_stores: impl FnOnce(&FirstStoresReceipt) -> io::Result<()>,
) -> Result<FirstStoresReceipt, ActionEvidenceError> {
    fixed::<32>(input.receiver).map_err(|_| ActionEvidenceError::Unavailable)?;
    fixed::<16>(input.generation).map_err(|_| ActionEvidenceError::Unavailable)?;
    if input.owner.uid == 0 || input.owner.gid == 0 {
        return Err(ActionEvidenceError::Unavailable);
    }
    initialize(input, persist_barrier, commit_stores).map_err(|_| ActionEvidenceError::Unavailable)
}

pub(crate) fn verify_first_stores(
    input: &FirstStoreInput<'_>,
    receipt: &FirstStoresReceipt,
) -> Result<File, ActionEvidenceError> {
    verify_receipt(input, receipt).map_err(|_| ActionEvidenceError::Unavailable)
}

fn verify_receipt(input: &FirstStoreInput<'_>, receipt: &FirstStoresReceipt) -> io::Result<File> {
    if receipt.v != 1
        || receipt.receiver_identity != input.receiver
        || receipt.generation != input.generation
    {
        return Err(io::Error::other("first-store receipt changed"));
    }
    verify_parent(input.parent, input.capture)?;
    let tls = optional_dir(input.capture, "tls")?
        .ok_or_else(|| io::Error::other("TLS directory unavailable"))?;
    let archive_parent = optional_dir(input.capture, "phone-control")?
        .ok_or_else(|| io::Error::other("archive directory unavailable"))?;
    let workflow = optional_dir(input.parent, "workflow")?
        .ok_or_else(|| io::Error::other("owner directory unavailable"))?;
    validate_stage(input.private_state, input.capture_stage, &tls)?;
    validate_stage(input.private_state, input.capture_stage, &archive_parent)?;
    validate_stage(input.private_state, input.parent_stage, &workflow)?;
    verify_identity(input, &tls)?;
    let tls_path = fd_path(&tls);
    let barrier = Barrier::existing(&tls_path, input.receiver, receipt.barrier.clone())?;
    barrier.with_generation(input.generation, || {
        verify_stores(&tls, &workflow, &archive_parent, input, receipt)
    })??;
    verify_parent(input.parent, input.capture)?;
    verify_child(input.capture, "tls", &tls)?;
    verify_child(input.capture, "phone-control", &archive_parent)?;
    verify_child(input.parent, "workflow", &workflow)?;
    verify_identity(input, &tls)?;
    Ok(tls)
}

fn initialize(
    input: &FirstStoreInput<'_>,
    persist_barrier: impl FnOnce(&FirstBarrierReceipt) -> io::Result<()>,
    commit_stores: impl FnOnce(&FirstStoresReceipt) -> io::Result<()>,
) -> io::Result<FirstStoresReceipt> {
    let FirstStoreInput {
        parent,
        capture,
        private_state,
        parent_stage,
        capture_stage,
        receiver,
        generation,
        owner,
        ..
    } = *input;
    verify_parent(parent, capture)?;
    validate_stage(private_state, parent_stage, parent)?;
    validate_stage(private_state, capture_stage, capture)?;
    let tls_existing = optional_dir(capture, "tls")?;
    let archive_existing = optional_dir(capture, "phone-control")?;
    let workflow_existing = optional_dir(parent, "workflow")?;
    if let Some(tls) = &tls_existing {
        validate_stage(private_state, capture_stage, tls)?;
        verify_identity(input, tls)?;
        require_absent(
            tls,
            &[
                "phone-admission-barrier.json",
                "phone-admission-barrier.lock",
                "phone-admission-registrar.lock",
                "phone-admission-cutoff.json",
                "phone-action-owner.json",
                "phone-action-owner.json.lock",
                "phone-action-evidence.json",
                "phone-action-evidence.json.lock",
            ],
        )?;
    }
    if let Some(archive) = &archive_existing {
        validate_stage(private_state, capture_stage, archive)?;
        require_absent(archive, &["action-evidence", "action-evidence.lock"])?;
    }
    if let Some(workflow) = &workflow_existing {
        validate_stage(private_state, parent_stage, workflow)?;
        require_absent(workflow, &["phone-action-owner", "phone-action-owner.lock"])?;
    }
    let tls = ensure_dir(capture, capture_stage, "tls", tls_existing, owner)?;
    let archive_parent = ensure_dir(
        capture,
        capture_stage,
        "phone-control",
        archive_existing,
        owner,
    )?;
    let workflow = ensure_dir(parent, parent_stage, "workflow", workflow_existing, owner)?;
    validate_stage(private_state, capture_stage, &tls)?;
    validate_stage(private_state, capture_stage, &archive_parent)?;
    validate_stage(private_state, parent_stage, &workflow)?;
    let binding = create_barrier(&tls, receiver, generation, owner)?;

    persist_barrier(&FirstBarrierReceipt {
        v: 1,
        receiver_identity: receiver.to_owned(),
        generation: generation.to_owned(),
        barrier: binding.clone(),
    })?;
    let tls_path = fd_path(&tls);
    let barrier = Barrier::existing(&tls_path, receiver, binding.clone())?;
    let receipt = barrier.with_generation(generation, || {
        let receipt = create_stores(&tls, &workflow, &archive_parent, input, &binding)?;
        verify_parent(parent, capture)?;
        verify_child(capture, "tls", &tls)?;
        verify_child(capture, "phone-control", &archive_parent)?;
        verify_child(parent, "workflow", &workflow)?;
        verify_identity(input, &tls)?;
        commit_stores(&receipt)?;
        Ok::<_, io::Error>(receipt)
    })??;
    verify_parent(parent, capture)?;
    verify_child(capture, "tls", &tls)?;
    verify_child(capture, "phone-control", &archive_parent)?;
    verify_child(parent, "workflow", &workflow)?;
    verify_identity(input, &tls)?;
    Ok(receipt)
}

fn verify_identity(input: &FirstStoreInput<'_>, tls: &File) -> io::Result<()> {
    if input.existing_identity {
        let identity =
            crate::TlsIdentity::load_existing(&fd_path(tls)).map_err(io::Error::other)?;
        if identity.public_key_sha256() != input.receiver {
            return Err(io::Error::other("receiver identity changed"));
        }
        Ok(())
    } else {
        require_absent(tls, &["receiver-key.pem", "receiver-cert.pem"])
    }
}

fn create_barrier(
    tls: &File,
    receiver: &str,
    generation: &str,
    owner: StoreOwner,
) -> io::Result<BarrierBinding> {
    let lock_nonce = random_hex(16).map_err(|_| io::Error::other("randomness unavailable"))?;
    let registrar_nonce = random_hex(16).map_err(|_| io::Error::other("randomness unavailable"))?;
    let cutoff_nonce = random_hex(16).map_err(|_| io::Error::other("randomness unavailable"))?;
    let lock = create_file(
        tls,
        "phone-admission-barrier.lock",
        lock_nonce.as_bytes(),
        owner,
    )?;
    let registrar = create_file(
        tls,
        "phone-admission-registrar.lock",
        registrar_nonce.as_bytes(),
        owner,
    )?;
    let cutoff_bytes = barrier_log::initialized_bytes(receiver, generation, &cutoff_nonce)?;
    let cutoff = create_file(tls, "phone-admission-cutoff.json", &cutoff_bytes, owner)?;
    let mut anchor = new_file(tls, "phone-admission-barrier.json")?;
    let binding = BarrierBinding {
        anchor: FileIdentity::from_metadata(&anchor.metadata()?)?,
        lock,
        lock_nonce,
        registrar,
        registrar_nonce,
        cutoff,
        cutoff_nonce,
    };
    let bytes = barrier::initialized_anchor(receiver, generation, &binding)?;
    finish_file(tls, &mut anchor, &bytes, owner)?;
    Ok(binding)
}

fn create_stores(
    tls: &File,
    workflow: &File,
    archive_parent: &File,
    input: &FirstStoreInput<'_>,
    binding: &BarrierBinding,
) -> io::Result<FirstStoresReceipt> {
    let FirstStoreInput {
        receiver,
        generation,
        owner,
        ..
    } = *input;
    let snapshot = serde_json::to_vec(&Snapshot {
        v: 1,
        actions: BTreeMap::new(),
    })
    .map_err(io::Error::other)?;
    let snapshot_sha256 = lower_hex(&Sha256::digest(&snapshot));
    let seal = serde_json::to_vec(&Seal {
        v: 1,
        bytes: snapshot.len() as u64,
        sha256: snapshot_sha256.clone(),
    })
    .map_err(io::Error::other)?;
    let owner_lock = create_file(workflow, "phone-action-owner.lock", &[], owner)?;
    let archive_lock = create_file(archive_parent, "action-evidence.lock", &[], owner)?;
    let owner_guard_lock = create_file(tls, "phone-action-owner.json.lock", &[], owner)?;
    let archive_guard_lock = create_file(tls, "phone-action-evidence.json.lock", &[], owner)?;
    let owner_store = create_dir(workflow, input.parent_stage, "phone-action-owner", owner)?;
    let archive_store = create_dir(
        archive_parent,
        input.capture_stage,
        "action-evidence",
        owner,
    )?;
    for store in [&owner_store, &archive_store] {
        create_file(store, "actions.json", &snapshot, owner)?;
        create_file(store, "seal.json", &seal, owner)?;
    }
    let guard = |kind| -> io::Result<Vec<u8>> {
        serde_json::to_vec(&ReceiverGuard {
            v: 1,
            kind,
            receiver_identity: receiver.to_owned(),
            generation: generation.to_owned(),
            barrier: Some(binding.clone()),
            bytes: snapshot.len() as u64,
            sha256: snapshot_sha256.clone(),
            pending: None,
        })
        .map_err(io::Error::other)
    };
    let owner_guard = guard(ActionEvidenceKind::Owner)?;
    let archive_guard = guard(ActionEvidenceKind::Archive)?;
    create_file(tls, "phone-action-owner.json", &owner_guard, owner)?;
    create_file(tls, "phone-action-evidence.json", &archive_guard, owner)?;
    let receipt = FirstStoresReceipt {
        v: 1,
        receiver_identity: receiver.to_owned(),
        generation: generation.to_owned(),
        barrier: binding.clone(),
        owner_guard_sha256: lower_hex(&Sha256::digest(&owner_guard)),
        archive_guard_sha256: lower_hex(&Sha256::digest(&archive_guard)),
        owner_snapshot_sha256: snapshot_sha256.clone(),
        archive_snapshot_sha256: snapshot_sha256,
        owner_lock,
        archive_lock,
        owner_guard_lock,
        archive_guard_lock,
    };
    verify_stores(tls, workflow, archive_parent, input, &receipt)?;
    verify_child(workflow, "phone-action-owner", &owner_store)?;
    verify_child(archive_parent, "action-evidence", &archive_store)?;
    Ok(receipt)
}

fn verify_stores(
    tls: &File,
    workflow: &File,
    archive_parent: &File,
    input: &FirstStoreInput<'_>,
    receipt: &FirstStoresReceipt,
) -> io::Result<()> {
    let tls_path = fd_path(tls);
    let owner_path = fd_path(workflow).join("phone-action-owner");
    let archive_path = fd_path(archive_parent).join("action-evidence");
    let owner_guard_path = tls_path.join("phone-action-owner.json");
    let archive_guard_path = tls_path.join("phone-action-evidence.json");
    let owner = ActionEvidenceStore::open_bound_under_barrier(
        &owner_path,
        &owner_guard_path,
        input.receiver,
        ActionEvidenceKind::Owner,
        input.generation,
        &receipt.barrier,
    )
    .map_err(|_| io::Error::other("owner evidence unavailable"))?;
    let archive = ActionEvidenceStore::open_bound_under_barrier(
        &archive_path,
        &archive_guard_path,
        input.receiver,
        ActionEvidenceKind::Archive,
        input.generation,
        &receipt.barrier,
    )
    .map_err(|_| io::Error::other("archive evidence unavailable"))?;
    let digest = |path: &std::path::Path| -> io::Result<String> {
        let bytes = super::read_bounded(path, super::MAX_STORE_BYTES)
            .map_err(|_| io::Error::other("created evidence unavailable"))?;
        Ok(lower_hex(&Sha256::digest(bytes)))
    };
    if digest(&owner_guard_path)? != receipt.owner_guard_sha256
        || digest(&archive_guard_path)? != receipt.archive_guard_sha256
        || digest(&owner_path.join("actions.json"))? != receipt.owner_snapshot_sha256
        || digest(&archive_path.join("actions.json"))? != receipt.archive_snapshot_sha256
        || FileIdentity::at(&owner_path.with_extension("lock"))? != receipt.owner_lock
        || FileIdentity::at(&archive_path.with_extension("lock"))? != receipt.archive_lock
        || FileIdentity::at(&tls_path.join("phone-action-owner.json.lock"))?
            != receipt.owner_guard_lock
        || FileIdentity::at(&tls_path.join("phone-action-evidence.json.lock"))?
            != receipt.archive_guard_lock
    {
        return Err(io::Error::other("created evidence changed"));
    }
    drop(owner);
    drop(archive);
    Ok(())
}

fn fd_path(directory: &File) -> PathBuf {
    PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd()))
}

fn optional_dir(parent: &File, name: &str) -> io::Result<Option<File>> {
    match openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => Ok(Some(File::from(fd))),
        Err(error) if error == rustix::io::Errno::NOENT => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn verify_parent(parent: &File, capture: &File) -> io::Result<()> {
    verify_child(parent, "capture", capture)
}

fn verify_child(parent: &File, name: &str, expected: &File) -> io::Result<()> {
    let child = optional_dir(parent, name)?
        .ok_or_else(|| io::Error::other("child directory unavailable"))?;
    let expected = expected.metadata()?;
    let actual = child.metadata()?;
    if !expected.is_dir() || expected.dev() != actual.dev() || expected.ino() != actual.ino() {
        return Err(io::Error::other("capture parent changed"));
    }
    Ok(())
}

fn require_absent(parent: &File, names: &[&str]) -> io::Result<()> {
    for name in names {
        match fs::symlink_metadata(fd_path(parent).join(name)) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
            Ok(_) => return Err(io::Error::other("first-store artifact already exists")),
        }
    }
    Ok(())
}

fn set_owner(file: &File, owner: StoreOwner) -> io::Result<()> {
    let metadata = file.metadata()?;
    if metadata.uid() != owner.uid || metadata.gid() != owner.gid {
        fchown(
            file,
            Some(Uid::from_raw(owner.uid)),
            Some(Gid::from_raw(owner.gid)),
        )?;
    }
    Ok(())
}

fn validate_stage(private_state: &File, stage: &File, destination: &File) -> io::Result<()> {
    let authority = private_state.metadata()?;
    let staging = stage.metadata()?;
    let target = destination.metadata()?;
    if !authority.is_dir()
        || authority.mode() & 0o077 != 0
        || !staging.is_dir()
        || staging.mode() & 0o077 != 0
        || staging.uid() != authority.uid()
        || staging.dev() != target.dev()
    {
        return Err(io::Error::other("private staging unavailable"));
    }
    Ok(())
}

fn create_dir(parent: &File, stage: &File, name: &str, owner: StoreOwner) -> io::Result<File> {
    let temporary = format!(".phone-first-{}", uuid::Uuid::new_v4());
    mkdirat(stage, temporary.as_str(), Mode::RWXU)?;
    let directory = optional_dir(stage, &temporary)?
        .ok_or_else(|| io::Error::other("new directory unavailable"))?;
    fchmod(&directory, Mode::RWXU)?;
    set_owner(&directory, owner)?;
    directory.sync_all()?;
    stage.sync_all()?;
    renameat_with(
        stage,
        temporary.as_str(),
        parent,
        name,
        RenameFlags::NOREPLACE,
    )?;
    stage.sync_all()?;
    parent.sync_all()?;
    verify_child(parent, name, &directory)?;
    Ok(directory)
}

fn ensure_dir(
    parent: &File,
    stage: &File,
    name: &str,
    existing: Option<File>,
    owner: StoreOwner,
) -> io::Result<File> {
    match existing {
        Some(directory) => Ok(directory),
        None => create_dir(parent, stage, name, owner),
    }
}

fn new_file(parent: &File, name: &str) -> io::Result<File> {
    Ok(File::from(openat(
        parent,
        name,
        OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    )?))
}

fn finish_file(
    parent: &File,
    file: &mut File,
    bytes: &[u8],
    owner: StoreOwner,
) -> io::Result<FileIdentity> {
    file.write_all(bytes)?;
    fchmod(&*file, Mode::RUSR | Mode::WUSR)?;
    set_owner(file, owner)?;
    file.sync_all()?;
    let identity = FileIdentity::from_metadata(&file.metadata()?)?;
    parent.sync_all()?;
    Ok(identity)
}

fn create_file(
    parent: &File,
    name: &str,
    bytes: &[u8],
    owner: StoreOwner,
) -> io::Result<FileIdentity> {
    let mut file = new_file(parent, name)?;
    finish_file(parent, &mut file, bytes, owner)
}

#[cfg(test)]
#[path = "evidence_init_tests.rs"]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "disposable first-store fixtures"
)]
mod tests;
