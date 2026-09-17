//! End-to-end contracts through the shipped CLI, using sanitized observed shapes.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "Assertions on controlled test fixtures"
)]

use serde_json::{Value, json};
use std::fs;
mod common;
use common::Fixture;

fn events() -> Vec<Value> {
    // Shape observed in rollout-2026-09-17T10-24-54-01a0ae77-ff80-7f92-82c7-989d3557a351.jsonl.
    // Content and identifiers are synthetic; this fixture specifies its own ordering.
    let execution = |id: &str| {
        json!({
            "type":"event_msg", "timestamp":"2026-09-17T08:00:02Z",
            "payload":{"type":"item_completed","turn_id":"turn-a","item":{
                "type":"CommandExecution","id":id,
                "command":["/bin/zsh","-lc","rtk git status --short -- TOKEN_NEVER_PERSIST"],
                "cwd":"/work/project", "status":"completed", "exit_code":0,
                "duration":{"secs":0,"nanos":1_000_000},
                "aggregated_output":"private output\n", "formatted_output":"2 changes\n"
            }}
        })
    };
    vec![
        json!({"type":"session_meta","payload":{"id":"session-a","session_id":"session-a","originator":"louiselm.nvim","cwd":"/work/project","model_provider":"openai"}}),
        json!({"type":"turn_context","payload":{"turn_id":"turn-a","model":"model-a","effort":"high","cwd":"/work/project"}}),
        json!({"type":"response_item","payload":{"type":"custom_tool_call","call_id":"outer-a","name":"exec","input":"await tools.exec_command(...)"}}),
        execution("call-a"),
        execution("call-a"),
        execution("call-b"),
    ]
}

#[test]
fn ranks_observed_executions_without_counting_updates_or_storing_payloads() {
    let fixture = Fixture::new();
    let source = fixture.log("codex", "history.jsonl", &events());
    fixture.ok(&["index", "--source", &source]);
    let stats = fixture.ok(&["stats", "commands", "--sort", "calls:desc"]);
    assert_eq!(stats["rows"][0]["calls"], 2);
    assert_eq!(stats["rows"][0]["family"], "git status");
    assert_eq!(stats["rows"][0]["retained_output_bytes"], 20);
    let calls = fixture.ok(&["calls"]);
    let id = calls["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["command_key"].is_string())
        .unwrap()["id"]
        .as_str()
        .unwrap();
    let call = fixture.ok(&["show", "call", id]);
    assert_eq!(call["record"]["session_id"], "codex:session-a");
    assert!(call["record"]["evidence"].is_array());
    let database = fs::read(fixture.0.join("state/index.sqlite3")).unwrap();
    assert!(!database.windows(19).any(|x| x == b"TOKEN_NEVER_PERSIST"));
    assert!(!database.windows(14).any(|x| x == b"private output"));
}

#[test]
fn unchanged_refresh_is_idempotent_and_rewrite_replaces_source_facts() {
    let fixture = Fixture::new();
    let source = fixture.log("codex", "history.jsonl", &events());
    fixture.ok(&["index", "--source", &source]);
    let refreshed = fixture.ok(&["index", "--source", &source]);
    assert_eq!(refreshed["indexed_sources"], 0);
    fixture.log("codex", "history.jsonl", &events()[..4]);
    fixture.ok(&["index", "--source", &source]);
    assert_eq!(fixture.ok(&["stats", "commands"])["rows"][0]["calls"], 1);
}

#[test]
fn queries_do_not_create_an_index_and_invalid_fields_are_errors() {
    let fixture = Fixture::new();
    let result = fixture.run(&["stats", "commands"]);
    assert_eq!(result.status.code(), Some(3));
    assert!(!fixture.0.join("state").exists());
    let source = fixture.log("codex", "history.jsonl", &events());
    fixture.ok(&["index", "--source", &source]);
    let result = fixture.run(&["stats", "commands", "--sort", "invented:desc"]);
    assert_eq!(result.status.code(), Some(2));
    let error: Value = serde_json::from_slice(&result.stderr).unwrap();
    assert_eq!(error["error"]["code"], "invalid_query");
}

#[test]
fn structured_file_changes_inside_orchestration_are_tools() {
    let fixture = Fixture::new();
    let source = fixture.log("codex","history.jsonl",&[
        json!({"type":"session_meta","payload":{"id":"s"}}),
        json!({"type":"event_msg","payload":{"type":"item_completed","item":{"type":"FileChange","id":"patch","status":"completed","stdout":"ok","stderr":"","changes":[{"diff":"PRIVATE_DIFF"}]}}}),
    ]);
    fixture.ok(&["index", "--source", &source]);
    let rows = fixture.ok(&["stats", "tools"]);
    assert_eq!(rows["rows"][0]["tool"], "<file change>");
    assert_eq!(rows["rows"][0]["calls"], 1);
}
