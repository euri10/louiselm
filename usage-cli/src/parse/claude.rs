//! Claude messages and sidechains; message and tool IDs survive replay.
use crate::command;
use crate::model::{Call, Evidence, Parsed, Request, Session, output, text, timestamp, usage};
use serde_json::Value;

pub(super) fn consume(event: &Value, source: &str, line: u64, parsed: &mut Parsed) {
    let Some(native_id) = text(&event["sessionId"]) else {
        return;
    };
    let root = format!("claude:{native_id}");
    let child = text(&event["agentId"]);
    let id = child
        .as_ref()
        .map_or_else(|| root.clone(), |id| format!("{root}:agent:{id}"));
    let session = parsed
        .sessions
        .entry(id.clone())
        .or_insert_with(|| Session {
            id,
            native_id,
            adapter: "claude".into(),
            project: text(&event["cwd"]),
            parent_session_id: child.map(|_| root),
            ..Session::default()
        });
    if let Some(project) = text(&event["cwd"]) {
        session.project = Some(project);
    }
    let session = session.clone();
    let evidence = Evidence {
        source_id: source.into(),
        record: line,
        row_id: None,
    };
    let msg = &event["message"];
    if event["type"] == "assistant"
        && let Some(id) = text(&msg["id"])
    {
        let request = Request {
            id: format!("{}:message:{id}", session.id),
            session_id: session.id.clone(),
            adapter: "claude".into(),
            project: session.project.clone(),
            model: text(&msg["model"]),
            time: timestamp(&event["timestamp"]),
            scope: "message".into(),
            basis: "native_reported".into(),
            usage: usage(
                &msg["usage"],
                &[
                    ("/input_tokens", "input_tokens"),
                    ("/output_tokens", "output_tokens"),
                    ("/cache_read_input_tokens", "cached_read_tokens"),
                    ("/cache_creation_input_tokens", "cached_write_tokens"),
                ],
            ),
            evidence: vec![evidence.clone()],
            ..Request::default()
        };
        parsed.requests.insert(request.id.clone(), request);
    }
    let Some(parts) = msg["content"].as_array() else {
        return;
    };
    for part in parts {
        let result = part["type"] == "tool_result";
        if !result && part["type"] != "tool_use" {
            continue;
        }
        let Some(id) = part[if result { "tool_use_id" } else { "id" }].as_str() else {
            parsed.note("missing_call_id");
            continue;
        };
        let base = Call::in_session(&session, id);
        let call = parsed.calls.entry(base.id.clone()).or_insert(base);
        call.evidence.push(evidence.clone());
        if result {
            output(call, &part["content"]);
            call.status = match part["is_error"].as_bool() {
                Some(true) => "failed",
                Some(false) => "completed",
                None => "result_observed",
            }
            .into();
        } else {
            call.tool = text(&part["name"]).unwrap_or_else(|| "<unknown>".into());
            call.model = text(&msg["model"]);
            call.time = timestamp(&event["timestamp"]);
            call.level = if ["Task", "Agent"].contains(&call.tool.as_str()) {
                "orchestrator"
            } else {
                "leaf"
            }
            .into();
            if call.status == "unobserved" {
                call.status = "requested".into();
            }
            if let Some(cmd) = part["input"]["command"].as_str() {
                command::extract(call, cmd);
            }
        }
    }
}
