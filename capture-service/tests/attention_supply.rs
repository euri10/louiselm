//! Supply conditions outlive Session and Run lifecycles and retain closed wire validation.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Behavioral fixtures abort on setup failure."
)]

use louiselm_capture::{AttentionError, AttentionStore, BrokerProjection};
use serde_json::{Value, json};

const RUN: &str = "11111111-2222-4333-8444-555555555555";
const OPERATION: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

fn pending() -> Value {
    json!({
        "sequence": 1,
        "change": {
            "type": "upsert",
            "attention": {
                "subject_kind": "supply",
                "subject_id": "1000",
                "kind": "skill_approval_pending",
                "source_operation_id": OPERATION,
                "created_at_ms": 100,
                "linked_run_id": null,
                "stage": null,
                "code": "admission_required"
            }
        }
    })
}

#[test]
fn supply_projection_survives_subject_cleanup_and_restart_until_exact_clear() {
    let root = tempfile::tempdir().unwrap();
    let store = AttentionStore::new(root.path(), None).unwrap();
    let pending: BrokerProjection = serde_json::from_value(pending()).expect("supply projection");
    assert!(store.project(&pending).unwrap().applied);
    let original = store.snapshot().unwrap().items;
    assert_eq!(original.len(), 1);
    assert!(original[0].eligible);
    assert_eq!(original[0].reason, "Skill approval is pending");
    assert_eq!(store.clear_session("1000").unwrap().items, original);
    for (sequence, kind, id) in [(2, "session", "1000"), (3, "run", RUN)] {
        let projection = serde_json::from_value(json!({
            "sequence": sequence,
            "change": {"type": "clear_subject", "subject_kind": kind, "subject_id": id}
        }))
        .unwrap();
        store.project(&projection).unwrap();
        assert_eq!(store.snapshot().unwrap().items, original);
    }
    drop(store);
    let store = AttentionStore::new(root.path(), None).unwrap();
    assert_eq!(store.snapshot().unwrap().items, original);
    let clear = serde_json::from_value(json!({
        "sequence": 4,
        "change": {
            "type": "clear",
            "key": {
                "subject_kind": "supply", "subject_id": "1000",
                "kind": "skill_approval_pending", "source_operation_id": OPERATION
            }
        }
    }))
    .unwrap();
    store.project(&clear).unwrap();
    drop(store);
    let store = AttentionStore::new(root.path(), None).unwrap();
    assert!(!store.project(&pending).unwrap().applied);
    assert_eq!(
        store.snapshot().unwrap().items,
        [] as [louiselm_capture::AttentionItem; 0]
    );
}

#[test]
fn supply_subject_requires_canonical_operator_uid_and_admission_kind() {
    let root = tempfile::tempdir().unwrap();
    let store = AttentionStore::new(root.path(), None).unwrap();
    for id in ["1", "1000", "4294967295"] {
        let mut value = pending();
        value["change"]["attention"]["subject_id"] = json!(id);
        let projection = serde_json::from_value(value).expect("supply projection");
        let isolated = tempfile::tempdir().unwrap();
        AttentionStore::new(isolated.path(), None)
            .unwrap()
            .project(&projection)
            .expect("canonical operator UID");
    }
    for id in [
        "0",
        "00",
        "01000",
        "+1000",
        "-1",
        "1.0",
        "1e3",
        "4294967296",
        "session",
    ] {
        let mut value = pending();
        value["change"]["attention"]["subject_id"] = json!(id);
        let projection = serde_json::from_value(value).unwrap();
        let error = store.project(&projection).unwrap_err();
        assert!(
            matches!(error, AttentionError::Invalid(message) if message.contains("Supply subject_id"))
        );
    }
    for kind in [
        "turn_ready",
        "permission_required",
        "run_parked",
        "session_failed",
        "skill_unverified",
    ] {
        let mut value = pending();
        value["change"]["attention"]["kind"] = json!(kind);
        let projection = serde_json::from_value(value).unwrap();
        let error = store.project(&projection).unwrap_err();
        assert!(
            matches!(error, AttentionError::Invalid(message) if message.contains("Supply condition kind"))
        );
    }
    assert_eq!(store.snapshot().unwrap().generation, 0);
}

#[test]
fn supply_rejects_lifecycle_fields_and_bulk_clear_without_changing_state() {
    let root = tempfile::tempdir().unwrap();
    let store = AttentionStore::new(root.path(), None).unwrap();
    let projection = serde_json::from_value(pending()).unwrap();
    store.project(&projection).unwrap();
    let original = store.snapshot().unwrap();
    for (field, value) in [("linked_run_id", RUN), ("stage", "review")] {
        let mut invalid = pending();
        invalid["sequence"] = json!(2);
        invalid["change"]["attention"][field] = json!(value);
        let projection = serde_json::from_value(invalid).unwrap();
        let error = store.project(&projection).unwrap_err();
        assert!(
            matches!(error, AttentionError::Invalid(message) if message.contains("Supply conditions cannot carry"))
        );
    }
    let invalid = serde_json::from_value(json!({
        "sequence": 2,
        "change": {"type": "clear_subject", "subject_kind": "supply", "subject_id": "1000"}
    }))
    .unwrap();
    let error = store.project(&invalid).unwrap_err();
    assert!(
        matches!(error, AttentionError::Invalid(message) if message.contains("Supply conditions require an exact clear"))
    );
    let mut unknown = pending();
    unknown["change"]["attention"]["operator_uid"] = json!(2000);
    let error = serde_json::from_value::<BrokerProjection>(unknown).unwrap_err();
    assert!(error.to_string().contains("unknown field `operator_uid`"));
    assert_eq!(store.snapshot().unwrap(), original);
}

#[test]
fn persisted_supply_conditions_revalidate_subject_metadata() {
    let root = tempfile::tempdir().unwrap();
    let store = AttentionStore::new(root.path(), None).unwrap();
    let projection = serde_json::from_value(pending()).unwrap();
    store.project(&projection).unwrap();
    drop(store);
    let path = root.path().join("attention.json");
    let original: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    for (field, value) in [
        ("subject_id", "01000"),
        ("linked_run_id", RUN),
        ("stage", "review"),
    ] {
        let mut corrupted = original.clone();
        corrupted["items"][0][field] = json!(value);
        std::fs::write(&path, serde_json::to_vec(&corrupted).unwrap()).unwrap();
        let error = AttentionStore::new(root.path(), None).unwrap_err();
        assert!(matches!(error, AttentionError::Invalid(message) if message.contains("Supply")));
    }
}
