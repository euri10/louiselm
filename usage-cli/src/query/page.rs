//! Shared projection, pagination, and bounded result envelopes.
use super::{json_value, metrics};
use crate::{
    cli::Query,
    error::{Failure, Result},
    model, store,
};
use rusqlite::{Connection, params_from_iter, types::Value as SqlValue};
use serde_json::{Map, Value, json};
pub(super) fn rows(
    connection: &Connection,
    query: &Query,
    sql: &str,
    mut parameters: Vec<SqlValue>,
    names: &[&str],
    documents: bool,
    discovery: &str,
) -> Result<Value> {
    let allowed = if documents && names.is_empty() {
        metrics::call_fields()
    } else {
        names.iter().map(|s| (*s).to_owned()).collect()
    };
    validate_fields(query.fields.as_deref(), &allowed, discovery)?;
    let generation = store::generation(connection)?;
    let signature = model::digest(format!("{sql}:{parameters:?}:{:?}", query.fields).as_bytes());
    let offset = cursor(query.cursor.as_deref(), generation, &signature)?;
    let limit_slot = parameters.len() + 1;
    parameters.extend([
        SqlValue::Integer(i64::from(query.limit) + 1),
        SqlValue::Integer(offset),
    ]);
    let sql = format!("{sql} LIMIT ?{limit_slot} OFFSET ?{}", limit_slot + 1);
    let mut statement = connection.prepare(&sql)?;
    let mut records = statement.query(params_from_iter(parameters))?;
    let mut result = Vec::new();
    while let Some(row) = records.next()? {
        let value = if documents {
            serde_json::from_str::<Value>(&row.get::<_, String>(0)?)?
        } else {
            let mut object = Map::new();
            for (i, name) in names.iter().enumerate() {
                let value = json_value(row.get(i)?);
                let value = if name.starts_with("option:") {
                    value
                        .as_str()
                        .and_then(|s| serde_json::from_str(s).ok())
                        .unwrap_or(Value::Null)
                } else {
                    value
                };
                object.insert((*name).to_owned(), value);
            }
            Value::Object(object)
        };
        result.push(project(value, query.fields.as_deref()));
    }
    let mut more = result.len() > query.limit as usize;
    result.truncate(query.limit as usize);
    loop {
        let next = more.then(|| {
            format!(
                "{generation}:{}:{signature}",
                offset + i64::try_from(result.len()).unwrap_or(i64::MAX)
            )
        });
        let response = json!({"schema_version":1,"generation":generation,"rows":result,"scope":query,"coverage":store::coverage(connection)?,"next_cursor":next});
        if serde_json::to_vec(&response)?.len() <= 32768 {
            return Ok(response);
        }
        if result.len() <= 1 {
            return Err(Failure::query(
                "One row exceeds the response budget; select fewer --fields",
            ));
        }
        result.pop();
        more = true;
    }
}

pub(super) fn validate_fields(
    fields: Option<&str>,
    allowed: &[String],
    discovery: &str,
) -> Result<()> {
    if let Some(fields) = fields {
        for field in fields.split(',') {
            if !allowed.iter().any(|name| name == field) {
                return Err(Failure::query(format!(
                    "Unknown projected field {field:?}; inspect louiselm-usage schema {discovery}. For stats, project only metrics and selected group dimensions."
                )));
            }
        }
    }
    Ok(())
}

pub(super) fn project(value: Value, fields: Option<&str>) -> Value {
    let Some(fields) = fields else {
        return value;
    };
    let mut object = Map::new();
    for field in fields.split(',') {
        let value = value.get(field).unwrap_or(&Value::Null);
        object.insert(field.to_owned(), value.clone());
    }
    Value::Object(object)
}

pub(super) fn cursor(cursor: Option<&str>, generation: i64, signature: &str) -> Result<i64> {
    let Some(cursor) = cursor else { return Ok(0) };
    let parts: Vec<_> = cursor.split(':').collect();
    if parts.len() != 3 || parts[0] != generation.to_string() || parts[2] != signature {
        return Err(Failure::query(
            "Cursor does not match this query/index generation; restart the query",
        ));
    }
    parts[1]
        .parse::<i64>()
        .ok()
        .filter(|n| *n >= 0)
        .ok_or_else(|| Failure::query("Invalid cursor offset"))
}
