//! Bounded HTTP/1.1 framing for the one reviewed Responses operation.
//!
//! Promoted from the `louiselm-qbr.5.1.3.8` admission fixture and widened only
//! to the request shape stock Codex 0.156.1 was observed to send
//! (`scripts/probe-codex-request-shape.py`, recorded on
//! `louiselm-qbr.5.1.3.2.1`). Anything else is refused, and a refused
//! connection never resynchronizes onto a later frame.

use serde_json::{Map, Value};

use super::ProviderRequest;
use crate::launch_protocol::{ErrorCode, ProtocolError};

const MAX_HEADER: usize = 32 * 1024;
/// Real Codex contexts reach megabytes; a one-word prompt was already 37 KB.
const MAX_BODY: usize = 32 * 1024 * 1024;
const MAX_BUFFER: usize = MAX_HEADER + MAX_BODY;
const MAX_HEADERS: usize = 32;

/// Reviewed headers relayed upstream unchanged.
const FORWARDED: [&str; 10] = [
    "accept",
    "content-type",
    "originator",
    "user-agent",
    "session-id",
    "thread-id",
    "x-client-request-id",
    "x-codex-beta-features",
    "x-codex-window-id",
    "x-codex-turn-metadata",
];

/// Reviewed headers consumed locally and never relayed.
const LOCAL: [&str; 3] = [
    "host",
    "content-length",
    "x-openai-internal-codex-responses-lite",
];

/// Reviewed top-level request fields; nested content passes through unchanged.
const FIELDS: [&str; 11] = [
    "model",
    "input",
    "tool_choice",
    "parallel_tool_calls",
    "reasoning",
    "store",
    "stream",
    "include",
    "prompt_cache_key",
    "text",
    "client_metadata",
];

fn invalid() -> ProtocolError {
    ProtocolError::new(ErrorCode::InvalidRequest, None, None)
}

/// Incremental parser for one local connection.
///
/// The first refusal poisons the parser: bytes after a malformed frame are
/// never reinterpreted as a new request.
pub struct Frames {
    host: String,
    buffer: Vec<u8>,
    poisoned: bool,
}

impl Frames {
    /// Parses requests addressed to exactly `host` (the endpoint's own
    /// `address:port` authority, as the runtime was configured to send it).
    #[must_use]
    pub fn new(host: String) -> Self {
        Self {
            host,
            buffer: Vec::new(),
            poisoned: false,
        }
    }

    /// Appends received bytes.
    ///
    /// # Errors
    /// Returns [`ErrorCode::MessageTooLarge`] when buffered bytes exceed the
    /// frame bound, or [`ErrorCode::InvalidRequest`] after an earlier refusal.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<(), ProtocolError> {
        if self.poisoned {
            return Err(invalid());
        }
        if bytes.len() > MAX_BUFFER.saturating_sub(self.buffer.len()) {
            self.poisoned = true;
            return Err(ProtocolError::new(ErrorCode::MessageTooLarge, None, None));
        }
        self.buffer.extend_from_slice(bytes);
        Ok(())
    }

    /// Returns the next complete request, or `None` while one is incomplete.
    ///
    /// # Errors
    /// Returns [`ErrorCode::InvalidRequest`] for any unreviewed method, target,
    /// header, encoding or field, and for every later call on this parser.
    pub fn next_request(&mut self) -> Result<Option<ProviderRequest>, ProtocolError> {
        if self.poisoned {
            return Err(invalid());
        }
        let result = self.parse();
        if result.is_err() {
            self.poisoned = true;
            self.buffer.clear();
        }
        result
    }

    /// Whether a partial request is buffered, so EOF would abandon it.
    #[must_use]
    pub fn is_partial(&self) -> bool {
        !self.buffer.is_empty()
    }

    fn parse(&mut self) -> Result<Option<ProviderRequest>, ProtocolError> {
        let Some(end) = self
            .buffer
            .windows(4)
            .position(|bytes| bytes == b"\r\n\r\n")
        else {
            return if self.buffer.len() > MAX_HEADER {
                Err(invalid())
            } else {
                Ok(None)
            };
        };
        let end = end + 4;
        if end > MAX_HEADER {
            return Err(invalid());
        }
        let header = &self.buffer[..end];
        // httparse permits bare LF; the reviewed client sends CRLF only.
        if header
            .windows(2)
            .any(|pair| pair[1] == b'\n' && pair[0] != b'\r')
        {
            return Err(invalid());
        }
        let mut fields = [httparse::EMPTY_HEADER; MAX_HEADERS];
        let mut parsed = httparse::Request::new(&mut fields);
        if parsed.parse(header).map_err(|_| invalid())? != httparse::Status::Complete(end)
            || parsed.method != Some("POST")
            || parsed.path != Some("/v1/responses")
            || parsed.version != Some(1)
        {
            return Err(invalid());
        }
        let mut seen = Vec::with_capacity(parsed.headers.len());
        let mut forwarded = Vec::new();
        let mut length = None;
        for field in parsed.headers.iter() {
            let name = field.name.to_ascii_lowercase();
            let value = std::str::from_utf8(field.value).map_err(|_| invalid())?;
            if seen.contains(&name)
                || !(FORWARDED.contains(&name.as_str()) || LOCAL.contains(&name.as_str()))
            {
                return Err(invalid());
            }
            match name.as_str() {
                "host" if value != self.host => return Err(invalid()),
                "content-type" if value != "application/json" => return Err(invalid()),
                "accept" if value != "text/event-stream" => return Err(invalid()),
                "content-length" => length = Some(value),
                _ => {}
            }
            if FORWARDED.contains(&name.as_str()) {
                forwarded.push((name.clone(), value.to_owned()));
            }
            seen.push(name);
        }
        for required in ["host", "content-type", "accept"] {
            if !seen.iter().any(|name| name == required) {
                return Err(invalid());
            }
        }
        let length = length.ok_or_else(invalid)?;
        if length.is_empty() || !length.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(invalid());
        }
        let length: usize = length.parse().map_err(|_| invalid())?;
        if length == 0 || length > MAX_BODY {
            return Err(invalid());
        }
        if self.buffer.len() - end < length {
            return Ok(None);
        }
        let body = self.buffer[end..end + length].to_vec();
        let (model, effort) = policy_fields(&body)?;
        self.buffer.drain(..end + length);
        Ok(Some(ProviderRequest {
            model,
            effort,
            headers: forwarded,
            body,
        }))
    }
}

fn policy_fields(body: &[u8]) -> Result<(String, Option<String>), ProtocolError> {
    // serde_json keeps the last duplicate key; a duplicated policy field could
    // then mean different things to the broker and to the upstream parser.
    let object: Map<String, Value> = serde_json::from_slice(body).map_err(|_| invalid())?;
    if object.keys().any(|key| !FIELDS.contains(&key.as_str()))
        || duplicate_top_level_key(body)?
        || object.get("stream") != Some(&Value::Bool(true))
    {
        return Err(invalid());
    }
    let model = object
        .get("model")
        .and_then(Value::as_str)
        .filter(|model| !model.is_empty() && model.len() <= 128)
        .ok_or_else(invalid)?
        .to_owned();
    let effort = match object.get("reasoning") {
        None => None,
        Some(Value::Object(reasoning)) => match reasoning.get("effort") {
            None => None,
            Some(Value::String(effort)) if effort.len() <= 32 => Some(effort.clone()),
            Some(_) => return Err(invalid()),
        },
        Some(_) => return Err(invalid()),
    };
    Ok((model, effort))
}

/// Detects a repeated top-level key without trusting the lossy map above.
fn duplicate_top_level_key(body: &[u8]) -> Result<bool, ProtocolError> {
    struct Keys(bool);
    impl<'de> serde::de::Visitor<'de> for Keys {
        type Value = bool;
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a JSON object")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(mut self, mut map: A) -> Result<bool, A::Error> {
            let mut keys = std::collections::BTreeSet::new();
            while let Some(key) = map.next_key::<String>()? {
                self.0 |= !keys.insert(key);
                map.next_value::<serde::de::IgnoredAny>()?;
            }
            Ok(self.0)
        }
    }
    let mut deserializer = serde_json::Deserializer::from_slice(body);
    serde::Deserializer::deserialize_map(&mut deserializer, Keys(false)).map_err(|_| invalid())
}
