//! Episode identities, exact resolution, crash intent and terminal precedence.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Tests assert disposable fixture outcomes."
)]
use super::*;
use crate::{
    Digest,
    posture::{DimensionInput, EvidenceKind, EvidenceRef, FailureCode},
};

fn authorization() -> LaunchAuthorization {
    LaunchAuthorization {
        conformance: crate::launch_protocol::ConformanceAuthorization::default(),
        schema: crate::launch_protocol::LAUNCH_AUTHORIZATION_SCHEMA.into(),
        protocol_version: crate::launch::PROTOCOL_VERSION,
        authorization_id: "authorization".into(),
        request_id: "request".into(),
        request_digest: Digest::of(b"request").to_string(),
        controller_uid: 1000,
        session_id: "session".into(),
        run_id: "00000000-0000-4000-8000-000000000001".into(),
        envelope_revision: 1,
        identity_slot: 0,
        assigned_uid: 2000,
        assigned_gid: 2000,
        provider_expires_at_ms: None,
        expires_at_ms: 50_000,
        broker_loss_grace_ms: 5000,
    }
}

fn posture(auth: &LaunchAuthorization, isolation: DimensionState, managed: FailureCode) -> Posture {
    let inputs = DimensionName::ALL
        .into_iter()
        .map(|name| {
            if name == DimensionName::Isolation && isolation == DimensionState::Waived {
                DimensionInput::waived(
                    name,
                    FailureCode::IsolationFailed,
                    vec![],
                    EvidenceRef::new(
                        EvidenceKind::WaiverReceipt,
                        &Digest::of(b"waiver").to_string(),
                    )
                    .unwrap(),
                )
            } else if name == DimensionName::Isolation && isolation == DimensionState::Verified {
                DimensionInput::verified(
                    name,
                    vec![
                        EvidenceRef::new(
                            EvidenceKind::IsolationReceipt,
                            &Digest::of(b"report").to_string(),
                        )
                        .unwrap(),
                    ],
                )
            } else {
                DimensionInput::failed(
                    name,
                    if name == DimensionName::ManagedSupply {
                        managed
                    } else {
                        FailureCode::EvidenceMissing
                    },
                    vec![],
                )
            }
        })
        .collect();
    Posture::evaluate(&auth.session_id, &auth.run_id, inputs).unwrap()
}

fn drain(outbox: &Outbox) -> Vec<ProjectionChange> {
    let mut changes = vec![];
    while let Some(item) = outbox.next().unwrap() {
        outbox.acknowledge(item.sequence, &item.digest()).unwrap();
        changes.push(item.change);
    }
    changes
}

#[test]
fn dimensions_keep_identity_until_exact_resolution_and_restart_preserves_order() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("posture");
    let store = PostureAttention::open(&path).unwrap();
    let outbox = Outbox::open(&root.path().join("outbox")).unwrap();
    let auth = authorization();
    let failed = posture(&auth, DimensionState::Failed, FailureCode::EvidenceMissing);
    store
        .observe(&auth, &failed, true, false, 100, &outbox)
        .unwrap();
    let first = drain(&outbox);
    assert_eq!(first.len(), 6);
    let record = store.read("session").unwrap().unwrap();
    let isolation = record.conditions[3].clone().unwrap();
    let managed = record.conditions[0].clone().unwrap();
    drop(store);
    let store = PostureAttention::open(&path).unwrap();
    store.reconcile(&outbox).unwrap();
    store
        .observe(&auth, &failed, true, false, 101, &outbox)
        .unwrap();
    assert!(drain(&outbox).is_empty());
    let waived = posture(&auth, DimensionState::Waived, FailureCode::WitnessMissing);
    store
        .observe(&auth, &waived, true, false, 102, &outbox)
        .unwrap();
    let changes = drain(&outbox);
    assert_eq!(changes.len(), 2);
    let ProjectionChange::Upsert(updated) = &changes[0] else {
        panic!("code update");
    };
    assert_eq!(updated.operation_id, managed.operation_id);
    assert_eq!(updated.created_at_ms, managed.created_at_ms);
    assert_eq!(
        updated.reason,
        AttentionReason::SkillUnverified(FailureCode::WitnessMissing)
    );
    assert_eq!(changes[1], ProjectionChange::Clear(isolation.clone()));
    assert!(matches!(
        store.observe(&auth, &failed, true, false, 101, &outbox),
        Err(BrokerError::InvalidGrant)
    ));
    assert!(drain(&outbox).is_empty());
    store
        .observe(&auth, &failed, true, false, 103, &outbox)
        .unwrap();
    let newer = store.read("session").unwrap().unwrap().conditions[3]
        .clone()
        .unwrap();
    assert_ne!(newer.operation_id, isolation.operation_id);
    drain(&outbox);
    let verified = posture(
        &auth,
        DimensionState::Verified,
        FailureCode::EvidenceMissing,
    );
    store
        .observe(&auth, &verified, false, false, 104, &outbox)
        .unwrap();
    assert_eq!(drain(&outbox), vec![ProjectionChange::Clear(newer)]);
    assert_eq!(
        store
            .read("session")
            .unwrap()
            .unwrap()
            .conditions
            .iter()
            .flatten()
            .count(),
        5
    );
}

#[test]
fn pending_projection_survives_storage_outage_and_terminal_subjects_never_reopen() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("posture");
    let store = PostureAttention::open(&path).unwrap();
    let outbox_path = root.path().join("outbox");
    let outbox = Outbox::open(&outbox_path).unwrap();
    let auth = authorization();
    let failed = posture(&auth, DimensionState::Failed, FailureCode::EvidenceMissing);
    fs::rename(outbox_path.join("entries"), outbox_path.join("retained")).unwrap();
    fs::write(outbox_path.join("entries"), b"fixture unavailable").unwrap();
    assert!(matches!(
        store.observe(&auth, &failed, true, false, 100, &outbox),
        Err(BrokerError::Storage(_))
    ));
    assert_eq!(store.read("session").unwrap().unwrap().pending.len(), 6);
    drop(store);
    fs::remove_file(outbox_path.join("entries")).unwrap();
    fs::rename(outbox_path.join("retained"), outbox_path.join("entries")).unwrap();
    let store = PostureAttention::open(&path).unwrap();
    store.reconcile(&outbox).unwrap();
    assert_eq!(drain(&outbox).len(), 6);
    store.end("session", &outbox).unwrap();
    assert_eq!(drain(&outbox).len(), 6);
    store
        .observe(&auth, &failed, true, false, 101, &outbox)
        .unwrap();
    assert!(drain(&outbox).is_empty());
    store.end_run(&auth.run_id, &outbox).unwrap();
    let mut sibling = auth.clone();
    sibling.session_id = "late-sibling".into();
    let late = posture(
        &sibling,
        DimensionState::Failed,
        FailureCode::EvidenceMissing,
    );
    store
        .observe(&sibling, &late, true, false, 102, &outbox)
        .unwrap();
    assert!(store.read("late-sibling").unwrap().is_none());
    assert!(drain(&outbox).is_empty());
    let mut malformed = serde_json::to_value(store.read("session").unwrap().unwrap()).unwrap();
    malformed["arbitrary_producer_text"] = serde_json::json!("not evidence");
    fs::write(
        store.path("session").unwrap(),
        serde_json::to_vec(&malformed).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        store.reconcile(&outbox),
        Err(BrokerError::Storage(_))
    ));
}

#[test]
fn session_end_preserves_siblings_and_run_end_preserves_other_runs() {
    let root = tempfile::tempdir().unwrap();
    let store = PostureAttention::open(&root.path().join("posture")).unwrap();
    let outbox = Outbox::open(&root.path().join("outbox")).unwrap();
    let auth = authorization();
    let mut sibling = auth.clone();
    sibling.session_id = "sibling".into();
    sibling.request_digest = Digest::of(b"sibling").to_string();
    let mut other = auth.clone();
    other.session_id = "other-run-session".into();
    other.run_id = "00000000-0000-4000-8000-000000000002".into();
    other.request_digest = Digest::of(b"other").to_string();
    for subject in [&auth, &sibling, &other] {
        store
            .observe(
                subject,
                &posture(
                    subject,
                    DimensionState::Failed,
                    FailureCode::EvidenceMissing,
                ),
                true,
                false,
                100,
                &outbox,
            )
            .unwrap();
    }
    assert_eq!(drain(&outbox).len(), 18);
    store.end(&auth.session_id, &outbox).unwrap();
    assert_eq!(drain(&outbox).len(), 6);
    assert!(!store.read(&sibling.session_id).unwrap().unwrap().ended);
    // Durable Run tombstone wins even if the process died before per-Session clears.
    crate::broker::write_new_record(
        &store
            .root
            .join("ended-runs")
            .join(record_name(&auth.run_id).unwrap()),
        &auth.run_id,
    )
    .unwrap();
    store.reconcile(&outbox).unwrap();
    assert_eq!(drain(&outbox).len(), 6);
    assert!(store.read(&sibling.session_id).unwrap().unwrap().ended);
    assert!(!store.read(&other.session_id).unwrap().unwrap().ended);
    store.reconcile(&outbox).unwrap();
    assert!(drain(&outbox).is_empty());
}

#[test]
fn quarantine_replaces_dimension_items_and_survives_restart_without_duplicates() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("posture");
    let store = PostureAttention::open(&path).unwrap();
    let outbox = Outbox::open(&root.path().join("outbox")).unwrap();
    let auth = authorization();
    let failed = posture(&auth, DimensionState::Failed, FailureCode::EvidenceMissing);
    store
        .observe(&auth, &failed, true, false, 100, &outbox)
        .unwrap();
    assert_eq!(drain(&outbox).len(), 6);
    store
        .observe(&auth, &failed, true, true, 101, &outbox)
        .unwrap();
    let changes = drain(&outbox);
    assert_eq!(
        changes
            .iter()
            .filter(|change| matches!(change, ProjectionChange::Clear(_)))
            .count(),
        5
    );
    let upserts = changes
        .iter()
        .filter(|change| matches!(change, ProjectionChange::Upsert(_)))
        .cloned()
        .collect::<Vec<_>>();
    let [ProjectionChange::Upsert(condition)] = upserts.as_slice() else {
        panic!("one quarantine item");
    };
    assert_eq!(
        condition.reason,
        AttentionReason::SkillUnverified(FailureCode::Quarantined)
    );
    let store = PostureAttention::open(&path).unwrap();
    store.reconcile(&outbox).unwrap();
    store
        .observe(&auth, &failed, true, true, 102, &outbox)
        .unwrap();
    assert!(drain(&outbox).is_empty());
    assert_eq!(
        store
            .read("session")
            .unwrap()
            .unwrap()
            .conditions
            .iter()
            .flatten()
            .count(),
        1
    );
    store.end("session", &outbox).unwrap();
    assert_eq!(drain(&outbox).len(), 1);
}

#[test]
fn quarantine_needs_no_retained_evidence_or_completed_park_to_raise_attention() {
    let root = tempfile::tempdir().unwrap();
    let store = PostureAttention::open(&root.path().join("posture")).unwrap();
    let outbox = Outbox::open(&root.path().join("outbox")).unwrap();
    let auth = authorization();
    let missing = posture(&auth, DimensionState::Failed, FailureCode::EvidenceMissing);
    store
        .observe(&auth, &missing, false, true, 100, &outbox)
        .unwrap();
    let changes = drain(&outbox);
    assert!(
        matches!(changes.as_slice(), [ProjectionChange::Upsert(condition)] if condition.reason == AttentionReason::SkillUnverified(FailureCode::Quarantined))
    );
}
