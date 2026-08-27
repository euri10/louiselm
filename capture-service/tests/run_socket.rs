use std::{fs, time::Duration};

use louiselm_capture::{
    GeneratedWorkReservation, ReserveResult, RunAdmission, RunSession, RunSocket, RunSocketMessage,
    RunStore,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
};

const TOKEN: &str = "generate-token-1234";
const RUN_ID: &str = "11111111-2222-4333-8444-555555555555";

#[tokio::test]
async fn socket_sends_snapshot_then_revision_only_invalidation() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let store = RunStore::new(temporary.path().join("runs")).expect("store");
    store
        .admit(
            RunAdmission {
                id: RUN_ID.to_owned(),
                generated_work_ceiling: 3,
                park_ttl_ms: 60_000,
            },
            TOKEN,
        )
        .expect("admit");
    let socket_path = temporary.path().join("run.sock");
    let capability_path = temporary.path().join("operator-capability");
    let socket = RunSocket::bind(&socket_path, &capability_path, store.clone())
        .await
        .expect("bind");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&socket_path)
                .expect("mode")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    let server = tokio::spawn(socket.serve());
    let stream = UnixStream::connect(&socket_path).await.expect("connect");
    let mut lines = BufReader::new(stream).lines();
    let initial_line = lines.next_line().await.expect("read").expect("snapshot");
    assert!(!initial_line.contains(TOKEN));
    let snapshot: RunSocketMessage = serde_json::from_str(&initial_line).expect("snapshot JSON");
    let RunSocketMessage::Snapshot { runs } = snapshot else {
        panic!("expected snapshot");
    };
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].revision, 1);

    store
        .attach(RunSession {
            id: RUN_ID.to_owned(),
            session_id: "codex/session".to_owned(),
            agent: "codex".to_owned(),
            acp_session_id: "session".to_owned(),
            working_dir: "/tmp/project".to_owned(),
            load_session: true,
        })
        .expect("attach");
    let changed = tokio::time::timeout(Duration::from_secs(1), lines.next_line())
        .await
        .expect("invalidation timeout")
        .expect("read")
        .expect("invalidation");
    assert_eq!(
        serde_json::from_str::<RunSocketMessage>(&changed).expect("invalidation JSON"),
        RunSocketMessage::RunChanged {
            id: RUN_ID.to_owned(),
            revision: 2,
        }
    );
    lines
        .get_mut()
        .write_all(b"{\"type\":\"snapshot\"}\n")
        .await
        .expect("request snapshot");
    let refreshed: RunSocketMessage = serde_json::from_str(
        &lines
            .next_line()
            .await
            .expect("read")
            .expect("refreshed snapshot"),
    )
    .expect("refreshed JSON");
    let RunSocketMessage::Snapshot { runs } = refreshed else {
        panic!("expected refreshed snapshot");
    };
    assert_eq!(runs[0].revision, 2);
    server.abort();
}

#[tokio::test]
async fn bind_refuses_live_or_non_socket_paths() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let store = RunStore::new(temporary.path().join("runs")).expect("store");
    let socket_path = temporary.path().join("run.sock");
    let capability_path = temporary.path().join("operator-capability");
    let first = RunSocket::bind(&socket_path, &capability_path, store.clone())
        .await
        .expect("first");
    assert!(
        RunSocket::bind(&socket_path, &capability_path, store.clone())
            .await
            .is_err()
    );
    drop(first);
    fs::remove_file(&socket_path).expect("remove socket");
    fs::write(&socket_path, b"keep me").expect("regular file");
    assert!(
        RunSocket::bind(&socket_path, &capability_path, store)
            .await
            .is_err()
    );
    assert_eq!(fs::read(&socket_path).expect("preserved"), b"keep me");
}

#[tokio::test]
async fn operator_mutations_require_the_private_capability_and_current_revision() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let store = RunStore::new(temporary.path().join("runs")).expect("store");
    store
        .admit(
            RunAdmission {
                id: RUN_ID.to_owned(),
                generated_work_ceiling: 1,
                park_ttl_ms: 60_000,
            },
            TOKEN,
        )
        .expect("admit");
    store
        .attach(RunSession {
            id: RUN_ID.to_owned(),
            session_id: "codex/session".to_owned(),
            agent: "codex".to_owned(),
            acp_session_id: "session".to_owned(),
            working_dir: "/tmp/project".to_owned(),
            load_session: true,
        })
        .expect("attach");
    let first = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    store
        .reserve_generated_work(
            GeneratedWorkReservation {
                run_id: RUN_ID.to_owned(),
                token: TOKEN.to_owned(),
                mutation_id: first.to_owned(),
                kind: "beads_issue".to_owned(),
                units: 1,
            },
            1,
        )
        .expect("reserve");
    store
        .confirm_generated_work(RUN_ID, TOKEN, first, "created")
        .expect("confirm");
    assert_eq!(
        store
            .reserve_generated_work(
                GeneratedWorkReservation {
                    run_id: RUN_ID.to_owned(),
                    token: TOKEN.to_owned(),
                    mutation_id: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".to_owned(),
                    kind: "beads_issue".to_owned(),
                    units: 1,
                },
                2,
            )
            .expect("Park"),
        ReserveResult::Exhausted
    );
    let parked = store.run(RUN_ID).expect("Parked");
    let socket_path = temporary.path().join("run.sock");
    let capability_path = temporary.path().join("operator-capability");
    let socket = RunSocket::bind(&socket_path, &capability_path, store.clone())
        .await
        .expect("bind");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&capability_path)
                .expect("capability mode")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    let capability = fs::read_to_string(&capability_path).expect("capability");
    assert_ne!(capability, TOKEN);
    let server = tokio::spawn(socket.serve());
    let stream = UnixStream::connect(&socket_path).await.expect("connect");
    let mut lines = BufReader::new(stream).lines();
    lines.next_line().await.expect("read").expect("snapshot");

    let invalid = serde_json::json!({
        "type": "raise",
        "request_id": "invalid",
        "id": RUN_ID,
        "expected_revision": parked.revision,
        "ceiling": 2,
        "capability": TOKEN,
    });
    lines
        .get_mut()
        .write_all(format!("{invalid}\n").as_bytes())
        .await
        .expect("invalid raise");
    assert!(matches!(
        serde_json::from_str::<RunSocketMessage>(
            &lines
                .next_line()
                .await
                .expect("read")
                .expect("invalid response")
        )
        .expect("response"),
        RunSocketMessage::MutationError { .. }
    ));
    assert_eq!(
        store.run(RUN_ID).expect("unchanged").generated_work.ceiling,
        1
    );

    let raise = serde_json::json!({
        "type": "raise",
        "request_id": "raise",
        "id": RUN_ID,
        "expected_revision": parked.revision,
        "ceiling": 2,
        "capability": capability,
    });
    lines
        .get_mut()
        .write_all(format!("{raise}\n").as_bytes())
        .await
        .expect("raise");
    let response = serde_json::from_str::<RunSocketMessage>(
        &lines
            .next_line()
            .await
            .expect("read")
            .expect("raise response"),
    )
    .expect("response");
    let RunSocketMessage::MutationResult { run, .. } = response else {
        panic!("expected mutation result");
    };
    assert_eq!(run.generated_work_ceiling, 2);
    assert_eq!(run.revision, parked.revision + 1);
    server.abort();
}
