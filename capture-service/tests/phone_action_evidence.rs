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
fn owner_guard_cannot_be_used_for_the_capture_archive() {
    let temp = tempfile::tempdir().unwrap();
    let archive = temp.path().join("archive");
    let owner = temp.path().join("owner");
    let receiver = "02".repeat(32);
    phone_evidence::initialize_store(&archive);
    let owner_guard = owner.with_extension("guard");
    phone_evidence::initialize(
        &owner,
        &owner_guard,
        &receiver,
        louiselm_capture::ActionEvidenceKind::Owner,
    );
    fs::copy(owner_guard, archive.with_extension("guard")).unwrap();
    assert!(matches!(
        phone_evidence::open(&archive),
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
fn replacing_the_store_lock_cannot_create_a_concurrent_writer() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("actions");
    phone_evidence::initialize_store(&path);
    let store = phone_evidence::open(&path).unwrap();
    store.reserve(&identity(temp.path(), 2)).unwrap();
    fs::remove_file(path.with_extension("lock")).unwrap();
    fs::write(path.with_extension("lock"), b"").unwrap();
    assert!(matches!(
        phone_evidence::open(&path),
        Err(ActionEvidenceError::Unavailable)
    ));
}

#[cfg(target_os = "linux")]
#[test]
fn same_process_reopen_cannot_release_a_live_store_or_guard_lock() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("actions");
    phone_evidence::initialize_store(&path);
    let store = phone_evidence::open(&path).unwrap();
    assert!(matches!(
        phone_evidence::open(&path),
        Err(ActionEvidenceError::Unavailable)
    ));
    let other_guard = temp.path().join("other-guard");
    fs::copy(path.with_extension("guard"), &other_guard).unwrap();
    fs::write(temp.path().join("other-guard.lock"), b"").unwrap();
    assert!(matches!(
        louiselm_capture::ActionEvidenceStore::open_bound(
            &path,
            &other_guard,
            &"02".repeat(32),
            louiselm_capture::ActionEvidenceKind::Archive,
        ),
        Err(ActionEvidenceError::Unavailable)
    ));

    let mut guard_lock = path.with_extension("guard").into_os_string();
    guard_lock.push(".lock");
    for lock in [path.with_extension("lock"), guard_lock.into()] {
        // Accept either existing flock or process-owned record locks while
        // preventing a rejected same-process open from releasing the lock.
        let status = std::process::Command::new("python3")
            .args([
                "-c",
                "import fcntl, sys\nf = open(sys.argv[1], 'r+')\ntry:\n fcntl.flock(f, fcntl.LOCK_EX | fcntl.LOCK_NB)\n fcntl.lockf(f, fcntl.LOCK_EX | fcntl.LOCK_NB)\nexcept BlockingIOError:\n sys.exit(0)\nsys.exit(1)",
            ])
            .arg(lock)
            .status()
            .unwrap();
        assert!(status.success(), "another process acquired a live lock");
    }
    drop(store);
}

#[test]
fn losing_the_receiver_guard_lock_refuses_live_and_restarted_access() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("actions");
    phone_evidence::initialize_store(&path);
    let store = phone_evidence::open(&path).unwrap();
    let original = identity(temp.path(), 2);
    store.reserve(&original).unwrap();
    let mut lock = path.with_extension("guard").into_os_string();
    lock.push(".lock");
    fs::remove_file(lock).unwrap();
    assert_eq!(
        store.lookup(&original.operation_id),
        Err(ActionEvidenceError::Unavailable)
    );
    drop(store);
    assert!(matches!(
        phone_evidence::open(&path),
        Err(ActionEvidenceError::Unavailable)
    ));
}

#[test]
fn damaged_live_guard_latches_unavailable_until_reopened() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("actions");
    let guard = path.with_extension("guard");
    phone_evidence::initialize_store(&path);
    let store = phone_evidence::open(&path).unwrap();
    let original = identity(temp.path(), 2);
    store.reserve(&original).unwrap();
    let retained_guard = fs::read(&guard).unwrap();
    fs::write(&guard, b"{}").unwrap();
    assert_eq!(
        store.lookup(&original.operation_id),
        Err(ActionEvidenceError::Unavailable)
    );
    fs::write(&guard, retained_guard).unwrap();
    assert_eq!(
        store.reserve(&original),
        Err(ActionEvidenceError::Unavailable)
    );
    drop(store);
    assert_eq!(
        phone_evidence::open(&path).unwrap().reserve(&original),
        Ok(ActionReservation::Existing)
    );
}

#[test]
fn interrupted_proven_update_completes_without_losing_prior_evidence() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("actions");
    let guard_path = path.with_extension("guard");
    phone_evidence::initialize_store(&path);
    let original = identity(temp.path(), 2);
    let second = identity(temp.path(), 3);
    let store = phone_evidence::open(&path).unwrap();
    store.reserve(&original).unwrap();
    let previous = fs::read(path.join("actions.json")).unwrap();
    let previous_seal = fs::read(path.join("seal.json")).unwrap();
    let mut guard: serde_json::Value =
        serde_json::from_slice(&fs::read(&guard_path).unwrap()).unwrap();
    store.reserve(&second).unwrap();
    let next = fs::read(path.join("actions.json")).unwrap();
    let next_guard: serde_json::Value =
        serde_json::from_slice(&fs::read(&guard_path).unwrap()).unwrap();
    drop(store);

    fs::write(path.join("actions.json"), &next).unwrap();
    fs::write(path.join("seal.json"), &previous_seal).unwrap();
    fs::write(path.join("pending-before.json"), &previous).unwrap();
    fs::write(path.join("pending-after.json"), &next).unwrap();
    guard["pending"] = serde_json::json!({
        "bytes":next_guard["bytes"],"sha256":next_guard["sha256"]
    });
    fs::write(&guard_path, serde_json::to_vec(&guard).unwrap()).unwrap();

    let recovered = phone_evidence::open(&path).unwrap();
    assert_eq!(
        recovered.lookup(&original.operation_id).unwrap().outcome,
        ActionOutcome::Unknown
    );
    assert_eq!(
        recovered.lookup(&second.operation_id).unwrap().outcome,
        ActionOutcome::Unknown
    );
}

#[test]
fn update_crash_points_preserve_the_last_proven_history() {
    for point in 0..7 {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("actions");
        let guard_path = path.with_extension("guard");
        phone_evidence::initialize_store(&path);
        let original = identity(temp.path(), 2);
        let second = identity(temp.path(), 3);
        let accepted = ActionOutcome::Accepted {
            evidence: "17".repeat(32),
        };
        let store = phone_evidence::open(&path).unwrap();
        store.reserve(&original).unwrap();
        store.report(&original, &accepted, 2_000).unwrap();
        let old = fs::read(path.join("actions.json")).unwrap();
        let old_seal = fs::read(path.join("seal.json")).unwrap();
        let old_guard = fs::read(&guard_path).unwrap();
        store.reserve(&second).unwrap();
        let new = fs::read(path.join("actions.json")).unwrap();
        let new_seal = fs::read(path.join("seal.json")).unwrap();
        let new_guard = fs::read(&guard_path).unwrap();
        drop(store);

        fs::write(
            path.join("actions.json"),
            if point >= 4 { &new } else { &old },
        )
        .unwrap();
        fs::write(
            path.join("seal.json"),
            if point >= 5 { &new_seal } else { &old_seal },
        )
        .unwrap();
        if point >= 1 {
            fs::write(path.join("pending-before.json"), &old).unwrap();
        }
        if point >= 2 {
            fs::write(path.join("pending-after.json"), &new).unwrap();
        }
        let mut guard: serde_json::Value = serde_json::from_slice(&old_guard).unwrap();
        let next_guard: serde_json::Value = serde_json::from_slice(&new_guard).unwrap();
        if (3..6).contains(&point) {
            guard["pending"] =
                serde_json::json!({"bytes":next_guard["bytes"],"sha256":next_guard["sha256"]});
        }
        fs::write(
            &guard_path,
            if point == 6 {
                new_guard
            } else {
                serde_json::to_vec(&guard).unwrap()
            },
        )
        .unwrap();

        let recovered = phone_evidence::open(&path).unwrap();
        assert_eq!(
            recovered.lookup(&original.operation_id).unwrap().outcome,
            accepted,
            "point {point}"
        );
        if point < 3 {
            assert_eq!(
                recovered.lookup(&second.operation_id),
                Err(ActionEvidenceError::Unavailable),
                "point {point}"
            );
        } else {
            assert_eq!(
                recovered.lookup(&second.operation_id).unwrap().outcome,
                ActionOutcome::Unknown,
                "point {point}"
            );
        }
    }
}

#[test]
fn pending_update_cannot_remove_an_existing_identity_or_replace_missing_proof() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("actions");
    let guard_path = path.with_extension("guard");
    phone_evidence::initialize_store(&path);
    let empty = fs::read(path.join("actions.json")).unwrap();
    let empty_guard: serde_json::Value =
        serde_json::from_slice(&fs::read(&guard_path).unwrap()).unwrap();
    let store = phone_evidence::open(&path).unwrap();
    store.reserve(&identity(temp.path(), 2)).unwrap();
    let previous = fs::read(path.join("actions.json")).unwrap();
    let mut guard: serde_json::Value =
        serde_json::from_slice(&fs::read(&guard_path).unwrap()).unwrap();
    drop(store);
    fs::write(path.join("pending-before.json"), &previous).unwrap();
    fs::write(path.join("pending-after.json"), &empty).unwrap();
    guard["pending"] =
        serde_json::json!({"bytes":empty_guard["bytes"],"sha256":empty_guard["sha256"]});
    fs::write(&guard_path, serde_json::to_vec(&guard).unwrap()).unwrap();

    assert!(matches!(
        phone_evidence::open(&path),
        Err(ActionEvidenceError::Unavailable)
    ));
    assert_eq!(fs::read(path.join("actions.json")).unwrap(), previous);
    fs::remove_file(path.join("pending-after.json")).unwrap();
    assert!(matches!(
        phone_evidence::open(&path),
        Err(ActionEvidenceError::Unavailable)
    ));
    assert_eq!(fs::read(path.join("actions.json")).unwrap(), previous);
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
