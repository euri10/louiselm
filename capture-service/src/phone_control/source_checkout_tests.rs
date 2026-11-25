//! Separate checkouts must come from the exact owner-frozen source.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test fixtures abort on setup or assertion failure"
)]

use std::{os::unix::fs::PermissionsExt, path::Path};

use sha2::{Digest, Sha256};

use super::source::{PhoneSourceError, PhoneSourceStore, SourcePreview};
use crate::{PhoneProjectStore, hex::lower_hex};

const RUN: &str = "11111111-1111-4111-8111-111111111111";

pub(super) fn setup() -> (tempfile::TempDir, PhoneSourceStore, String) {
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let project = root.path().join("project");
    std::fs::create_dir(&project).unwrap();
    let projects = PhoneProjectStore::open(root.path().join("projects")).unwrap();
    let approved = projects.register_root(&project).unwrap();
    let project_id = projects.register_project(&project, &[approved]).unwrap();
    let sources = PhoneSourceStore::open(root.path().join("sources")).unwrap();
    let proof = sources
        .freeze_with(
            &projects,
            &project_id,
            RUN,
            &root.path().join("snapshot"),
            &[],
            |_, output, _| {
                std::fs::create_dir(output)?;
                std::fs::set_permissions(output, std::fs::Permissions::from_mode(0o700))?;
                let bytes = format!(
                    "{{\"schema\":\"louiselm.workspace.snapshot/1\",\"base_commit\":\"{}\"}}",
                    "02".repeat(20)
                );
                std::fs::write(output.join("snapshot.json"), &bytes)?;
                std::fs::set_permissions(
                    output.join("snapshot.json"),
                    std::fs::Permissions::from_mode(0o600),
                )?;
                Ok(SourcePreview {
                    snapshot_digest: format!(
                        "sha256:{}",
                        lower_hex(&Sha256::digest(bytes.as_bytes()))
                    ),
                    base_digest: format!("sha256:{}", "01".repeat(32)),
                    base_commit: "02".repeat(20),
                })
            },
        )
        .unwrap();
    (root, sources, proof.source_id)
}

pub(super) fn materialize(
    _: &Path,
    _: &str,
    output: &Path,
    _: &str,
) -> Result<(SourcePreview, String), PhoneSourceError> {
    std::fs::create_dir(output)?;
    std::fs::set_permissions(output, std::fs::Permissions::from_mode(0o700))?;
    let bytes = format!(
        "{{\"schema\":\"louiselm.workspace.snapshot/1\",\"base_commit\":\"{}\"}}",
        "02".repeat(20)
    );
    Ok((
        SourcePreview {
            snapshot_digest: format!("sha256:{}", lower_hex(&Sha256::digest(bytes.as_bytes()))),
            base_digest: format!("sha256:{}", "01".repeat(32)),
            base_commit: "02".repeat(20),
        },
        "03".repeat(20),
    ))
}

#[test]
fn checkout_is_bound_to_exact_source_and_remains_unreviewed() {
    let (root, sources, source_id) = setup();
    assert!(
        sources
            .prepare_checkout_with(
                RUN,
                &"aa".repeat(16),
                &root.path().join("wrong"),
                materialize
            )
            .is_err()
    );
    let proof = sources
        .prepare_checkout_with(RUN, &source_id, &root.path().join("checkout"), materialize)
        .unwrap();
    assert_eq!(proof.run_id, RUN);
    assert_eq!(proof.source_id, source_id);
    assert_eq!(proof.checkout_id.len(), 32);
    assert!(!proof.reviewed);
    assert!(matches!(
        sources.prepare_checkout_with(RUN, &source_id, &root.path().join("other"), materialize),
        Err(PhoneSourceError::Conflict)
    ));
    drop(sources);
    assert_eq!(
        PhoneSourceStore::open(root.path().join("sources"))
            .unwrap()
            .read_checkout(RUN)
            .unwrap(),
        proof
    );
}

#[test]
fn current_checkout_guard_requires_exact_source_and_checkout_identities() {
    let (root, sources, source_id) = setup();
    let proof = sources
        .prepare_checkout_with(RUN, &source_id, &root.path().join("checkout"), materialize)
        .unwrap();
    let value = sources
        .with_current_checkout_by(
            RUN,
            &source_id,
            &proof.checkout_id,
            |_| Ok(()),
            |source, checkout| {
                assert_eq!(source.project_id, checkout.project_id);
                assert_eq!(source.source_id, checkout.source_id);
                42
            },
        )
        .unwrap();
    assert_eq!(value, 42);
    assert!(
        sources
            .with_current_checkout_by(
                RUN,
                &"ff".repeat(16),
                &proof.checkout_id,
                |_| Ok(()),
                |_, _| ()
            )
            .is_err()
    );
    assert!(
        sources
            .with_current_checkout_by(RUN, &source_id, &"ee".repeat(16), |_| Ok(()), |_, _| ())
            .is_err()
    );
}

#[test]
fn replaced_source_or_checkout_cannot_establish_checkout_proof() {
    let (root, sources, source_id) = setup();
    std::fs::write(root.path().join("snapshot/snapshot.json"), "changed").unwrap();
    assert!(
        sources
            .prepare_checkout_with(RUN, &source_id, &root.path().join("checkout"), materialize)
            .is_err()
    );
    assert!(sources.read_checkout(RUN).is_err());
}

#[test]
fn checkout_symlink_or_source_change_during_materialization_refuses_proof() {
    let (root, sources, source_id) = setup();
    let result = sources.prepare_checkout_with(
        RUN,
        &source_id,
        &root.path().join("checkout"),
        |snapshot, digest, output, run| {
            let replacement = root.path().join("replacement");
            let result = materialize(snapshot, digest, &replacement, run)?;
            std::os::unix::fs::symlink(replacement, output)?;
            Ok(result)
        },
    );
    assert!(result.is_err());
    assert!(sources.read_checkout(RUN).is_err());

    let (second, sources, source_id) = setup();
    let result = sources.prepare_checkout_with(
        RUN,
        &source_id,
        &second.path().join("checkout"),
        |snapshot, digest, output, run| {
            let result = materialize(snapshot, digest, output, run)?;
            std::fs::write(
                snapshot.join("snapshot.json"),
                "changed during materialization",
            )?;
            Ok(result)
        },
    );
    assert!(result.is_err());
    assert!(sources.read_checkout(RUN).is_err());
}

#[test]
fn checkout_parent_permission_change_refuses_proof() {
    let (root, sources, source_id) = setup();
    let result = sources.prepare_checkout_with(
        RUN,
        &source_id,
        &root.path().join("checkout"),
        |snapshot, digest, output, run| {
            let result = materialize(snapshot, digest, output, run)?;
            std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o777))?;
            Ok(result)
        },
    );
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(result.is_err());
    assert!(sources.read_checkout(RUN).is_err());
}

#[test]
fn nonprivate_checkout_and_damaged_checkout_record_are_refused() {
    let (root, sources, source_id) = setup();
    let result = sources.prepare_checkout_with(
        RUN,
        &source_id,
        &root.path().join("checkout"),
        |snapshot, digest, output, run| {
            let result = materialize(snapshot, digest, output, run)?;
            std::fs::set_permissions(output, std::fs::Permissions::from_mode(0o755))?;
            Ok(result)
        },
    );
    assert!(result.is_err());
    assert!(sources.read_checkout(RUN).is_err());

    let (root, sources, source_id) = setup();
    sources
        .prepare_checkout_with(RUN, &source_id, &root.path().join("checkout"), materialize)
        .unwrap();
    let path = root
        .path()
        .join("sources")
        .join(format!("{RUN}.checkout.json"));
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    value["project_id"] = "ff".repeat(16).into();
    std::fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(sources.read_checkout(RUN).is_err());
}

#[test]
#[ignore = "requires the optional root-owned installed skills integration"]
fn installed_integration_materializes_the_exact_frozen_source() {
    use std::process::Command;

    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let project = root.path().join("project");
    std::fs::create_dir(&project).unwrap();
    let git = |args: &[&str]| {
        assert!(
            Command::new("/usr/bin/git")
                .current_dir(&project)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    };
    git(&["init", "--quiet"]);
    std::fs::write(project.join("source"), "original").unwrap();
    git(&["add", "--", "source"]);
    git(&[
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@example.invalid",
        "commit",
        "--quiet",
        "-m",
        "initial",
    ]);
    std::fs::write(project.join("source"), "reviewed dirty source").unwrap();
    let projects = PhoneProjectStore::open(root.path().join("projects")).unwrap();
    let approved = projects.register_root(&project).unwrap();
    let project_id = projects.register_project(&project, &[approved]).unwrap();
    let store = PhoneSourceStore::open(root.path().join("sources")).unwrap();
    let source = store
        .freeze(
            &projects,
            &project_id,
            RUN,
            &root.path().join("snapshot"),
            &["source".into()],
        )
        .unwrap();
    std::fs::rename(project, root.path().join("old-project")).unwrap();
    let checkout = root.path().join("checkout");
    let proof = store
        .prepare_checkout(RUN, &source.source_id, &checkout)
        .unwrap();
    assert_eq!(
        std::fs::read(checkout.join("source")).unwrap(),
        b"reviewed dirty source"
    );
    assert_eq!(proof.source_id, source.source_id);
    assert!(!proof.reviewed);
    assert_eq!(store.read_checkout(RUN).unwrap(), proof);
    store
        .with_current_checkout(RUN, &source.source_id, &proof.checkout_id, |_, _| ())
        .unwrap();
    std::fs::write(checkout.join("source"), "unreviewed replacement").unwrap();
    assert!(
        store
            .with_current_checkout(RUN, &source.source_id, &proof.checkout_id, |_, _| ())
            .is_err()
    );
    let branch = Command::new("/usr/bin/git")
        .arg("-C")
        .arg(&checkout)
        .args(["symbolic-ref", "--quiet", "--short", "HEAD"])
        .output()
        .unwrap();
    assert!(branch.status.success());
    assert_eq!(branch.stdout, format!("run/{RUN}\n").as_bytes());
}
