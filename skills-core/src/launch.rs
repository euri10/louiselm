//! The closed launch request and its resolution into a confinement plan.
//!
//! A request is a set of identifiers and nothing else. Everything that
//! decides what actually runs — the command, its arguments, its runtime, its
//! network policy — is looked up from registries the caller cannot write; a
//! caller who can name a command can run one, so the request is never
//! allowed to carry one. `deny_unknown_fields` on the request means a
//! caller who tries to smuggle in an extra `command` or `mount` field fails
//! to deserialize at all rather than having the extra field silently
//! ignored.
//!
//! Identifiers that end up inside a filesystem path (`session_id`) are
//! restricted to a safe charset before they touch a `Path::join`: a request
//! is untrusted input, and validating it here, once, at the boundary, is
//! what lets everything downstream — [`crate::sandbox::Cgroup::create`],
//! the Session's home and workspace — treat the identifier as already safe.

use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    canonical::{Digest, DigestError},
    registry::{Registry, RegistryError, RuntimeMeasurement},
    sandbox::{Channel, ConfinementPlan, IdentityPlan, default_system_roots},
};

/// The launch request schema this build reads.
pub const REQUEST_SCHEMA: &str = "louiselm.launch.request/2";

/// Protocol version carried by every launch request and control message.
pub const PROTOCOL_VERSION: u32 = 1;

/// Longest accepted encoded launch request.
pub const MAX_REQUEST_BYTES: usize = 64 * 1024;

/// The longest an identifier field may be.
///
/// Generous enough for any UUID or descriptive name a registry would use;
/// tight enough that a request cannot be used to smuggle in a large opaque
/// blob under the guise of an identifier.
const MAX_IDENTIFIER_LEN: usize = 128;

/// A closed request for one verified Session.
///
/// Every field is an identifier into something already registered or
/// otherwise fixed. There is no caller-selected command, environment,
/// mount, path, host identity, backend flag, or systemd property.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchRequest {
    /// Schema identifier.
    pub schema: String,
    /// Protocol version.
    pub protocol_version: u32,
    /// Idempotency key for this exact launch request.
    pub request_id: String,
    /// Single-use authorization the Control broker must consume.
    pub authorization_id: String,
    /// Identifies the Session being launched.
    pub session_id: String,
    /// The Run this Session belongs to.
    pub run_id: String,
    /// The registered Agent to launch.
    pub agent_id: String,
    /// The registered capability envelope the Session runs under.
    pub envelope_id: String,
    /// Exact capability-envelope revision authorized for this Session.
    pub envelope_revision: u64,
    /// Digest of the Skill Generation this Session is bound to.
    ///
    /// Whether that Generation is actually admitted and current is a Skill
    /// Admission concern, out of scope for this crate's launcher: resolving
    /// a request only checks that the identifier is a well-formed digest.
    pub skill_generation_id: String,
    /// Digest of the Session-input manifest this Session is bound to.
    ///
    /// Same boundary as `skill_generation_id`: this crate validates the
    /// shape, not the manifest's contents or provenance.
    pub session_input_manifest_id: String,
}

/// What resolving a [`LaunchRequest`] actually measured and decided.
#[derive(Clone, Debug)]
pub struct Resolution {
    /// The confinement plan a backend can spawn.
    pub plan: ConfinementPlan,
    /// The runtime measurement resolution took, bound into the plan.
    ///
    /// Returned rather than discarded: a caller building a launch receipt
    /// needs exactly this measurement, and re-measuring later would open a
    /// gap between what was checked and what the receipt describes.
    pub runtime: RuntimeMeasurement,
}

/// A request that could not be resolved.
#[derive(Debug, Error)]
pub enum LaunchError {
    /// Encoded request exceeded the fixed input boundary.
    #[error("launch request exceeds {max} bytes")]
    RequestTooLarge {
        /// Fixed maximum this build accepts.
        max: usize,
    },
    /// Bytes were not one closed launch request.
    #[error("launch request is malformed")]
    MalformedRequest,
    /// Bytes decoded, but were not the request's exact canonical encoding.
    #[error("launch request is not canonically encoded")]
    NonCanonical,
    /// The request answers a different schema.
    #[error("request answers schema '{found}', not '{expected}'")]
    Schema {
        /// Schema the request carries.
        found: String,
        /// Schema this build reads.
        expected: &'static str,
    },
    /// The request answers a protocol version this build does not implement.
    #[error("request uses protocol version {found}, not {expected}")]
    ProtocolVersion {
        /// Version the request carries.
        found: u32,
        /// Version this build reads.
        expected: u32,
    },
    /// An identifier field is not well-formed.
    #[error("'{field}' is not a valid identifier: {reason}")]
    MalformedIdentifier {
        /// The field that failed.
        field: &'static str,
        /// Why.
        reason: String,
    },
    /// A referenced registry entry does not exist or does not check out.
    #[error(transparent)]
    Registry(#[from] RegistryError),
}

impl LaunchRequest {
    /// Serializes this request to its deterministic wire bytes.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("a launch request is always serializable")
    }

    /// Returns the content address bound by its single-use authorization.
    #[must_use]
    pub fn digest(&self) -> Digest {
        Digest::of(&self.canonical_bytes())
    }

    /// Validates the closed request without consulting launch authority.
    pub fn validate(&self) -> Result<(), LaunchError> {
        if self.canonical_bytes().len() > MAX_REQUEST_BYTES {
            return Err(LaunchError::RequestTooLarge {
                max: MAX_REQUEST_BYTES,
            });
        }
        if self.schema != REQUEST_SCHEMA {
            return Err(LaunchError::Schema {
                found: self.schema.clone(),
                expected: REQUEST_SCHEMA,
            });
        }
        if self.protocol_version != PROTOCOL_VERSION {
            return Err(LaunchError::ProtocolVersion {
                found: self.protocol_version,
                expected: PROTOCOL_VERSION,
            });
        }
        validate_identifier("request_id", &self.request_id)?;
        validate_identifier("authorization_id", &self.authorization_id)?;
        validate_identifier("session_id", &self.session_id)?;
        validate_identifier("run_id", &self.run_id)?;
        validate_identifier("agent_id", &self.agent_id)?;
        validate_identifier("envelope_id", &self.envelope_id)?;
        validate_digest("skill_generation_id", &self.skill_generation_id)?;
        validate_digest("session_input_manifest_id", &self.session_input_manifest_id)?;
        Ok(())
    }

    /// Parses one bounded exact canonical launch request.
    pub fn parse_canonical(bytes: &[u8]) -> Result<Self, LaunchError> {
        if bytes.len() > MAX_REQUEST_BYTES {
            return Err(LaunchError::RequestTooLarge {
                max: MAX_REQUEST_BYTES,
            });
        }
        let request: Self =
            serde_json::from_slice(bytes).map_err(|_| LaunchError::MalformedRequest)?;
        if request.canonical_bytes() != bytes {
            return Err(LaunchError::NonCanonical);
        }
        request.validate()?;
        Ok(request)
    }
}

/// Resolves `request` into a plan a [`crate::sandbox::Backend`] can spawn.
///
/// `sessions_root` is fixed by the launcher, never by the request: it is
/// where every Session's private home and workspace live, joined with the
/// request's own validated `session_id`. `identity` is likewise the
/// launcher's decision, not the request's — assigning a distinct host uid is
/// a privilege and allocation policy this crate does not own.
pub fn resolve(
    request: &LaunchRequest,
    registry: &Registry,
    sessions_root: &Path,
    identity: IdentityPlan,
) -> Result<Resolution, LaunchError> {
    request.validate()?;

    let agent = registry.agent(&request.agent_id)?;
    let runtime = registry.runtime(&agent.runtime_id)?;
    let measurement = runtime.measure()?;
    let envelope = registry.envelope(&request.envelope_id)?;

    let session_root = sessions_root.join(&request.session_id);
    let plan = ConfinementPlan {
        session_id: request.session_id.clone(),
        runtime_root: runtime.root.clone(),
        executable: runtime.executable_path(),
        arguments: agent.arguments.clone(),
        environment: agent.environment.clone(),
        home: session_root.join("home"),
        workspace: session_root.join("workspace"),
        system_roots: default_system_roots(),
        network: envelope.network,
        identity,
        channels: vec![Channel::AcpStdio {
            id: "acp".to_owned(),
        }],
    };
    Ok(Resolution {
        plan,
        runtime: measurement,
    })
}

/// Validates a non-digest identifier: ASCII alphanumeric, `-`, or `_` only.
///
/// This is the whole reason `session_id` can be joined onto a directory root
/// without also checking the result stays inside it: a value drawn from this
/// charset cannot spell `/`, `..`, or a NUL, so there is nothing for a
/// traversal check to catch that the charset does not already forbid.
fn validate_identifier(field: &'static str, value: &str) -> Result<(), LaunchError> {
    if value.is_empty() {
        return Err(LaunchError::MalformedIdentifier {
            field,
            reason: "must not be empty".to_owned(),
        });
    }
    if value.len() > MAX_IDENTIFIER_LEN {
        return Err(LaunchError::MalformedIdentifier {
            field,
            reason: format!("must be at most {MAX_IDENTIFIER_LEN} bytes"),
        });
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(LaunchError::MalformedIdentifier {
            field,
            reason: "must be ASCII alphanumeric, '-', or '_'".to_owned(),
        });
    }
    Ok(())
}

fn validate_digest(field: &'static str, value: &str) -> Result<Digest, LaunchError> {
    let digest =
        Digest::parse(value).map_err(|error: DigestError| LaunchError::MalformedIdentifier {
            field,
            reason: error.to_string(),
        })?;
    if digest.to_string() != value {
        return Err(LaunchError::MalformedIdentifier {
            field,
            reason: "must use canonical sha256:<lowercase hex> spelling".to_owned(),
        });
    }
    Ok(digest)
}
