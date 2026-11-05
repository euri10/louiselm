//! Closed, bounded withdrawal evidence survives independently of its origin.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Behavioral fixtures abort on setup failure."
)]

use louiselm_capture::{AttentionDraft, AttentionStore, BrokerProjection};
use serde_json::{Value, json};

const OPERATION: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

fn projection() -> Value {
    json!({
        "sequence": 1,
        "change": {
            "type": "upsert",
            "attention": {
                "subject_kind": "supply", "subject_id": "1000",
                "kind": "endorsement_withdrawn", "source_operation_id": OPERATION,
                "created_at_ms": 100,
                "withdrawal": {
                    "package_digest": format!("sha256:{}", "a".repeat(64)),
                    "withdrawal_digest": format!("sha256:{}", "b".repeat(64)),
                    "endorser": "Local reviewer", "urgency": "subject_only"
                }
            }
        }
    })
}

#[test]
fn withdrawal_projection_persists_metadata_updates_without_changing_identity() {
    let root = tempfile::tempdir().unwrap();
    let store = AttentionStore::new(root.path(), None).unwrap();
    let first: BrokerProjection =
        serde_json::from_value(projection()).expect("withdrawal projection");
    assert!(store.project(&first).unwrap().applied);
    assert!(!store.project(&first).unwrap().applied);
    let mut updated = projection();
    updated["sequence"] = json!(2);
    updated["change"]["attention"]["withdrawal"]["urgency"] = json!("relied_on");
    updated["change"]["attention"]["withdrawal"]["endorser"] = json!("Renamed reviewer");
    store
        .project(&serde_json::from_value(updated).unwrap())
        .unwrap();
    let snapshot = store.snapshot().unwrap();
    assert_eq!(snapshot.items.len(), 1);
    let item = serde_json::to_value(&snapshot.items[0]).unwrap();
    assert_eq!(item["source_operation_id"], OPERATION);
    assert_eq!(item["created_at_ms"], 100);
    assert_eq!(item["reason"], "Endorsement was withdrawn");
    assert_eq!(item["withdrawal"]["urgency"], "relied_on");
    assert_eq!(item["withdrawal"]["endorser"], "Renamed reviewer");
    assert!(item.get("code").is_none());
    assert!(item.get("next_action").is_none());
    assert_eq!(store.clear_session("1000").unwrap().items, snapshot.items);
    drop(store);
    let store = AttentionStore::new(root.path(), None).unwrap();
    assert_eq!(store.snapshot().unwrap(), snapshot);
    let key = json!({
        "subject_kind": "supply", "subject_id": "1000",
        "kind": "endorsement_withdrawn", "source_operation_id": OPERATION
    });
    // Delivery eligibility/acknowledgment is not resolution.
    let ineligible = store
        .set_eligible(&serde_json::from_value(key.clone()).unwrap(), false)
        .unwrap();
    assert_eq!(ineligible.items.len(), 1);
    assert!(!ineligible.items[0].eligible);
    let clear = serde_json::from_value(json!({
        "sequence": 3, "change": {"type": "clear", "key": key}
    }))
    .unwrap();
    store.project(&clear).unwrap();
    assert!(!store.project(&first).unwrap().applied);
    assert_eq!(
        store.snapshot().unwrap().items,
        [] as [louiselm_capture::AttentionItem; 0]
    );
}

#[test]
fn withdrawal_local_upsert_compares_metadata_and_keeps_identical_retries_inert() {
    let root = tempfile::tempdir().unwrap();
    let store = AttentionStore::new(root.path(), None).unwrap();
    let draft: AttentionDraft =
        serde_json::from_value(projection()["change"]["attention"].clone()).unwrap();
    let first = store.upsert(draft.clone()).unwrap();
    assert_eq!(store.upsert(draft).unwrap(), first);
    let mut changed = projection()["change"]["attention"].clone();
    changed["withdrawal"]["urgency"] = json!("relied_on");
    let next = store
        .upsert(serde_json::from_value(changed).unwrap())
        .unwrap();
    assert_eq!(next.items.len(), 1);
    assert_eq!(next.generation, first.generation + 1);
    assert_eq!(
        serde_json::to_value(next.items[0].clone()).unwrap()["withdrawal"]["urgency"],
        "relied_on"
    );
}

#[test]
fn withdrawal_rejects_invalid_metadata_and_foreign_context_without_mutation() {
    let root = tempfile::tempdir().unwrap();
    let store = AttentionStore::new(root.path(), None).unwrap();
    store
        .project(&serde_json::from_value(projection()).unwrap())
        .unwrap();
    let original = store.snapshot().unwrap();
    let mut invalid = Vec::new();
    for (field, value) in [
        ("subject_kind", json!("session")),
        ("subject_kind", json!("run")),
        ("kind", json!("skill_approval_pending")),
        ("code", json!("admission_required")),
        ("linked_run_id", json!(OPERATION)),
        ("stage", json!("review")),
        ("withdrawal", Value::Null),
    ] {
        let mut value_with_field = projection();
        value_with_field["change"]["attention"][field] = value;
        invalid.push(value_with_field);
    }
    let mut missing = projection();
    missing["change"]["attention"]
        .as_object_mut()
        .unwrap()
        .remove("withdrawal");
    invalid.push(missing);
    for (kind, code) in [
        ("skill_approval_pending", json!("admission_required")),
        ("turn_ready", Value::Null),
    ] {
        let mut foreign = projection();
        foreign["change"]["attention"]["subject_kind"] = json!("session");
        foreign["change"]["attention"]["kind"] = json!(kind);
        foreign["change"]["attention"]["code"] = code;
        invalid.push(foreign);
    }
    for field in ["package_digest", "withdrawal_digest"] {
        for value in [
            "a".repeat(64),
            format!("sha256:{}", "A".repeat(64)),
            format!("sha256:{}", "a".repeat(63)),
        ] {
            let mut malformed = projection();
            malformed["change"]["attention"]["withdrawal"][field] = json!(value);
            invalid.push(malformed);
        }
    }
    for label in [
        String::new(),
        " \t".into(),
        "\u{a0}\u{2003}".into(),
        "a".repeat(257),
        "é".repeat(129),
        "review\nprivate".into(),
        "review\u{85}private".into(),
    ] {
        let mut malformed = projection();
        malformed["change"]["attention"]["withdrawal"]["endorser"] = json!(label);
        invalid.push(malformed);
    }
    for mut value in invalid {
        value["sequence"] = json!(2);
        let projection = serde_json::from_value(value.clone()).unwrap();
        assert!(store.project(&projection).is_err(), "accepted {value}");
        assert_eq!(store.snapshot().unwrap(), original);
    }
    for label in ["é".repeat(128), format!("key sha256:{}", "a".repeat(64))] {
        let mut valid = projection();
        valid["sequence"] = json!(2);
        valid["change"]["attention"]["withdrawal"]["endorser"] = json!(label);
        let isolated = tempfile::tempdir().unwrap();
        let store = AttentionStore::new(isolated.path(), None).unwrap();
        valid["sequence"] = json!(1);
        store
            .project(&serde_json::from_value(valid).unwrap())
            .unwrap();
    }
}

#[test]
fn withdrawal_wire_object_is_closed_and_required_fields_are_typed() {
    for (field, value) in [
        (
            "next_action",
            json!("private command must never cross the wire"),
        ),
        ("profile", json!("private profile content")),
        ("urgency", json!("critical")),
        ("endorser", json!(1)),
        ("package_digest", Value::Null),
    ] {
        let mut unknown = projection();
        unknown["change"]["attention"]["withdrawal"][field] = value;
        assert!(serde_json::from_value::<BrokerProjection>(unknown).is_err());
    }
    for field in ["package_digest", "withdrawal_digest", "endorser", "urgency"] {
        let mut missing = projection();
        missing["change"]["attention"]["withdrawal"]
            .as_object_mut()
            .unwrap()
            .remove(field);
        assert!(serde_json::from_value::<BrokerProjection>(missing).is_err());
    }
}

#[test]
fn persisted_withdrawal_metadata_is_revalidated() {
    let root = tempfile::tempdir().unwrap();
    let store = AttentionStore::new(root.path(), None).unwrap();
    store
        .project(&serde_json::from_value(projection()).unwrap())
        .unwrap();
    drop(store);
    let path = root.path().join("attention.json");
    let original: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    for (field, value) in [
        ("package_digest", "invalid"),
        ("endorser", "\nprivate"),
        ("urgency", "critical"),
        ("next_action", "private command"),
    ] {
        let mut corrupted = original.clone();
        corrupted["items"][0]["withdrawal"][field] = json!(value);
        std::fs::write(&path, serde_json::to_vec(&corrupted).unwrap()).unwrap();
        assert!(AttentionStore::new(root.path(), None).is_err());
    }
}
