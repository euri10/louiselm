//! Sanitized shapes from local native stores; values and ordering are synthetic.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "Controlled fixtures"
)]
use serde_json::json;
use std::fs;
mod common;
use common::Fixture;

#[test]
fn claude_replays_and_child_sessions_keep_native_identity() {
    let f = Fixture::new();
    let request = json!({"sessionId":"s","agentId":"child","isSidechain":true,"cwd":"/other","timestamp":"2026-09-17T10:00:00Z","type":"assistant","message":{"id":"msg","model":"m","usage":{"input_tokens":7,"output_tokens":3},"content":[{"type":"tool_use","id":"tc","name":"Bash","input":{"command":"git status --short"}}]}});
    let source = f.log("claude", "claude.jsonl", &[request.clone(), request, json!({"sessionId":"s","agentId":"child","type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"tc","content":"result","is_error":false}]}})]);
    f.ok(&["index", "--source", &source]);
    let calls = f.ok(&["calls"]);
    assert_eq!(calls["rows"].as_array().unwrap().len(), 1);
    assert_eq!(calls["rows"][0]["parent_session_id"], "claude:s");
    assert_eq!(calls["rows"][0]["session_id"], "claude:s:agent:child");
    assert_eq!(calls["rows"][0]["retained_output_bytes"], 6);
    assert!(calls["rows"][0]["exit_code"].is_null());
}

#[test]
fn copilot_uses_historical_model_and_explicit_completion() {
    let f = Fixture::new();
    let source = f.log("copilot", "events.jsonl", &[
        json!({"type":"session.start","data":{"sessionId":"s","context":{"cwd":"/other"}}}),
        json!({"type":"session.model_change","data":{"newModel":"m1"}}),
        json!({"type":"tool.execution_start","timestamp":"2026-09-17T10:00:00Z","data":{"toolCallId":"tc","toolName":"bash","turnId":"t","arguments":{"command":"rg --files"}}}),
        json!({"type":"session.model_change","data":{"newModel":"m2"}}),
        json!({"type":"tool.execution_complete","timestamp":"2026-09-17T10:00:01Z","data":{"toolCallId":"tc","success":true,"result":{"content":"result"}}}),
    ]);
    f.ok(&["index", "--source", &source]);
    let calls = f.ok(&["calls"]);
    assert_eq!(calls["rows"][0]["model"], "m1");
    assert_eq!(calls["rows"][0]["duration_ms"], 1000.0);
    assert_eq!(calls["rows"][0]["status"], "completed");
}

#[test]
fn gemini_patch_journal_replaces_message_tools() {
    let f = Fixture::new();
    // Tool shape verified against installed official ChatRecordingService code.
    let message = json!({"id":"msg","type":"gemini","model":"m","timestamp":"2026-09-17T10:00:00Z","toolCalls":[{"id":"tc","name":"run_shell_command","args":{"command":"git status"},"status":"success","result":[{"functionResponse":{"response":{"output":"result"}}}]}]});
    let source = f.log(
        "gemini",
        "session-test.jsonl",
        &[
            json!({"sessionId":"s","projectHash":"not-a-path"}),
            json!({"$set":{"messages":[message.clone()]}}),
            message,
        ],
    );
    f.ok(&["index", "--source", &source]);
    let calls = f.ok(&["calls"]);
    assert_eq!(calls["rows"].as_array().unwrap().len(), 1);
    assert!(calls["rows"][0]["project"].is_null());
    assert_eq!(calls["rows"][0]["family"], "git status");
}

#[test]
fn adapter_metadata_does_not_backfill_current_options() {
    let f = Fixture::new();
    fs::write(
        f.0.join("meta.json"),
        json!({"session_id":"s","cwd":"/other","model":"current-model","reasoning_effort":"high"})
            .to_string(),
    )
    .unwrap();
    let source = f.log("adapter", "history.jsonl", &[
        json!({"role":"assistant","tool_calls":[{"id":"tc","name":"shell","arguments":{"command":"git status"}}]}),
        json!({"role":"tool","tool_call_id":"tc","content":"result"}),
    ]);
    f.ok(&["index", "--source", &source]);
    let calls = f.ok(&["calls"]);
    assert!(calls["rows"][0]["model"].is_null());
    assert_eq!(calls["rows"][0]["options"], json!({}));
    assert_eq!(calls["rows"][0]["native_session_id"], "s");
}

#[test]
fn adapter_does_not_follow_identity_metadata_symlinks() {
    let f = Fixture::new();
    fs::write(
        f.0.join("outside.json"),
        json!({"session_id":"should-not-read"}).to_string(),
    )
    .unwrap();
    std::os::unix::fs::symlink(f.0.join("outside.json"), f.0.join("meta.json")).unwrap();
    let source = f.log(
        "adapter",
        "history.jsonl",
        &[json!({"role":"assistant","tool_calls":[{"id":"c","name":"shell"}]})],
    );
    assert_eq!(
        f.run(&["index", "--source", &source]).status.code(),
        Some(4)
    );
    assert_eq!(f.ok(&["calls"])["rows"], json!([]));
}

#[test]
fn claude_uses_later_explicit_cwd_after_metadata_only_preamble() {
    let f = Fixture::new();
    let source = f.log("claude","history.jsonl",&[
        json!({"sessionId":"s","type":"file-history-snapshot"}),
        json!({"sessionId":"s","type":"assistant","cwd":"/other","message":{"content":[{"type":"tool_use","id":"c","name":"Bash","input":{"command":"git status"}}]}}),
    ]);
    f.ok(&["index", "--source", &source]);
    assert_eq!(
        f.ok(&["stats", "commands", "--project", "/other"])["rows"][0]["calls"],
        1
    );
}

#[test]
fn opencode_refresh_reads_mutable_sqlite_rows_including_wal() {
    let f = Fixture::new();
    let db = rusqlite::Connection::open(f.0.join("opencode.db")).unwrap();
    db.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE session(id TEXT,directory TEXT,parent_id TEXT); CREATE TABLE message(id TEXT,session_id TEXT,data TEXT,time_created INTEGER); CREATE TABLE part(id TEXT,message_id TEXT,session_id TEXT,data TEXT,time_created INTEGER); INSERT INTO session VALUES('s','/other',NULL);").unwrap();
    db.execute("INSERT INTO message VALUES('m','s',?1,1000)", [json!({"role":"assistant","modelID":"m1","providerID":"route","tokens":{"input":8,"output":3}}).to_string()]).unwrap();
    db.execute("INSERT INTO part VALUES('p','m','s',?1,1000)", [json!({"type":"tool","tool":"bash","callID":"tc","state":{"status":"running","input":{"command":"git status"},"time":{"start":1000}}}).to_string()]).unwrap();
    let source = format!("opencode={}", f.0.join("opencode.db").display());
    f.ok(&["index", "--source", &source]);
    db.execute("UPDATE part SET data=?1", [json!({"type":"tool","tool":"bash","callID":"tc","state":{"status":"completed","input":{"command":"git status"},"output":"result","time":{"start":1000,"end":1500}}}).to_string()]).unwrap();
    f.ok(&["index", "--source", &source]);
    let calls = f.ok(&["calls"]);
    assert_eq!(calls["rows"].as_array().unwrap().len(), 1);
    assert_eq!(calls["rows"][0]["retained_output_bytes"], 6);
    assert_eq!(calls["rows"][0]["duration_ms"], 500.0);
    assert_eq!(calls["rows"][0]["provider_route"], "route");
    assert!(calls["rows"][0]["provider"].is_null());
}
