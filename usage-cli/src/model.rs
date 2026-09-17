//! Typed, payload-free indexed facts and measurement helpers.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Evidence {
    pub source_id: String,
    pub record: u64,
    pub row_id: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Call {
    pub id: String,
    pub session_id: String,
    pub native_session_id: String,
    pub turn_id: Option<String>,
    pub rpc_request_id: Option<String>,
    pub adapter: String,
    pub agent: Option<String>,
    pub provider: Option<String>,
    pub provider_route: Option<String>,
    pub model: Option<String>,
    pub options: BTreeMap<String, Value>,
    pub mixed_options: bool,
    pub options_complete: bool,
    pub project: Option<String>,
    pub parent_session_id: Option<String>,
    pub time: Option<i64>,
    pub tool: String,
    pub level: String,
    pub status: String,
    pub exit_code: Option<i64>,
    pub duration_ms: Option<f64>,
    pub command_key: Option<String>,
    pub family: Option<String>,
    pub signature: Option<String>,
    pub executable: Option<String>,
    pub wrapper: Option<String>,
    pub normalization: Option<String>,
    pub argument_bytes: Option<u64>,
    pub retained_output_bytes: Option<u64>,
    pub produced_output_bytes: Option<u64>,
    pub output_lines: Option<u64>,
    pub output_fingerprint: Option<String>,
    pub truncated: Option<bool>,
    pub evidence: Vec<Evidence>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Session {
    pub id: String,
    pub native_id: String,
    pub adapter: String,
    pub project: Option<String>,
    pub parent_session_id: Option<String>,
    pub louiselm_origin: bool,
}

impl Call {
    pub fn in_session(session: &Session, id: &str) -> Self {
        Self {
            id: format!("{}:call:{id}", session.id),
            session_id: session.id.clone(),
            native_session_id: session.native_id.clone(),
            adapter: session.adapter.clone(),
            project: session.project.clone(),
            parent_session_id: session.parent_session_id.clone(),
            level: "leaf".into(),
            status: "unobserved".into(),
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Request {
    pub id: String,
    pub session_id: String,
    pub adapter: String,
    pub project: Option<String>,
    pub model: Option<String>,
    pub provider_route: Option<String>,
    pub time: Option<i64>,
    pub scope: String,
    pub basis: String,
    pub usage: BTreeMap<String, u64>,
    pub evidence: Vec<Evidence>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Independent observations of option completeness/change, dispatch, and peer response; no single lifecycle enum represents them"
)]
pub(crate) struct Turn {
    pub id: String,
    pub session_id: String,
    pub native_session_id: String,
    pub agent: String,
    pub provider: String,
    pub model: Value,
    pub options: BTreeMap<String, Value>,
    pub options_complete: bool,
    pub mixed_options: bool,
    pub time: Option<i64>,
    pub status: String,
    pub dispatched: bool,
    pub peer_response: bool,
    pub rpc_request_id: Option<String>,
    pub usage: BTreeMap<String, u64>,
    pub evidence: Vec<Evidence>,
}

#[derive(Default, Serialize)]
pub(crate) struct Parsed {
    pub sessions: BTreeMap<String, Session>,
    pub calls: BTreeMap<String, Call>,
    pub turns: BTreeMap<String, Turn>,
    pub requests: BTreeMap<String, Request>,
    pub diagnostics: BTreeMap<String, u64>,
}

impl Parsed {
    pub fn note(&mut self, code: &str) {
        *self.diagnostics.entry(code.to_owned()).or_default() += 1;
    }
}

pub(crate) fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(crate) fn timestamp(value: &Value) -> Option<i64> {
    value
        .as_str()
        .and_then(|text| OffsetDateTime::parse(text, &Rfc3339).ok())
        .and_then(|time| i64::try_from(time.unix_timestamp_nanos() / 1_000_000).ok())
}

pub(crate) fn elapsed(start: i64, end: i64) -> Option<f64> {
    u32::try_from(end.checked_sub(start)?).ok().map(f64::from)
}

pub(crate) fn text(value: &Value) -> Option<String> {
    value
        .as_str()
        .filter(|s| s.len() <= 4096)
        .map(str::to_owned)
}

pub(crate) fn usage(value: &Value, mappings: &[(&str, &str)]) -> BTreeMap<String, u64> {
    mappings
        .iter()
        .filter_map(|(from, to)| {
            value
                .pointer(from)
                .and_then(Value::as_u64)
                .map(|v| ((*to).into(), v))
        })
        .collect()
}

pub(crate) fn output(call: &mut Call, value: &Value) {
    // Only text content has a byte measurement. JSON containers and nontext
    // payloads are not surrogate token or output-size measurements.
    let content = if let Some(s) = value.as_str() {
        Some(s.to_owned())
    } else if let Some(parts) = value.as_array() {
        let strings: Vec<&str> = parts
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect();
        (!strings.is_empty()).then(|| strings.concat())
    } else {
        None
    };
    if let Some(content) = content {
        call.retained_output_bytes = Some(content.len() as u64);
        call.output_lines = Some(content.lines().count() as u64);
        call.output_fingerprint = Some(digest(content.as_bytes()));
    }
}
