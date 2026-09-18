//! Subject-specific field and historical-option discovery.

use super::{metrics, page, select::Selection};
use crate::{
    cli::{Query, Subject},
    error::{Failure, Result},
    model,
};
use clap::ValueEnum;
use rusqlite::{Connection, params_from_iter};
use serde_json::{Value, json};

fn subject(value: &str) -> Result<Subject> {
    Subject::from_str(value, false).map_err(|_| {
        Failure::query("Unknown subject; use tools, commands, sessions, turns or requests")
    })
}

pub(super) fn record_fields(kind: &str) -> Result<Vec<String>> {
    let value = match kind {
        "call" => return Ok(metrics::call_fields()),
        "session" => serde_json::to_value(model::Session::default())?,
        "turn" => serde_json::to_value(model::Turn::default())?,
        "request" => serde_json::to_value(model::Request::default())?,
        _ => {
            return Err(Failure::query(
                "Show kind must be call, session, turn or request",
            ));
        }
    };
    Ok(value
        .as_object()
        .into_iter()
        .flat_map(|v| v.keys().cloned())
        .collect())
}

fn dimensions(subject: Subject) -> Result<Vec<String>> {
    let kind = match subject {
        Subject::Turns => "turn",
        Subject::Requests => "request",
        _ => "call",
    };
    let fields = record_fields(kind)?;
    Ok(metrics::DIMENSIONS
        .iter()
        .filter(|name| fields.iter().any(|field| field == **name))
        .copied()
        .chain(["day", "month"])
        .map(str::to_owned)
        .collect())
}

pub(crate) fn describe(command: Option<&str>, selected: Option<&str>) -> Result<Value> {
    match (command, selected) {
        (Some("stats"), Some(name)) => {
            let subject = subject(name)?;
            let metrics: Vec<_> = metrics::metrics(subject)
                .iter()
                .map(|(name, _)| *name)
                .collect();
            let defaults: Vec<_> = subject.default_group().split(',').collect();
            Ok(json!({
                "schema_version":1,"command":"stats","subject":name,
                "fields":defaults.iter().chain(&metrics).collect::<Vec<_>>(),
                "metrics":metrics,"dimensions":dimensions(subject)?,"default_group":defaults,
                "projection":"Fields are metrics plus selected group dimensions; default fields shown above",
                "dynamic_dimensions":if matches!(subject, Subject::Requests) { vec![] } else { vec!["option:<observed option ID>"] },
                "options_command":format!("louiselm-usage schema options {name}"),
                "scope":match subject {
                    Subject::Turns => "Dispatched LouiseLM durable turns; --cohort fixed excludes incomplete/mixed options",
                    Subject::Requests => "Native response/message usage; scope and basis always grouped; no proved historical option join; never add to durable-turn totals",
                    _ => "Canonical calls; default leaf only, possible mirrors excluded; sessions counts only Sessions with selected calls",
                },
            }))
        }
        (Some("calls"), None) => Ok(json!({
            "schema_version":1,"command":"calls","fields":metrics::call_fields(),
            "sort_fields":metrics::DIMENSIONS.iter().copied().chain(["day","month","time","retained_output_bytes","duration_ms","id","option:<observed option ID>"]).collect::<Vec<_>>(),
            "options_command":"louiselm-usage schema options calls",
        })),
        (Some("show"), Some(kind)) => Ok(json!({
            "schema_version":1,"command":"show","kind":kind,"fields":record_fields(kind)?,
        })),
        (None | Some("stats" | "sources" | "index" | "show"), None) => {
            let mut value = metrics::schema(command);
            value["discovery"] = json!([
                "schema stats <tools|commands|sessions|turns|requests>",
                "schema calls",
                "schema show <call|session|turn|request>",
                "schema options <calls|tools|commands|sessions|turns|requests>",
            ]);
            Ok(value)
        }
        _ => Err(Failure::query(
            "Unknown schema command/subject combination; inspect louiselm-usage schema",
        )),
    }
}

pub(crate) fn options(
    connection: &Connection,
    selected: Option<&str>,
    limit: u32,
    cursor: Option<String>,
) -> Result<Value> {
    let name = selected.ok_or_else(|| Failure::query(
        "Choose a query subject: schema options calls, schema options turns, or schema options <stats subject>",
    ))?;
    let subject = if name == "calls" {
        Subject::Tools
    } else {
        subject(name)?
    };
    let supports_options = !matches!(subject, Subject::Requests);
    let query = Query {
        limit,
        cursor,
        level: "leaf".into(),
        overlap: "exclude".into(),
        children: "all".into(),
        cohort: "all".into(),
        ..Query::default()
    };
    let mut selection = Selection::new(&format!("options {name}"));
    selection.filters(
        &query,
        !matches!(subject, Subject::Turns | Subject::Requests),
    )?;
    if matches!(subject, Subject::Turns) {
        selection
            .conditions
            .push("json_extract(data,'$.dispatched')=1".into());
    }
    if matches!(subject, Subject::Commands) {
        selection
            .conditions
            .push("json_extract(data,'$.command_key') IS NOT NULL".into());
    }
    let table = subject.table();
    let predicate = selection.predicate();
    let (records, complete, mixed): (i64,i64,i64) = connection.query_row(
        &format!("SELECT count(*),count(CASE WHEN json_extract(data,'$.options_complete')=1 THEN 1 END),count(CASE WHEN json_extract(data,'$.mixed_options')=1 THEN 1 END) FROM {table} WHERE {predicate}"),
        params_from_iter(&selection.parameters), |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
    )?;
    let sql = format!(
        "WITH selected AS (SELECT data FROM {table} WHERE {predicate}) SELECT option.key,option.type,count(*),(SELECT count(*) FROM selected)-count(*) FROM selected,json_each(selected.data,'$.options') AS option GROUP BY option.key,option.type ORDER BY option.key,option.type"
    );
    let mut result = page::rows(
        connection,
        &query,
        &sql,
        selection.parameters,
        &["option_id", "type", "observations", "missing_records"],
        false,
        &format!("options {name}"),
    )?;
    result["scope"] = json!({"subject":name,"options_supported":supports_options,
        "mode":"Observed option IDs/types for this subject's default selection; call and turn observations overlap, not usage totals",
        "missing_records":"Subject records without this exact option ID/type",
        "comparison":if supports_options { "For fixed comparisons inspect mixed_option_records and options_complete_records; never substitute keys from another subject" } else { "Native requests have no proved historical option join; use stats turns for historical cohorts" },
    });
    result["coverage"]["subject_records"] = json!(records);
    result["coverage"]["options_complete_records"] = if supports_options {
        json!(complete)
    } else {
        Value::Null
    };
    result["coverage"]["mixed_option_records"] = if supports_options {
        json!(mixed)
    } else {
        Value::Null
    };
    Ok(result)
}
