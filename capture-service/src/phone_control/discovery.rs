//! Closed, bounded responses for authenticated Session discovery.

use serde::Deserialize;
use serde_json::Value;

use super::authority::Refusal;
use super::{Request, fixed, valid_string};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionList {
    v: u64,
    #[serde(rename = "type")]
    kind: String,
    controller_id: String,
    operation_id: String,
    sessions: Vec<SessionSummary>,
    next_after: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionSummary {
    subject_id: String,
    name: String,
    agent: String,
    project: String,
    status: String,
    #[serde(rename = "truncated")]
    _truncated: bool,
}

pub(super) fn validate_session_list(bytes: &[u8], request: &Request) -> Result<Value, Refusal> {
    if bytes.len() >= super::MAX_FRAME {
        return Err(Refusal::Unavailable);
    }
    // A controller may return the same closed refusal as any other read.
    if let Ok(value) = serde_json::from_slice::<Value>(bytes)
        && value.get("type").and_then(Value::as_str) == Some("error")
    {
        return super::socket::validate_read_response(bytes, request);
    }
    let page: SessionList = serde_json::from_slice(bytes).map_err(|_| Refusal::Unavailable)?;
    let payload = fixed::<17>(&request.payload).map_err(|_| Refusal::Unavailable)?;
    let after = &payload[..16];
    if page.v != 1
        || page.kind != "session_list"
        || page.controller_id != request.controller_id
        || page.operation_id != request.operation_id
        || page.sessions.len() > usize::from(payload[16])
    {
        return Err(Refusal::Unavailable);
    }
    let mut previous = after.to_vec();
    for row in &page.sessions {
        let id = fixed::<16>(&row.subject_id).map_err(|_| Refusal::Unavailable)?;
        if id.as_slice() <= previous.as_slice()
            || ![&row.name, &row.agent, &row.project]
                .into_iter()
                .all(|value| valid_string(value) && value.len() <= 512)
            || !matches!(
                row.status.as_str(),
                "starting"
                    | "ready"
                    | "admitting"
                    | "configuring"
                    | "preparing"
                    | "prompting"
                    | "running"
                    | "waiting_permission"
                    | "cancelling"
                    | "error"
            )
        {
            return Err(Refusal::Unavailable);
        }
        previous = id.to_vec();
    }
    if !page.next_after.is_empty()
        && (page.sessions.len() != usize::from(payload[16])
            || page.sessions.last().map(|row| &row.subject_id) != Some(&page.next_after))
    {
        return Err(Refusal::Unavailable);
    }
    serde_json::from_slice(bytes).map_err(|_| Refusal::Unavailable)
}
