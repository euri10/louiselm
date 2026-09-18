//! Query contracts for bounded, reproducible agent-facing analysis.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "Controlled fixtures"
)]
use serde_json::{Value, json};
mod common;
use common::Fixture;

fn populated() -> Fixture {
    let f = Fixture::new();
    let mut events = vec![json!({"type":"session_meta","payload":{"id":"s","cwd":"/project"}})];
    for i in 0..25 {
        events.push(json!({"type":"response_item","timestamp":"2026-09-17T10:00:00Z","payload":{"type":"function_call","call_id":format!("c{i:02}"),"name":"shell","arguments":"{\"command\":\"git status --short\"}"}}));
        events.push(json!({"type":"response_item","payload":{"type":"function_call_output","call_id":format!("c{i:02}"),"output":"same"}}));
    }
    let source = f.log("codex", "events.jsonl", &events);
    f.ok(&["index", "--source", &source]);
    f
}

#[test]
fn unknown_projection_fails_even_when_selection_is_empty() {
    let f = populated();
    for (args, discovery) in [
        (vec!["calls", "--sort", "invented"], "schema calls"),
        (
            vec!["stats", "turns", "--sort", "invented"],
            "schema stats turns",
        ),
        (
            vec!["stats", "requests", "--group-by", "invented"],
            "schema stats requests",
        ),
        (
            vec!["calls", "--project", "/absent", "--fields", "invented"],
            "schema calls",
        ),
        (
            vec!["stats", "turns", "--fields", "invented"],
            "schema stats turns",
        ),
        (
            vec!["show", "call", "codex:s:call:c00", "--fields", "invented"],
            "schema show call",
        ),
    ] {
        let output = f.run(&args);
        assert_eq!(output.status.code(), Some(2));
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        let message = error["error"]["message"].as_str().unwrap();
        assert!(message.contains("invented"), "{message}");
        assert!(message.contains(discovery), "{message}");
    }
}

#[test]
fn schema_describes_each_query_subject_without_an_index() {
    let f = Fixture::new();
    for (subject, metric, dimension) in [
        ("tools", "calls", "tool"),
        ("commands", "calls", "family"),
        ("sessions", "calls", "session_id"),
        ("turns", "turns", "provider"),
        ("requests", "requests", "scope"),
    ] {
        let schema = f.ok(&["schema", "stats", subject]);
        assert!(
            schema["fields"]
                .as_array()
                .unwrap()
                .contains(&json!(metric))
        );
        assert!(
            schema["dimensions"]
                .as_array()
                .unwrap()
                .contains(&json!(dimension))
        );
        if ["turns", "requests"].contains(&subject) {
            assert!(
                !schema["fields"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("calls"))
            );
            assert!(
                !schema["dimensions"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("tool"))
            );
        }
    }
    for kind in ["call", "session", "turn", "request"] {
        let schema = f.ok(&["schema", "show", kind]);
        assert!(schema["fields"].as_array().unwrap().contains(&json!("id")));
    }
    assert!(
        f.ok(&["schema", "calls"])["fields"]
            .as_array()
            .unwrap()
            .contains(&json!("evidence"))
    );
    assert!(!f.0.join("state").exists());
}

#[test]
fn pages_are_stable_scoped_and_generation_bound() {
    let f = populated();
    let first = f.ok(&["calls", "--limit", "2", "--fields", "id,session_id"]);
    let cursor = first["next_cursor"].as_str().unwrap();
    let second = f.ok(&[
        "calls",
        "--limit",
        "2",
        "--fields",
        "id,session_id",
        "--cursor",
        cursor,
    ]);
    assert_ne!(first["rows"][0]["id"], second["rows"][0]["id"]);
    assert_eq!(
        f.run(&["calls", "--fields", "id", "--cursor", cursor])
            .status
            .code(),
        Some(2)
    );
    assert_eq!(first["rows"].as_array().unwrap().len(), 2);
    assert!(serde_json::to_vec(&first).unwrap().len() <= 32768);
}

#[test]
fn repeated_outputs_have_measured_denominators_not_savings_claims() {
    let f = populated();
    let stats = f.ok(&["stats", "commands"]);
    assert_eq!(stats["rows"][0]["calls"], 25);
    assert_eq!(stats["rows"][0]["repeat_output_calls"], 24);
    assert_eq!(stats["rows"][0]["exit_measured_calls"], 0);
    assert!(stats["rows"][0]["nonzero_exits"].is_null());
}

#[test]
fn native_usage_is_deduplicated_and_separate_from_turn_usage() {
    let f = Fixture::new();
    let usage = json!({"type":"event_msg","payload":{"type":"token_usage_record","response_id":"resp","usage":{"input_tokens":10,"output_tokens":0,"total_tokens":10,"cached_input_tokens":4}}});
    let source = f.log(
        "codex",
        "events.jsonl",
        &[
            json!({"type":"session_meta","payload":{"id":"s"}}),
            usage.clone(),
            usage,
        ],
    );
    f.ok(&["index", "--source", &source]);
    let stats = f.ok(&["stats", "requests"]);
    assert_eq!(stats["rows"][0]["requests"], 1);
    assert_eq!(stats["rows"][0]["reported_input_tokens"], 10);
    assert_eq!(stats["rows"][0]["reported_output_tokens"], 0);
    assert_eq!(stats["rows"][0]["output_measured_records"], 1);
    assert_eq!(f.ok(&["stats", "turns"])["rows"], json!([]));
}

#[test]
fn sources_and_show_are_bounded_and_inspectable() {
    let f = populated();
    let sources = f.ok(&["sources", "--limit", "1"]);
    assert_eq!(sources["rows"].as_array().unwrap().len(), 1);
    assert_eq!(sources["rows"][0]["state"], "indexed");
    let show = f.ok(&["show", "call", "codex:s:call:c00"]);
    assert!(show["record"]["evidence"][0]["path"].is_string());
    let schema = f.ok(&["schema", "show", "call"]);
    let fields = schema["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f.as_str().unwrap())
        .collect::<Vec<_>>()
        .join(",");
    let projected = f.ok(&["show", "call", "codex:s:call:c00", "--fields", &fields]);
    assert!(projected["record"]["native_turn_id"].is_null());
    assert!(serde_json::to_vec(&show).unwrap().len() <= 32768);
    let result = f.run(&["stats", "commands", "--level", "all"]);
    let json: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert!(json["rows"][0].get("level").is_some());
}
