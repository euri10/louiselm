//! Gemini full records and append/replace journals, keyed by message/tool ID.
use crate::command;
use crate::model::{Call, Evidence, Parsed, Request, Session, output, text, timestamp, usage};
use serde_json::Value;

#[derive(Default)]
pub(super) struct State {
    session: Option<Session>,
}

impl State {
    pub fn consume(&mut self, event: &Value, source: &str, line: u64, parsed: &mut Parsed) {
        let record = event.get("$set").unwrap_or(event);
        if let Some(native_id) = text(&record["sessionId"]) {
            self.session = Some(Session {
                id: format!("gemini:{native_id}"),
                native_id,
                adapter: "gemini".into(),
                project: text(&record["directories"][0]),
                ..Session::default()
            });
        }
        let Some(session) = &mut self.session else {
            parsed.note("missing_session_id");
            return;
        };
        if let Some(path) = text(&record["directories"][0]) {
            session.project = Some(path);
        }
        parsed.sessions.insert(session.id.clone(), session.clone());
        if let Some(messages) = record["messages"].as_array() {
            parsed.calls.retain(|_, call| call.session_id != session.id);
            parsed
                .requests
                .retain(|_, request| request.session_id != session.id);
            for message in messages {
                Self::message(message, session, source, line, parsed);
            }
        } else if record["id"].is_string() {
            Self::message(record, session, source, line, parsed);
        }
    }

    fn message(msg: &Value, session: &Session, source: &str, line: u64, parsed: &mut Parsed) {
        if msg["type"] != "gemini" {
            return;
        }
        let Some(id) = text(&msg["id"]) else {
            parsed.note("missing_message_id");
            return;
        };
        let evidence = Evidence {
            source_id: source.into(),
            record: line,
            row_id: None,
        };
        let request = Request {
            id: format!("{}:message:{id}", session.id),
            session_id: session.id.clone(),
            adapter: "gemini".into(),
            project: session.project.clone(),
            model: text(&msg["model"]),
            time: timestamp(&msg["timestamp"]),
            scope: "message".into(),
            // Gemini's recorder itself substitutes zero for absent upstream fields.
            basis: "recorder_defaulted".into(),
            usage: usage(
                &msg["tokens"],
                &[
                    ("/input", "input_tokens"),
                    ("/output", "output_tokens"),
                    ("/cached", "cached_read_tokens"),
                    ("/thoughts", "thought_tokens"),
                    ("/tool", "tool_tokens"),
                    ("/total", "total_tokens"),
                ],
            ),
            evidence: vec![evidence.clone()],
            ..Request::default()
        };
        let message_id = request.id.clone();
        parsed.requests.insert(message_id.clone(), request);
        parsed
            .calls
            .retain(|_, call| call.turn_id.as_ref() != Some(&message_id));
        if let Some(tools) = msg["toolCalls"].as_array() {
            for tool in tools {
                let Some(id) = tool["id"].as_str() else {
                    parsed.note("missing_call_id");
                    continue;
                };
                let mut call = Call::in_session(session, id);
                call.tool = text(&tool["name"]).unwrap_or_else(|| "<unknown>".into());
                call.model = text(&msg["model"]);
                call.time = timestamp(&tool["timestamp"]).or_else(|| timestamp(&msg["timestamp"]));
                call.turn_id = Some(message_id.clone());
                call.status = text(&tool["status"]).unwrap_or_else(|| "unobserved".into());
                if let Some(cmd) = tool["args"]["command"].as_str() {
                    command::extract(&mut call, cmd);
                }
                // Structured response parts are not measured as if they were text.
                output(&mut call, &tool["result"]);
                call.evidence.push(evidence.clone());
                parsed.calls.insert(call.id.clone(), call);
            }
        }
    }
}
