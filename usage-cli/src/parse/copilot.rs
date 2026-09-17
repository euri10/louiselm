//! Copilot event stream with explicitly paired tool start/completion records.
use crate::command;
use crate::model::{
    Call, Evidence, Parsed, Request, Session, elapsed, output, text, timestamp, usage,
};
use serde_json::Value;

#[derive(Default)]
pub(super) struct State {
    session: Option<Session>,
    model: Option<String>,
}

impl State {
    pub fn consume(&mut self, event: &Value, source: &str, line: u64, parsed: &mut Parsed) {
        let data = &event["data"];
        let kind = event["type"].as_str().unwrap_or("");
        if kind == "session.start" {
            if let Some(native_id) = text(&data["sessionId"]) {
                let session = Session {
                    id: format!("copilot:{native_id}"),
                    native_id,
                    adapter: "copilot".into(),
                    project: text(&data["context"]["cwd"]),
                    ..Session::default()
                };
                parsed.sessions.insert(session.id.clone(), session.clone());
                self.session = Some(session);
            }
        } else if kind == "session.model_change" {
            self.model = text(&data["newModel"]);
        } else if let Some(session) = &self.session {
            let evidence = Evidence {
                source_id: source.into(),
                record: line,
                row_id: None,
            };
            if kind == "assistant.message"
                && let Some(id) = text(&data["messageId"])
            {
                let request = Request {
                    id: format!("{}:message:{id}", session.id),
                    session_id: session.id.clone(),
                    adapter: "copilot".into(),
                    project: session.project.clone(),
                    model: self.model.clone(),
                    time: timestamp(&event["timestamp"]),
                    scope: "message".into(),
                    basis: "native_reported".into(),
                    usage: usage(data, &[("/outputTokens", "output_tokens")]),
                    evidence: vec![evidence],
                    ..Request::default()
                };
                parsed.requests.insert(request.id.clone(), request);
            } else if ["tool.execution_start", "tool.execution_complete"].contains(&kind) {
                let Some(id) = data["toolCallId"].as_str() else {
                    parsed.note("missing_call_id");
                    return;
                };
                let base = Call::in_session(session, id);
                let call = parsed.calls.entry(base.id.clone()).or_insert(base);
                call.evidence.push(evidence);
                if kind == "tool.execution_start" {
                    call.tool = text(&data["toolName"]).unwrap_or_else(|| "<unknown>".into());
                    call.model.clone_from(&self.model);
                    call.time = timestamp(&event["timestamp"]);
                    call.turn_id = text(&data["turnId"]);
                    if call.status == "unobserved" {
                        call.status = "requested".into();
                    }
                    if let Some(cmd) = data["arguments"]["command"].as_str() {
                        command::extract(call, cmd);
                    }
                } else {
                    output(call, &data["result"]["content"]);
                    call.status = match data["success"].as_bool() {
                        Some(true) => "completed",
                        Some(false) => "failed",
                        None => "result_observed",
                    }
                    .into();
                    call.duration_ms = timestamp(&event["timestamp"])
                        .zip(call.time)
                        .and_then(|(end, start)| elapsed(start, end));
                }
            }
        }
    }
}
