//! Personal receiver initialization and reopening of both evidence stores.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

use super::{
    ActionEvidenceError, ActionEvidenceKind, ActionEvidenceStore,
    authority::random_hex,
    barrier::{Barrier, BarrierBinding, ReceiverBarrier, namespace_exclusion::Startup},
    evidence::first_init::{FirstStoreInput, StoreOwner, initialize_first_stores},
};
use crate::PairingRegistry;

fn stores(
    capture: &Path,
    receiver: &str,
) -> Result<(String, super::barrier::BarrierBinding), ActionEvidenceError> {
    let tls = capture.join("tls");
    let owner_path = capture
        .parent()
        .ok_or(ActionEvidenceError::Unavailable)?
        .join("workflow/phone-action-owner");
    let archive = ActionEvidenceStore::open_bound(
        capture.join("phone-control/action-evidence"),
        tls.join("phone-action-evidence.json"),
        receiver,
        ActionEvidenceKind::Archive,
    )?;
    let generation = archive
        .bound_generation()
        .ok_or(ActionEvidenceError::Unavailable)?;
    let binding = archive
        .bound_barrier()
        .ok_or(ActionEvidenceError::Unavailable)?;
    let barrier = Barrier::existing(&tls, receiver, binding.clone())
        .map_err(|_| ActionEvidenceError::Unavailable)?;
    barrier
        .with_generation(generation, || {
            ActionEvidenceStore::open_bound_under_barrier(
                &owner_path,
                &tls.join("phone-action-owner.json"),
                receiver,
                ActionEvidenceKind::Owner,
                generation,
                binding,
            )
            .map(|_| ())
        })
        .map_err(|_| ActionEvidenceError::Unavailable)??;
    Ok((generation.to_owned(), binding.clone()))
}

pub(crate) fn has_artifacts(capture: &Path) -> Result<bool, ActionEvidenceError> {
    let parent = capture.parent().ok_or(ActionEvidenceError::Unavailable)?;
    let paths = [
        capture.join("tls/phone-action-owner.json"),
        capture.join("tls/phone-action-owner.json.lock"),
        capture.join("tls/phone-action-evidence.json"),
        capture.join("tls/phone-action-evidence.json.lock"),
        capture.join("tls/phone-admission-barrier.json"),
        capture.join("tls/phone-admission-barrier.lock"),
        capture.join("tls/phone-admission-registrar.lock"),
        capture.join("tls/phone-admission-cutoff.json"),
        capture.join("phone-control/action-evidence"),
        capture.join("phone-control/action-evidence.lock"),
        parent.join("workflow/phone-action-owner"),
        parent.join("workflow/phone-action-owner.lock"),
    ];
    for path in paths {
        match fs::symlink_metadata(path) {
            Ok(_) => return Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return Err(ActionEvidenceError::Unavailable),
        }
    }
    Ok(false)
}

/// Initialize once or reopen; failure leaves ordinary reads available.
pub(crate) fn prepare(
    capture: &Path,
    receiver: &str,
    pairing: &PairingRegistry,
) -> Result<(), ActionEvidenceError> {
    #[cfg(test)]
    let startup = Startup::for_fixture(capture).map_err(|_| ActionEvidenceError::Unavailable)?;
    #[cfg(not(test))]
    let startup = Startup::try_acquire().map_err(|_| ActionEvidenceError::Unavailable)?;
    let registrar = startup
        .registrar()
        .map_err(|_| ActionEvidenceError::Unavailable)?;
    if let Ok((generation, _)) = stores(capture, receiver) {
        if pairing
            .personal_send_ready(receiver, &generation)
            .map_err(|_| ActionEvidenceError::Unavailable)?
        {
            return Ok(());
        }
        if pairing
            .begin_personal_send(receiver, &generation)
            .map_err(|_| ActionEvidenceError::Unavailable)?
        {
            return pairing
                .finish_personal_send(receiver, &generation)
                .map_err(|_| ActionEvidenceError::Unavailable);
        }
        return Err(ActionEvidenceError::Unavailable);
    }
    if has_artifacts(capture)? {
        return Err(ActionEvidenceError::Unavailable);
    }
    let generation = random_hex(16).map_err(|_| ActionEvidenceError::Unavailable)?;
    if !pairing
        .begin_personal_send(receiver, &generation)
        .map_err(|_| ActionEvidenceError::Unavailable)?
    {
        return Err(ActionEvidenceError::Unavailable);
    }

    let parent_path = capture.parent().ok_or(ActionEvidenceError::Unavailable)?;
    let stage_path: PathBuf = parent_path.join(format!(".phone-first-{}", uuid::Uuid::new_v4()));
    let result = (|| -> io::Result<()> {
        fs::DirBuilder::new().mode(0o700).create(&stage_path)?;
        fs::set_permissions(&stage_path, fs::Permissions::from_mode(0o700))?;
        let parent = File::open(parent_path)?;
        parent.sync_all()?;
        let capture = File::open(capture)?;
        let stage = File::open(&stage_path)?;
        let metadata = capture.metadata()?;
        let input = FirstStoreInput {
            parent: &parent,
            capture: &capture,
            private_state: &stage,
            parent_stage: &stage,
            capture_stage: &stage,
            receiver,
            generation: &generation,
            owner: StoreOwner {
                uid: metadata.uid(),
                gid: metadata.gid(),
            },
            existing_identity: true,
            _startup: &startup,
            _registrar: &registrar,
        };
        initialize_first_stores(
            &input,
            |receipt| {
                let mut file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(stage_path.join("barrier.json"))?;
                serde_json::to_writer(&mut file, receipt).map_err(io::Error::other)?;
                file.write_all(b"\n")?;
                file.sync_all()?;
                stage.sync_all()
            },
            |_| {
                pairing
                    .finish_personal_send(receiver, &generation)
                    .map_err(io::Error::other)
            },
        )
        .map_err(io::Error::other)?;
        fs::remove_file(stage_path.join("barrier.json"))?;
        fs::remove_dir(&stage_path)?;
        parent.sync_all()
    })();
    // On failure retain the private staging directory as evidence; a later
    // startup cannot retry a Starting marker as a fresh empty store.
    result.map_err(|_| ActionEvidenceError::Unavailable)
}

pub(super) fn open_registration(
    receiver: &str,
    archive: &ActionEvidenceStore,
    pairing: &PairingRegistry,
) -> Result<(String, ReceiverBarrier), ActionEvidenceError> {
    check_archive_health(archive, receiver, pairing)?;
    let (tls, _) = archive_paths(archive)?;
    let generation = archive
        .bound_generation()
        .ok_or(ActionEvidenceError::Unavailable)?;
    let binding = archive
        .bound_barrier()
        .ok_or(ActionEvidenceError::Unavailable)?;
    let barrier = Barrier::existing(&tls, receiver, binding.clone())
        .map_err(|_| ActionEvidenceError::Unavailable)?;
    let receiver_barrier = barrier
        .receiver(generation)
        .map_err(|_| ActionEvidenceError::Unavailable)?;
    Ok((generation.to_owned(), receiver_barrier))
}

/// Recheck both stores and their common barrier before offering or starting Send.
pub(super) fn check_archive_health(
    archive: &ActionEvidenceStore,
    receiver: &str,
    pairing: &PairingRegistry,
) -> Result<(), ActionEvidenceError> {
    let (tls, _) = archive_paths(archive)?;
    let generation = archive
        .bound_generation()
        .ok_or(ActionEvidenceError::Unavailable)?;
    let binding = archive
        .bound_barrier()
        .ok_or(ActionEvidenceError::Unavailable)?;
    let barrier = Barrier::existing(&tls, receiver, binding.clone())
        .map_err(|_| ActionEvidenceError::Unavailable)?;
    // Owner operations acquire the barrier before their journal lock as well.
    barrier
        .with_generation(generation, || {
            check_archive_health_under_barrier(archive, receiver, pairing, generation, binding)
        })
        .map_err(|_| ActionEvidenceError::Unavailable)?
}

fn check_under_barrier(
    tls: &Path,
    owner_path: &Path,
    receiver: &str,
    archive: &ActionEvidenceStore,
    pairing: &PairingRegistry,
    generation: &str,
    binding: &BarrierBinding,
) -> Result<(), ActionEvidenceError> {
    if archive.bound_generation() != Some(generation) || archive.bound_barrier() != Some(binding) {
        return Err(ActionEvidenceError::Unavailable);
    }
    archive.verify_available(receiver)?;
    if !pairing
        .personal_send_ready(receiver, generation)
        .map_err(|_| ActionEvidenceError::Unavailable)?
    {
        return Err(ActionEvidenceError::Unavailable);
    }
    ActionEvidenceStore::open_bound_under_barrier(
        owner_path,
        &tls.join("phone-action-owner.json"),
        receiver,
        ActionEvidenceKind::Owner,
        generation,
        binding,
    )
    .map(|_| ())
}

fn archive_paths(archive: &ActionEvidenceStore) -> Result<(PathBuf, PathBuf), ActionEvidenceError> {
    let owner = archive
        .owner_journal_path()
        .ok_or(ActionEvidenceError::Unavailable)?;
    let tls = owner
        .parent()
        .and_then(Path::parent)
        .ok_or(ActionEvidenceError::Unavailable)?
        .join("capture/tls");
    Ok((tls, owner))
}

/// Caller holds this exact generation and binding through the receiver barrier.
pub(super) fn check_archive_health_under_barrier(
    archive: &ActionEvidenceStore,
    receiver: &str,
    pairing: &PairingRegistry,
    generation: &str,
    binding: &BarrierBinding,
) -> Result<(), ActionEvidenceError> {
    let (tls, owner) = archive_paths(archive)?;
    check_under_barrier(
        &tls, &owner, receiver, archive, pairing, generation, binding,
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "disposable evidence fixture")]
mod tests {
    use super::*;

    #[test]
    fn gate_health_requires_the_exact_held_barrier_and_current_stores() {
        let root = tempfile::tempdir().unwrap();
        let capture = root.path().join("louiselm/capture");
        let tls = capture.join("tls");
        let identity = crate::TlsIdentity::load_or_create(&tls).unwrap();
        let receiver = identity.public_key_sha256();
        let pairing = PairingRegistry::open(capture.join("pairing")).unwrap();
        pairing
            .with_personal_receiver(|unbound| {
                assert!(unbound);
                Ok::<_, crate::PairingError>(((), Some(receiver.to_owned())))
            })
            .unwrap();
        prepare(&capture, receiver, &pairing).unwrap();
        let archive = ActionEvidenceStore::open_bound(
            capture.join("phone-control/action-evidence"),
            tls.join("phone-action-evidence.json"),
            receiver,
            ActionEvidenceKind::Archive,
        )
        .unwrap();
        let generation = archive.bound_generation().unwrap();
        let binding = archive.bound_barrier().unwrap();
        let barrier = Barrier::existing(&tls, receiver, binding.clone()).unwrap();
        barrier
            .with_generation(generation, || {
                assert!(
                    check_archive_health_under_barrier(
                        &archive, receiver, &pairing, generation, binding
                    )
                    .is_ok()
                );
                assert!(matches!(
                    check_archive_health_under_barrier(
                        &archive,
                        receiver,
                        &pairing,
                        &"11".repeat(16),
                        binding
                    ),
                    Err(ActionEvidenceError::Unavailable)
                ));
                let mut other = binding.clone();
                other.cutoff_nonce = "22".repeat(16);
                assert!(matches!(
                    check_archive_health_under_barrier(
                        &archive, receiver, &pairing, generation, &other
                    ),
                    Err(ActionEvidenceError::Unavailable)
                ));
                fs::remove_dir_all(capture.join("phone-control/action-evidence")).unwrap();
                assert!(matches!(
                    check_archive_health_under_barrier(
                        &archive, receiver, &pairing, generation, binding
                    ),
                    Err(ActionEvidenceError::Unavailable)
                ));
            })
            .unwrap();
    }
}
