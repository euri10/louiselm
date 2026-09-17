//! The existing durable recorder's immutable turn and option facts.

use crate::error::{Failure, Result};
use crate::model::{Evidence, Parsed, Turn, timestamp};
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use std::path::Path;
use std::time::Duration;

pub(super) fn read(path: &Path, source: &str) -> Result<Parsed> {
    let mut db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    db.busy_timeout(Duration::from_secs(1))?;
    let transaction = db.transaction()?;
    let mut query = transaction.prepare(
        "SELECT id,agent,provider,acp_session_id,prepared_at,options,model FROM turns ORDER BY id",
    )?;
    let mut records = query.query([])?;
    let mut parsed = Parsed::default();
    while let Some(row) = records.next()? {
        let native: String = row.get(0)?;
        let agent: String = row.get(1)?;
        let session: String = row.get(3)?;
        let options: String = row.get(5)?;
        let model: Option<String> = row.get(6)?;
        let options: std::collections::BTreeMap<String, Value> = serde_json::from_str(&options)?;
        if options.values().any(|v| !v.is_string() && !v.is_boolean()) {
            return Err(Failure::storage(
                "Invalid typed option tuple in durable source",
            ));
        }
        let mut turn = Turn {
            id: format!("louiselm:{native}"),
            session_id: format!("louiselm:{agent}:{session}"),
            native_session_id: session,
            agent,
            provider: row.get(2)?,
            model: model.map_or(Ok(Value::Null), |s| serde_json::from_str(&s))?,
            options,
            options_complete: true,
            time: timestamp(&Value::String(row.get(4)?)),
            status: "unobserved".into(),
            evidence: vec![Evidence {
                source_id: source.into(),
                record: 0,
                row_id: Some(native.clone()),
            }],
            ..Turn::default()
        };
        events(&transaction, &native, &mut turn)?;
        turn.mixed_options = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM option_events WHERE turn_id=?1)",
            [&native],
            |r| r.get(0),
        )?;
        parsed.turns.insert(turn.id.clone(), turn);
    }
    Ok(parsed)
}

fn events(db: &Connection, id: &str, turn: &mut Turn) -> Result<()> {
    let mut query =
        db.prepare("SELECT kind,data FROM turn_events WHERE turn_id=?1 ORDER BY sequence")?;
    let mut rows = query.query([id])?;
    while let Some(row) = rows.next()? {
        let kind: String = row.get(0)?;
        let raw: String = row.get(1)?;
        let data: Value = serde_json::from_str(&raw)?;
        match kind.as_str() {
            "dispatch" => {
                turn.dispatched = true;
                if !data["request_id"].is_null() {
                    turn.rpc_request_id = Some(data["request_id"].to_string());
                }
            }
            "outcome" => {
                data["outcome"]
                    .as_str()
                    .unwrap_or("unobserved")
                    .clone_into(&mut turn.status);
                turn.peer_response = data["peer_response"].as_bool().unwrap_or(false);
                for field in [
                    "total_tokens",
                    "input_tokens",
                    "output_tokens",
                    "thought_tokens",
                    "cached_read_tokens",
                    "cached_write_tokens",
                ] {
                    if let Some(value) = data["usage"][field].as_u64() {
                        turn.usage.insert(field.to_owned(), value);
                    }
                }
            }
            _ => (),
        }
    }
    Ok(())
}
