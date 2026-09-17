//! Retention survives reopen, serializes pins with cleanup, and refuses uncertainty.
#![allow(
    clippy::unwrap_used,
    reason = "Fixtures assert persistence and refusals."
)]

use super::*;
use crate::launch::{LaunchRequest, REQUEST_SCHEMA};

pub(super) fn launch() -> LaunchRequest {
    LaunchRequest {
        schema: REQUEST_SCHEMA.into(),
        protocol_version: 1,
        request_id: "request".into(),
        authorization_id: "authorization".into(),
        session_id: "session".into(),
        run_id: "run".into(),
        agent_id: "agent".into(),
        envelope_id: "envelope".into(),
        envelope_revision: 1,
        skill_generation_id: crate::Digest::of(b"generation").to_string(),
        session_input_manifest_id: crate::Digest::of(b"inputs").to_string(),
    }
}

pub(super) fn owner() -> (u32, u32) {
    (
        rustix::process::geteuid().as_raw(),
        rustix::process::getegid().as_raw(),
    )
}

#[test]
fn policy_and_pin_survive_restart_without_renewing_deadline() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("retention");
    Store::create(&path).unwrap();
    let mut store = Store::lock(&path, owner()).unwrap();
    let record = store.register(&launch(), 100).unwrap();
    assert_eq!(record.expires_at_ms, 100 + DEFAULT_RETENTION_MS);
    assert!(!record.pinned);
    store.pin("session", true).unwrap();
    drop(store);
    let store = Store::lock(&path, owner()).unwrap();
    let record = store.read("session").unwrap();
    assert!(record.pinned);
    assert_eq!(record.expires_at_ms, 100 + DEFAULT_RETENTION_MS);
    assert_eq!(record.primary_evidence, PrimaryEvidence::NotChecked);
    assert!(
        Store::lock(&path, owner()).is_err(),
        "cleanup cannot race pinning"
    );
}

#[test]
fn no_pin_can_claim_bytes_after_cleanup_begins() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("retention");
    Store::create(&path).unwrap();
    let mut store = Store::lock(&path, owner()).unwrap();
    let mut record = store.register(&launch(), 100).unwrap();
    record.primary_evidence = PrimaryEvidence::CleanupIncomplete;
    store.write(&record).unwrap();
    assert!(store.pin("session", true).is_err());
    assert!(store.read("../session").is_err());
}
