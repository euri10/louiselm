//! Partial coverage, overlap, and metadata safety remain visible under failure.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "Controlled fixtures"
)]
use serde_json::{Value, json};
use std::fs;
mod common;
use common::Fixture;

#[test]
fn explicit_file_format_does_not_require_a_standard_filename() {
    let f = Fixture::new();
    let source = f.log(
        "codex",
        "archive.log",
        &[json!({"type":"session_meta","payload":{"id":"s"}})],
    );
    assert_eq!(f.ok(&["index", "--source", &source])["indexed_sources"], 1);
}

#[test]
fn possible_mirrors_are_excluded_but_still_queryable() {
    let f = Fixture::new();
    let native = f.log("codex","native.jsonl",&[
        json!({"type":"session_meta","payload":{"id":"s"}}),
        json!({"type":"response_item","payload":{"type":"function_call","call_id":"c","name":"shell"}}),
    ]);
    let proxy = f.log("acp","proxy.jsonl",&[
        json!({"kind":"frame","payload":{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"tool_call","toolCallId":"c","name":"shell"}}}}),
    ]);
    f.ok(&["index", "--source", &native, "--source", &proxy]);
    assert_eq!(f.ok(&["stats", "tools"])["rows"][0]["calls"], 1);
    assert_eq!(
        f.ok(&["stats", "tools", "--overlap", "include"])["rows"][0]["calls"],
        2
    );
    assert_eq!(
        f.ok(&["calls", "--adapter", "acp", "--overlap", "include"])["rows"][0]["overlap"],
        "possible_mirror"
    );
}

#[test]
fn malformed_records_and_incomplete_tail_preserve_usable_facts_and_exit_partial() {
    let f = Fixture::new();
    let source = f.log("codex","events.jsonl",&[
        json!({"type":"session_meta","payload":{"id":"s"}}),
        json!({"type":"response_item","payload":{"type":"function_call","call_id":"c","name":"shell"}}),
    ]);
    let path = f.0.join("events.jsonl");
    let mut bytes = fs::read(&path).unwrap();
    bytes.extend_from_slice(b"not json\n{\"incomplete\":");
    fs::write(&path, bytes).unwrap();
    let output = f.run(&["index", "--source", &source]);
    assert_eq!(output.status.code(), Some(4));
    assert_eq!(f.ok(&["stats", "tools"])["rows"][0]["calls"], 1);
    let sources = f.ok(&["sources", "--state", "partial"]);
    assert_eq!(sources["rows"][0]["diagnostics"]["incomplete_tail"], 1);
    assert_eq!(sources["rows"][0]["diagnostics"]["malformed_record"], 1);
    assert_eq!(
        f.run(&["index", "--source", &source]).status.code(),
        Some(4)
    );
}

#[test]
fn source_failure_is_isolated_and_missing_snapshots_remain_disclosed() {
    let f = Fixture::new();
    let source = f.log(
        "codex",
        "events.jsonl",
        &[json!({"type":"session_meta","payload":{"id":"s"}})],
    );
    fs::write(f.0.join("broken.db"), "not sqlite").unwrap();
    let broken = format!("opencode={}", f.0.join("broken.db").display());
    assert_eq!(
        f.run(&["index", "--source", &source, "--source", &broken])
            .status
            .code(),
        Some(4)
    );
    assert_eq!(
        f.ok(&["sources", "--state", "indexed"])["rows"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    fs::remove_file(f.0.join("events.jsonl")).unwrap();
    f.run(&["index", "--source", &broken]);
    assert_eq!(
        f.ok(&["sources", "--state", "missing_retained_snapshot"])["rows"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn response_budget_and_sql_identifier_validation_hold_for_many_rows() {
    let f = Fixture::new();
    let mut events = vec![json!({"type":"session_meta","payload":{"id":"s"}})];
    for i in 0..100 {
        events.push(json!({"type":"response_item","payload":{"type":"function_call","call_id":format!("c{i}"),"name":"shell"}}));
    }
    let source = f.log("codex", "events.jsonl", &events);
    f.ok(&["index", "--source", &source]);
    let output = f.run(&["calls", "--limit", "1000"]);
    assert!(output.status.success());
    assert!(output.stdout.len() <= 32769);
    let parsed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(parsed["next_cursor"].is_string());
    assert_eq!(
        f.run(&["stats", "tools", "--group-by", "tool);DROP TABLE calls"])
            .status
            .code(),
        Some(2)
    );
    assert_eq!(f.ok(&["stats", "tools"])["rows"][0]["calls"], 100);
}
