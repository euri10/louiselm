//! Read-only transaction over `OpenCode`'s mutable session/message/part tables.
use crate::command;
use crate::error::Result;
use crate::model::{Call, Evidence, Parsed, Request, Session, elapsed, output, text, usage};
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use std::path::Path;
use std::time::Duration;

pub(super) fn read(path: &Path, source: &str) -> Result<Parsed> {
    let mut connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    connection.busy_timeout(Duration::from_secs(1))?;
    let transaction = connection.transaction()?;
    let mut parsed = Parsed::default();
    let mut statement = transaction.prepare("SELECT id,directory,parent_id FROM session")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let native_id: String = row.get(0)?;
        let session = Session {
            id: format!("opencode:{native_id}"),
            native_id,
            adapter: "opencode".into(),
            project: row.get(1)?,
            parent_session_id: row
                .get::<_, Option<String>>(2)?
                .map(|id| format!("opencode:{id}")),
            ..Session::default()
        };
        parsed.sessions.insert(session.id.clone(), session);
    }
    messages(&transaction, source, &mut parsed)?;
    parts(&transaction, source, &mut parsed)?;
    Ok(parsed)
}

fn messages(connection: &Connection, source: &str, parsed: &mut Parsed) -> Result<()> {
    let mut statement =
        connection.prepare("SELECT id,session_id,data,time_created FROM message")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let row_id: String = row.get(0)?;
        let native_id: String = row.get(1)?;
        let data: String = row.get(2)?;
        let Ok(msg) = serde_json::from_str::<Value>(&data) else {
            parsed.note("malformed_message");
            continue;
        };
        if msg["role"] != "assistant" {
            continue;
        }
        let session_id = format!("opencode:{native_id}");
        let request = Request {
            id: format!("{session_id}:message:{row_id}"),
            session_id: session_id.clone(),
            adapter: "opencode".into(),
            project: parsed
                .sessions
                .get(&session_id)
                .and_then(|s| s.project.clone()),
            model: text(&msg["modelID"]),
            provider_route: text(&msg["providerID"]),
            time: row.get(3)?,
            scope: "message".into(),
            basis: "native_reported".into(),
            usage: usage(
                &msg["tokens"],
                &[
                    ("/input", "input_tokens"),
                    ("/output", "output_tokens"),
                    ("/reasoning", "thought_tokens"),
                    ("/total", "total_tokens"),
                    ("/cache/read", "cached_read_tokens"),
                    ("/cache/write", "cached_write_tokens"),
                ],
            ),
            evidence: vec![Evidence {
                source_id: source.into(),
                record: 0,
                row_id: Some(row_id),
            }],
        };
        parsed.requests.insert(request.id.clone(), request);
    }
    Ok(())
}

fn parts(connection: &Connection, source: &str, parsed: &mut Parsed) -> Result<()> {
    let mut statement =
        connection.prepare("SELECT id,message_id,session_id,data,time_created FROM part")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let row_id: String = row.get(0)?;
        let message: String = row.get(1)?;
        let native: String = row.get(2)?;
        let data: String = row.get(3)?;
        let Ok(part) = serde_json::from_str::<Value>(&data) else {
            parsed.note("malformed_part");
            continue;
        };
        if part["type"] != "tool" {
            continue;
        }
        let Some(id) = part["callID"].as_str() else {
            parsed.note("missing_call_id");
            continue;
        };
        let Some(session) = parsed.sessions.get(&format!("opencode:{native}")) else {
            parsed.note("unassociated_call");
            continue;
        };
        let state = &part["state"];
        let mut call = Call::in_session(session, id);
        let request_id = format!("{}:message:{message}", session.id);
        if let Some(request) = parsed.requests.get(&request_id) {
            call.model.clone_from(&request.model);
            call.provider_route.clone_from(&request.provider_route);
        }
        call.turn_id = Some(request_id);
        call.tool = text(&part["tool"]).unwrap_or_else(|| "<unknown>".into());
        call.status = text(&state["status"]).unwrap_or_else(|| "unobserved".into());
        call.time = state["time"]["start"].as_i64().or(row.get(4)?);
        call.duration_ms = state["time"]["start"]
            .as_i64()
            .zip(state["time"]["end"].as_i64())
            .and_then(|(start, end)| elapsed(start, end));
        if let Some(cmd) = state["input"]["command"].as_str() {
            command::extract(&mut call, cmd);
        }
        output(&mut call, &state["output"]);
        call.evidence.push(Evidence {
            source_id: source.into(),
            record: 0,
            row_id: Some(row_id),
        });
        parsed.calls.insert(call.id.clone(), call);
    }
    Ok(())
}
