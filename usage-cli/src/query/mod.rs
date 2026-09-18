//! SQL filtering and aggregation with closed identifiers and bounded responses.

use crate::cli::{Query, Subject};
use crate::error::{Failure, Result};
mod inspect;
mod metrics;
mod page;
pub(crate) use inspect::{show, sources};
mod schema;
mod select;
use page::rows;
use rusqlite::{Connection, params_from_iter, types::Value as SqlValue};
pub(crate) use schema::{describe as schema, options};
use select::Selection;
use serde_json::{Value, json};

fn json_value(value: SqlValue) -> Value {
    match value {
        SqlValue::Null | SqlValue::Blob(_) => Value::Null,
        SqlValue::Integer(i) => json!(i),
        SqlValue::Real(f) => json!(f),
        SqlValue::Text(s) => Value::String(s),
    }
}

pub(crate) fn stats(
    connection: &Connection,
    subject: Subject,
    query: &Query,
    group: Option<&str>,
) -> Result<Value> {
    let is_turn = matches!(subject, Subject::Turns);
    let is_request = matches!(subject, Subject::Requests);
    let call_fields = !is_turn && !is_request;
    if is_request
        && (query.cohort == "fixed"
            || !query.options.is_empty()
            || !query.provider.is_empty()
            || !query.agent.is_empty())
    {
        return Err(Failure::query(
            "Native request usage has no proved durable-turn configuration join; use stats turns for historical cohorts",
        ));
    }
    let metrics = metrics::metrics(subject);
    let table = subject.table();
    let default_group = subject.default_group();
    let mut groups: Vec<_> = group.unwrap_or(default_group).split(',').collect();
    if call_fields && query.level == "all" && !groups.contains(&"level") {
        groups.push("level");
    }
    if is_request {
        for key in ["scope", "basis"] {
            if !groups.contains(&key) {
                groups.push(key);
            }
        }
    }
    if groups
        .iter()
        .collect::<std::collections::BTreeSet<_>>()
        .len()
        != groups.len()
    {
        return Err(Failure::query("Group dimensions must be unique"));
    }
    if groups.len() > 8 {
        return Err(Failure::query("Group by at most eight dimensions"));
    }
    let discovery = format!("stats {}", subject.name());
    let mut selection = Selection::new(&discovery);
    selection.filters(query, call_fields)?;
    if is_turn {
        selection
            .conditions
            .push("json_extract(data,'$.dispatched')=1".into());
    }
    let mixed_excluded = mixed_excluded(connection, query, &selection, is_turn)?;
    if matches!(subject, Subject::Commands) {
        selection
            .conditions
            .push("json_extract(data,'$.command_key') IS NOT NULL".into());
    }
    let mut columns = Vec::new();
    for (i, group) in groups.iter().enumerate() {
        columns.push(format!("{} AS g{i}", selection.dimension(group)?));
    }
    columns.extend(metrics.iter().map(|(name, sql)| format!("{sql} AS {name}")));
    let order = sort(query, &groups, &metrics, &discovery)?;
    let grouping = (1..=groups.len())
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT {} FROM {table} WHERE {} GROUP BY {grouping} ORDER BY {order},{grouping}",
        columns.join(","),
        selection.predicate()
    );
    let names: Vec<_> = groups
        .iter()
        .copied()
        .chain(metrics.iter().map(|(name, _)| *name))
        .collect();
    let mut response = rows(
        connection,
        query,
        &sql,
        selection.parameters,
        &names,
        false,
        &discovery,
    )?;
    response["coverage"]["mixed_turns_excluded"] = json!(mixed_excluded);
    Ok(response)
}

fn mixed_excluded(
    connection: &Connection,
    query: &Query,
    selection: &Selection,
    is_turn: bool,
) -> Result<i64> {
    if !is_turn || query.cohort != "fixed" {
        return Ok(0);
    }
    let predicate = selection
        .conditions
        .iter()
        .filter(|s| !s.starts_with("json_extract(data,'$.mixed_options')=0"))
        .cloned()
        .collect::<Vec<_>>()
        .join(" AND ");
    Ok(connection.query_row(&format!("SELECT count(*) FROM turn_facts WHERE {predicate} AND json_extract(data,'$.mixed_options')=1"),params_from_iter(&selection.parameters),|r|r.get(0))?)
}

fn sort(
    query: &Query,
    groups: &[&str],
    metrics: &[(&str, &str)],
    discovery: &str,
) -> Result<String> {
    let default = format!("{}:desc", metrics[0].0);
    let requested = query.sort.as_deref().unwrap_or(&default);
    let (key, direction) = requested.split_once(':').unwrap_or((requested, "desc"));
    if !["asc", "desc"].contains(&direction) {
        return Err(Failure::query("Sort direction must be asc or desc"));
    }
    if metrics.iter().any(|(name, _)| key == *name) {
        return Ok(format!("{key} {direction} NULLS LAST"));
    }
    if let Some(i) = groups.iter().position(|g| *g == key) {
        return Ok(format!("g{i} {direction} NULLS LAST"));
    }
    Err(Failure::query(format!(
        "Unknown sort field {key:?}; inspect louiselm-usage schema {discovery}; use a metric or selected group dimension"
    )))
}

pub(crate) fn calls(connection: &Connection, query: &Query) -> Result<Value> {
    let mut selection = Selection::new("calls");
    selection.filters(query, true)?;
    let order = if query.sort.is_none() {
        "id ASC".to_owned()
    } else {
        let requested = query.sort.as_deref().unwrap_or("id:asc");
        let (key, direction) = requested.split_once(':').unwrap_or((requested, "desc"));
        if !["asc", "desc"].contains(&direction) {
            return Err(Failure::query("Sort direction must be asc or desc"));
        }
        format!("{} {direction} NULLS LAST,id", selection.dimension(key)?)
    };
    let sql = format!(
        "SELECT data FROM call_facts WHERE {} ORDER BY {order}",
        selection.predicate()
    );
    rows(
        connection,
        query,
        &sql,
        selection.parameters,
        &[],
        true,
        "calls",
    )
}
