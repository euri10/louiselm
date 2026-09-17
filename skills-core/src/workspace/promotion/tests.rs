#![allow(clippy::unwrap_used, reason = "Fixture failures abort tests.")]

use super::*;
use crate::workspace::{SourceFile, SourceFiles, entries};
use std::{fs, os::unix::fs::PermissionsExt};

fn files(values: &[(&str, &[u8], bool)]) -> SourceFiles {
    values
        .iter()
        .map(|(name, bytes, executable)| {
            (
                name.to_string(),
                SourceFile {
                    bytes: bytes.to_vec(),
                    executable: *executable,
                },
            )
        })
        .collect()
}

#[test]
fn application_preserves_git_and_applies_exact_add_modify_delete_and_mode() {
    let root = tempfile::tempdir().unwrap();
    let checkout = root.path().join("checkout");
    let before = files(&[("old", b"remove", false), ("file", b"before", false)]);
    let after = files(&[("file", b"after", true), ("nested/new", b"added", false)]);
    crate::workspace::filesystem::write_files(&checkout, &before, false).unwrap();
    fs::set_permissions(&checkout, fs::Permissions::from_mode(0o700)).unwrap();
    fs::create_dir(checkout.join(".git")).unwrap();
    fs::write(checkout.join(".git/hook"), b"must remain literal").unwrap();
    let destination = Destination::open(&checkout).unwrap();
    let changes = Changes::new(entries(&before), after).unwrap();
    destination.check(&changes.baseline).unwrap();
    let journal = root.path().join("journal");
    let result = destination.apply(&changes, &journal, |_| Ok(())).unwrap();
    assert!(result.complete);
    assert_eq!(fs::read(checkout.join("file")).unwrap(), b"after");
    assert_ne!(
        fs::metadata(checkout.join("file"))
            .unwrap()
            .permissions()
            .mode()
            & 0o111,
        0
    );
    assert!(!checkout.join("old").exists());
    assert_eq!(fs::read(checkout.join("nested/new")).unwrap(), b"added");
    assert_eq!(
        fs::read(checkout.join(".git/hook")).unwrap(),
        b"must remain literal"
    );
    assert!(
        destination.apply(&changes, &journal, |_| Ok(())).is_err(),
        "retry never repeats writes"
    );
}

#[test]
fn changed_baseline_and_late_change_refuse_without_overwriting_local_work() {
    let root = tempfile::tempdir().unwrap();
    let checkout = root.path().join("checkout");
    let before = files(&[("file", b"before", false)]);
    crate::workspace::filesystem::write_files(&checkout, &before, false).unwrap();
    fs::set_permissions(&checkout, fs::Permissions::from_mode(0o700)).unwrap();
    let destination = Destination::open(&checkout).unwrap();
    let changes = Changes::new(entries(&before), files(&[("file", b"after", false)])).unwrap();
    fs::write(checkout.join("file"), b"local edit").unwrap();
    assert!(destination.check(&changes.baseline).is_err());
    assert!(
        destination
            .apply(&changes, &root.path().join("journal"), |_| Ok(()))
            .is_err()
    );
    assert_eq!(fs::read(checkout.join("file")).unwrap(), b"local edit");
}

#[test]
fn partial_application_keeps_completed_effects_and_refuses_replay() {
    let root = tempfile::tempdir().unwrap();
    let checkout = root.path().join("checkout");
    let before = files(&[("a", b"old", false), ("b", b"old", false)]);
    crate::workspace::filesystem::write_files(&checkout, &before, false).unwrap();
    fs::set_permissions(&checkout, fs::Permissions::from_mode(0o700)).unwrap();
    let destination = Destination::open(&checkout).unwrap();
    let changes = Changes::new(
        entries(&before),
        files(&[("a", b"new", false), ("b", b"new", false)]),
    )
    .unwrap();
    let journal = root.path().join("journal");
    let result = destination.apply(&changes, &journal, |event| match event {
        StepEvent::Begin { index: 1 } => Err(WorkspaceError::Invalid("injected revocation")),
        StepEvent::Done { index: 0 } => {
            assert_eq!(fs::read(checkout.join("a")).unwrap(), b"new");
            Ok(())
        }
        _ => Ok(()),
    });
    assert!(result.is_err());
    assert_eq!(fs::read(checkout.join("a")).unwrap(), b"new");
    assert_eq!(fs::read(checkout.join("b")).unwrap(), b"old");
    assert!(journal.join("0.done").exists());
    assert!(!journal.join("1.done").exists());
    assert!(!journal.join("complete.json").exists());
    assert!(destination.apply(&changes, &journal, |_| Ok(())).is_err());
}

#[test]
fn metadata_links_untracked_files_and_concurrent_promotion_refuse() {
    let root = tempfile::tempdir().unwrap();
    let checkout = root.path().join("checkout");
    let before = files(&[("file", b"before", false)]);
    crate::workspace::filesystem::write_files(&checkout, &before, false).unwrap();
    fs::set_permissions(&checkout, fs::Permissions::from_mode(0o700)).unwrap();
    let destination = Destination::open(&checkout).unwrap();
    assert!(
        Destination::open(&checkout).is_err(),
        "cooperating writers are exclusive"
    );
    let baseline = entries(&before);
    fs::write(checkout.join("untracked"), b"keep").unwrap();
    assert!(destination.check(&baseline).is_err());
    fs::remove_file(checkout.join("untracked")).unwrap();
    fs::set_permissions(checkout.join("file"), fs::Permissions::from_mode(0o700)).unwrap();
    assert!(destination.check(&baseline).is_err());
    fs::remove_file(checkout.join("file")).unwrap();
    std::os::unix::fs::symlink(root.path(), checkout.join("file")).unwrap();
    assert!(destination.check(&baseline).is_err());
}

#[test]
fn file_directory_transitions_and_empty_baseline_are_supported() {
    for (before, after) in [
        (
            files(&[("node", b"old", false)]),
            files(&[("node/child", b"new", false)]),
        ),
        (
            files(&[("node/child", b"old", false)]),
            files(&[("node", b"new", false)]),
        ),
        (files(&[]), files(&[("new", b"new", false)])),
    ] {
        let root = tempfile::tempdir().unwrap();
        let checkout = root.path().join("checkout");
        crate::workspace::filesystem::write_files(&checkout, &before, false).unwrap();
        fs::set_permissions(&checkout, fs::Permissions::from_mode(0o700)).unwrap();
        let destination = Destination::open(&checkout).unwrap();
        let changes = Changes::new(entries(&before), after).unwrap();
        assert!(
            destination
                .apply(&changes, &root.path().join("journal"), |_| Ok(()))
                .unwrap()
                .complete
        );
    }
}

#[test]
fn transfer_rejects_changed_bytes_manifests_and_oversized_frames() {
    let baseline = files(&[("file", b"before", false)]);
    let changes = Changes::new(entries(&baseline), files(&[("file", b"after", true)])).unwrap();
    let digest = Digest::of(b"selected").to_string();
    let job = crate::workspace::verification::JobPreview {
        schema: "louiselm.workspace.verification-preview/1".into(),
        state: "prepared".into(),
        job_digest: digest.clone(),
        snapshot_digest: digest.clone(),
        bundle_digest: digest.clone(),
        base_digest: Digest::of(&serde_json::to_vec(&changes.baseline).unwrap()).to_string(),
        result_digest: Digest::of(&serde_json::to_vec(&entries(&changes.files)).unwrap())
            .to_string(),
        plan_digest: digest,
        command_count: 1,
    };
    let mut wire = Vec::new();
    transfer::send_changes(&mut wire, &changes, &job).unwrap();
    assert_eq!(
        transfer::receive_changes(&mut wire.as_slice(), &job)
            .unwrap()
            .preview,
        changes.preview
    );
    let mut tampered = wire.clone();
    *tampered.last_mut().unwrap() ^= 1;
    assert!(transfer::receive_changes(&mut tampered.as_slice(), &job).is_err());
    let mut other = job.clone();
    other.plan_digest = Digest::of(b"other-plan").to_string();
    assert!(transfer::receive_changes(&mut wire.as_slice(), &other).is_err());
    assert!(
        transfer::receive::<serde_json::Value>(&mut u32::MAX.to_be_bytes().as_slice()).is_err()
    );
}

#[test]
fn failed_effect_receipt_does_not_claim_the_applied_bytes_were_rolled_back() {
    let root = tempfile::tempdir().unwrap();
    let checkout = root.path().join("checkout");
    let before = files(&[("file", b"before", false)]);
    crate::workspace::filesystem::write_files(&checkout, &before, false).unwrap();
    fs::set_permissions(&checkout, fs::Permissions::from_mode(0o700)).unwrap();
    let destination = Destination::open(&checkout).unwrap();
    let changes = Changes::new(entries(&before), files(&[("file", b"after", false)])).unwrap();
    let journal = root.path().join("journal");
    assert!(
        destination
            .apply(&changes, &journal, |event| {
                if matches!(event, StepEvent::Begin { index: 0 }) {
                    fs::create_dir(journal.join("0.done")).unwrap();
                }
                Ok(())
            })
            .is_err()
    );
    assert_eq!(fs::read(checkout.join("file")).unwrap(), b"after");
    assert!(journal.join("0.intent").is_file());
    assert!(!journal.join("0.done").is_file());
    assert!(!journal.join("complete.json").exists());
}

/// Same inherited-descriptor hazard as retention storage: a child forked before
/// its exec keeps the flock until it execs, so a dropped destination must
/// release the description rather than only its own descriptor (louiselm-xx07b).
#[test]
fn an_inherited_descriptor_cannot_keep_the_destination_locked_after_release() {
    let root = tempfile::tempdir().unwrap();
    let destination = root.path().join("destination");
    fs::create_dir(&destination).unwrap();
    fs::set_permissions(&destination, fs::Permissions::from_mode(0o700)).unwrap();
    let opened = Destination::open(&destination).unwrap();
    let inherited = opened.root.try_clone().unwrap();
    drop(opened);
    Destination::open(&destination).unwrap();
    drop(inherited);
}
