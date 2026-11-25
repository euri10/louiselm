#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "test fixtures and assertions"
)]

use std::{fs, sync::Arc, thread};

use super::{Begin, Fence, OwnerJournal, handle};
use crate::phone_control::{ActionEvidenceError, ActionIdentity, ActionOutcome};

fn identity(project: &std::path::Path, id: u8) -> ActionIdentity {
    ActionIdentity {
        operation_id: format!("{id:02x}").repeat(16),
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
fn fence_survives_restart_and_excludes_a_late_original() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("owner");
    let request = identity(temp.path(), 1);
    let owner = OwnerJournal::open(&path).unwrap();
    let Fence::Fenced(evidence) = owner.fence(&request, 2_000).unwrap() else {
        panic!("the first fence should win");
    };
    drop(owner);
    let owner = OwnerJournal::open(&path).unwrap();
    assert_eq!(
        owner.fence(&request, 3_000),
        Ok(Fence::Fenced(evidence.clone()))
    );
    assert_eq!(
        owner.begin(&request),
        Ok(Begin::Existing(ActionOutcome::FencedNoEffect { evidence }))
    );
    let mut changed = request.clone();
    changed.payload_hash = "08".repeat(32);
    assert_eq!(owner.begin(&changed), Err(ActionEvidenceError::Conflict));
}

#[test]
fn begun_attempt_remains_unknown_and_cannot_be_fenced_by_absence() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("owner");
    let request = identity(temp.path(), 2);
    let owner = OwnerJournal::open(&path).unwrap();
    assert_eq!(owner.begin(&request), Ok(Begin::New));
    drop(owner);
    let owner = OwnerJournal::open(&path).unwrap();
    assert_eq!(
        owner.begin(&request),
        Ok(Begin::Existing(ActionOutcome::Unknown))
    );
    assert_eq!(owner.fence(&request, 2_000), Ok(Fence::Uncertain));
    fs::remove_file(path.join("actions.json")).unwrap();
    assert_eq!(
        owner.fence(&request, 3_000),
        Err(ActionEvidenceError::Unavailable)
    );
}

#[test]
fn only_native_owner_outcomes_for_the_exact_identity_are_retained() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("owner");
    let request = identity(temp.path(), 4);
    let owner = OwnerJournal::open(&path).unwrap();
    assert_eq!(owner.begin(&request), Ok(Begin::New));
    let outcome = ActionOutcome::Accepted {
        evidence: "09".repeat(32),
    };
    let mut changed = request.clone();
    changed.subject_id = "0a".repeat(16);
    assert_eq!(
        owner.report(&changed, &outcome, 2_000),
        Err(ActionEvidenceError::Conflict)
    );
    owner.report(&request, &outcome, 2_000).unwrap();
    drop(owner);
    let owner = OwnerJournal::open(&path).unwrap();
    assert_eq!(owner.begin(&request), Ok(Begin::Existing(outcome)));
    assert_eq!(owner.fence(&request, 3_000), Ok(Fence::Uncertain));
}

#[test]
fn owner_can_fence_a_begun_action_after_proving_no_effect() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("owner");
    let request = identity(temp.path(), 6);
    let owner = OwnerJournal::open(&path).unwrap();
    assert_eq!(owner.begin(&request), Ok(Begin::New));
    let fence = ActionOutcome::FencedNoEffect {
        evidence: "0b".repeat(32),
    };
    owner.report(&request, &fence, 2_000).unwrap();
    drop(owner);
    let owner = OwnerJournal::open(&path).unwrap();
    assert_eq!(owner.begin(&request), Ok(Begin::Existing(fence)));
}

#[test]
fn concurrent_begin_and_fence_have_only_one_safe_winner() {
    let temp = tempfile::tempdir().unwrap();
    let request = identity(temp.path(), 3);
    let owner = Arc::new(OwnerJournal::open(temp.path().join("owner")).unwrap());
    let first = Arc::clone(&owner);
    let original = request.clone();
    let begun = thread::spawn(move || first.begin(&original));
    let second = Arc::clone(&owner);
    let original = request.clone();
    let fenced = thread::spawn(move || second.fence(&original, 2_000));
    let begun = begun.join().unwrap().unwrap();
    let fenced = fenced.join().unwrap().unwrap();
    assert!(matches!(
        (&begun, &fenced),
        (Begin::New, Fence::Uncertain) | (Begin::Existing(_), Fence::Fenced(_))
    ));
    assert!(matches!(owner.begin(&request), Ok(Begin::Existing(_))));
}

#[test]
fn malformed_owner_commands_do_not_initialize_storage() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("owner");
    let request = identity(temp.path(), 5);
    for message in [
        serde_json::json!({"v":2,"type":"begin","identity":request}),
        serde_json::json!({"v":1,"type":"begin","identity":request,"role":"operator"}),
        serde_json::json!({"v":1,"type":"fence","identity":request}),
    ] {
        assert_eq!(
            handle(&path, message.to_string().as_bytes())["code"],
            "malformed"
        );
        assert!(!path.exists());
    }
}
