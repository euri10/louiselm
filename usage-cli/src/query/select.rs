//! Closed dimensions and typed, parameter-bound filters.
use super::metrics::DIMENSIONS;
use crate::{
    cli::Query,
    error::{Failure, Result},
    model,
};
use rusqlite::types::Value as SqlValue;
use serde_json::Value;
pub(super) struct Selection {
    pub(super) conditions: Vec<String>,
    pub(super) parameters: Vec<SqlValue>,
}

impl Selection {
    pub(super) fn new() -> Self {
        Self {
            conditions: Vec::new(),
            parameters: Vec::new(),
        }
    }

    fn bind(&mut self, value: SqlValue) -> String {
        self.parameters.push(value);
        format!("?{}", self.parameters.len())
    }

    pub(super) fn dimension(&mut self, name: &str) -> Result<String> {
        if ["day", "month"].contains(&name) {
            let format = if name == "day" { "%Y-%m-%d" } else { "%Y-%m" };
            return Ok(format!(
                "strftime('{format}',json_extract(data,'$.time')/1000,'unixepoch')"
            ));
        }
        if DIMENSIONS.contains(&name)
            || ["time", "retained_output_bytes", "duration_ms", "id"].contains(&name)
        {
            return Ok(format!("json_extract(data,'$.{name}')"));
        }
        if let Some(key) = name.strip_prefix("option:") {
            if key.is_empty() {
                return Err(Failure::query("Option dimension needs a key"));
            }
            let path = format!("$.options.{}", serde_json::to_string(key)?);
            let parameter = self.bind(SqlValue::Text(path));
            return Ok(format!(
                "json_object('type',json_type(data,{parameter}),'value',json_extract(data,{parameter}))"
            ));
        }
        Err(Failure::query(
            "Unknown field; inspect louiselm-usage schema stats",
        ))
    }

    fn equal(&mut self, name: &str, values: &[String]) -> Result<()> {
        if values.is_empty() {
            return Ok(());
        }
        let field = self.dimension(name)?;
        let slots: Vec<_> = values
            .iter()
            .map(|s| self.bind(SqlValue::Text(s.clone())))
            .collect();
        self.conditions
            .push(format!("{field} IN ({})", slots.join(",")));
        Ok(())
    }

    pub(super) fn filters(&mut self, query: &Query, call_fields: bool) -> Result<()> {
        validate_scope(query, call_fields)?;
        for (name, values) in [
            ("session_id", &query.session),
            ("adapter", &query.adapter),
            ("agent", &query.agent),
            ("provider", &query.provider),
            ("model", &query.model),
        ] {
            self.equal(name, values)?;
        }
        for (name, value) in [
            ("project", &query.project),
            ("command_key", &query.command_key),
            ("family", &query.family),
            ("tool", &query.tool),
            ("status", &query.status),
            ("signature", &query.signature),
            ("wrapper", &query.wrapper),
            ("parent_session_id", &query.parent_session),
        ] {
            if let Some(value) = value {
                self.equal(name, std::slice::from_ref(value))?;
            }
        }
        if !query.source_id.is_empty() {
            let slots: Vec<_> = query
                .source_id
                .iter()
                .map(|id| self.bind(SqlValue::Text(id.clone())))
                .collect();
            let slots = slots.join(",");
            self.conditions.push(if call_fields {
                format!(
                    "EXISTS (SELECT 1 FROM json_each(data,'$.source_ids') WHERE value IN ({slots}))"
                )
            } else {
                format!("source_id IN ({slots})")
            });
        }
        if query.children != "all" {
            self.conditions.push(format!(
                "json_extract(data,'$.parent_session_id') IS {}NULL",
                if query.children == "child" {
                    "NOT "
                } else {
                    ""
                }
            ));
        }
        for (name, value) in [
            ("retained_output_bytes", query.min_output_bytes),
            ("duration_ms", query.min_duration_ms),
        ] {
            if let Some(value) = value {
                let value = i64::try_from(value)
                    .map_err(|_| Failure::query("Range exceeds signed 64-bit limit"))?;
                let parameter = self.bind(SqlValue::Integer(value));
                self.conditions
                    .push(format!("json_extract(data,'$.{name}')>={parameter}"));
            }
        }
        if call_fields && query.overlap == "exclude" {
            self.conditions
                .push("json_extract(data,'$.overlap')='none_detected'".into());
        }
        if let Some(path) = &query.project_tree {
            let path = path.trim_end_matches('/');
            let exact = self.bind(SqlValue::Text(path.to_owned()));
            let prefix = self.bind(SqlValue::Text(format!("{path}/")));
            self.conditions.push(format!("(json_extract(data,'$.project')={exact} OR substr(json_extract(data,'$.project'),1,length({prefix}))={prefix})"));
        }
        for (value, operator) in [(&query.since, ">="), (&query.until, "<")] {
            if let Some(value) = value {
                let time = model::timestamp(&Value::String(value.clone())).ok_or_else(|| {
                    Failure::query("Use an RFC3339 timestamp with an explicit offset")
                })?;
                let slot = self.bind(SqlValue::Integer(time));
                self.conditions
                    .push(format!("json_extract(data,'$.time'){operator}{slot}"));
            }
        }
        if call_fields && query.level != "all" {
            self.equal("level", std::slice::from_ref(&query.level))?;
        }
        if query.cohort == "fixed" {
            self.conditions.push("json_extract(data,'$.mixed_options')=0 AND json_extract(data,'$.options_complete')=1".into());
        }
        self.options(&query.options)?;
        Ok(())
    }

    fn options(&mut self, options: &[String]) -> Result<()> {
        let mut seen = std::collections::BTreeSet::new();
        for option in options {
            let (key, input) = option
                .split_once('=')
                .ok_or_else(|| Failure::query("Use --option KEY=JSON_VALUE"))?;
            if !seen.insert(key) {
                return Err(Failure::query("Specify each option key once"));
            }
            let value: Value = serde_json::from_str(input).map_err(|_| {
                Failure::query("Option value must be a JSON string, boolean or number")
            })?;
            let (kind, value) = match value {
                Value::String(s) => ("text", SqlValue::Text(s)),
                Value::Bool(b) => (
                    if b { "true" } else { "false" },
                    SqlValue::Integer(i64::from(b)),
                ),
                Value::Number(n) if n.is_i64() => {
                    ("integer", SqlValue::Integer(n.as_i64().unwrap_or(0)))
                }
                _ => {
                    return Err(Failure::query(
                        "Option value must be a JSON string, boolean or integer",
                    ));
                }
            };
            let path = self.bind(SqlValue::Text(format!(
                "$.options.{}",
                serde_json::to_string(key)?
            )));
            let value = self.bind(value);
            self.conditions.push(format!(
                "json_type(data,{path})='{kind}' AND json_extract(data,{path})={value}"
            ));
        }
        Ok(())
    }

    pub(super) fn predicate(&self) -> String {
        if self.conditions.is_empty() {
            "1".into()
        } else {
            self.conditions.join(" AND ")
        }
    }
}

fn validate_scope(query: &Query, call_fields: bool) -> Result<()> {
    if !call_fields
        && (query.tool.is_some()
            || query.command_key.is_some()
            || query.family.is_some()
            || query.signature.is_some()
            || query.wrapper.is_some()
            || query.min_output_bytes.is_some()
            || query.min_duration_ms.is_some()
            || query.children != "all"
            || query.parent_session.is_some())
    {
        return Err(Failure::query(
            "These call-only filters are unavailable for usage records; use calls or stats commands/tools",
        ));
    }

    Ok(())
}
