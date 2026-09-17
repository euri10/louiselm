//! Discoverable metric definitions, units, and closed query fields.
use crate::{cli::Subject, sources};
use serde_json::{Value, json};

pub(super) const DIMENSIONS: &[&str] = &[
    "tool",
    "family",
    "command_key",
    "signature",
    "session_id",
    "turn_id",
    "project",
    "adapter",
    "agent",
    "provider",
    "provider_route",
    "model",
    "wrapper",
    "status",
    "normalization",
    "level",
    "exit_code",
    "parent_session_id",
    "scope",
    "basis",
    "overlap",
];
pub(super) const METRICS: &[(&str, &str)] = &[
    ("calls", "count(*)"),
    (
        "sessions",
        "count(DISTINCT json_extract(data,'$.session_id'))",
    ),
    (
        "retained_output_bytes",
        "sum(json_extract(data,'$.retained_output_bytes'))",
    ),
    (
        "produced_output_bytes",
        "sum(json_extract(data,'$.produced_output_bytes'))",
    ),
    (
        "produced_measured_calls",
        "count(json_extract(data,'$.produced_output_bytes'))",
    ),
    (
        "output_measured_calls",
        "count(json_extract(data,'$.retained_output_bytes'))",
    ),
    (
        "output_missing_calls",
        "count(*)-count(json_extract(data,'$.retained_output_bytes'))",
    ),
    (
        "mean_output_bytes",
        "avg(json_extract(data,'$.retained_output_bytes'))",
    ),
    (
        "max_output_bytes",
        "max(json_extract(data,'$.retained_output_bytes'))",
    ),
    (
        "argument_bytes",
        "sum(json_extract(data,'$.argument_bytes'))",
    ),
    (
        "arguments_measured_calls",
        "count(json_extract(data,'$.argument_bytes'))",
    ),
    ("output_lines", "sum(json_extract(data,'$.output_lines'))"),
    ("duration_ms", "sum(json_extract(data,'$.duration_ms'))"),
    (
        "duration_measured_calls",
        "count(json_extract(data,'$.duration_ms'))",
    ),
    (
        "exit_measured_calls",
        "count(json_extract(data,'$.exit_code'))",
    ),
    (
        "nonzero_exits",
        "sum(CASE WHEN json_extract(data,'$.exit_code') IS NOT NULL THEN json_extract(data,'$.exit_code')!=0 END)",
    ),
    (
        "failed_calls",
        "sum(CASE WHEN json_extract(data,'$.status') IN ('failed','error') THEN 1 ELSE 0 END)",
    ),
    ("truncated_calls", "sum(json_extract(data,'$.truncated'))"),
    (
        "truncation_measured_calls",
        "count(json_extract(data,'$.truncated'))",
    ),
    (
        "distinct_commands",
        "count(DISTINCT json_extract(data,'$.command_key'))",
    ),
    (
        "repeat_command_calls",
        "count(json_extract(data,'$.command_key'))-count(DISTINCT json_extract(data,'$.command_key'))",
    ),
    (
        "distinct_outputs",
        "count(DISTINCT json_extract(data,'$.output_fingerprint'))",
    ),
    (
        "repeat_output_calls",
        "count(json_extract(data,'$.output_fingerprint'))-count(DISTINCT json_extract(data,'$.output_fingerprint'))",
    ),
    (
        "historical_options_known_calls",
        "sum(json_extract(data,'$.options_complete'))",
    ),
    (
        "mixed_option_calls",
        "sum(json_extract(data,'$.mixed_options'))",
    ),
    (
        "conflicting_calls",
        "sum(json_extract(data,'$.conflicting_observations'))",
    ),
];
const USAGE_METRICS: &[(&str, &str)] = &[
    (
        "sessions",
        "count(DISTINCT json_extract(data,'$.session_id'))",
    ),
    (
        "reported_input_tokens",
        "sum(json_extract(data,'$.usage.input_tokens'))",
    ),
    (
        "reported_output_tokens",
        "sum(json_extract(data,'$.usage.output_tokens'))",
    ),
    (
        "reported_total_tokens",
        "sum(json_extract(data,'$.usage.total_tokens'))",
    ),
    (
        "reported_thought_tokens",
        "sum(json_extract(data,'$.usage.thought_tokens'))",
    ),
    (
        "reported_cached_read_tokens",
        "sum(json_extract(data,'$.usage.cached_read_tokens'))",
    ),
    (
        "reported_cached_write_tokens",
        "sum(json_extract(data,'$.usage.cached_write_tokens'))",
    ),
    (
        "input_measured_records",
        "count(json_extract(data,'$.usage.input_tokens'))",
    ),
    (
        "output_measured_records",
        "count(json_extract(data,'$.usage.output_tokens'))",
    ),
    (
        "total_measured_records",
        "count(json_extract(data,'$.usage.total_tokens'))",
    ),
    (
        "thought_measured_records",
        "count(json_extract(data,'$.usage.thought_tokens'))",
    ),
    (
        "cached_read_measured_records",
        "count(json_extract(data,'$.usage.cached_read_tokens'))",
    ),
    (
        "cached_write_measured_records",
        "count(json_extract(data,'$.usage.cached_write_tokens'))",
    ),
    (
        "mean_reported_input_tokens",
        "avg(json_extract(data,'$.usage.input_tokens'))",
    ),
];

pub(super) fn metrics(subject: Subject) -> Vec<(&'static str, &'static str)> {
    match subject {
        Subject::Turns | Subject::Requests => std::iter::once((
            if matches!(subject, Subject::Turns) {
                "turns"
            } else {
                "requests"
            },
            "count(*)",
        ))
        .chain(USAGE_METRICS.iter().copied())
        .collect(),
        _ => METRICS.to_vec(),
    }
}

pub(crate) fn schema(command: Option<&str>) -> Value {
    json!({
        "schema_version":1,"command":command,
        "commands":["sources","index","stats","calls","show","schema"],
        "subjects":["tools","commands","sessions","turns","requests"],
        "formats":sources::FORMATS,"dimensions":DIMENSIONS,
        "dynamic_dimensions":["option:<native option ID> (typed)","day (UTC)","month (UTC)"],
        "call_fields":call_fields(),
        "call_metrics":METRICS.iter().map(|(name,_)|name).collect::<Vec<_>>(),
        "usage_metrics":USAGE_METRICS.iter().map(|(name,_)|name).collect::<Vec<_>>(),
        "units":{"retained_output_bytes":"UTF-8 retained text, not billed tokens","argument_bytes":"recorded shell program bytes, not all tool arguments","duration_ms":"sum of observed durations, not elapsed wall time"},
        "scopes":{"calls":"canonical tool invocations and independently recorded executions; default leaf only; possible mirrors excluded","sessions":"sessions with selected calls, not every idle session","turns":"LouiseLM dispatched durable turns","requests":"native response/message usage, NEVER add to durable-turn totals; grouped by scope and basis"},
        "missing":"null unavailable, zero observed; Gemini recorder_defaulted fields may include substituted upstream zeros",
        "output":{"default_rows":20,"max_bytes":32768,"cursor":"generation + query signature; refresh invalidates changed generations"},
        "filters":"Same-field repeated values OR; different fields AND. --option KEY=JSON_VALUE retains type. RFC3339 --since inclusive / --until exclusive",
        "comparisons":"observational, not causal savings; byte repetition does not establish dispensability; cached tokens are not additive to total",
        "overlap":"Proven mirrors merged, conflicting measurements flagged. --overlap include exposes possible mirrors, not disjoint work",
        "exit_codes":{"0":"success","2":"invalid query","3":"index or I/O failure","4":"partial indexing; inspect sources"}
    })
}

pub(super) fn call_fields() -> Vec<String> {
    let mut fields = serde_json::to_value(crate::model::Call::default())
        .ok()
        .and_then(|v| v.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()))
        .unwrap_or_default();
    fields.extend(
        [
            "source_ids",
            "native_turn_id",
            "observation_count",
            "conflicting_observations",
            "overlap",
        ]
        .map(str::to_owned),
    );
    fields
}
