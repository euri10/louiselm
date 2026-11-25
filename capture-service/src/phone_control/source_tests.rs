//! Frozen source identity is established by the trusted operator's own process.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test fixtures abort on setup or assertion failure"
)]

use std::{os::unix::fs::PermissionsExt, path::Path};

use sha2::{Digest, Sha256};

use super::source::{
    PhoneSourceError, PhoneSourceStore, SourcePreview, read_bounded, run_skills, run_trusted,
};
use crate::{PhoneProjectStore, hex::lower_hex};

const RUN: &str = "11111111-1111-4111-8111-111111111111";

fn private_temp() -> tempfile::TempDir {
    let temporary = tempfile::tempdir().unwrap();
    std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    temporary
}

fn frozen(
    project: &Path,
    snapshot: &Path,
    _: &[String],
) -> Result<SourcePreview, PhoneSourceError> {
    assert!(project.is_dir());
    std::fs::create_dir(snapshot)?;
    std::fs::set_permissions(snapshot, std::fs::Permissions::from_mode(0o700))?;
    let bytes = format!(
        "{{\"schema\":\"louiselm.workspace.snapshot/1\",\"base_commit\":\"{}\"}}",
        "02".repeat(20)
    );
    std::fs::write(snapshot.join("snapshot.json"), &bytes)?;
    std::fs::set_permissions(
        snapshot.join("snapshot.json"),
        std::fs::Permissions::from_mode(0o600),
    )?;
    Ok(SourcePreview {
        snapshot_digest: format!("sha256:{}", lower_hex(&Sha256::digest(bytes.as_bytes()))),
        base_digest: format!("sha256:{}", "01".repeat(32)),
        base_commit: "02".repeat(20),
    })
}

#[test]
fn frozen_identity_is_bound_to_selected_project_and_survives_restart() {
    let temporary = private_temp();
    let project = temporary.path().join("project");
    std::fs::create_dir(&project).unwrap();
    let projects = PhoneProjectStore::open(temporary.path().join("projects")).unwrap();
    let root = projects.register_root(&project).unwrap();
    let project_id = projects.register_project(&project, &[root]).unwrap();
    let source_path = temporary.path().join("snapshot");
    let store_path = temporary.path().join("sources");
    let store = PhoneSourceStore::open(&store_path).unwrap();
    let proof = store
        .freeze_with(&projects, &project_id, RUN, &source_path, &[], frozen)
        .unwrap();
    assert_eq!(proof.project_id, project_id);
    assert_eq!(proof.run_id, RUN);
    assert_eq!(proof.source_id.len(), 32);
    assert!(matches!(
        store.freeze_with(
            &projects,
            &project_id,
            RUN,
            &temporary.path().join("other"),
            &[],
            frozen
        ),
        Err(PhoneSourceError::Conflict)
    ));
    drop(store);
    std::fs::remove_dir(&project).unwrap();
    let restored = PhoneSourceStore::open(&store_path).unwrap();
    assert_eq!(restored.read(RUN).unwrap(), proof);
}

#[test]
fn replacement_during_freeze_never_establishes_a_source_identity() {
    let temporary = private_temp();
    let project = temporary.path().join("project");
    std::fs::create_dir(&project).unwrap();
    let projects = PhoneProjectStore::open(temporary.path().join("projects")).unwrap();
    let root = projects.register_root(&project).unwrap();
    let project_id = projects.register_project(&project, &[root]).unwrap();
    let store = PhoneSourceStore::open(temporary.path().join("sources")).unwrap();
    let result = store.freeze_with(
        &projects,
        &project_id,
        RUN,
        &temporary.path().join("snapshot"),
        &[],
        |original, output, includes| {
            let preview = frozen(original, output, includes)?;
            std::fs::rename(original, temporary.path().join("old-project"))?;
            std::fs::create_dir(original)?;
            Ok(preview)
        },
    );
    assert!(result.is_err());
    assert!(store.read(RUN).is_err());
}

#[test]
fn substituted_snapshot_symlink_is_not_a_freeze_of_the_selected_output() {
    let temporary = private_temp();
    let project = temporary.path().join("project");
    std::fs::create_dir(&project).unwrap();
    let projects = PhoneProjectStore::open(temporary.path().join("projects")).unwrap();
    let root = projects.register_root(&project).unwrap();
    let project_id = projects.register_project(&project, &[root]).unwrap();
    let store = PhoneSourceStore::open(temporary.path().join("sources")).unwrap();
    let result = store.freeze_with(
        &projects,
        &project_id,
        RUN,
        &temporary.path().join("snapshot"),
        &[],
        |original, output, includes| {
            let replacement = temporary.path().join("replacement");
            let preview = frozen(original, &replacement, includes)?;
            std::os::unix::fs::symlink(replacement, output)?;
            Ok(preview)
        },
    );
    assert!(result.is_err());
    assert!(store.read(RUN).is_err());
}

#[test]
fn a_nonprivate_frozen_snapshot_is_not_recorded() {
    let temporary = private_temp();
    let project = temporary.path().join("project");
    std::fs::create_dir(&project).unwrap();
    let projects = PhoneProjectStore::open(temporary.path().join("projects")).unwrap();
    let root = projects.register_root(&project).unwrap();
    let project_id = projects.register_project(&project, &[root]).unwrap();
    let store = PhoneSourceStore::open(temporary.path().join("sources")).unwrap();
    let result = store.freeze_with(
        &projects,
        &project_id,
        RUN,
        &temporary.path().join("snapshot"),
        &[],
        |original, output, includes| {
            let preview = frozen(original, output, includes)?;
            std::fs::set_permissions(output, std::fs::Permissions::from_mode(0o755))?;
            Ok(preview)
        },
    );
    assert!(result.is_err());
    assert!(store.read(RUN).is_err());
}

#[test]
fn a_snapshot_parent_that_becomes_writable_by_others_is_refused() {
    let temporary = private_temp();
    let project = temporary.path().join("project");
    std::fs::create_dir(&project).unwrap();
    let projects = PhoneProjectStore::open(temporary.path().join("projects")).unwrap();
    let root = projects.register_root(&project).unwrap();
    let project_id = projects.register_project(&project, &[root]).unwrap();
    let store = PhoneSourceStore::open(temporary.path().join("sources")).unwrap();
    let result = store.freeze_with(
        &projects,
        &project_id,
        RUN,
        &temporary.path().join("snapshot"),
        &[],
        |path, output, includes| {
            let preview = frozen(path, output, includes)?;
            std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(0o777))?;
            Ok(preview)
        },
    );
    std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(result.is_err());
    assert!(store.read(RUN).is_err());
}

#[test]
fn damaged_retained_proof_is_not_accepted() {
    let temporary = private_temp();
    let project = temporary.path().join("project");
    std::fs::create_dir(&project).unwrap();
    let projects = PhoneProjectStore::open(temporary.path().join("projects")).unwrap();
    let root = projects.register_root(&project).unwrap();
    let project_id = projects.register_project(&project, &[root]).unwrap();
    let store_path = temporary.path().join("sources");
    let store = PhoneSourceStore::open(&store_path).unwrap();
    store
        .freeze_with(
            &projects,
            &project_id,
            RUN,
            &temporary.path().join("snapshot"),
            &[],
            frozen,
        )
        .unwrap();
    let path = store_path.join(format!("{RUN}.json"));
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    value["project_id"] = "03".repeat(16).into();
    std::fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(store.read(RUN).is_err());
}

#[test]
fn freeze_holds_its_run_identity_lock_until_proof_is_persisted() {
    use fs2::FileExt;

    let temporary = private_temp();
    let project = temporary.path().join("project");
    std::fs::create_dir(&project).unwrap();
    let projects = PhoneProjectStore::open(temporary.path().join("projects")).unwrap();
    let root = projects.register_root(&project).unwrap();
    let project_id = projects.register_project(&project, &[root]).unwrap();
    let state = temporary.path().join("sources");
    let store = PhoneSourceStore::open(&state).unwrap();
    let lock_path = state.join(format!(".{RUN}.lock"));
    std::fs::File::create(&lock_path).unwrap();
    let competing = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(lock_path)
        .unwrap();
    store
        .freeze_with(
            &projects,
            &project_id,
            RUN,
            &temporary.path().join("snapshot"),
            &[],
            |path, output, includes| {
                assert_eq!(
                    competing.try_lock_exclusive().unwrap_err().kind(),
                    std::io::ErrorKind::WouldBlock
                );
                frozen(path, output, includes)
            },
        )
        .unwrap();
    competing.try_lock_exclusive().unwrap();
    FileExt::unlock(&competing).unwrap();
}

#[test]
fn changed_snapshot_metadata_does_not_establish_a_proof() {
    let temporary = private_temp();
    let project = temporary.path().join("project");
    std::fs::create_dir(&project).unwrap();
    let projects = PhoneProjectStore::open(temporary.path().join("projects")).unwrap();
    let root = projects.register_root(&project).unwrap();
    let project_id = projects.register_project(&project, &[root]).unwrap();
    let store = PhoneSourceStore::open(temporary.path().join("sources")).unwrap();
    let result = store.freeze_with(
        &projects,
        &project_id,
        RUN,
        &temporary.path().join("snapshot"),
        &[],
        |path, output, includes| {
            let preview = frozen(path, output, includes)?;
            std::fs::write(output.join("snapshot.json"), b"substituted")?;
            Ok(preview)
        },
    );
    assert!(result.is_err());
    assert!(store.read(RUN).is_err());
}

#[test]
fn missing_skills_integration_is_explicitly_unsupported() {
    let temporary = private_temp();
    assert!(matches!(
        run_skills(
            &temporary.path().join("absent"),
            temporary.path(),
            temporary.path(),
            &[]
        ),
        Err(PhoneSourceError::Unsupported)
    ));
}

#[test]
fn trusted_process_output_is_bounded_and_oversize_is_refused() {
    assert_eq!(read_bounded(std::io::repeat(b'x'), 64).unwrap().len(), 65);
    let executable = Path::new("/usr/bin/printf");
    assert_eq!(
        run_trusted(executable, &["%s".into(), "ok".into()], "10s", 8).unwrap(),
        b"ok"
    );
    assert!(run_trusted(executable, &["%0100000d".into(), "0".into()], "10s", 64).is_err());
}

#[test]
#[ignore = "requires the optional root-owned installed skills integration"]
fn installed_integration_freezes_the_selected_original_project() {
    use std::process::Command;

    let temporary = private_temp();
    let project = temporary.path().join("project");
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
    std::fs::write(project.join("tracked"), "initial").unwrap();
    git(&["add", "--", "tracked"]);
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
    std::fs::write(project.join("tracked"), "selected dirty bytes").unwrap();
    let projects = PhoneProjectStore::open(temporary.path().join("projects")).unwrap();
    let root = projects.register_root(&project).unwrap();
    let project_id = projects.register_project(&project, &[root]).unwrap();
    let source = PhoneSourceStore::open(temporary.path().join("sources")).unwrap();
    let snapshot = temporary.path().join("snapshot");
    let proof = source
        .freeze(&projects, &project_id, RUN, &snapshot, &["tracked".into()])
        .unwrap();
    assert_eq!(
        std::fs::read(snapshot.join("files/tracked")).unwrap(),
        b"selected dirty bytes"
    );
    assert_eq!(source.read(RUN).unwrap(), proof);
}
