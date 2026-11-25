//! Capability startup checks against private stores and actual local sockets.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Tests fail directly on fixture setup or contract violations."
)]

use super::*;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    net::UnixStream,
};

fn paths(root: &std::path::Path) -> Paths {
    Paths {
        config: root.join("config"),
        data: root.join("data"),
        state: root.join("state"),
    }
}

fn attention_only() -> Features {
    Features {
        attention: true,
        runs: false,
        receiver: false,
        transcription: false,
        push: false,
        phone_control: false,
    }
}

#[tokio::test]
async fn attention_only_serves_snapshots_without_capture_run_or_delivery_resources() {
    let root = tempfile::tempdir().unwrap();
    let paths = paths(root.path());
    let socket = paths.attention_socket();
    let prepared = prepare(paths.clone(), attention_only()).unwrap();
    let server = tokio::spawn(serve_prepared(prepared, attention_only(), None, None));
    let stream = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(stream) = UnixStream::connect(&socket).await {
                break stream;
            }
            assert!(
                !server.is_finished(),
                "Attention service stopped before readiness"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let mut lines = BufReader::new(stream).lines();
    let line = tokio::time::timeout(Duration::from_secs(5), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let snapshot: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(snapshot["type"], "snapshot");
    for absent in [
        paths.runs(),
        paths.run_socket(),
        paths.captures(),
        paths.pairing(),
        paths.tls(),
        paths.uploads(),
        paths.phone_control_store(),
        paths.phone_control_socket(),
        paths.phone_control_admin_socket(),
    ] {
        assert!(
            !absent.exists(),
            "unexpected optional resource: {}",
            absent.display()
        );
    }
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn phone_control_routes_exist_only_after_explicit_opt_in() {
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    let root = tempfile::tempdir().unwrap();
    let paths = paths(root.path());
    let mut features = attention_only();
    features.attention = false;
    features.receiver = true;
    let disabled = prepare(paths.clone(), features).unwrap();
    let disabled_app = disabled.receiver.as_ref().unwrap().0.router();
    let request = || {
        Request::post("/v1/phone-control/enrollment/offer")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"v":1,"type":"enrollment_offer"}"#))
            .unwrap()
    };
    assert_eq!(
        disabled_app.oneshot(request()).await.unwrap().status(),
        StatusCode::NOT_FOUND
    );
    assert!(!paths.phone_control_store().exists());
    let evidence = paths
        .phone_control_store()
        .parent()
        .unwrap()
        .join("action-evidence");
    assert!(!evidence.exists());

    features.phone_control = true;
    let enabled = prepare(paths, features).unwrap();
    assert!(evidence.exists());
    let enabled_app = enabled.receiver.as_ref().unwrap().0.router();
    let response = enabled_app.oneshot(request()).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = to_bytes(response.into_body(), 1024).await.unwrap();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        value,
        serde_json::json!({"v":1,"type":"error","code":"unauthenticated"})
    );
}

#[test]
fn disabling_runs_preserves_and_rejects_retained_obligations() {
    let root = tempfile::tempdir().unwrap();
    let paths = paths(root.path());
    let runs = RunStore::new(paths.runs()).unwrap();
    let id = "11111111-1111-4111-8111-111111111111";
    runs.admit(
        crate::RunAdmission {
            id: id.into(),
            generated_work_ceiling: 1,
            park_ttl_ms: 60000,
        },
        "test-token-at-least-sixteen-bytes",
    )
    .unwrap();
    let before = std::fs::read(paths.runs().join(format!("{id}.json"))).unwrap();
    let Err(error) = prepare(paths.clone(), attention_only()) else {
        panic!("retained Runs must block disabling")
    };
    assert!(error.to_string().contains("retained Runs"));
    assert_eq!(
        std::fs::read(paths.runs().join(format!("{id}.json"))).unwrap(),
        before
    );
    assert!(!paths.attention().exists());
}
