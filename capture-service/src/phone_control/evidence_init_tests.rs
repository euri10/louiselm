//! Disposable first-store initialization and publication ordering.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};

use serde::Serialize;
use tempfile::TempDir;

use super::{
    FirstStoreInput, FirstStoresReceipt, StoreOwner, initialize_first_stores, verify_first_stores,
};
use crate::phone_control::{
    ActionEvidenceError, ActionEvidenceKind, ActionEvidenceStore,
    barrier::{
        Barrier,
        namespace_exclusion::{Registrar, Startup},
    },
};

struct Fixture {
    root: TempDir,
    parent: PathBuf,
    capture: PathBuf,
    parent_fd: File,
    capture_fd: File,
    private: File,
    stage: File,
    registrar: Registrar,
    startup: Startup,
    receiver: String,
    generation: String,
    owner: StoreOwner,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let parent = root.path().join("louiselm");
        let capture = parent.join("capture");
        let stage_path = root.path().join("private-staging");
        fs::create_dir_all(&capture).unwrap();
        fs::create_dir(&stage_path).unwrap();
        fs::set_permissions(&stage_path, fs::Permissions::from_mode(0o700)).unwrap();
        let parent_fd = File::open(&parent).unwrap();
        let capture_fd = File::open(&capture).unwrap();
        let private = File::open(root.path()).unwrap();
        let stage = File::open(stage_path).unwrap();
        let startup = Startup::for_fixture(root.path()).unwrap();
        let registrar = startup.registrar().unwrap();
        let metadata = parent_fd.metadata().unwrap();
        let owner = StoreOwner {
            uid: if metadata.uid() == 0 {
                1000
            } else {
                metadata.uid()
            },
            gid: if metadata.gid() == 0 {
                1000
            } else {
                metadata.gid()
            },
        };
        Self {
            root,
            parent,
            capture,
            parent_fd,
            capture_fd,
            private,
            stage,
            registrar,
            startup,
            receiver: "02".repeat(32),
            generation: "09".repeat(16),
            owner,
        }
    }

    fn input(&self) -> FirstStoreInput<'_> {
        FirstStoreInput {
            parent: &self.parent_fd,
            capture: &self.capture_fd,
            private_state: &self.private,
            parent_stage: &self.stage,
            capture_stage: &self.stage,
            receiver: &self.receiver,
            generation: &self.generation,
            owner: self.owner,
            _startup: &self.startup,
            _registrar: &self.registrar,
        }
    }

    fn tls(&self) -> PathBuf {
        self.capture.join("tls")
    }
}

fn persist_private(path: &Path, value: &impl Serialize, parent: &Path) -> io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    if fs::read(path)? != bytes {
        return Err(io::Error::other("private record changed"));
    }
    File::open(parent)?.sync_all()
}

fn assert_owned_artifacts(fixture: &Fixture) {
    for (path, mode) in [
        (fixture.parent.join("workflow"), 0o700),
        (fixture.parent.join("workflow/phone-action-owner"), 0o700),
        (
            fixture.parent.join("workflow/phone-action-owner.lock"),
            0o600,
        ),
        (
            fixture
                .parent
                .join("workflow/phone-action-owner/actions.json"),
            0o600,
        ),
        (
            fixture.parent.join("workflow/phone-action-owner/seal.json"),
            0o600,
        ),
        (fixture.capture.join("phone-control"), 0o700),
        (fixture.capture.join("phone-control/action-evidence"), 0o700),
        (
            fixture.capture.join("phone-control/action-evidence.lock"),
            0o600,
        ),
        (
            fixture
                .capture
                .join("phone-control/action-evidence/actions.json"),
            0o600,
        ),
        (
            fixture
                .capture
                .join("phone-control/action-evidence/seal.json"),
            0o600,
        ),
        (fixture.tls(), 0o700),
        (fixture.tls().join("phone-action-owner.json"), 0o600),
        (fixture.tls().join("phone-action-owner.json.lock"), 0o600),
        (fixture.tls().join("phone-action-evidence.json"), 0o600),
        (fixture.tls().join("phone-action-evidence.json.lock"), 0o600),
        (fixture.tls().join("phone-admission-barrier.json"), 0o600),
        (fixture.tls().join("phone-admission-barrier.lock"), 0o600),
        (fixture.tls().join("phone-admission-registrar.lock"), 0o600),
        (fixture.tls().join("phone-admission-cutoff.json"), 0o600),
    ] {
        let metadata = fs::metadata(path).unwrap();
        assert_eq!(
            (metadata.uid(), metadata.gid(), metadata.mode() & 0o7777),
            (fixture.owner.uid, fixture.owner.gid, mode)
        );
    }
}

fn assert_reopened_store_bindings(fixture: &Fixture) {
    let owner = ActionEvidenceStore::open_bound(
        fixture.parent.join("workflow/phone-action-owner"),
        fixture.tls().join("phone-action-owner.json"),
        &fixture.receiver,
        ActionEvidenceKind::Owner,
    )
    .unwrap();
    let archive = ActionEvidenceStore::open_bound(
        fixture.capture.join("phone-control/action-evidence"),
        fixture.tls().join("phone-action-evidence.json"),
        &fixture.receiver,
        ActionEvidenceKind::Archive,
    )
    .unwrap();
    assert_eq!(owner.bound_generation(), Some(fixture.generation.as_str()));
    assert_eq!(
        archive.bound_generation(),
        Some(fixture.generation.as_str())
    );
    assert!(owner.bound_barrier().is_some());
    assert_eq!(owner.bound_barrier(), archive.bound_barrier());
}

#[test]
fn both_production_stores_are_committed_before_receiver_publication() {
    let fixture = Fixture::new();
    let input = fixture.input();
    let staged = fixture.root.path().join("private-barrier.json");
    let committed = fixture.root.path().join("private-committed.json");
    let receipt = initialize_first_stores(
        &input,
        |binding| {
            assert!(!fixture.parent.join("workflow/phone-action-owner").exists());
            assert!(
                !fixture
                    .capture
                    .join("phone-control/action-evidence")
                    .exists()
            );
            assert!(!fixture.tls().join("receiver-key.pem").exists());
            persist_private(&staged, binding, fixture.root.path())
        },
        |receipt| {
            assert!(staged.exists());
            assert!(
                fixture
                    .parent
                    .join("workflow/phone-action-owner/actions.json")
                    .exists()
            );
            assert!(
                fixture
                    .capture
                    .join("phone-control/action-evidence/actions.json")
                    .exists()
            );
            assert!(fixture.tls().join("phone-action-owner.json").exists());
            assert!(fixture.tls().join("phone-action-evidence.json").exists());
            assert!(!fixture.tls().join("receiver-key.pem").exists());
            assert!(!fixture.tls().join("receiver-cert.pem").exists());
            let barrier =
                Barrier::existing(&fixture.tls(), &fixture.receiver, receipt.barrier.clone())?;
            assert!(matches!(
                barrier.with_generation(&fixture.generation, || ()),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock
            ));
            persist_private(&committed, receipt, fixture.root.path())
        },
    )
    .unwrap();
    assert!(staged.exists());
    assert!(committed.exists());
    let record = serde_json::to_value(&receipt).unwrap();
    let owner_digest = record["owner_snapshot_sha256"].as_str().unwrap();
    let archive_digest = record["archive_snapshot_sha256"].as_str().unwrap();
    assert_eq!(owner_digest, archive_digest);
    verify_first_stores(&input, &receipt).unwrap();
    assert_owned_artifacts(&fixture);

    let moved = fixture.parent.join("moved-capture");
    fs::rename(&fixture.capture, &moved).unwrap();
    fs::create_dir(&fixture.capture).unwrap();
    assert!(matches!(
        verify_first_stores(&input, &receipt),
        Err(ActionEvidenceError::Unavailable)
    ));
    fs::remove_dir(&fixture.capture).unwrap();
    fs::rename(&moved, &fixture.capture).unwrap();
    verify_first_stores(&input, &receipt).unwrap();

    assert_reopened_store_bindings(&fixture);
    let guard = fixture.tls().join("phone-action-evidence.json");
    let retained_guard = fs::read(&guard).unwrap();
    fs::write(&guard, b"{}").unwrap();
    assert!(matches!(
        verify_first_stores(&input, &receipt),
        Err(ActionEvidenceError::Unavailable)
    ));
    fs::write(&guard, retained_guard).unwrap();
    verify_first_stores(&input, &receipt).unwrap();
    assert!(matches!(
        initialize_first_stores(&input, |_| Ok(()), |_| Ok(())),
        Err(ActionEvidenceError::Unavailable)
    ));
}

#[test]
fn failed_private_barrier_commit_never_creates_store_records_or_publishes_a_key() {
    let fixture = Fixture::new();
    let input = fixture.input();
    let mut reached = false;
    assert!(matches!(
        initialize_first_stores(
            &input,
            |_| {
                reached = true;
                Err(io::Error::other("injected protected storage failure"))
            },
            |_| panic!("store commit must not run")
        ),
        Err(ActionEvidenceError::Unavailable)
    ));
    assert!(reached);
    assert!(!fixture.parent.join("workflow/phone-action-owner").exists());
    assert!(
        !fixture
            .capture
            .join("phone-control/action-evidence")
            .exists()
    );
    assert!(!fixture.tls().join("receiver-key.pem").exists());
    assert!(matches!(
        initialize_first_stores(&input, |_| Ok(()), |_| Ok(())),
        Err(ActionEvidenceError::Unavailable)
    ));
}

#[test]
fn failed_store_commit_keeps_receiver_unpublished_and_refuses_fresh_retry() {
    let fixture = Fixture::new();
    let input = fixture.input();
    let mut reached = false;
    assert!(matches!(
        initialize_first_stores(
            &input,
            |_| Ok(()),
            |_| {
                reached = true;
                Err(io::Error::other("injected protected commit failure"))
            }
        ),
        Err(ActionEvidenceError::Unavailable)
    ));
    assert!(reached);
    assert!(
        fixture
            .parent
            .join("workflow/phone-action-owner/actions.json")
            .exists()
    );
    assert!(
        fixture
            .capture
            .join("phone-control/action-evidence/actions.json")
            .exists()
    );
    assert!(!fixture.tls().join("receiver-key.pem").exists());
    assert!(!fixture.tls().join("receiver-cert.pem").exists());
    assert!(matches!(
        initialize_first_stores(&input, |_| Ok(()), |_| Ok(())),
        Err(ActionEvidenceError::Unavailable)
    ));
}

#[test]
fn lost_commit_reply_can_reverify_only_the_exact_durable_receipt() {
    let fixture = Fixture::new();
    let input = fixture.input();
    let committed = fixture.root.path().join("committed.json");
    assert!(matches!(
        initialize_first_stores(
            &input,
            |_| Ok(()),
            |receipt| {
                persist_private(&committed, receipt, fixture.root.path())?;
                Err(io::Error::other("injected lost commit reply"))
            }
        ),
        Err(ActionEvidenceError::Unavailable)
    ));
    assert!(!fixture.tls().join("receiver-key.pem").exists());
    let receipt: FirstStoresReceipt =
        serde_json::from_slice(&fs::read(&committed).unwrap()).unwrap();
    verify_first_stores(&input, &receipt).unwrap();
    assert!(matches!(
        initialize_first_stores(&input, |_| Ok(()), |_| Ok(())),
        Err(ActionEvidenceError::Unavailable)
    ));
}

#[test]
fn verified_tls_handle_never_follows_a_replacement_directory() {
    let fixture = Fixture::new();
    let input = fixture.input();
    let receipt = initialize_first_stores(&input, |_| Ok(()), |_| Ok(())).unwrap();
    let verified_tls = verify_first_stores(&input, &receipt).unwrap();
    let original = verified_tls.metadata().unwrap();
    let retained = fixture.capture.join("retained-tls");
    fs::rename(fixture.tls(), &retained).unwrap();
    fs::create_dir(fixture.tls()).unwrap();
    let retained_path = PathBuf::from(format!(
        "/proc/self/fd/{}/receiver-key.pem",
        std::os::fd::AsRawFd::as_raw_fd(&verified_tls)
    ));
    fs::write(retained_path, b"candidate").unwrap();
    assert_eq!(
        fs::read(retained.join("receiver-key.pem")).unwrap(),
        b"candidate"
    );
    assert!(!fixture.tls().join("receiver-key.pem").exists());
    assert_ne!(fs::metadata(fixture.tls()).unwrap().ino(), original.ino());
    assert!(matches!(
        verify_first_stores(&input, &receipt),
        Err(ActionEvidenceError::Unavailable)
    ));
}

#[test]
fn an_existing_owner_journal_is_never_replaced_or_chowned() {
    let fixture = Fixture::new();
    let journal = fixture.parent.join("workflow/phone-action-owner");
    fs::create_dir_all(&journal).unwrap();
    fs::write(journal.join("actions.json"), b"retained evidence").unwrap();
    let before = fs::metadata(&journal).unwrap();
    assert!(matches!(
        initialize_first_stores(&fixture.input(), |_| Ok(()), |_| Ok(())),
        Err(ActionEvidenceError::Unavailable)
    ));
    let after = fs::metadata(&journal).unwrap();
    assert_eq!(
        (before.uid(), before.gid(), before.ino()),
        (after.uid(), after.gid(), after.ino())
    );
    assert_eq!(
        fs::read(journal.join("actions.json")).unwrap(),
        b"retained evidence"
    );
    assert!(!fixture.tls().exists());
}

#[test]
fn a_racing_destination_is_never_replaced_or_chowned() {
    let fixture = Fixture::new();
    let journal = fixture.parent.join("workflow/phone-action-owner");
    let mut before = None;
    assert!(matches!(
        initialize_first_stores(
            &fixture.input(),
            |_| {
                fs::create_dir(&journal)?;
                fs::set_permissions(&journal, fs::Permissions::from_mode(0o751))?;
                fs::write(journal.join("actions.json"), b"do not replace")?;
                let metadata = fs::metadata(&journal)?;
                before = Some((
                    metadata.uid(),
                    metadata.gid(),
                    metadata.ino(),
                    metadata.mode(),
                ));
                Ok(())
            },
            |_| panic!("store commit must not run")
        ),
        Err(ActionEvidenceError::Unavailable)
    ));
    let after = fs::metadata(&journal).unwrap();
    assert_eq!(
        before,
        Some((after.uid(), after.gid(), after.ino(), after.mode()))
    );
    assert_eq!(
        fs::read(journal.join("actions.json")).unwrap(),
        b"do not replace"
    );
    assert!(!fixture.tls().join("receiver-key.pem").exists());
}

#[test]
fn a_public_staging_directory_refuses_before_any_artifact_is_created() {
    let fixture = Fixture::new();
    fs::set_permissions(
        fixture.root.path().join("private-staging"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    assert!(matches!(
        initialize_first_stores(&fixture.input(), |_| Ok(()), |_| Ok(())),
        Err(ActionEvidenceError::Unavailable)
    ));
    assert!(!fixture.tls().exists());
    assert!(!fixture.parent.join("workflow").exists());
}
