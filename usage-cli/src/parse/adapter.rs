//! Adapter history and identity metadata; mutable current options are not history.
use crate::command;
use crate::error::Result;
use crate::model::{Call, Evidence, Parsed, Session, output, text};
use serde_json::Value;
use std::fs::File;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

pub(super) fn session(path: &Path, parsed: &mut Parsed) -> Result<Option<Session>> {
    let Some(parent) = path.parent() else {
        return Ok(None);
    };
    let meta = parent.join("meta.json");
    if !meta.exists() {
        parsed.note("missing_identity_metadata");
        return Ok(None);
    }
    let metadata = std::fs::symlink_metadata(&meta)?;
    if !metadata.is_file() || metadata.uid() != rustix::process::getuid().as_raw() {
        parsed.note("untrusted_identity_metadata");
        return Ok(None);
    }
    let mut bytes = Vec::new();
    File::open(meta)?
        .take(super::MAX_RECORD + 1)
        .read_to_end(&mut bytes)?;
    let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
        parsed.note("malformed_identity_metadata");
        return Ok(None);
    };
    let Some(native_id) = text(&value["session_id"]) else {
        parsed.note("missing_session_id");
        return Ok(None);
    };
    let session = Session {
        id: format!("adapter:{native_id}"),
        native_id,
        adapter: "adapter".into(),
        project: text(&value["cwd"]),
        ..Session::default()
    };
    parsed.sessions.insert(session.id.clone(), session.clone());
    Ok(Some(session))
}

pub(super) fn consume(
    event: &Value,
    session: &Session,
    source: &str,
    line: u64,
    parsed: &mut Parsed,
) {
    let evidence = Evidence {
        source_id: source.into(),
        record: line,
        row_id: None,
    };
    if event["role"] == "tool" {
        if let Some(id) = event["tool_call_id"].as_str() {
            let base = Call::in_session(session, id);
            let call = parsed.calls.entry(base.id.clone()).or_insert(base);
            output(call, &event["content"]);
            call.status = "result_observed".into();
            call.evidence.push(evidence);
        }
    } else if event["role"] == "assistant"
        && let Some(calls) = event["tool_calls"].as_array()
    {
        for tool in calls {
            let Some(id) = tool["id"].as_str() else {
                parsed.note("missing_call_id");
                continue;
            };
            let base = Call::in_session(session, id);
            let call = parsed.calls.entry(base.id.clone()).or_insert(base);
            call.tool = text(&tool["name"]).unwrap_or_else(|| "<unknown>".into());
            if call.status == "unobserved" {
                call.status = "requested".into();
            }
            let decoded;
            let args = if let Some(raw) = tool["arguments"].as_str() {
                decoded = serde_json::from_str::<Value>(raw).unwrap_or(Value::Null);
                &decoded
            } else {
                &tool["arguments"]
            };
            if let Some(cmd) = args["command"].as_str().or_else(|| args["cmd"].as_str()) {
                command::extract(call, cmd);
            }
            call.evidence.push(evidence.clone());
        }
    }
}
