//! Closed, bounded broker-to-supervisor tool commands.
#![allow(
    clippy::unwrap_used,
    reason = "Tests assert valid fixture construction."
)]

use louiselm_skills::launch_protocol::{
    ProtocolMessage, TOOL_EXECUTION_SCHEMA, ToolExecutionRequest, decode_message,
};

fn request() -> ToolExecutionRequest {
    ToolExecutionRequest {
        schema: TOOL_EXECUTION_SCHEMA.to_owned(),
        protocol_version: 1,
        request_id: "tool-1".to_owned(),
        session_id: "session-1".to_owned(),
        run_id: "run-1".to_owned(),
        envelope_revision: 1,
        sequence: 1,
        command: "printf permitted > result".to_owned(),
        timeout_ms: 1_000,
    }
}

#[test]
fn tool_command_round_trips_without_privileged_options() {
    let request = request();
    assert_eq!(
        decode_message(&request.canonical_bytes()).unwrap(),
        ProtocolMessage::ToolExecution(request.clone())
    );
    for key in [
        "mounts",
        "uid",
        "environment",
        "capabilities",
        "executable",
        "cwd",
    ] {
        let mut value = serde_json::to_value(&request).unwrap();
        value[key] = serde_json::json!(["untrusted"]);
        assert!(
            decode_message(&serde_json::to_vec(&value).unwrap()).is_err(),
            "{key}"
        );
    }
}

#[test]
fn malformed_and_unbounded_tool_commands_are_refused() {
    let valid = request();
    for bad in ["", "bad\0command"] {
        let mut request = valid.clone();
        request.command = bad.to_owned();
        assert!(decode_message(&request.canonical_bytes()).is_err());
    }
    for timeout in [0, 30_001] {
        let mut request = valid.clone();
        request.timeout_ms = timeout;
        assert!(decode_message(&request.canonical_bytes()).is_err());
    }
    let mut request = valid;
    request.command = "x".repeat(16_385);
    assert!(decode_message(&request.canonical_bytes()).is_err());
}

#[test]
fn zero_revision_is_valid_when_it_is_the_current_launch_revision() {
    let mut request = request();
    request.envelope_revision = 0;
    assert!(decode_message(&request.canonical_bytes()).is_ok());
}
