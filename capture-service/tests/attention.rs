//! Behavioral coverage for attention.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Test fixtures abort on setup failure and assert failures directly."
)]

use std::{fs, time::Duration};

use louiselm_capture::{
    AttentionCode, AttentionDraft, AttentionKey, AttentionKind, AttentionSnapshot, AttentionSocket,
    AttentionSocketMessage, AttentionStore, AttentionSubjectKind,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    net::UnixStream,
};

const SESSION_ID: &str = "codex/session-1";
const RUN_ID: &str = "11111111-2222-4333-8444-555555555555";
const OPERATION_ID: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

#[test]
fn broker_projection_order_survives_clear_and_receiver_restart() {
    use louiselm_capture::{BrokerProjection, ProjectionChange};
    let temporary = tempfile::tempdir().unwrap();
    let store = AttentionStore::new(
        temporary.path(),
        Some(louiselm_capture::RunStore::new(temporary.path().join("runs")).unwrap()),
    )
    .unwrap();
    let item = draft();
    let created = BrokerProjection {
        sequence: 1,
        change: ProjectionChange::Upsert {
            attention: item.clone(),
        },
    };
    let first = store.project(&created).unwrap();
    assert!(first.applied);
    assert!(!store.project(&created).unwrap().applied);
    let mut conflicting = created.clone();
    if let ProjectionChange::Upsert { attention } = &mut conflicting.change {
        attention.created_at_ms += 1;
    }
    assert!(store.project(&conflicting).is_err());
    let cleared = BrokerProjection {
        sequence: 2,
        change: ProjectionChange::Clear {
            key: AttentionKey {
                subject_kind: item.subject_kind,
                subject_id: item.subject_id,
                kind: item.kind,
                source_operation_id: item.source_operation_id,
            },
        },
    };
    store.project(&cleared).unwrap();
    drop(store);
    let restarted = AttentionStore::new(
        temporary.path(),
        Some(louiselm_capture::RunStore::new(temporary.path().join("runs")).unwrap()),
    )
    .unwrap();
    assert!(!restarted.project(&created).unwrap().applied);
    assert!(restarted.snapshot().unwrap().items.is_empty());
    let mut gap = created;
    gap.sequence = 4;
    assert!(restarted.project(&gap).is_err());
    assert!(restarted.snapshot().unwrap().items.is_empty());
}

#[tokio::test]
async fn broker_projection_socket_authenticates_and_consumes_the_shared_wire_fixture() {
    use louiselm_capture::{BrokerAttentionConfig, BrokerAttentionSocket};
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let temporary = tempfile::tempdir().unwrap();
    let store = AttentionStore::new(
        temporary.path().join("attention"),
        Some(louiselm_capture::RunStore::new(temporary.path().join("runs")).unwrap()),
    )
    .unwrap();
    let socket_path = temporary.path().join("attention.sock");
    let capability_path = temporary.path().join("capability");
    fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o711)).unwrap();
    fs::write(&capability_path, "projection-test-capability").unwrap();
    let config = BrokerAttentionConfig {
        socket: socket_path.clone(),
        broker_uid: fs::metadata(temporary.path()).unwrap().uid(),
        capability_sha256: format!("{:x}", Sha256::digest("projection-test-capability")),
    };
    let socket = BrokerAttentionSocket::bind(config, store.clone())
        .await
        .unwrap();
    let server = tokio::spawn(socket.serve());
    let projection: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/broker_attention_projection.json"
    ))
    .unwrap();
    for valid in [false, true] {
        let stream = UnixStream::connect(&socket_path).await.unwrap();
        let mut lines = BufReader::new(stream).lines();
        let capability = if valid {
            fs::read_to_string(&capability_path).unwrap()
        } else {
            "wrong".into()
        };
        let request = serde_json::json!({"type":"project", "request_id":"projection-1", "projection":projection, "capability":capability});
        lines
            .get_mut()
            .write_all(format!("{request}\n").as_bytes())
            .await
            .unwrap();
        let line = tokio::time::timeout(Duration::from_secs(2), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let reply: AttentionSocketMessage = serde_json::from_str(&line).unwrap();
        if valid {
            let AttentionSocketMessage::ProjectionResult { result, .. } = reply else {
                panic!("projection ACK")
            };
            assert_eq!(
                result.digest,
                format!("{:x}", Sha256::digest(projection.to_string().as_bytes()))
            );
            assert!(result.applied);
            let snapshot = store.snapshot().unwrap();
            assert_eq!(snapshot.items.len(), 1);
            assert!(snapshot.items[0].eligible);
        } else {
            assert!(matches!(
                reply,
                AttentionSocketMessage::MutationError { .. }
            ));
            assert!(store.snapshot().unwrap().items.is_empty());
        }
    }
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
}

fn draft() -> AttentionDraft {
    AttentionDraft {
        subject_kind: AttentionSubjectKind::Session,
        subject_id: SESSION_ID.to_owned(),
        kind: AttentionKind::TurnReady,
        source_operation_id: OPERATION_ID.to_owned(),
        created_at_ms: 100,
        linked_run_id: Some(RUN_ID.to_owned()),
        stage: Some("review/turn-1".to_owned()),
        code: None,
    }
}

#[test]
fn skill_conditions_have_closed_codes_and_fixed_safe_reasons() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let store = AttentionStore::new(
        temporary.path(),
        Some(louiselm_capture::RunStore::new(temporary.path().join("runs")).unwrap()),
    )
    .expect("store");
    let approval = AttentionDraft {
        subject_kind: AttentionSubjectKind::Run,
        subject_id: RUN_ID.to_owned(),
        kind: AttentionKind::SkillApprovalPending,
        source_operation_id: OPERATION_ID.to_owned(),
        created_at_ms: 100,
        linked_run_id: None,
        stage: None,
        code: Some(AttentionCode::AdmissionRequired),
    };
    let approval_snapshot = store.upsert(approval.clone()).expect("approval pending");
    assert_eq!(
        approval_snapshot.items[0].reason,
        "Skill approval is pending"
    );
    assert_eq!(
        approval_snapshot.items[0].code,
        Some(AttentionCode::AdmissionRequired)
    );

    let unverified = AttentionDraft {
        subject_kind: AttentionSubjectKind::Session,
        subject_id: SESSION_ID.to_owned(),
        kind: AttentionKind::SkillUnverified,
        source_operation_id: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".to_owned(),
        created_at_ms: 101,
        linked_run_id: Some(RUN_ID.to_owned()),
        stage: None,
        code: Some(AttentionCode::WitnessMissing),
    };
    let snapshot = store.upsert(unverified.clone()).expect("unverified skill");
    assert_eq!(snapshot.items[1].reason, "Skill supply is unverified");
    assert_eq!(snapshot.items[1].code, Some(AttentionCode::WitnessMissing));
    assert_eq!(
        store.summary().expect("summary").kinds,
        [
            (AttentionKind::SkillApprovalPending, 1),
            (AttentionKind::SkillUnverified, 1),
        ]
        .into_iter()
        .collect()
    );

    let mut missing_code = unverified.clone();
    missing_code.code = None;
    assert!(store.upsert(missing_code).is_err());
    let mut wrong_code = approval;
    wrong_code.code = Some(AttentionCode::WitnessMissing);
    assert!(store.upsert(wrong_code).is_err());
    let mut unrelated_code = draft();
    unrelated_code.code = Some(AttentionCode::WitnessMissing);
    assert!(store.upsert(unrelated_code).is_err());
    let mut text_field = unverified;
    text_field.stage = Some("candidate/summary".to_owned());
    assert!(store.upsert(text_field).is_err());

    let hostile = serde_json::json!({
        "subject_kind": "session",
        "subject_id": SESSION_ID,
        "kind": "skill_unverified",
        "source_operation_id": "cccccccc-cccc-4ccc-8ccc-cccccccccccc",
        "created_at_ms": 102,
        "linked_run_id": RUN_ID,
        "stage": null,
        "code": "/home/operator/.ssh/id_ed25519"
    });
    assert!(serde_json::from_value::<AttentionDraft>(hostile).is_err());
    assert_eq!(store.snapshot().expect("unchanged").generation, 2);
}

#[test]
fn session_kind_clear_preserves_other_unresolved_conditions() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let store = AttentionStore::new(
        temporary.path(),
        Some(louiselm_capture::RunStore::new(temporary.path().join("runs")).unwrap()),
    )
    .expect("store");
    store.upsert(draft()).expect("turn ready");
    store
        .upsert(AttentionDraft {
            kind: AttentionKind::PermissionRequired,
            source_operation_id: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".to_owned(),
            ..draft()
        })
        .expect("permission");

    let reopened = AttentionStore::new(
        temporary.path(),
        Some(louiselm_capture::RunStore::new(temporary.path().join("runs")).unwrap()),
    )
    .expect("reopen");
    let cleared = reopened
        .clear_session_kind(SESSION_ID, AttentionKind::TurnReady)
        .expect("clear ready kind");
    assert_eq!(cleared.generation, 3);
    assert_eq!(cleared.items.len(), 1);
    assert_eq!(cleared.items[0].kind, AttentionKind::PermissionRequired);
    assert_eq!(
        reopened
            .clear_session_kind(SESSION_ID, AttentionKind::TurnReady)
            .expect("idempotent clear")
            .generation,
        3
    );
}

#[test]
fn quarantined_code_survives_durable_attention_round_trip() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let store = AttentionStore::new(temporary.path(), None).expect("store");
    let mut value = serde_json::to_value(draft()).expect("draft");
    value["kind"] = serde_json::json!("skill_unverified");
    value["code"] = serde_json::json!("quarantined");
    value["stage"] = serde_json::Value::Null;
    let condition: AttentionDraft = serde_json::from_value(value).expect("quarantine code");
    store.upsert(condition).expect("persist quarantine");
    let reopened = AttentionStore::new(temporary.path(), None).expect("reopen");
    let snapshot = serde_json::to_value(reopened.snapshot().expect("snapshot")).expect("json");
    assert_eq!(snapshot["items"][0]["code"], "quarantined");
}

fn key() -> AttentionKey {
    AttentionKey {
        subject_kind: AttentionSubjectKind::Session,
        subject_id: SESSION_ID.to_owned(),
        kind: AttentionKind::TurnReady,
        source_operation_id: OPERATION_ID.to_owned(),
    }
}

#[test]
fn attention_store_is_idempotent_revisioned_and_restart_safe() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let store = AttentionStore::new(
        temporary.path(),
        Some(louiselm_capture::RunStore::new(temporary.path().join("runs")).unwrap()),
    )
    .expect("store");
    assert_eq!(store.snapshot().expect("empty").generation, 0);

    let added = store.upsert(draft()).expect("upsert");
    assert_eq!(added.generation, 1);
    assert_eq!(added.items.len(), 1);
    assert!(!added.items[0].eligible);
    assert_eq!(added.items[0].reason, "Agent turn is ready");

    assert_eq!(store.upsert(draft()).expect("replay").generation, 1);
    assert_eq!(
        store
            .set_eligible(&key(), true)
            .expect("eligible")
            .generation,
        2
    );
    assert_eq!(
        store
            .set_eligible(&key(), true)
            .expect("replay eligible")
            .generation,
        2
    );

    let reopened = AttentionStore::new(
        temporary.path(),
        Some(louiselm_capture::RunStore::new(temporary.path().join("runs")).unwrap()),
    )
    .expect("reopen");
    let persisted = reopened.snapshot().expect("persisted");
    assert_eq!(persisted.generation, 2);
    assert!(persisted.items[0].eligible);
    let older = AttentionDraft {
        source_operation_id: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".to_owned(),
        ..draft()
    };
    assert_eq!(
        reopened
            .upsert(older)
            .expect("second condition")
            .items
            .len(),
        2
    );
    assert_eq!(
        reopened
            .clear_session(SESSION_ID)
            .expect("clear session")
            .generation,
        4
    );
    assert!(
        reopened
            .snapshot()
            .expect("session cleared")
            .items
            .is_empty()
    );
    assert_eq!(
        reopened
            .clear_session(SESSION_ID)
            .expect("replay clear session")
            .generation,
        4
    );
    assert_eq!(reopened.clear(&key()).expect("clear").generation, 4);
    assert!(reopened.snapshot().expect("cleared").items.is_empty());
    assert_eq!(reopened.clear(&key()).expect("replay clear").generation, 4);
}

#[test]
fn attention_validation_rejects_untrusted_fields_before_state_changes() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let store = AttentionStore::new(
        temporary.path(),
        Some(louiselm_capture::RunStore::new(temporary.path().join("runs")).unwrap()),
    )
    .expect("store");
    let mut invalid = draft();
    invalid.stage = Some("arbitrary text".to_owned());
    assert!(store.upsert(invalid).is_err());
    assert_eq!(store.snapshot().expect("unchanged").generation, 0);

    let mut invalid = draft();
    invalid.subject_kind = AttentionSubjectKind::Run;
    assert!(store.upsert(invalid).is_err());
    assert_eq!(store.snapshot().expect("unchanged").generation, 0);

    let persisted = temporary.path().join("attention.json");
    fs::write(
        persisted,
        r#"{"schema_version":1,"generation":0,"items":[],"unknown":"reject"}"#,
    )
    .expect("malformed state");
    assert!(
        AttentionStore::new(
            temporary.path(),
            Some(louiselm_capture::RunStore::new(temporary.path().join("runs")).unwrap())
        )
        .is_err()
    );
}

#[tokio::test]
async fn mutation_result_skips_interleaved_attention_changes() {
    let (mut server, client) = UnixStream::pair().expect("socket pair");
    let snapshot = AttentionSnapshot {
        generation: 2,
        items: vec![],
    };
    // louiselm-efhro: the socket multiplexes invalidations with correlated ACKs.
    for message in [
        AttentionSocketMessage::AttentionChanged { generation: 1 },
        AttentionSocketMessage::AttentionChanged { generation: 2 },
        AttentionSocketMessage::MutationResult {
            request_id: "clear".to_owned(),
            snapshot: snapshot.clone(),
        },
    ] {
        server
            .write_all(format!("{}\n", serde_json::to_string(&message).unwrap()).as_bytes())
            .await
            .expect("write frame");
    }
    let mut lines = BufReader::new(client).lines();
    assert_eq!(mutation_result(&mut lines, "clear").await, snapshot);
}

async fn mutation_result(
    lines: &mut Lines<BufReader<UnixStream>>,
    expected_id: &str,
) -> AttentionSnapshot {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let message: AttentionSocketMessage =
                serde_json::from_str(&lines.next_line().await.expect("read").expect("reply"))
                    .expect("reply JSON");
            match message {
                AttentionSocketMessage::AttentionChanged { .. } => {}
                AttentionSocketMessage::MutationResult {
                    request_id,
                    snapshot,
                } => {
                    assert_eq!(request_id, expected_id);
                    return snapshot;
                }
                _ => panic!("expected mutation result, got {message:?}"),
            }
        }
    })
    .await
    .expect("mutation reply deadline")
}

#[tokio::test]
async fn attention_socket_requires_capability_and_retries_without_new_generation() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let store = AttentionStore::new(
        temporary.path().join("attention"),
        Some(louiselm_capture::RunStore::new(temporary.path().join("runs")).unwrap()),
    )
    .expect("store");
    let socket_path = temporary.path().join("attention.sock");
    let capability_path = temporary.path().join("operator-capability");
    let socket = AttentionSocket::bind(&socket_path, &capability_path, store.clone())
        .await
        .expect("bind");
    let capability = fs::read_to_string(&capability_path).expect("capability");
    let server = tokio::spawn(socket.serve());
    let stream = UnixStream::connect(&socket_path).await.expect("connect");
    let mut lines = BufReader::new(stream).lines();
    let initial: AttentionSocketMessage = serde_json::from_str(
        &lines
            .next_line()
            .await
            .expect("read")
            .expect("initial snapshot"),
    )
    .expect("snapshot JSON");
    assert!(matches!(initial, AttentionSocketMessage::Snapshot { .. }));

    let invalid = serde_json::json!({
        "type": "upsert",
        "request_id": "invalid",
        "attention": draft(),
        "capability": "wrong"
    });
    lines
        .get_mut()
        .write_all(format!("{invalid}\n").as_bytes())
        .await
        .expect("invalid request");
    let error: AttentionSocketMessage =
        serde_json::from_str(&lines.next_line().await.expect("read").expect("error"))
            .expect("error JSON");
    assert!(matches!(
        error,
        AttentionSocketMessage::MutationError { .. }
    ));
    assert_eq!(store.snapshot().expect("unchanged").generation, 0);

    let valid = serde_json::json!({
        "type": "upsert",
        "request_id": "request-1",
        "attention": draft(),
        "capability": capability
    });
    lines
        .get_mut()
        .write_all(format!("{valid}\n").as_bytes())
        .await
        .expect("valid request");
    assert_eq!(mutation_result(&mut lines, "request-1").await.generation, 1);
    assert_eq!(store.snapshot().expect("added").generation, 1);

    let replay = serde_json::json!({
        "type": "upsert",
        "request_id": "request-2",
        "attention": draft(),
        "capability": fs::read_to_string(&capability_path).expect("capability")
    });
    lines
        .get_mut()
        .write_all(format!("{replay}\n").as_bytes())
        .await
        .expect("replay request");
    assert_eq!(mutation_result(&mut lines, "request-2").await.generation, 1);
    assert_eq!(store.snapshot().expect("replay").generation, 1);

    let clear_session_kind = serde_json::json!({
        "type": "clear_session_kind",
        "request_id": "request-3",
        "session_id": SESSION_ID,
        "kind": "turn_ready",
        "capability": capability
    });
    lines
        .get_mut()
        .write_all(format!("{clear_session_kind}\n").as_bytes())
        .await
        .expect("clear session request");
    assert!(
        mutation_result(&mut lines, "request-3")
            .await
            .items
            .is_empty()
    );
    assert!(store.snapshot().expect("session cleared").items.is_empty());
    server.abort();
    let _ = tokio::time::timeout(Duration::from_secs(1), server).await;
}
