//! Durable owner admission versus no-effect fencing.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "test fixtures and assertions"
)]

use std::{
    fs,
    io::Write,
    process::{Command, Stdio},
};

use louiselm_capture::ActionIdentity;

fn identity(project: &std::path::Path, id: u8) -> ActionIdentity {
    ActionIdentity {
        operation_id: format!("{id:02x}").repeat(16),
        receiver_identity: "02".repeat(32),
        controller_id: "03".repeat(16),
        subject_id: "04".repeat(16),
        action: 3,
        payload_hash: "05".repeat(32),
        grant_id: "06".repeat(16),
        grant_revision: 1,
        period_id: "07".repeat(16),
        expected_revision: 2,
        project: fs::canonicalize(project).unwrap().to_str().unwrap().into(),
        agent: "codex".into(),
        recorded_at_ms: 1_000,
    }
}

#[test]
fn local_owner_command_persists_a_fence_across_processes() {
    let temp = tempfile::tempdir().unwrap();
    let request = identity(temp.path(), 5);
    let invoke = |message: serde_json::Value| {
        let mut child = Command::new(env!("CARGO_BIN_EXE_louiselm-capture"))
            .args(["phone-control", "owner-journal"])
            .env_clear()
            .env("LOUISELM_CAPTURE_CONFIG_DIR", temp.path())
            .env("LOUISELM_CAPTURE_DATA_DIR", temp.path())
            .env("LOUISELM_CAPTURE_STATE_DIR", temp.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(message.to_string().as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()
    };
    let fenced = invoke(serde_json::json!({"v":1,"type":"fence","identity":request,"at_ms":2000}));
    assert_eq!(fenced["type"], "fenced");
    let late = invoke(serde_json::json!({"v":1,"type":"begin","identity":request}));
    assert_eq!(late["type"], "existing");
    assert_eq!(
        late["outcome"]["fenced_no_effect"]["evidence"],
        fenced["evidence"]
    );
}
