//! Durable Run lifecycle, rather than editor-local callbacks, owns local Park alerts.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Behavioral fixtures abort on setup failure."
)]

use louiselm_capture::{
    AttentionDraft, AttentionKey, AttentionKind, AttentionStore, AttentionSubjectKind,
    BrokerProjection, ProjectionChange, RunAdmission, RunDraft, RunSession, RunStore,
};

// Captured 2026-09-15: attention generation488 still advertised this disposed Run.
// ACP01a0a402-a05c-75d3-9ef5-c0b1866d7666; louiselm-t6dy records the safe projection.
const RUN: &str = "6cd3acf4-2756-4806-9538-aa6041c71eaa";
const PARK_OPERATION: &str = "a5cec8aa-441f-4afa-b7c0-7b26efc21ba9";
const OTHER_OPERATION: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

fn park_alert() -> AttentionDraft {
    AttentionDraft {
        subject_kind: AttentionSubjectKind::Run,
        subject_id: RUN.into(),
        kind: AttentionKind::RunParked,
        source_operation_id: PARK_OPERATION.into(),
        created_at_ms: 100,
        linked_run_id: None,
        stage: None,
        code: None,
    }
}

fn park_key() -> AttentionKey {
    AttentionKey {
        subject_kind: AttentionSubjectKind::Run,
        subject_id: RUN.into(),
        kind: AttentionKind::RunParked,
        source_operation_id: PARK_OPERATION.into(),
    }
}

fn park(runs: &RunStore, ttl_ms: u64) {
    runs.admit(
        RunAdmission {
            id: RUN.into(),
            generated_work_ceiling: 1,
            park_ttl_ms: ttl_ms,
        },
        "qa-test-token-1234",
    )
    .unwrap();
    runs.attach(RunSession {
        id: RUN.into(),
        session_id: "codex/qa".into(),
        agent: "codex".into(),
        acp_session_id: "qa".into(),
        working_dir: "/tmp/qa".into(),
        load_session: true,
    })
    .unwrap();
    runs.park_cold(
        RunDraft {
            id: RUN.into(),
            session_id: "codex/qa".into(),
            agent: "codex".into(),
            acp_session_id: "qa".into(),
            working_dir: "/tmp/qa".into(),
            load_session: true,
            claimed_issue_ids: vec![],
        },
        100,
    )
    .unwrap();
}

#[test]
fn resumed_park_is_cleared_after_restart_without_clearing_broker_or_session_items() {
    let root = tempfile::tempdir().unwrap();
    let runs = RunStore::new(root.path().join("runs")).unwrap();
    let attention = AttentionStore::new(
        root.path().join("attention"),
        Some(louiselm_capture::RunStore::new(root.path().join("runs")).unwrap()),
    )
    .unwrap();
    park(&runs, u64::MAX - 200);
    let initial = attention.upsert(park_alert()).unwrap();
    assert!(!initial.items[0].eligible);
    attention.set_eligible(&park_key(), true).unwrap();
    assert!(attention.snapshot().unwrap().items[0].eligible);
    attention
        .project(&BrokerProjection {
            sequence: 1,
            change: ProjectionChange::Upsert {
                attention: AttentionDraft {
                    source_operation_id: OTHER_OPERATION.into(),
                    ..park_alert()
                },
            },
        })
        .unwrap();
    attention
        .upsert(AttentionDraft {
            subject_kind: AttentionSubjectKind::Session,
            subject_id: "codex/qa".into(),
            kind: AttentionKind::TurnReady,
            ..park_alert()
        })
        .unwrap();
    let before = attention.snapshot().unwrap();
    runs.begin_resume(
        RUN,
        runs.view(RUN).unwrap().revision,
        OTHER_OPERATION,
        101,
        1000,
    )
    .unwrap();
    runs.finalize_resume(RUN, runs.view(RUN).unwrap().revision, OTHER_OPERATION, true)
        .unwrap();
    drop(attention);

    let restarted = AttentionStore::new(
        root.path().join("attention"),
        Some(louiselm_capture::RunStore::new(root.path().join("runs")).unwrap()),
    )
    .unwrap();
    let current = restarted.snapshot().unwrap();
    assert_eq!(
        current.items.len(),
        2,
        "only the local Park condition resolves"
    );
    assert_eq!(current.generation, before.generation + 1);
    assert!(
        current
            .items
            .iter()
            .any(|item| item.source_operation_id == OTHER_OPERATION && item.eligible)
    );
    assert_eq!(
        restarted.upsert(park_alert()).unwrap(),
        current,
        "late upsert must not recreate Park"
    );
    assert!(restarted.set_eligible(&park_key(), true).is_err());
    assert_eq!(restarted.snapshot().unwrap(), current);
}

#[test]
fn expired_disposal_clears_park_and_rejects_late_delivery_callbacks() {
    let root = tempfile::tempdir().unwrap();
    let runs = RunStore::new(root.path().join("runs")).unwrap();
    let attention = AttentionStore::new(
        root.path().join("attention"),
        Some(louiselm_capture::RunStore::new(root.path().join("runs")).unwrap()),
    )
    .unwrap();
    park(&runs, u64::MAX - 200);
    attention.upsert(park_alert()).unwrap();
    attention.set_eligible(&park_key(), true).unwrap();
    assert_eq!(
        runs.reap_expired(u64::MAX - 100, |_| Ok(()))
            .unwrap()
            .disposed,
        1
    );
    let cleared = attention.snapshot().unwrap();
    assert!(cleared.items.is_empty());
    assert_eq!(cleared.generation, 3);
    assert_eq!(attention.upsert(park_alert()).unwrap(), cleared);
    assert!(attention.set_eligible(&park_key(), true).is_err());
    assert_eq!(attention.snapshot().unwrap(), cleared);
}

#[test]
fn missing_local_run_does_not_resolve_an_unknown_park_condition() {
    let root = tempfile::tempdir().unwrap();
    let attention = AttentionStore::new(
        root.path().join("attention"),
        Some(louiselm_capture::RunStore::new(root.path().join("runs")).unwrap()),
    )
    .unwrap();
    attention.upsert(park_alert()).unwrap();
    let eligible = attention.set_eligible(&park_key(), true).unwrap();
    assert_eq!(attention.snapshot().unwrap(), eligible);
}

#[test]
fn expired_park_is_not_deliverable_while_cleanup_is_still_pending() {
    let root = tempfile::tempdir().unwrap();
    let runs = RunStore::new(root.path().join("runs")).unwrap();
    park(&runs, 1);
    let attention = AttentionStore::new(root.path().join("attention"), Some(runs.clone())).unwrap();
    assert!(attention.upsert(park_alert()).unwrap().items.is_empty());
    assert_eq!(runs.view(RUN).unwrap().state, "cold_parked");
    assert_eq!(attention.snapshot().unwrap().generation, 0);
}

#[test]
fn failed_resume_can_publish_a_new_valid_park_with_normal_eligibility() {
    let root = tempfile::tempdir().unwrap();
    let runs = RunStore::new(root.path().join("runs")).unwrap();
    park(&runs, u64::MAX - 200);
    let attention = AttentionStore::new(root.path().join("attention"), Some(runs.clone())).unwrap();
    attention.upsert(park_alert()).unwrap();
    attention.set_eligible(&park_key(), true).unwrap();
    runs.begin_resume(
        RUN,
        runs.view(RUN).unwrap().revision,
        OTHER_OPERATION,
        101,
        u64::MAX - 200,
    )
    .unwrap();
    assert!(attention.snapshot().unwrap().items.is_empty());
    runs.finalize_resume(
        RUN,
        runs.view(RUN).unwrap().revision,
        OTHER_OPERATION,
        false,
    )
    .unwrap();
    let parked = attention.upsert(park_alert()).unwrap();
    assert_eq!(parked.items.len(), 1);
    assert!(
        !parked.items[0].eligible,
        "reconciliation must not bypass inactivity policy"
    );
    assert!(attention.set_eligible(&park_key(), true).unwrap().items[0].eligible);
}

#[test]
fn invalid_run_state_returns_an_error_without_erasing_attention() {
    let root = tempfile::tempdir().unwrap();
    let runs = RunStore::new(root.path().join("runs")).unwrap();
    park(&runs, u64::MAX - 200);
    let attention = AttentionStore::new(root.path().join("attention"), Some(runs.clone())).unwrap();
    attention.upsert(park_alert()).unwrap();
    let stored = root.path().join("attention/attention.json");
    let before = std::fs::read(&stored).unwrap();
    let run_path = root.path().join("runs").join(format!("{RUN}.json"));
    let mut invalid = serde_json::to_value(runs.run(RUN).unwrap()).unwrap();
    invalid["state"] = "unknown".into();
    std::fs::write(&run_path, serde_json::to_vec(&invalid).unwrap()).unwrap();
    assert!(attention.snapshot().is_err());
    assert_eq!(std::fs::read(&stored).unwrap(), before);
    std::fs::write(&run_path, "invalid JSON").unwrap();
    assert!(attention.upsert(park_alert()).is_err());
    assert_eq!(std::fs::read(&stored).unwrap(), before);
}

#[tokio::test(flavor = "current_thread")]
async fn blocked_attention_read_does_not_block_the_socket_executor() {
    use fs2::FileExt;
    use louiselm_capture::AttentionSocket;
    use std::{fs::OpenOptions, sync::mpsc, time::Duration};
    use tokio::{
        io::{AsyncBufReadExt, BufReader},
        net::UnixStream,
    };

    let root = tempfile::tempdir().unwrap();
    let runs = RunStore::new(root.path().join("runs")).unwrap();
    let attention = AttentionStore::new(root.path().join("attention"), Some(runs)).unwrap();
    let socket_path = root.path().join("attention.sock");
    let socket = AttentionSocket::bind(&socket_path, root.path().join("capability"), attention)
        .await
        .unwrap();
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.path().join("attention/attention.lock"))
        .unwrap();
    lock.lock_exclusive().unwrap();
    let (send, receive) = mpsc::channel();
    let release = std::thread::spawn(move || {
        let executor_progressed = receive.recv_timeout(Duration::from_secs(2)).is_ok();
        FileExt::unlock(&lock).unwrap();
        executor_progressed
    });
    let server = tokio::spawn(socket.serve());
    let stream = UnixStream::connect(&socket_path).await.unwrap();
    let reader = tokio::spawn(async move { BufReader::new(stream).lines().next_line().await });
    tokio::time::sleep(Duration::from_millis(20)).await;
    let heartbeat_delivered = send.send(()).is_ok();
    assert!(
        release.join().unwrap() && heartbeat_delivered,
        "Attention I/O blocked the executor"
    );
    assert!(
        tokio::time::timeout(Duration::from_secs(2), reader)
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .is_some()
    );
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
}
