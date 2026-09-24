//! Authenticated, read-only Run facts on the existing broker endpoint.
#![allow(
    clippy::unwrap_used,
    reason = "Fixtures assert transport and storage outcomes."
)]

use louiselm_capture::{
    AttentionStore, BrokerAttentionConfig, BrokerAttentionSocket, RunAdmission, RunStore,
};
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
};

const ID: &str = "12345678-1234-4234-8234-123456789abc";
const TOKEN: &str = "broker-only-secret";

async fn query(path: &std::path::Path, token: &str, id: &str) -> serde_json::Value {
    let mut stream = UnixStream::connect(path).await.unwrap();
    let request = serde_json::json!({"type":"run_lifecycle", "request_id":"read-1", "run_id":id, "capability":token});
    stream
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();
    let line = BufReader::new(stream)
        .lines()
        .next_line()
        .await
        .unwrap()
        .unwrap();
    serde_json::from_str(&line).unwrap()
}

#[tokio::test]
async fn broker_reads_only_exact_run_facts_and_refuses_unavailable_authority() {
    let root = tempfile::tempdir().unwrap();
    let runtime = root.path().join("runtime");
    fs::create_dir(&runtime).unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o711)).unwrap();
    let config = BrokerAttentionConfig {
        socket: runtime.join("project.sock"),
        broker_uid: fs::metadata(&runtime).unwrap().uid(),
        capability_sha256: format!("{:x}", Sha256::digest(TOKEN)),
    };
    let runs = RunStore::new(root.path().join("runs")).unwrap();
    runs.admit(
        RunAdmission {
            id: ID.into(),
            generated_work_ceiling: 5,
            park_ttl_ms: 1000,
        },
        "operator-run-secret",
    )
    .unwrap();
    let attention = AttentionStore::new(root.path().join("attention"), Some(runs.clone())).unwrap();
    let server = tokio::spawn(
        BrokerAttentionSocket::bind(config.clone(), attention)
            .await
            .unwrap()
            .serve(),
    );
    assert_eq!(
        query(&config.socket, TOKEN, ID).await,
        serde_json::json!({
            "type":"run_lifecycle_result", "request_id":"read-1",
            "result":{"run_id":ID,"revision":1,"state":"admitted"}
        })
    );
    for (token, id) in [
        ("wrong", ID),
        (TOKEN, "87654321-1234-4234-8234-123456789abc"),
        (TOKEN, "../escape"),
    ] {
        let reply = query(&config.socket, token, id).await;
        assert_eq!(reply["type"], "mutation_error");
        assert!(reply.get("result").is_none());
    }
    assert_eq!(runs.run(ID).unwrap().revision, 1);
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
    let unset = AttentionStore::new(root.path().join("unset"), None).unwrap();
    let server = tokio::spawn(
        BrokerAttentionSocket::bind(config.clone(), unset)
            .await
            .unwrap()
            .serve(),
    );
    assert_eq!(
        query(&config.socket, TOKEN, ID).await["type"],
        "mutation_error"
    );
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
}

#[tokio::test]
#[ignore = "cross-crate broker binary supplied by scripts/test-skill-requests"]
async fn cross_crate_requests_survive_restart_park_and_disposal() {
    use louiselm_capture::{RunDraft, RunSession};
    let root = tempfile::tempdir().unwrap();
    let runtime = root.path().join("runtime");
    fs::create_dir(&runtime).unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o711)).unwrap();
    let runs = RunStore::new(root.path().join("runs")).unwrap();
    runs.admit(
        RunAdmission {
            id: ID.into(),
            generated_work_ceiling: 5,
            park_ttl_ms: 1000,
        },
        TOKEN,
    )
    .unwrap();
    runs.attach(RunSession {
        id: ID.into(),
        session_id: "skill-session".into(),
        agent: "codex".into(),
        acp_session_id: "fixture".into(),
        working_dir: "/tmp".into(),
        load_session: true,
    })
    .unwrap();
    let store = AttentionStore::new(root.path().join("attention"), Some(runs.clone())).unwrap();
    let config = BrokerAttentionConfig {
        socket: runtime.join("broker.sock"),
        broker_uid: fs::metadata(&runtime).unwrap().uid(),
        capability_sha256: format!("{:x}", Sha256::digest(TOKEN)),
    };
    let token = root.path().join("capability");
    fs::write(&token, TOKEN).unwrap();
    fs::set_permissions(&token, fs::Permissions::from_mode(0o600)).unwrap();
    let endpoint = root.path().join("endpoint.json");
    fs::write(
        &endpoint,
        serde_json::to_vec(&serde_json::json!({"socket":config.socket,
        "receiver_uid":config.broker_uid,"capability_file":token}))
        .unwrap(),
    )
    .unwrap();
    let mut server = tokio::spawn(
        BrokerAttentionSocket::bind(config.clone(), store.clone())
            .await
            .unwrap()
            .serve(),
    );
    let broker = root.path().join("broker");
    fs::create_dir(&broker).unwrap();
    for (phase, expected) in [("accept", 2), ("park", 2), ("disposed", 1)] {
        if phase == "park" {
            runs.park_cold(
                RunDraft {
                    id: ID.into(),
                    session_id: "skill-session".into(),
                    agent: "codex".into(),
                    acp_session_id: "fixture".into(),
                    working_dir: "/tmp".into(),
                    load_session: true,
                    claimed_issue_ids: vec![],
                },
                1000,
            )
            .unwrap();
            server.abort();
            assert!(server.await.unwrap_err().is_cancelled());
            server = tokio::spawn(
                BrokerAttentionSocket::bind(config.clone(), store.clone())
                    .await
                    .unwrap()
                    .serve(),
            );
        } else if phase == "disposed" {
            runs.reap_expired(2001, |_| Ok(())).unwrap();
            assert_eq!(runs.run(ID).unwrap().state, "disposed");
        }
        run_broker_fixture(&broker, &endpoint, phase).await;
        let items = store.snapshot().unwrap().items;
        assert_eq!(items.len(), expected, "phase {phase}");
        assert!(
            items
                .iter()
                .all(|item| item.kind == louiselm_capture::AttentionKind::SkillApprovalPending)
        );
    }
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
}

async fn run_broker_fixture(broker: &std::path::Path, endpoint: &std::path::Path, phase: &str) {
    let mut command =
        std::process::Command::new(std::env::var_os("LOUISELM_TEST_REQUEST_BROKER").unwrap());
    command
        .args([
            "--ignored",
            "--exact",
            "skill_requests::cross_service_requests",
            "--nocapture",
        ])
        .env("LOUISELM_REQUEST_STATE", broker)
        .env("LOUISELM_REQUEST_ENDPOINT", endpoint)
        .env("LOUISELM_REQUEST_PHASE", phase);
    let output = tokio::task::spawn_blocking(move || command.output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        output.status.success(),
        "broker phase {phase}: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
#[ignore = "cross-crate broker binary and Neovim supplied by scripts/test-skill-requests"]
async fn cross_crate_posture_reaches_neovim() {
    for fixture in [
        "conformance::status::current::posture_waiver_reaches_neovim",
        "conformance::status::current::posture_revocation_reaches_neovim",
        "skill_requests::verified_admission_reaches_neovim",
    ] {
        let root = tempfile::tempdir().unwrap();
        let runtime = root.path().join("runtime");
        fs::create_dir(&runtime).unwrap();
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o711)).unwrap();
        let store = AttentionStore::new(root.path().join("attention"), None).unwrap();
        let token = root.path().join("token");
        fs::write(&token, TOKEN).unwrap();
        fs::set_permissions(&token, fs::Permissions::from_mode(0o600)).unwrap();
        let config = BrokerAttentionConfig {
            socket: runtime.join("broker.sock"),
            broker_uid: fs::metadata(root.path()).unwrap().uid(),
            capability_sha256: format!("{:x}", Sha256::digest(TOKEN)),
        };
        let endpoint = root.path().join("endpoint.json");
        fs::write(
            &endpoint,
            serde_json::to_vec(&serde_json::json!({
                "socket": config.socket, "receiver_uid": config.broker_uid, "capability_file": token
            }))
            .unwrap(),
        )
        .unwrap();
        let broker = tokio::spawn(
            BrokerAttentionSocket::bind(config, store.clone())
                .await
                .unwrap()
                .serve(),
        );
        let socket = root.path().join("observer.sock");
        let observer = tokio::spawn(
            louiselm_capture::AttentionSocket::bind(&socket, &root.path().join("operator"), store)
                .await
                .unwrap()
                .serve(),
        );
        let mut command =
            std::process::Command::new(std::env::var_os("LOUISELM_TEST_REQUEST_BROKER").unwrap());
        command
            .args(["--ignored", "--exact", fixture, "--nocapture"])
            .env("LOUISELM_REQUEST_ENDPOINT", endpoint)
            .env("LOUISELM_ATTENTION_SOCKET", socket);
        let output = tokio::task::spawn_blocking(move || command.output())
            .await
            .unwrap()
            .unwrap();
        broker.abort();
        observer.abort();
        assert!(broker.await.unwrap_err().is_cancelled());
        assert!(observer.await.unwrap_err().is_cancelled());
        assert!(
            output.status.success(),
            "{fixture}: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
