//! Saved decisions outlive Run disposal, delivery acknowledgement and receiver restart.
#![allow(
    clippy::unwrap_used,
    reason = "Behavioral fixtures assert exact outcomes."
)]

use louiselm_capture::{
    AttentionDraft, AttentionKey, AttentionStore, BrokerProjection, ProjectionChange, RunAdmission,
    RunDraft, RunSession, RunStore,
};
use serde_json::json;

const RUN: &str = "11111111-2222-4333-8444-555555555555";
const OPERATION: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

fn draft_value() -> serde_json::Value {
    json!({ "subject_kind": "run", "subject_id": RUN, "kind": "promotion_pending",
        "source_operation_id": OPERATION, "created_at_ms": 100,
        "linked_run_id": null, "stage": null, "result_id": "saved-result-1" })
}

fn key() -> AttentionKey {
    serde_json::from_value(json!({ "subject_kind": "run", "subject_id": RUN,
        "kind": "promotion_pending", "source_operation_id": OPERATION }))
    .unwrap()
}

#[test]
fn pending_promotion_survives_disposed_run_acknowledgement_and_reopen_until_exact_broker_clear() {
    let root = tempfile::tempdir().unwrap();
    let runs = RunStore::new(root.path().join("runs")).unwrap();
    runs.admit(
        RunAdmission {
            id: RUN.into(),
            generated_work_ceiling: 1,
            park_ttl_ms: 100,
        },
        "fixture-token-123456789",
    )
    .unwrap();
    runs.attach(RunSession {
        id: RUN.into(),
        session_id: "codex/fixture".into(),
        agent: "codex".into(),
        acp_session_id: "fixture".into(),
        working_dir: "/fixture".into(),
        load_session: true,
    })
    .unwrap();
    runs.park_cold(
        RunDraft {
            id: RUN.into(),
            session_id: "codex/fixture".into(),
            agent: "codex".into(),
            acp_session_id: "fixture".into(),
            working_dir: "/fixture".into(),
            load_session: true,
            claimed_issue_ids: vec![],
        },
        100,
    )
    .unwrap();
    let store = AttentionStore::new(root.path().join("attention"), Some(runs.clone())).unwrap();
    let draft: AttentionDraft = serde_json::from_value(draft_value()).unwrap();
    let created = BrokerProjection {
        sequence: 1,
        change: ProjectionChange::Upsert {
            attention: draft.clone(),
        },
    };
    assert!(store.project(&created).unwrap().applied);
    assert!(!store.project(&created).unwrap().applied);
    assert!(store.snapshot().unwrap().items[0].eligible);
    assert_eq!(runs.reap_expired(1000, |_| Ok(())).unwrap().disposed, 1);
    store
        .project(&BrokerProjection {
            sequence: 2,
            change: ProjectionChange::ClearSubject {
                subject_kind: louiselm_capture::AttentionSubjectKind::Run,
                subject_id: RUN.into(),
            },
        })
        .unwrap();
    assert_eq!(store.snapshot().unwrap().items.len(), 1);
    store.set_eligible(&key(), false).unwrap();
    assert_eq!(store.snapshot().unwrap().items.len(), 1);
    assert!(
        store.clear(&key()).is_err(),
        "dismissal is not a saved-result decision"
    );
    assert!(
        store.upsert(draft).is_err(),
        "only ordered broker projections create saved-result state"
    );
    drop(store);
    let reopened = AttentionStore::new(root.path().join("attention"), Some(runs)).unwrap();
    let snapshot = reopened.snapshot().unwrap();
    assert_eq!(snapshot.items.len(), 1);
    assert!(!snapshot.items[0].eligible);
    let encoded = serde_json::to_value(&snapshot.items[0]).unwrap();
    assert_eq!(encoded["result_id"], "saved-result-1");
    assert_eq!(encoded["reason"], "Saved result awaits review");
    reopened
        .project(&BrokerProjection {
            sequence: 3,
            change: ProjectionChange::Clear { key: key() },
        })
        .unwrap();
    assert_eq!(reopened.snapshot().unwrap().items.len(), 0);
    assert!(!reopened.project(&created).unwrap().applied);
    assert_eq!(reopened.snapshot().unwrap().items.len(), 0);
}

#[test]
fn promotion_metadata_is_closed_bounded_run_scoped_and_not_candidate_content() {
    let root = tempfile::tempdir().unwrap();
    let store = AttentionStore::new(root.path(), None).unwrap();
    for (field, value) in [
        ("result_id", json!("../source")),
        ("result_id", json!("x".repeat(129))),
        ("result_id", json!(null)),
        ("result_id", json!("candidate prose\n")),
        ("subject_kind", json!("session")),
        ("stage", json!("candidate diff")),
        ("code", json!("quarantined")),
        ("payload", json!("source bytes")),
    ] {
        let mut value_map = draft_value();
        value_map[field] = value;
        let refused =
            serde_json::from_value::<AttentionDraft>(value_map).map_or(true, |attention| {
                store
                    .project(&BrokerProjection {
                        sequence: 1,
                        change: ProjectionChange::Upsert { attention },
                    })
                    .is_err()
            });
        assert!(refused, "{field} must be refused");
        assert_eq!(store.snapshot().unwrap().items.len(), 0);
    }
}
