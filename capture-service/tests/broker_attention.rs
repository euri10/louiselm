//! Projection-only credentials and kernel identity checks.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Behavioral fixtures abort on setup failure."
)]

use louiselm_capture::{
    AttentionSocket, AttentionStore, BrokerAttentionConfig, BrokerAttentionSocket,
};
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
};

const TOKEN: &str = "dedicated-projection-token";

#[tokio::test]
async fn projection_requires_both_kernel_identity_and_scoped_credential() {
    let root = tempfile::tempdir().unwrap();
    let runtime = root.path().join("runtime");
    fs::create_dir(&runtime).unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o711)).unwrap();
    let uid = fs::metadata(&runtime).unwrap().uid();
    let config = BrokerAttentionConfig {
        socket: runtime.join("project.sock"),
        broker_uid: uid,
        capability_sha256: format!("{:x}", Sha256::digest(TOKEN)),
    };
    let store = AttentionStore::new(root.path().join("attention")).unwrap();
    let socket = BrokerAttentionSocket::bind(config.clone(), store.clone())
        .await
        .unwrap();
    let server = tokio::spawn(socket.serve());
    let projection: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/broker_attention_projection.json"
    ))
    .unwrap();
    for capability in ["wrong", TOKEN] {
        let mut stream = UnixStream::connect(&config.socket).await.unwrap();
        let request = serde_json::json!({"type":"project", "request_id":"projection-1",
            "projection":projection, "capability":capability});
        stream
            .write_all(format!("{request}\n").as_bytes())
            .await
            .unwrap();
        let response = tokio::time::timeout(
            Duration::from_secs(2),
            BufReader::new(stream).lines().next_line(),
        )
        .await
        .unwrap()
        .unwrap()
        .unwrap();
        let response: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(
            response["type"],
            if capability == TOKEN {
                "projection_result"
            } else {
                "mutation_error"
            }
        );
        assert_eq!(
            store.snapshot().unwrap().items.len(),
            usize::from(capability == TOKEN)
        );
    }
    for request in [
        serde_json::json!({"type":"snapshot"}),
        serde_json::json!({"type":"clear_session", "request_id":"forbidden", "session_id":"session", "capability":TOKEN}),
    ] {
        let mut stream = UnixStream::connect(&config.socket).await.unwrap();
        stream
            .write_all(format!("{request}\n").as_bytes())
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(
                Duration::from_secs(2),
                BufReader::new(stream).lines().next_line()
            )
            .await
            .unwrap()
            .unwrap()
            .is_none()
        );
    }
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());

    let mut wrong_identity = config.clone();
    wrong_identity.broker_uid = uid + 1;
    let server = tokio::spawn(
        BrokerAttentionSocket::bind(wrong_identity, store.clone())
            .await
            .unwrap()
            .serve(),
    );
    let stream = UnixStream::connect(&config.socket).await.unwrap();
    assert!(
        tokio::time::timeout(
            Duration::from_secs(2),
            BufReader::new(stream).lines().next_line()
        )
        .await
        .unwrap()
        .unwrap()
        .is_none()
    );
    assert_eq!(store.snapshot().unwrap().items.len(), 1);
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn operator_endpoint_never_accepts_broker_projections() {
    let root = tempfile::tempdir().unwrap();
    let store = AttentionStore::new(root.path().join("attention")).unwrap();
    let path = root.path().join("attention.sock");
    let capability = root.path().join("operator");
    let server = tokio::spawn(
        AttentionSocket::bind(&path, &capability, store.clone())
            .await
            .unwrap()
            .serve(),
    );
    let mut lines = BufReader::new(UnixStream::connect(&path).await.unwrap()).lines();
    assert!(
        lines
            .next_line()
            .await
            .unwrap()
            .unwrap()
            .contains("snapshot")
    );
    let projection: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/broker_attention_projection.json"
    ))
    .unwrap();
    let request = serde_json::json!({"type":"project", "request_id":"projection-1",
        "projection":projection, "capability":fs::read_to_string(capability).unwrap()});
    lines
        .get_mut()
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();
    assert!(lines.next_line().await.unwrap().is_none());
    assert!(store.snapshot().unwrap().items.is_empty());
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn producer_token_cannot_authorize_ordinary_attention_or_run_mutations() {
    use louiselm_capture::{RunSocket, RunStore};
    let root = tempfile::tempdir().unwrap();
    let capability = root.path().join("operator");
    let attention_path = root.path().join("attention.sock");
    let run_path = root.path().join("run.sock");
    let attention = AttentionStore::new(root.path().join("attention")).unwrap();
    let run = RunStore::new(root.path().join("runs")).unwrap();
    let attention_server = tokio::spawn(
        AttentionSocket::bind(&attention_path, &capability, attention.clone())
            .await
            .unwrap()
            .serve(),
    );
    let run_server = tokio::spawn(
        RunSocket::bind(&run_path, &capability, run)
            .await
            .unwrap()
            .serve(),
    );
    for (path, request) in [
        (
            &attention_path,
            serde_json::json!({"type":"clear_session", "request_id":"forbidden", "session_id":"session", "capability":TOKEN}),
        ),
        (
            &run_path,
            serde_json::json!({"type":"raise", "request_id":"forbidden", "id":"run", "expected_revision":0, "ceiling":5, "capability":TOKEN}),
        ),
    ] {
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let mut lines = BufReader::new(UnixStream::connect(path).await.unwrap()).lines();
        lines.next_line().await.unwrap().unwrap();
        lines
            .get_mut()
            .write_all(format!("{request}\n").as_bytes())
            .await
            .unwrap();
        let reply = tokio::time::timeout(Duration::from_secs(2), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let reply: serde_json::Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(reply["type"], "mutation_error");
        assert_eq!(reply["message"], "operator capability is invalid");
    }
    assert!(attention.snapshot().unwrap().items.is_empty());
    attention_server.abort();
    run_server.abort();
    assert!(attention_server.await.unwrap_err().is_cancelled());
    assert!(run_server.await.unwrap_err().is_cancelled());
}
