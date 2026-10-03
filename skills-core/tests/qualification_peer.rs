//! Explicit offline peer options are fixture evidence, not vendor acceptance.
#![allow(
    clippy::unwrap_used,
    reason = "Disposable peer checks abort on setup or protocol failure."
)]

use serde_json::{Value, json};
use std::{
    io::Write,
    process::{Command, Stdio},
};

fn exchange(requests: &[Value]) -> Vec<Value> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_louiselm-tool-test-agent"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    for request in requests {
        serde_json::to_writer(&mut input, request).unwrap();
        input.write_all(b"\n").unwrap();
    }
    drop(input);
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn default_peer_advertises_no_model_selection() {
    let replies = exchange(&[
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
        json!({"jsonrpc":"2.0","id":2,"method":"session/new","params":{}}),
    ]);
    assert!(replies[1]["result"].get("configOptions").is_none());
}

#[test]
fn qualification_peer_confirms_only_its_explicit_route_options_without_prompting() {
    let replies = exchange(&[
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
        json!({"jsonrpc":"2.0","id":2,"method":"session/new","params":{"_meta":{"louiselmFixture":{"qualification":true}}}}),
        json!({"jsonrpc":"2.0","id":3,"method":"session/set_config_option","params":{"sessionId":"fixture-acp","configId":"model","value":"fixture-small"}}),
        json!({"jsonrpc":"2.0","id":4,"method":"session/set_config_option","params":{"sessionId":"fixture-acp","configId":"reasoning_effort","value":"low"}}),
    ]);
    assert_eq!(replies.len(), 4);
    assert_eq!(
        replies[1]["result"]["configOptions"][0]["currentValue"],
        "fixture-big"
    );
    assert_eq!(
        replies[3]["result"]["configOptions"][0]["currentValue"],
        "fixture-small"
    );
    assert_eq!(
        replies[3]["result"]["configOptions"][1]["currentValue"],
        "low"
    );
}
