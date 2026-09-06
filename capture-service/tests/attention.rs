//! Behavioral coverage for attention.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Test fixtures abort on setup failure and assert failures directly."
)]

use std::{fs, time::Duration};

use louiselm_capture::{
    AttentionCode, AttentionDraft, AttentionKey, AttentionKind, AttentionSocket,
    AttentionSocketMessage, AttentionStore, AttentionSubjectKind,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
};

const SESSION_ID: &str = "codex/session-1";
const RUN_ID: &str = "11111111-2222-4333-8444-555555555555";
const OPERATION_ID: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

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
    let store = AttentionStore::new(temporary.path()).expect("store");
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
    let store = AttentionStore::new(temporary.path()).expect("store");
    store.upsert(draft()).expect("turn ready");
    store
        .upsert(AttentionDraft {
            kind: AttentionKind::PermissionRequired,
            source_operation_id: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".to_owned(),
            ..draft()
        })
        .expect("permission");

    let reopened = AttentionStore::new(temporary.path()).expect("reopen");
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
    let store = AttentionStore::new(temporary.path()).expect("store");
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

    let reopened = AttentionStore::new(temporary.path()).expect("reopen");
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
    let store = AttentionStore::new(temporary.path()).expect("store");
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
    assert!(AttentionStore::new(temporary.path()).is_err());
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "One socket conversation proves authorization and retry generation semantics."
)]
async fn attention_socket_requires_capability_and_retries_without_new_generation() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let store = AttentionStore::new(temporary.path().join("attention")).expect("store");
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
    let result: AttentionSocketMessage =
        serde_json::from_str(&lines.next_line().await.expect("read").expect("result"))
            .expect("result JSON");
    assert!(matches!(
        result,
        AttentionSocketMessage::MutationResult { .. }
    ));
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
    lines
        .next_line()
        .await
        .expect("read")
        .expect("replay result");
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
    let cleared: AttentionSocketMessage = serde_json::from_str(
        &lines
            .next_line()
            .await
            .expect("read")
            .expect("clear result"),
    )
    .expect("clear result JSON");
    assert!(matches!(
        cleared,
        AttentionSocketMessage::MutationResult { .. }
    ));
    assert!(store.snapshot().expect("session cleared").items.is_empty());
    server.abort();
    let _ = tokio::time::timeout(Duration::from_secs(1), server).await;
}
