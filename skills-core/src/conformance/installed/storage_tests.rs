#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Private store fixtures assert durable evidence."
)]

use super::super::PROFILE;
use std::os::unix::fs::PermissionsExt;

fn private_root() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
    directory
}
use super::*;
use crate::conformance::{Check, Cleanup, Outcome, REPORT_SCHEMA, REQUIRED_CHECKS};

fn host() -> HostSnapshot {
    let digest = Digest::of(b"fixture").to_string();
    HostSnapshot {
        profile: PROFILE.into(),
        machine_digest: digest.clone(),
        boot_id: "00000000-0000-0000-0000-000000000001".into(),
        release_digest: digest.clone(),
        inputs: ["launcher", "backend", "policy", "kernel", "loader"]
            .into_iter()
            .map(|key| (key.into(), digest.clone()))
            .collect(),
    }
}

fn report() -> Report {
    Report {
        schema: REPORT_SCHEMA.into(),
        scope: Scope::InstalledHost,
        checks: REQUIRED_CHECKS
            .iter()
            .map(|name| Check {
                name: (*name).into(),
                control: Outcome::Allowed,
                confined: Outcome::Denied("kernel denial".into()),
            })
            .collect(),
        completed: true,
        cleanup: Cleanup::Confirmed,
    }
}

#[test]
fn completed_store_unlocks_even_while_a_duplicate_descriptor_exists() {
    let directory = private_root();
    let root = directory.path().join("certificates");
    let uid = rustix::process::geteuid().as_raw();
    let store = CertificateStore::open_owned(&root, uid).unwrap();
    let duplicate = store.lock.try_clone().unwrap();
    assert!(matches!(
        CertificateStore::open_owned(&root, uid),
        Err(CertificationError::Pending)
    ));
    drop(store);
    let reopened = CertificateStore::open_owned(&root, uid).unwrap();
    drop(reopened);
    drop(duplicate);
}

#[test]
fn interrupted_attempt_never_replays_or_loses_observed_failure() {
    let directory = private_root();
    let root = directory.path().join("certificates");
    let uid = rustix::process::geteuid().as_raw();
    let mut store = CertificateStore::open_owned(&root, uid).unwrap();
    store.begin(&host()).unwrap();
    let mut partial = report();
    partial.completed = false;
    partial.checks[0].confined = Outcome::Allowed;
    store.observe(&partial).unwrap();
    drop(store);
    assert!(matches!(
        CertificateStore::open_owned(&root, uid),
        Err(CertificationError::Pending)
    ));
    let status = CertificateStore::inspect_owned(&root, &host(), uid).unwrap();
    assert!(status.pending);
    assert!(status.certificate.is_none());
    assert_eq!(status.history.failures.len(), 1);
}

#[test]
fn failures_survive_reboot_and_incomplete_runs_but_covered_pass_clears_them() {
    let directory = private_root();
    let root = directory.path().join("certificates");
    let uid = rustix::process::geteuid().as_raw();
    let mut store = CertificateStore::open_owned(&root, uid).unwrap();
    let mut observed = report();
    observed.checks[0].confined = Outcome::Allowed;
    store.begin(&host()).unwrap();
    store
        .finish(&Certificate::new(host(), observed).unwrap())
        .unwrap();
    drop(store);
    let mut rebooted = host();
    rebooted.boot_id = "00000000-0000-0000-0000-000000000002".into();
    rebooted.release_digest = Digest::of(b"new release").to_string();
    let mut store = CertificateStore::open_owned(&root, uid).unwrap();
    store.begin(&rebooted).unwrap();
    let mut incomplete = report();
    incomplete.completed = false;
    store
        .finish(&Certificate::new(rebooted.clone(), incomplete).unwrap())
        .unwrap();
    assert_eq!(
        CertificateStore::inspect_owned(&root, &rebooted, uid)
            .unwrap()
            .history
            .failures
            .len(),
        1
    );
    store.begin(&rebooted).unwrap();
    store
        .finish(&Certificate::new(rebooted.clone(), report()).unwrap())
        .unwrap();
    let status = CertificateStore::inspect_owned(&root, &rebooted, uid).unwrap();
    assert!(status.history.failures.is_empty());
    assert!(status.certificate.is_some());
    // The failed old-host certificate remains inspectable, never passing.
    assert!(
        CertificateStore::inspect_owned(&root, &host(), uid)
            .unwrap()
            .certificate
            .is_some_and(|certificate| !certificate.is_current(&host()))
    );
    fs::write(root.join("state.json"), b"{}").unwrap();
    drop(store);
    assert!(CertificateStore::open_owned(&root, uid).is_err());
}

#[test]
fn admission_inspection_keeps_matching_nonpassing_evidence_and_waiver_condition() {
    use crate::conformance::admission::{self, Admission, Attendance, Condition, Request, Waiver};
    let directory = private_root();
    let root = directory.path().join("certificates");
    let uid = rustix::process::geteuid().as_raw();
    let host = host();
    let mut store = CertificateStore::open_owned(&root, uid).unwrap();
    let mut incomplete = report();
    incomplete.completed = false;
    let certificate = Certificate::new(host.clone(), incomplete).unwrap();
    store.begin(&host).unwrap();
    store.finish(&certificate).unwrap();
    drop(store);
    let status = CertificateStore::inspect_owned(&root, &host, uid).unwrap();
    let missing_waiver = Waiver {
        session_id: "session".into(),
        condition: Condition::Missing,
    };
    assert_eq!(
        admission::evaluate(
            &status,
            &host,
            &Request {
                session_id: "session",
                attendance: Attendance::Interactive,
                waiver: Some(&missing_waiver),
            }
        ),
        Admission::Waivable(Condition::Stale),
        "a Missing waiver cannot cover existing nonpassing evidence"
    );
    assert_eq!(status.certificate, Some(certificate));
}

#[test]
fn cleanup_failure_and_tampered_report_never_become_success() {
    let directory = private_root();
    let root = directory.path().join("certificates");
    let uid = rustix::process::geteuid().as_raw();
    let mut store = CertificateStore::open_owned(&root, uid).unwrap();
    let mut observed = report();
    observed.cleanup = Cleanup::Unconfirmed;
    store.begin(&host()).unwrap();
    store
        .finish(&Certificate::new(host(), observed).unwrap())
        .unwrap();
    store.begin(&host()).unwrap();
    let certificate = Certificate::new(host(), report()).unwrap();
    store.finish(&certificate).unwrap();
    let status = CertificateStore::inspect_owned(&root, &host(), uid).unwrap();
    assert_eq!(status.history.failures[0].check, "cleanup");
    let path = root.join(
        report_name(&Digest::of(&certificate.canonical_bytes().unwrap()).to_string()).unwrap(),
    );
    fs::write(path, b"{}").unwrap();
    assert!(CertificateStore::inspect_owned(&root, &host(), uid).is_err());
}
