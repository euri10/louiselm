//! Historical cohorts and ACP RPC identity; never a timestamp guess.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "Controlled disposable fixtures"
)]

mod common;
use common::Fixture;
use rusqlite::Connection;
use serde_json::{Value, json};

fn turns(fixture: &Fixture) -> String {
    let path = fixture.0.join("turns.sqlite3");
    let db = Connection::open(&path).unwrap();
    db.execute_batch("CREATE TABLE turns(id TEXT PRIMARY KEY,agent TEXT,provider TEXT,acp_session_id TEXT,prepared_at TEXT,options TEXT,model TEXT,cost_baseline TEXT);
      CREATE TABLE turn_events(turn_id TEXT,sequence INTEGER,observed_at TEXT,kind TEXT,data TEXT);
      CREATE TABLE option_events(id TEXT,turn_id TEXT);
      INSERT INTO turns VALUES('durable-a','configured-agent','Access service','session-a','2026-09-17T08:00:00Z','{\"effort\":\"high\"}','\"model-a\"',NULL);
      INSERT INTO turns VALUES('durable-b','configured-agent','Access service','session-a','2026-09-17T08:01:00Z','{\"effort\":\"high\"}','\"model-a\"',NULL);
      INSERT INTO turn_events VALUES('durable-a',1,'2026-09-17T08:00:00Z','dispatch','{\"request_id\":8}');
      INSERT INTO turn_events VALUES('durable-a',2,'2026-09-17T08:00:02Z','outcome','{\"outcome\":\"completed\",\"peer_response\":true,\"usage\":{\"input_tokens\":100,\"output_tokens\":10}}');
      INSERT INTO turn_events VALUES('durable-b',1,'2026-09-17T08:01:00Z','dispatch','{\"request_id\":9}');
      INSERT INTO turn_events VALUES('durable-b',2,'2026-09-17T08:01:02Z','outcome','{\"outcome\":\"completed\",\"peer_response\":true}');
      INSERT INTO option_events VALUES('changed-and-returned','durable-b');").unwrap();
    format!("louiselm={}", path.display())
}

#[test]
fn fixed_cohorts_preserve_reported_usage_missing_values_and_mixed_exclusions() {
    let fixture = Fixture::new();
    let source = turns(&fixture);
    fixture.ok(&["index", "--source", &source]);
    let all = fixture.ok(&["stats", "turns", "--group-by", "provider,model"]);
    assert_eq!(all["rows"][0]["turns"], 2);
    assert_eq!(all["rows"][0]["reported_input_tokens"], 100);
    assert_eq!(all["rows"][0]["input_measured_records"], 1);
    let fixed = fixture.ok(&["stats", "turns", "--cohort", "fixed"]);
    assert_eq!(fixed["rows"][0]["turns"], 1);
    assert_eq!(fixed["coverage"]["mixed_turns_excluded"], 1);
    let changed = fixture.ok(&["show", "turn", "louiselm:durable-b"]);
    assert_eq!(changed["record"]["mixed_options"], true);
    assert!(changed["record"]["usage"]["input_tokens"].is_null());
}

#[test]
fn discovered_options_lead_to_a_non_null_durable_cohort_without_guessing() {
    let fixture = Fixture::new();
    let source = turns(&fixture);
    let db = Connection::open(fixture.0.join("turns.sqlite3")).unwrap();
    db.execute_batch("INSERT INTO turns SELECT 'undispatched',agent,provider,acp_session_id,prepared_at,'{\"unstarted_option\":true}',model,cost_baseline FROM turns WHERE id='durable-a';
      INSERT INTO turns SELECT 'missing',agent,provider,acp_session_id,prepared_at,'{}',model,cost_baseline FROM turns WHERE id='durable-a';
      INSERT INTO turn_events VALUES('missing',1,'2026-09-17T09:00:00Z','dispatch','{\"request_id\":10}');").unwrap();
    let native = fixture.log("codex", "native.jsonl", &[
        json!({"type":"session_meta","payload":{"id":"native"}}),
        json!({"type":"turn_context","payload":{"effort":"high","summary":"concise"}}),
        json!({"type":"response_item","payload":{"type":"function_call","call_id":"call","name":"shell","arguments":"{\"command\":\"git status\"}"}}),
        json!({"type":"event_msg","payload":{"type":"token_usage_record","response_id":"response","usage":{"input_tokens":10}}}),
    ]);
    fixture.ok(&["index", "--source", &source, "--source", &native]);
    let options = fixture.ok(&["schema", "options", "turns", "--limit", "1"]);
    assert_eq!(options["scope"]["subject"], "turns");
    assert_eq!(options["coverage"]["subject_records"], 3);
    assert_eq!(options["coverage"]["mixed_option_records"], 1);
    assert_eq!(
        options["rows"],
        json!([{"option_id":"effort","type":"text","observations":2,"missing_records":1}])
    );
    assert!(options["next_cursor"].is_null());
    let key = options["rows"][0]["option_id"].as_str().unwrap();
    let dimension = format!("option:{key}");
    let fields = format!("{dimension},turns,input_measured_records");
    let stats = fixture.ok(&[
        "stats",
        "turns",
        "--cohort",
        "fixed",
        "--group-by",
        &dimension,
        "--fields",
        &fields,
        "--limit",
        "2",
    ]);
    assert!(
        stats["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row[&dimension]["value"] == "high" && row["turns"] == 1)
    );
    assert_eq!(stats["coverage"]["mixed_turns_excluded"], 1);
    let calls = fixture.ok(&["schema", "options", "calls", "--limit", "1"]);
    assert_eq!(calls["rows"][0]["option_id"], "codex.effort");
    assert_eq!(calls["coverage"]["subject_records"], 1);
    let cursor = calls["next_cursor"].as_str().unwrap();
    assert_eq!(
        fixture.ok(&[
            "schema", "options", "calls", "--limit", "1", "--cursor", cursor
        ])["rows"][0]["option_id"],
        "codex.summary"
    );
    assert_eq!(
        fixture
            .run(&["schema", "options", "turns", "--cursor", cursor])
            .status
            .code(),
        Some(2)
    );
    let requests = fixture.ok(&["schema", "options", "requests"]);
    assert_eq!(requests["rows"], json!([]));
    assert_eq!(requests["coverage"]["subject_records"], 1);
    assert_eq!(requests["scope"]["options_supported"], false);
    let unscoped = fixture.run(&["schema", "options"]);
    assert_eq!(unscoped.status.code(), Some(2));
    let error: Value = serde_json::from_slice(&unscoped.stderr).unwrap();
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("schema options turns")
    );
}

#[test]
fn acp_call_joins_recorded_rpc_identity_and_not_a_nearby_native_turn() {
    let fixture = Fixture::new();
    let source = turns(&fixture);
    // Wire shape observed in local proxy session logs; values/order synthetic.
    let acp = fixture.log("acp","wire.jsonl", &[
        json!({"kind":"frame","direction":"client_to_agent","payload":{"id":8,"method":"session/prompt","params":{"sessionId":"session-a"}}}),
        json!({"kind":"frame","direction":"agent_to_client","payload":{"method":"session/update","params":{"sessionId":"session-a","update":{"sessionUpdate":"tool_call","toolCallId":"call-a","name":"shell","rawInput":{"command":"git status"}}}}}),
        json!({"kind":"frame","direction":"agent_to_client","payload":{"method":"session/update","params":{"sessionId":"session-a","update":{"sessionUpdate":"tool_call_update","toolCallId":"call-a","status":"completed","rawOutput":"clean"}}}}),
    ]);
    fixture.ok(&["index", "--source", &source, "--source", &acp]);
    let calls = fixture.ok(&["calls", "--provider", "Access service"]);
    assert_eq!(calls["rows"].as_array().unwrap().len(), 1);
    assert_eq!(calls["rows"][0]["turn_id"], "louiselm:durable-a");
    assert_eq!(calls["rows"][0]["options"]["effort"], "high");
    assert_eq!(fixture.ok(&["stats", "commands"])["rows"][0]["calls"], 1);
}

#[test]
fn native_and_acp_observations_count_once_with_an_explicit_louiselm_origin() {
    let fixture = Fixture::new();
    let native = fixture.log("codex","native.jsonl", &[
        json!({"type":"session_meta","payload":{"id":"session-a","session_id":"session-a","originator":"louiselm.nvim"}}),
        json!({"type":"response_item","payload":{"type":"function_call","call_id":"call-a","name":"exec_command","arguments":"{\"cmd\":\"git status\"}"}}),
    ]);
    let acp = fixture.log("acp","wire.jsonl", &[
        json!({"kind":"frame","payload":{"method":"session/update","params":{"sessionId":"session-a","update":{"sessionUpdate":"tool_call","toolCallId":"call-a","name":"shell","rawInput":{"command":"git status"}}}}}),
    ]);
    fixture.ok(&["index", "--source", &native, "--source", &acp]);
    let stats = fixture.ok(&["stats", "commands"]);
    assert_eq!(stats["rows"][0]["calls"], 1);
    assert_eq!(
        fixture.ok(&["calls"])["rows"][0]["session_id"],
        "codex:session-a"
    );
}

#[test]
fn repeated_rpc_ids_on_resume_remain_unassociated() {
    let fixture = Fixture::new();
    let source = turns(&fixture);
    let db = Connection::open(fixture.0.join("turns.sqlite3")).unwrap();
    db.execute_batch("INSERT INTO turns SELECT 'durable-c',agent,provider,acp_session_id,prepared_at,options,model,cost_baseline FROM turns WHERE id='durable-a'; INSERT INTO turn_events VALUES('durable-c',1,'2026-09-17T09:00:00Z','dispatch','{\"request_id\":8}');").unwrap();
    let acp = fixture.log("acp","wire.jsonl", &[
        json!({"kind":"frame","payload":{"id":8,"method":"session/prompt","params":{"sessionId":"session-a"}}}),
        json!({"kind":"frame","payload":{"method":"session/update","params":{"sessionId":"session-a","update":{"sessionUpdate":"tool_call","toolCallId":"call-a","name":"shell"}}}}),
    ]);
    fixture.ok(&["index", "--source", &source, "--source", &acp]);
    let calls = fixture.ok(&["calls"]);
    assert!(calls["rows"][0]["provider"].is_null());
    assert!(calls["rows"][0]["turn_id"].is_null());
}
