#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "Assertions on controlled request fixtures"
)]

use super::*;
use crate::launch_protocol::ErrorCode;

const HOST: &str = "127.0.0.1:40773";

// Header set and top-level fields observed from stock Codex 0.156.1
// (`scripts/probe-codex-request-shape.py`, comment on louiselm-qbr.5.1.3.2.1).
// Values are synthetic; field order follows the observed request.
fn body(model: &str) -> String {
    format!(
        r#"{{"model":"{model}","input":[{{"type":"message","role":"user","content":[{{"type":"input_text","text":"synthetic"}}]}}],"tool_choice":"auto","parallel_tool_calls":false,"reasoning":{{"effort":"high","context":"all_turns"}},"store":false,"stream":true,"include":["reasoning.encrypted_content"],"prompt_cache_key":"00000000-0000-0000-0000-000000000000","text":{{"verbosity":"low"}},"client_metadata":{{"session_id":"s"}}}}"#
    )
}

fn frame_with(body: &str, extra: &str) -> Vec<u8> {
    format!(
        "POST /v1/responses HTTP/1.1\r\nx-codex-beta-features: synthetic\r\nx-codex-window-id: w\r\nx-codex-turn-metadata: {{}}\r\nx-openai-internal-codex-responses-lite: true\r\nx-client-request-id: r\r\nsession-id: s\r\nthread-id: t\r\naccept: text/event-stream\r\ncontent-type: application/json\r\noriginator: codex_exec\r\nuser-agent: codex_exec/0.156.1\r\n{extra}host: {HOST}\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

fn frame(body: &str) -> Vec<u8> {
    frame_with(body, "")
}

fn frames() -> Frames {
    Frames::new(HOST.into())
}

#[test]
fn observed_codex_request_is_accepted_with_its_policy_fields() {
    let mut frames = frames();
    frames.feed(&frame(&body("gpt-5.6-luna"))).unwrap();
    let request = frames.next_request().unwrap().unwrap();
    assert_eq!(request.model, "gpt-5.6-luna");
    assert_eq!(request.effort.as_deref(), Some("high"));
    assert_eq!(request.body, body("gpt-5.6-luna").into_bytes());
    let names: Vec<_> = request
        .headers
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();
    assert!(
        !names
            .iter()
            .any(|name| ["host", "content-length", "authorization"].contains(name))
    );
    assert!(names.contains(&"session-id") && names.contains(&"accept"));
    assert!(!frames.is_partial());
    assert!(frames.next_request().unwrap().is_none());
}

#[test]
fn no_request_before_the_last_byte_of_a_split_frame() {
    let bytes = frame(&body("m"));
    for split in 1..bytes.len() {
        let mut frames = frames();
        frames.feed(&bytes[..split]).unwrap();
        assert!(frames.next_request().unwrap().is_none(), "split {split}");
        assert!(frames.is_partial());
        frames.feed(&bytes[split..]).unwrap();
        assert!(frames.next_request().unwrap().is_some(), "split {split}");
    }
}

#[test]
fn two_requests_in_one_read_are_two_requests() {
    let mut frames = frames();
    frames
        .feed(&[frame(&body("a")), frame(&body("b"))].concat())
        .unwrap();
    assert_eq!(frames.next_request().unwrap().unwrap().model, "a");
    assert_eq!(frames.next_request().unwrap().unwrap().model, "b");
    assert!(frames.next_request().unwrap().is_none());
}

#[test]
fn unreviewed_or_ambiguous_requests_are_refused_and_poison_the_connection() {
    let valid = String::from_utf8(frame(&body("m"))).unwrap();
    let bad = [
        valid.replacen("POST ", "GET ", 1),
        valid.replacen("/v1/responses", "http://other/v1/responses", 1),
        valid.replacen("/v1/responses", "/v1/responses?x=1", 1),
        valid.replacen("/v1/responses", "/v1/responses/compact", 1),
        valid.replacen(HOST, "api.openai.com", 1),
        valid.replacen("HTTP/1.1", "HTTP/1.0", 1),
        valid.replacen("text/event-stream", "application/json", 1),
        valid.replacen(
            "content-type: application/json",
            "content-type: text/plain",
            1,
        ),
        valid.replacen("session-id: s\r\n", "session-id: s\r\nsession-id: s\r\n", 1),
        valid.replacen("\r\nhost:", "\nhost:", 1),
        String::from_utf8(frame_with(&body("m"), "authorization: Bearer stolen\r\n")).unwrap(),
        String::from_utf8(frame_with(&body("m"), "transfer-encoding: chunked\r\n")).unwrap(),
        String::from_utf8(frame_with(&body("m"), "content-encoding: zstd\r\n")).unwrap(),
        String::from_utf8(frame_with(&body("m"), "x-forwarded-host: other\r\n")).unwrap(),
        String::from_utf8(frame(&body("m").replacen(
            "\"stream\":true",
            "\"stream\":false",
            1,
        )))
        .unwrap(),
        String::from_utf8(frame(&body("m").replacen(
            "{\"model\"",
            "{\"url\":\"https://other\",\"model\"",
            1,
        )))
        .unwrap(),
        String::from_utf8(frame(&body("m").replacen(
            "{\"model\":\"m\"",
            "{\"model\":\"m\",\"model\":\"other\"",
            1,
        )))
        .unwrap(),
        String::from_utf8(frame(&body("m").replacen(
            "\"effort\":\"high\"",
            "\"effort\":7",
            1,
        )))
        .unwrap(),
        String::from_utf8(frame("{}garbage")).unwrap(),
        format!(
            "POST /v1/responses HTTP/1.1\r\nhost: {HOST}\r\naccept: text/event-stream\r\ncontent-type: application/json\r\ncontent-length: +5\r\n\r\n"
        ),
        format!(
            "POST /v1/responses HTTP/1.1\r\nhost: {HOST}\r\naccept: text/event-stream\r\ncontent-type: application/json\r\ncontent-length: 999999999999999999999999\r\n\r\n"
        ),
    ];
    for bytes in bad {
        let mut frames = frames();
        frames.feed(bytes.as_bytes()).unwrap();
        let error = frames.next_request().expect_err(&bytes);
        assert_eq!(error.code, ErrorCode::InvalidRequest, "{bytes:?}");
        // A refused connection cannot resynchronize onto a later valid frame.
        assert!(frames.feed(&frame(&body("m"))).is_err());
        assert!(frames.next_request().is_err());
    }
}

#[test]
fn oversized_input_is_refused_before_buffering() {
    let mut frames = frames();
    assert_eq!(
        frames
            .feed(&vec![b'x'; 32 * 1024 * 1024 + 32 * 1024 + 1])
            .unwrap_err()
            .code,
        ErrorCode::MessageTooLarge
    );
    let mut frames = super::Frames::new(HOST.into());
    frames.feed(&vec![b'x'; 32 * 1024 + 1]).unwrap();
    assert!(frames.next_request().is_err());
}

#[test]
fn request_debug_output_never_contains_prompt_content() {
    let mut frames = frames();
    frames.feed(&frame(&body("m"))).unwrap();
    let rendered = format!("{:?}", frames.next_request().unwrap().unwrap());
    assert!(!rendered.contains("synthetic"));
    assert!(rendered.contains("body_bytes"));
}

fn approved() -> ApprovedProviderRequests {
    ApprovedProviderRequests {
        disclosure_profile: disclosure::profile_digest(),
        provider: "openai".into(),
        upstream: "https://api.openai.com/v1/responses".into(),
        addresses: vec!["192.0.2.1".parse().unwrap()],
        max_run_requests: 20,
        models: vec!["gpt-5.6-luna".into(), "gpt-6-astra".into()],
        max_effort: ReasoningEffort::High,
        expires_at_ms: 2000,
    }
}

#[test]
fn metadata_profile_rejects_unreviewed_nested_fields() {
    let payload = body("gpt-5.6-luna").replace(
        r#""client_metadata":{"session_id":"s"}"#,
        r#""client_metadata":{"session_id":"s","unreviewed":"secret"}"#,
    );
    let wire = format!(
        "POST /v1/responses HTTP/1.1\r\naccept: text/event-stream\r\ncontent-type: application/json\r\nhost: {HOST}\r\ncontent-length: {}\r\n\r\n{payload}",
        payload.len()
    );
    let mut frames = Frames::new(HOST.into());
    frames.feed(wire.as_bytes()).unwrap();
    assert_eq!(
        frames.next_request().unwrap_err().code,
        ErrorCode::ProviderDisclosureDenied
    );
}

#[test]
fn metadata_profile_refuses_ambiguous_or_malformed_values() {
    for metadata in [
        "null",
        "[]",
        r#"{"session_id":7}"#,
        r#"{"session_id":""}"#,
        r#"{"session_id":"a","session_id":"b"}"#,
        r#"{"session_id":{"hidden":"value"}}"#,
        r#"{"session_id":"line\nbreak"}"#,
        r#"{"x-codex-turn-metadata":"{\"unknown\":true}"}"#,
        r#"{"x-codex-turn-metadata":"{\"analytics_enabled\":\"true\"}"}"#,
        r#"{"x-codex-turn-metadata":"{\"window_number\":-1}"}"#,
        r#"{"x-codex-turn-metadata":"{\"turn_id\":\"a\",\"turn_id\":\"b\"}"}"#,
        r#"{"x-codex-turn-metadata":{}}"#,
    ] {
        let payload = body("m").replace(r#"{"session_id":"s"}"#, metadata);
        let mut parser = frames();
        parser.feed(&frame(&payload)).unwrap();
        assert_eq!(
            parser.next_request().unwrap_err().code,
            ErrorCode::ProviderDisclosureDenied,
            "{metadata}"
        );
    }
    let payload = body("m").replace(
        r#""session_id":"s""#,
        &format!(r#""session_id":"{}""#, "s".repeat(4097)),
    );
    let mut parser = frames();
    parser.feed(&frame(&payload)).unwrap();
    assert_eq!(
        parser.next_request().unwrap_err().code,
        ErrorCode::ProviderDisclosureDenied
    );
}

#[test]
fn metadata_profile_bounds_the_top_level_cache_identifier() {
    for value in ["null", "7", "{}", r#""line\nbreak""#] {
        let payload = body("m").replace(
            r#""prompt_cache_key":"00000000-0000-0000-0000-000000000000""#,
            &format!(r#""prompt_cache_key":{value}"#),
        );
        let mut parser = frames();
        parser.feed(&frame(&payload)).unwrap();
        assert_eq!(
            parser.next_request().unwrap_err().code,
            ErrorCode::ProviderDisclosureDenied
        );
    }
}

#[test]
fn observed_metadata_is_forwarded_byte_for_byte() {
    // Shape captured offline by probe-codex-request-shape.py, Codex 0.156.1,
    // 2026-09-25 (louiselm-qbr.11.1); all values here are synthetic.
    let turn = r#"{"installation_id":"i","session_id":"s","thread_id":"t","agent_name":"codex","turn_id":"u","window_id":"w","window_number":1,"context_window_id":"c","request_kind":"test","root_turn_id":"r","thread_source":"exec","turn_trigger":"user","sandbox":"enabled","sandbox_mode":"read-only","auto_review_enabled":true,"node_repl_auto_review_required":false,"node_repl_disabled":true,"turn_started_at_unix_ms":1,"analytics_enabled":false,"model":"m","reasoning_effort":"high"}"#;
    let metadata = serde_json::json!({
        "root_turn_id":"r", "thread_id":"t", "session_id":"s",
        "x-codex-window-id":"w", "turn_id":"u",
        "x-codex-turn-metadata":turn, "x-codex-installation-id":"i"
    });
    let payload = body("m").replace(r#"{"session_id":"s"}"#, &metadata.to_string());
    let wire = String::from_utf8(frame(&payload)).unwrap().replace(
        "x-codex-turn-metadata: {}",
        &format!("x-codex-turn-metadata: {turn}"),
    );
    let mut parser = frames();
    parser.feed(wire.as_bytes()).unwrap();
    let request = parser.next_request().unwrap().unwrap();
    assert_eq!(request.body, payload.as_bytes());
    assert!(
        request
            .headers
            .contains(&("x-codex-turn-metadata".into(), turn.into()))
    );
    request.validate_disclosure().unwrap();
}

#[test]
fn permission_requires_the_exact_profile_without_a_deserialization_default() {
    let original = approved();
    let notice = original.disclosure_notice().unwrap();
    assert!(notice.contains(&original.disclosure_profile));
    assert!(notice.contains(disclosure::NOTICE));
    let mut value = serde_json::to_value(&original).unwrap();
    value.as_object_mut().unwrap().remove("disclosure_profile");
    assert!(serde_json::from_value::<ApprovedProviderRequests>(value).is_err());
    for digest in [
        "",
        "codex-responses-metadata/1",
        &crate::Digest::of(b"other profile").to_string(),
    ] {
        let mut changed = original.clone();
        changed.disclosure_profile = digest.into();
        assert!(!changed.valid(1000));
        assert_eq!(
            changed.disclosure_notice().unwrap_err().code,
            ErrorCode::ProviderDisclosureDenied
        );
    }
    assert_eq!(
        disclosure::profile_id().unwrap(),
        "codex-responses-metadata/1"
    );
}

#[test]
fn approval_bounds_destination_budget_and_lifetime() {
    assert!(approved().valid(1999));
    assert!(!approved().valid(2000));
    let invalid: [fn(&mut ApprovedProviderRequests); 13] = [
        |a| a.provider = "OpenAI".into(),
        |a| a.upstream = "http://api.openai.com/v1/responses".into(),
        |a| a.upstream = "https://api.openai.com/v1/chat/completions".into(),
        |a| a.upstream = "https://user@api.openai.com/v1/responses".into(),
        |a| a.upstream = "https://api.openai.com/v1/responses?x=1".into(),
        |a| a.addresses.clear(),
        |a| a.addresses.push("192.0.2.1".parse().unwrap()),
        |a| a.max_run_requests = 0,
        |a| a.max_run_requests = MAX_RUN_REQUESTS + 1,
        |a| a.models.clear(),
        |a| a.models.reverse(),
        |a| a.models.push("gpt-6-astra".into()),
        |a| a.models = vec![String::new()],
    ];
    for change in invalid {
        let mut approval = approved();
        change(&mut approval);
        assert!(!approval.valid(0), "{approval:?}");
    }
}

#[test]
fn only_allowlisted_models_at_or_below_the_effort_ceiling_are_permitted() {
    let approval = approved();
    assert!(approval.permits("gpt-5.6-luna", Some("high")));
    assert!(approval.permits("gpt-6-astra", Some("low")));
    assert!(approval.permits("gpt-5.6-luna", Some("none")));
    for (model, effort) in [
        ("gpt-6-sol", Some("low")),
        ("GPT-5.6-luna", Some("low")),
        ("gpt-5.6-luna", Some("xhigh")),
        ("gpt-6-astra", Some("max")),
        // Not an API effort value; unknown values are never ordered below the ceiling.
        ("gpt-6-astra", Some("ultra")),
        ("gpt-6-astra", Some("")),
        // The Provider default is not a stated ceiling-compliant value.
        ("gpt-5.6-luna", None),
    ] {
        assert!(!approval.permits(model, effort), "{model} {effort:?}");
    }
    let mut raised = approved();
    raised.max_effort = ReasoningEffort::Max;
    assert!(raised.permits("gpt-6-astra", Some("max")));
}
