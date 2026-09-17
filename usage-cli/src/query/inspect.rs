//! Source coverage and bounded evidence inspection; never opens source payloads.
use super::page;

pub(crate) fn options(
    connection: &Connection,
    limit: u32,
    cursor: Option<String>,
) -> Result<Value> {
    let sql = "SELECT option.key,option.type,count(*) FROM (SELECT data FROM call_facts UNION ALL SELECT data FROM turn_facts) AS fact,json_each(fact.data,'$.options') AS option GROUP BY option.key,option.type ORDER BY option.key,option.type";
    let query = Query {
        limit,
        cursor,
        ..Query::default()
    };
    let mut result = page::rows(
        connection,
        &query,
        sql,
        vec![],
        &["option_id", "type", "observations"],
        false,
    )?;
    result["scope"] = json!({"mode":"observed option IDs and JSON types; call/turn observations overlap and are not usage totals"});
    Ok(result)
}
use crate::{
    cli::{Query, SourceQuery},
    error::{Failure, Result},
    model, store,
};
use rusqlite::{Connection, OptionalExtension, types::Value as SqlValue};
use serde_json::{Value, json};

pub(crate) fn sources(connection: &Connection, options: &SourceQuery) -> Result<Value> {
    let mut conditions = Vec::new();
    let mut parameters = Vec::new();
    for (name, value) in [("state", &options.state), ("format", &options.adapter)] {
        if let Some(value) = value {
            parameters.push(SqlValue::Text(value.clone()));
            conditions.push(format!("{name}=?{}", parameters.len()));
        }
    }
    let predicate = if conditions.is_empty() {
        "1".into()
    } else {
        conditions.join(" AND ")
    };
    let sql = format!(
        "SELECT json_object('id',id,'adapter',format,'path',path,'state',state,'diagnostics',json(diagnostics),'observed_at',observed_at,'digest',digest) FROM sources WHERE {predicate} ORDER BY id"
    );
    let query = Query {
        limit: options.limit,
        cursor: options.cursor.clone(),
        ..Query::default()
    };
    let mut result = page::rows(
        connection,
        &query,
        &sql,
        parameters,
        &[
            "id",
            "adapter",
            "path",
            "state",
            "diagnostics",
            "observed_at",
            "digest",
        ],
        true,
    )?;
    result["scope"] = json!({"state":options.state,"adapter":options.adapter,"mode":"indexed metadata; --discover reads directory entries only"});
    Ok(result)
}

pub(crate) fn show(
    connection: &Connection,
    kind: &str,
    id: &str,
    fields: Option<&str>,
    limit: u32,
    cursor: Option<&str>,
) -> Result<Value> {
    let table = match kind {
        "call" => "call_facts",
        "session" => "sessions",
        "turn" => "turn_facts",
        "request" => "request_facts",
        _ => {
            return Err(Failure::query(
                "Show kind must be call, session, turn or request",
            ));
        }
    };
    let raw: Option<String> = connection
        .query_row(
            &format!("SELECT data FROM {table} WHERE id=?1 ORDER BY source_id LIMIT 1"),
            [id],
            |r| r.get(0),
        )
        .optional()?;
    let raw = raw.ok_or_else(|| Failure::query("Record ID not found in this index"))?;
    let mut record: Value = serde_json::from_str(&raw)?;
    let generation = store::generation(connection)?;
    let signature = model::digest(format!("show:{kind}:{id}:{fields:?}").as_bytes());
    let offset = usize::try_from(page::cursor(cursor, generation, &signature)?)
        .map_err(|_| Failure::query("Cursor offset too large"))?;
    if kind == "call" {
        let mut statement = connection
            .prepare("SELECT data FROM call_observations WHERE id=?1 ORDER BY source_id")?;
        let mut rows = statement.query([id])?;
        let mut evidence = Vec::new();
        while let Some(row) = rows.next()? {
            let data: Value = serde_json::from_str(&row.get::<_, String>(0)?)?;
            if let Some(items) = data["evidence"].as_array() {
                evidence.extend(items.iter().cloned());
            }
        }
        evidence.sort_by_key(Value::to_string);
        evidence.dedup();
        record["evidence"] = Value::Array(evidence);
    }
    let all = record["evidence"].as_array().cloned().unwrap_or_default();
    let mut selected: Vec<_> = all
        .iter()
        .skip(offset)
        .take(limit as usize)
        .cloned()
        .collect();
    for item in &mut selected {
        if let Some(source) = item["source_id"].as_str() {
            let (path, state, digest): (String, String, String) = connection.query_row(
                "SELECT path,state,digest FROM sources WHERE id=?1",
                [source],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            item["path"] = json!(path);
            item["source_state"] = json!(state);
            item["source_generation_digest"] = json!(digest);
        }
    }
    let allowed: Vec<String> = record
        .as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    page::validate_fields(fields, &allowed)?;
    loop {
        record["evidence"] = json!(selected);
        let next = (offset + selected.len() < all.len())
            .then(|| format!("{generation}:{}:{signature}", offset + selected.len()));
        let result = json!({"schema_version":1,"generation":generation,"record":page::project(record.clone(),fields),"evidence_total":all.len(),"next_cursor":next});
        if serde_json::to_vec(&result)?.len() <= 32768 {
            return Ok(result);
        }
        if selected.len() <= 1 {
            return Err(Failure::query(
                "Record exceeds response budget; choose fewer --fields",
            ));
        }
        selected.pop();
    }
}
