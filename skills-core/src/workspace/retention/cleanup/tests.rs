//! Filesystem cleanup contract uses real locks, metadata, unlink and restart reads.
#![allow(
    clippy::unwrap_used,
    reason = "Fixtures assert exact filesystem outcomes."
)]
use super::super::tests::{launch, owner};
use super::*;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt, symlink};

struct Fixture {
    temp: tempfile::TempDir,
    marker: SealedStorage,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        Store::create(&temp.path().join("policy")).unwrap();
        Store::lock(&temp.path().join("policy"), owner())
            .unwrap()
            .register(&launch(), 100)
            .unwrap();
        fs::DirBuilder::new()
            .mode(0o711)
            .create(temp.path().join("sessions"))
            .unwrap();
        fs::DirBuilder::new()
            .mode(0o711)
            .create(temp.path().join("sessions/session"))
            .unwrap();
        let launch = launch();
        let digest = crate::Digest::of(b"bytes").to_string();
        let marker = SealedStorage {
            schema: MARKER_SCHEMA.into(),
            inputs: InputReferences {
                manifest: launch.session_input_manifest_id.clone(),
                generation: launch.skill_generation_id.clone(),
                snapshot: digest.clone(),
                base: digest.clone(),
                cache: digest.clone(),
                runtime: digest,
                isolation: "isolation".into(),
            },
            launch,
            disposed: false,
        };
        marker
            .persist(&temp.path().join("sessions/session"))
            .unwrap();
        fs::write(temp.path().join("sessions/session/a"), "private evidence").unwrap();
        fs::write(temp.path().join("sessions/session/b"), "private cache").unwrap();
        Self { temp, marker }
    }
    fn seal(&mut self, disposed: bool) {
        fs::set_permissions(
            self.temp.path().join("sessions/session"),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        self.marker.disposed = disposed;
        self.marker
            .persist(&self.temp.path().join("sessions/session"))
            .unwrap();
    }
    fn clean(&self, now: u64) -> CleanupReport {
        sweep(
            &self.temp.path().join("sessions"),
            &self.temp.path().join("policy"),
            owner(),
            owner().0,
            now,
        )
        .unwrap()
    }
    fn store(&self) -> Store {
        Store::lock(&self.temp.path().join("policy"), owner()).unwrap()
    }
}

#[test]
fn expired_live_parked_unproven_and_pinned_storage_remains() {
    let mut f = Fixture::new();
    let expired = 100 + super::super::DEFAULT_RETENTION_MS;
    assert_eq!(
        f.clean(expired).retained,
        1,
        "live/Parked root is not sealed"
    );
    f.seal(false);
    assert_eq!(
        f.clean(expired).failed,
        1,
        "seal alone is not proven Disposal"
    );
    f.seal(true);
    assert_eq!(f.clean(expired - 1).retained, 1);
    f.store().pin("session", true).unwrap();
    assert_eq!(f.clean(expired).retained, 1);
    f.store().pin("session", false).unwrap();
    assert_eq!(f.clean(expired).removed, 1);
    assert!(!f.temp.path().join("sessions/session/a").exists());
    let record = f.store().read("session").unwrap();
    assert_eq!(record.primary_evidence, PrimaryEvidence::Removed);
    assert_eq!(record.evidence.inputs, Some(f.marker.inputs.clone()));
    assert!(f.store().pin("session", true).is_err());
    assert_eq!(
        fs::metadata(f.temp.path().join("sessions/session"))
            .unwrap()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        f.clean(expired).removed,
        0,
        "repeated timer never renews or reuses storage"
    );
}

#[test]
fn partial_cleanup_is_durable_unavailable_and_retries_under_same_barrier() {
    let mut f = Fixture::new();
    f.seal(true);
    FAIL_AFTER.set(1);
    let result = f.clean(u64::MAX);
    FAIL_AFTER.set(usize::MAX);
    assert_eq!(result.failed, 1);
    assert_eq!(
        f.store().read("session").unwrap().primary_evidence,
        PrimaryEvidence::CleanupIncomplete
    );
    assert!(f.store().pin("session", true).is_err());
    assert!(
        f.temp
            .path()
            .join("sessions/session/workspace-retention.json")
            .is_file()
    );
    assert_eq!(f.clean(u64::MAX).removed, 1);
}

#[test]
fn corrupt_or_foreign_state_retains_bytes_and_links_never_escape() {
    let mut f = Fixture::new();
    f.seal(true);
    f.marker.launch.run_id = "foreign".into();
    f.marker
        .persist(&f.temp.path().join("sessions/session"))
        .unwrap();
    assert_eq!(f.clean(u64::MAX).failed, 1);
    assert!(f.temp.path().join("sessions/session/a").exists());
    f.marker.launch = launch();
    f.seal(true);
    let outside = f.temp.path().join("outside");
    fs::write(&outside, "keep").unwrap();
    symlink(&outside, f.temp.path().join("sessions/session/escape")).unwrap();
    assert_eq!(f.clean(u64::MAX).removed, 1);
    assert_eq!(fs::read(outside).unwrap(), b"keep");
}

#[test]
fn cleanup_and_pin_use_the_same_lock() {
    let mut f = Fixture::new();
    f.seal(true);
    let _pin_lock = f.store();
    assert!(
        sweep(
            &f.temp.path().join("sessions"),
            &f.temp.path().join("policy"),
            owner(),
            owner().0,
            u64::MAX
        )
        .is_err()
    );
    assert!(f.temp.path().join("sessions/session/a").exists());
}

#[test]
fn missing_policy_corrupt_marker_and_symlink_root_never_delete() {
    let mut f = Fixture::new();
    f.seal(true);
    let directory = f.temp.path().join("sessions/session");
    fs::write(directory.join(MARKER), b"corrupt").unwrap();
    assert_eq!(f.clean(u64::MAX).failed, 1);
    assert!(directory.join("a").exists());
    f.seal(true);
    fs::remove_file(f.temp.path().join("policy/session.json")).unwrap();
    assert_eq!(f.clean(u64::MAX).failed, 1);
    assert!(directory.join("a").exists());
    f.store().register(&launch(), 100).unwrap();
    let moved = f.temp.path().join("outside-session");
    fs::rename(&directory, &moved).unwrap();
    symlink(&moved, &directory).unwrap();
    assert_eq!(f.clean(u64::MAX).failed, 1);
    assert!(moved.join("a").exists());
}

#[test]
fn nested_mount_is_refused_before_any_unlink() {
    if std::env::var_os("LOUISELM_REQUIRE_TOOL_ISOLATION").is_none() {
        eprintln!("skipping: nested-mount retention check requires the disposable VM");
        return;
    }
    assert!(rustix::process::geteuid().is_root());
    let mut f = Fixture::new();
    f.seal(true);
    let mount = f.temp.path().join("sessions/session/mounted");
    fs::create_dir(&mount).unwrap();
    let outside = f.temp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("keep"), b"primary").unwrap();
    assert!(
        std::process::Command::new("/usr/bin/mount")
            .arg("--bind")
            .arg(&outside)
            .arg(&mount)
            .status()
            .unwrap()
            .success()
    );
    let report = f.clean(u64::MAX);
    let unchanged =
        outside.join("keep").exists() && f.temp.path().join("sessions/session/a").exists();
    assert!(
        std::process::Command::new("/usr/bin/umount")
            .arg(&mount)
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(report.failed, 1);
    assert!(unchanged);
    assert_eq!(
        f.store().read("session").unwrap().primary_evidence,
        PrimaryEvidence::NotChecked
    );
}
