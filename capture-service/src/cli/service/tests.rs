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
