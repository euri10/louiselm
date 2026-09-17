//! ACP frame lifecycles with explicitly observed prompt RPC associations.

use crate::{
    command,
    model::{Call, Evidence, Parsed, Session, output, timestamp},
};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Default)]
pub(super) struct State {
    prompts: BTreeMap<String, String>,
}

impl State {
    pub fn consume(&mut self, event: &Value, source: &str, line: u64, parsed: &mut Parsed) {
        if event["kind"] != "frame" {
            return;
        }
        let p = &event["payload"];
        let session = p["params"]["sessionId"]
            .as_str()
            .or_else(|| event["session_id"].as_str());
        match p["method"].as_str() {
            Some("session/prompt") => {
                if let Some(session) = session.filter(|_| !p["id"].is_null()) {
                    self.prompts.insert(session.to_owned(), p["id"].to_string());
                }
            }
            Some("session/update") => {
                let Some(session) = session else {
                    parsed.note("unassociated_acp_update");
                    return;
                };
                self.update(session, &p["params"]["update"], event, source, line, parsed);
            }
            None if !p["id"].is_null() => {
                let request = p["id"].to_string();
                self.prompts.retain(|_, value| *value != request);
            }
            _ => (),
        }
    }

    fn update(
        &self,
        session: &str,
        update: &Value,
        event: &Value,
        source: &str,
        line: u64,
        parsed: &mut Parsed,
    ) {
        if !matches!(
            update["sessionUpdate"].as_str(),
            Some("tool_call" | "tool_call_update")
        ) {
            return;
        }
        let Some(native_call) = update["toolCallId"].as_str() else {
            parsed.note("missing_acp_call_id");
            return;
        };
        // Claude ACP explicitly identifies its native tool in this metadata. Its
        // session and tool-use IDs match the native history (verified local source).
        let claude = update["_meta"]["claudeCode"]["toolName"].is_string();
        let adapter = if claude { "claude" } else { "acp" };
        let session_id = format!("{adapter}:{session}");
        parsed
            .sessions
            .entry(session_id.clone())
            .or_insert_with(|| Session {
                id: session_id.clone(),
                native_id: session.to_owned(),
                adapter: adapter.to_owned(),
                ..Session::default()
            });
        let candidate_id = format!("{session_id}:call:{native_call}");
        // Later updates can omit metadata already supplied on the initial call.
        let id = if !claude
            && parsed
                .calls
                .contains_key(&format!("claude:{session}:call:{native_call}"))
        {
            format!("claude:{session}:call:{native_call}")
        } else {
            candidate_id
        };
        let call = parsed.calls.entry(id.clone()).or_insert_with(|| Call {
            id,
            session_id,
            native_session_id: session.to_owned(),
            adapter: adapter.to_owned(),
            rpc_request_id: self.prompts.get(session).cloned(),
            level: "leaf".into(),
            status: "unobserved".into(),
            time: timestamp(&event["timestamp"]),
            ..Call::default()
        });
        if let Some(tool) = update["_meta"]["claudeCode"]["toolName"]
            .as_str()
            .or_else(|| update["name"].as_str())
        {
            tool.clone_into(&mut call.tool);
        }
        if call.tool.is_empty() {
            "<acp tool>".clone_into(&mut call.tool);
        }
        if let Some(status) = update["status"].as_str() {
            status.clone_into(&mut call.status);
        }
        if let Some(cmd) = update["rawInput"]["command"]
            .as_str()
            .or_else(|| update["rawInput"]["cmd"].as_str())
        {
            command::extract(call, cmd);
        }
        if !update["rawOutput"].is_null() {
            output(call, &update["rawOutput"]);
        } else if !update["content"].is_null() {
            let texts: Vec<Value> = update["content"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|c| {
                    c["content"]["text"]
                        .as_str()
                        .map(|s| serde_json::json!({"text":s}))
                })
                .collect();
            output(call, &Value::Array(texts));
        }
        call.evidence.push(Evidence {
            source_id: source.into(),
            record: line,
            row_id: None,
        });
    }
}
