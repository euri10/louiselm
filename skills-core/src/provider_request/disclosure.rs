//! Reviewed metadata shape observed offline from Codex 0.156.1 on 2026-09-25.
//! See `scripts/probe-codex-request-shape.py` and louiselm-qbr.11.1. Rules are
//! consumed from the exact bytes whose digest the Provider permission binds.

use crate::{
    Digest,
    launch_protocol::{ErrorCode, ProtocolError},
};
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

const PROFILE: &[u8] = include_bytes!("disclosure.json");

/// Safe disclosure text: no metadata values, payloads or anonymity promise.
pub const NOTICE: &str = "Reviewed client metadata is forwarded unchanged to the approved Provider, including stable installation and Session/thread/turn identifiers and client runtime settings. Stable identifiers permit cross-Run linkage. This is not anonymity.";

/// Exact consumed schema identity; changing any profile bytes invalidates approval.
#[must_use]
pub fn profile_digest() -> String {
    Digest::of(PROFILE).to_string()
}

/// Safe version label from the consumed profile, for existing permission views.
/// # Errors
/// Refuses an invalid embedded profile rather than guessing a display identity.
pub fn profile_id() -> Result<String, ProtocolError> {
    Ok(profile()?.id)
}

/// Safe permission presentation for one exact supported disclosure profile.
/// # Errors
/// Refuses absent, unsupported or changed profile identities; never echoes them.
pub fn notice(digest: &str) -> Result<String, ProtocolError> {
    if digest != profile_digest() {
        return Err(denied());
    }
    Ok(format!(
        "Metadata profile {} ({digest}). {NOTICE}",
        profile_id()?
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Profile {
    id: String,
    max_string_bytes: usize,
    max_turn_bytes: usize,
    prompt_cache_key: Kind,
    headers: BTreeMap<String, Kind>,
    client_metadata: BTreeMap<String, Kind>,
    turn_metadata: BTreeMap<String, Kind>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Kind {
    String,
    Boolean,
    Unsigned,
    Turn,
}

fn profile() -> Result<Profile, ProtocolError> {
    serde_json::from_slice(PROFILE).map_err(|_| denied())
}

pub(super) fn denied() -> ProtocolError {
    ProtocolError::new(ErrorCode::ProviderDisclosureDenied, None, None)
}

#[derive(Default, Deserialize)]
struct Object(#[serde(deserialize_with = "unique_fields")] BTreeMap<String, Value>);

fn unique_fields<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<String, Value>, D::Error> {
    struct Visitor;
    impl<'de> serde::de::Visitor<'de> for Visitor {
        type Value = BTreeMap<String, Value>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("metadata object with unique fields")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut map: A,
        ) -> Result<Self::Value, A::Error> {
            let mut fields = BTreeMap::new();
            while let Some((key, value)) = map.next_entry::<String, Value>()? {
                if fields.insert(key, value).is_some() {
                    return Err(serde::de::Error::custom("duplicate metadata field"));
                }
            }
            Ok(fields)
        }
    }
    deserializer.deserialize_map(Visitor)
}

#[derive(Deserialize)]
struct Body {
    #[serde(default)]
    client_metadata: Object,
    #[serde(default, deserialize_with = "present_value")]
    prompt_cache_key: Option<Value>,
}

// Preserve explicit null for validation instead of treating it as omission.
fn present_value<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}

pub(super) fn validate(body: &[u8], headers: &[(String, String)]) -> Result<(), ProtocolError> {
    let profile = profile()?;
    let mut seen = BTreeSet::new();
    for (name, value) in headers {
        if !seen.insert(name) {
            return Err(denied());
        }
        let kind = profile.headers.get(name).ok_or_else(denied)?;
        profile.value(kind, &Value::String(value.clone()))?;
    }
    let body: Body = serde_json::from_slice(body).map_err(|_| denied())?;
    if let Some(value) = &body.prompt_cache_key {
        profile.value(&profile.prompt_cache_key, value)?;
    }
    profile.object(&profile.client_metadata, &body.client_metadata.0)
}

impl Profile {
    fn object(
        &self,
        rules: &BTreeMap<String, Kind>,
        fields: &BTreeMap<String, Value>,
    ) -> Result<(), ProtocolError> {
        for (name, value) in fields {
            self.value(rules.get(name).ok_or_else(denied)?, value)?;
        }
        Ok(())
    }

    fn value(&self, kind: &Kind, value: &Value) -> Result<(), ProtocolError> {
        let valid = match kind {
            Kind::String => value.as_str().is_some_and(|value| {
                !value.is_empty()
                    && value.len() <= self.max_string_bytes
                    && !value.chars().any(char::is_control)
            }),
            Kind::Boolean => value.is_boolean(),
            Kind::Unsigned => value.as_u64().is_some(),
            Kind::Turn => {
                let bytes = value
                    .as_str()
                    .filter(|value| value.len() <= self.max_turn_bytes)
                    .ok_or_else(denied)?;
                let fields: Object = serde_json::from_str(bytes).map_err(|_| denied())?;
                // Turn rules contain only leaf kinds: no recursive metadata grammar.
                if self
                    .turn_metadata
                    .values()
                    .any(|kind| matches!(kind, Kind::Turn))
                {
                    return Err(denied());
                }
                self.object(&self.turn_metadata, &fields.0)?;
                true
            }
        };
        if valid { Ok(()) } else { Err(denied()) }
    }
}
