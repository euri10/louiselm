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
pub const REQUEST_SCHEMA: &str = "louiselm.launch.request/1";

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
    /// Identifies the Session being launched.
    pub session_id: String,
    /// The Run this Session belongs to.
    pub run_id: String,
    /// The registered Agent to launch.
    pub agent_id: String,
    /// The registered capability envelope the Session runs under.
    pub envelope_id: String,
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
    /// The request answers a different schema.
    #[error("request answers schema '{found}', not '{expected}'")]
    Schema {
        /// Schema the request carries.
        found: String,
        /// Schema this build reads.
        expected: &'static str,
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
    if request.schema != REQUEST_SCHEMA {
        return Err(LaunchError::Schema {
            found: request.schema.clone(),
            expected: REQUEST_SCHEMA,
        });
    }
    validate_identifier("session_id", &request.session_id)?;
    validate_identifier("run_id", &request.run_id)?;
    validate_identifier("agent_id", &request.agent_id)?;
    validate_identifier("envelope_id", &request.envelope_id)?;
    validate_digest("skill_generation_id", &request.skill_generation_id)?;
    validate_digest(
        "session_input_manifest_id",
        &request.session_input_manifest_id,
    )?;

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
    Digest::parse(value).map_err(|error: DigestError| LaunchError::MalformedIdentifier {
        field,
        reason: error.to_string(),
    })
}
