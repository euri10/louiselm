//! Durable phone-action evidence and replay-guard integration tests.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "test fixtures and assertions"
)]

use std::{fs, sync::Arc, thread};

#[path = "support/phone_evidence.rs"]
mod phone_evidence;

use louiselm_capture::{ActionEvidenceError, ActionIdentity, ActionOutcome, ActionReservation};

fn identity(project: &std::path::Path, operation: u8) -> ActionIdentity {
    ActionIdentity {
        operation_id: format!("{operation:02x}").repeat(16),
        receiver_identity: "02".repeat(32),
        controller_id: "03".repeat(16),
        subject_id: "04".repeat(16),
        action: 3,
        payload_hash: "05".repeat(32),
        grant_id: "06".repeat(16),
        grant_revision: 1,
        period_id: "07".repeat(16),
        expected_revision: 2,
        project: fs::canonicalize(project).unwrap().to_str().unwrap().into(),
        agent: "codex".into(),
        recorded_at_ms: 1_000,
    }
}

#[test]
fn reservation_and_owner_outcome_survive_restart_without_payload() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("actions");
    phone_evidence::initialize_store(&path);
    let original = identity(temp.path(), 1);
    let store = phone_evidence::open(&path).unwrap();
    assert_eq!(
        store.lookup(&original.operation_id),
        Err(ActionEvidenceError::Unavailable)
    );
    assert_eq!(store.reserve(&original), Ok(ActionReservation::New));
    drop(store);

    let store = phone_evidence::open(&path).unwrap();
    let mut duplicate = original.clone();
    duplicate.recorded_at_ms += 10;
    assert_eq!(store.reserve(&duplicate), Ok(ActionReservation::Existing));
    let mut altered = original.clone();
    altered.payload_hash = "08".repeat(32);
    assert_eq!(store.reserve(&altered), Err(ActionEvidenceError::Conflict));
    assert_eq!(
        store.lookup(&original.operation_id).unwrap().outcome,
        ActionOutcome::Unknown
    );
    let accepted = ActionOutcome::Accepted {
        evidence: "09".repeat(32),
    };
    store.report(&original, &accepted, 2_000).unwrap();
    drop(store);

    let store = phone_evidence::open(&path).unwrap();
    let evidence = store.lookup(&original.operation_id).unwrap();
    assert_eq!(evidence.outcome, accepted);
    assert_eq!(evidence.updated_at_ms, 2_000);
    assert_eq!(store.reserve(&original), Ok(ActionReservation::Existing));
    let bytes = fs::read(path.join("actions.json")).unwrap();
    let text = String::from_utf8(bytes).unwrap();
    assert!(!text.contains("secret prompt"));
    assert!(!text.contains("payload\":\""));
}

#[test]
fn missing_or_damaged_store_fails_closed_across_restart() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("actions");
    phone_evidence::initialize_store(&path);
    let original = identity(temp.path(), 2);
    let store = phone_evidence::open(&path).unwrap();
    store.reserve(&original).unwrap();
    fs::remove_file(path.join("actions.json")).unwrap();
    assert_eq!(
        store.lookup(&original.operation_id),
        Err(ActionEvidenceError::Unavailable)
    );
    assert_eq!(
        store.reserve(&original),
        Err(ActionEvidenceError::Unavailable)
    );
    drop(store);
    assert!(matches!(
        phone_evidence::open(&path),
        Err(ActionEvidenceError::Unavailable)
    ));
}

#[test]
fn receiver_guard_refuses_missing_corrupt_and_rolled_back_history() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("actions");
    let guard = path.with_extension("guard");
    phone_evidence::initialize_store(&path);
    let original = identity(temp.path(), 2);
    let store = phone_evidence::open(&path).unwrap();
    store.reserve(&original).unwrap();
    let old_snapshot = fs::read(path.join("actions.json")).unwrap();
    let old_seal = fs::read(path.join("seal.json")).unwrap();
    store.reserve(&identity(temp.path(), 3)).unwrap();
    drop(store);

    let current_guard = fs::read(&guard).unwrap();
    fs::remove_file(&guard).unwrap();
    assert!(matches!(
        phone_evidence::open(&path),
        Err(ActionEvidenceError::Unavailable)
    ));
    fs::write(&guard, b"{}").unwrap();
    assert!(matches!(
        phone_evidence::open(&path),
        Err(ActionEvidenceError::Unavailable)
    ));
    fs::write(&guard, &current_guard).unwrap();
    fs::write(path.join("actions.json"), old_snapshot).unwrap();
    fs::write(path.join("seal.json"), old_seal).unwrap();
    assert!(matches!(
        phone_evidence::open(&path),
        Err(ActionEvidenceError::Unavailable)
    ));
}

#[test]
fn loss_of_archive_and_lock_does_not_erase_unknown_actions() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("actions");
    phone_evidence::initialize_store(&path);
    let original = identity(temp.path(), 2);
    let store = phone_evidence::open(&path).unwrap();
    assert_eq!(store.reserve(&original), Ok(ActionReservation::New));
    drop(store);
    fs::remove_dir_all(&path).unwrap();
    fs::remove_file(path.with_extension("lock")).unwrap();
    assert!(matches!(
        phone_evidence::open(&path),
        Err(ActionEvidenceError::Unavailable)
    ));
    assert!(!path.exists());
}

#[test]
fn losing_the_live_lock_refuses_further_access() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("actions");
    phone_evidence::initialize_store(&path);
    let store = phone_evidence::open(&path).unwrap();
    let original = identity(temp.path(), 2);
    store.reserve(&original).unwrap();
    fs::remove_file(path.with_extension("lock")).unwrap();
    assert_eq!(
        store.lookup(&original.operation_id),
        Err(ActionEvidenceError::Unavailable)
    );
}

#[test]
fn corruption_and_uncertain_writes_do_not_reopen_an_empty_store() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("actions");
    phone_evidence::initialize_store(&path);
    let original = identity(temp.path(), 4);
    let store = phone_evidence::open(&path).unwrap();
    store.reserve(&original).unwrap();
    let bytes = fs::read(path.join("actions.json")).unwrap();
    fs::write(path.join("actions.json"), b"{}").unwrap();
    assert_eq!(
        store.lookup(&original.operation_id),
        Err(ActionEvidenceError::Unavailable)
    );
    fs::write(path.join("actions.json"), bytes).unwrap();
    assert_eq!(
        store.reserve(&original),
        Err(ActionEvidenceError::Unavailable)
    );
    drop(store);
    let store = phone_evidence::open(&path).unwrap();
    assert_eq!(store.reserve(&original), Ok(ActionReservation::Existing));
    drop(store);
    fs::rename(&path, temp.path().join("lost-actions")).unwrap();
    assert!(matches!(
        phone_evidence::open(&path),
        Err(ActionEvidenceError::Unavailable)
    ));
}

#[test]
fn only_the_exact_owner_can_record_one_terminal_outcome() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("actions");
    phone_evidence::initialize_store(&path);
    let original = identity(temp.path(), 5);
    let store = phone_evidence::open(&path).unwrap();
    store.reserve(&original).unwrap();
    let rejected = ActionOutcome::Rejected {
        evidence: "12".repeat(32),
    };
    let mut other = original.clone();
    other.subject_id = "13".repeat(16);
    assert_eq!(
        store.report(&other, &rejected, 2_000),
        Err(ActionEvidenceError::Conflict)
    );
    assert_eq!(
        store.report(&original, &rejected, 999),
        Err(ActionEvidenceError::Malformed)
    );
    store.report(&original, &rejected, 2_000).unwrap();
    store.report(&original, &rejected, 2_500).unwrap();
    assert_eq!(
        store.lookup(&original.operation_id).unwrap().updated_at_ms,
        2_000
    );
    assert_eq!(
        store.report(&original, &ActionOutcome::Unknown, 3_000),
        Err(ActionEvidenceError::Conflict)
    );
}

#[test]
fn owner_reported_fence_survives_restart_and_never_reopens_the_original() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("actions");
    phone_evidence::initialize_store(&path);
    let original = identity(temp.path(), 6);
    let fence = ActionOutcome::FencedNoEffect {
        evidence: "16".repeat(32),
    };
    let store = phone_evidence::open(&path).unwrap();
    store.reserve(&original).unwrap();
    store.report(&original, &fence, 2_000).unwrap();
    drop(store);
    let store = phone_evidence::open(&path).unwrap();
    assert_eq!(store.lookup(&original.operation_id).unwrap().outcome, fence);
    assert_eq!(store.reserve(&original), Ok(ActionReservation::Existing));
    assert_eq!(
        store.report(
            &original,
            &ActionOutcome::Accepted {
                evidence: "17".repeat(32)
            },
            3_000
        ),
        Err(ActionEvidenceError::Conflict)
    );
}

#[test]
fn owner_reports_and_fences_are_single_winner() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("actions");
    phone_evidence::initialize_store(&path);
    let original = identity(temp.path(), 3);
    let store = Arc::new(phone_evidence::open(&path).unwrap());
    store.reserve(&original).unwrap();
    let fenced = ActionOutcome::FencedNoEffect {
        evidence: "10".repeat(32),
    };
    let accepted = ActionOutcome::Accepted {
        evidence: "11".repeat(32),
    };
    let threads: Vec<_> = [fenced.clone(), accepted.clone()]
        .into_iter()
        .map(|outcome| {
            let store = Arc::clone(&store);
            let original = original.clone();
            thread::spawn(move || store.report(&original, &outcome, 2_000))
        })
        .collect();
    let results: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| **result == Err(ActionEvidenceError::Conflict))
            .count(),
        1
    );
    let outcome = store.lookup(&original.operation_id).unwrap().outcome;
    assert!(outcome == fenced || outcome == accepted);
    assert_eq!(store.reserve(&original), Ok(ActionReservation::Existing));
}
