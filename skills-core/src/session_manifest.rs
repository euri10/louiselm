//! Canonical binding of one Session's resolved inputs.
//!
//! This is a record, not an authorization or a measurement API. Trusted callers
//! supply the result of registry measurement, current Generation/view resolution
//! and snapshot capture. Parsing proves structure and identity, never provenance,
//! native-source control, or continued currency of a stored artifact. Raw records
//! must not be promoted to Verified posture. No input content is included, but
//! Agent arguments and environment can be sensitive: do not log this record.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    CanonicalPath, Digest,
    posture::{DimensionName, FailureCode, PROVIDER_DISCLOSURE_NOTICE},
    registry::{AgentRegistration, RuntimeMeasurement},
};

mod resolution;
mod validation;

pub use resolution::InputResolutionError;

/// Canonical Session input record schema.
pub const INPUT_MANIFEST_SCHEMA: &str = "louiselm.session.input-manifest/1";
/// Maximum encoded record size, checked before parsing.
pub const MAX_INPUT_MANIFEST_BYTES: usize = 4 * 1024 * 1024;

/// A measured per-Session file, distinct from an admitted Skill package.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeasuredInput {
    /// Canonical path within this input category's snapshot root.
    pub path: String,
    /// Normalized executable bit.
    pub executable: bool,
    /// Exact byte length.
    pub size: u64,
    /// Content address as bare lowercase SHA-256 hex.
    pub sha256: String,
}

impl MeasuredInput {
    /// Binds bytes already captured by the trusted snapshot owner.
    ///
    /// # Errors
    /// Rejects paths that cannot be represented in a portable snapshot.
    pub fn from_bytes(
        path: &str,
        executable: bool,
        bytes: &[u8],
    ) -> Result<Self, SessionManifestError> {
        CanonicalPath::parse(path, false).map_err(|_| SessionManifestError::Malformed {
            field: "measured_input",
            reason: "invalid snapshot path",
        })?;
        Ok(Self {
            path: path.to_owned(),
            executable,
            size: bytes.len() as u64,
            sha256: Digest::of(bytes).hex().to_owned(),
        })
    }
}

/// Resolved inputs from trusted components. Missing inputs are explicit refusals.
///
/// `Some([])` means an explicitly empty snapshot; `None` means unresolved.
#[derive(Clone, Debug)]
pub struct SessionInputs {
    /// Exact registered Agent configuration.
    pub agent: Option<AgentRegistration>,
    /// Measurement returned by the registered runtime.
    pub runtime: Option<RuntimeMeasurement>,
    /// Resolved current witnessed Generation identity.
    pub skill_generation_id: Option<String>,
    /// Resolved materialized view identity.
    pub view_digest: Option<String>,
    /// Complete per-Session project-instruction snapshot.
    pub project_instructions: Option<Vec<MeasuredInput>>,
    /// Complete measured tool-schema snapshot.
    pub tool_schemas: Option<Vec<MeasuredInput>>,
    /// Complete measured plugin-schema snapshot.
    pub plugin_schemas: Option<Vec<MeasuredInput>>,
    /// Measured immutable cache base; an empty cache still has an explicit digest.
    pub cache_base_digest: Option<String>,
    /// Governing supply policy identity.
    pub policy_digest: Option<String>,
    /// Trusted isolation evidence reference.
    pub isolation_receipt: Option<String>,
    /// Exact capability envelope identity.
    pub envelope_id: Option<String>,
    /// Exact capability envelope revision.
    pub envelope_revision: Option<u64>,
    /// Must be explicitly empty in verified v1.
    pub acp_mcp_servers: Option<Vec<String>>,
}

/// The complete approved supply and this Agent's view of it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillInputs {
    /// Witnessed Generation content address.
    pub generation_digest: String,
    /// Materialized Instruction view content address.
    pub view_digest: String,
}

/// Exact capability envelope revision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvelopeInput {
    /// Registered envelope identifier.
    pub id: String,
    /// Revision bound by this Session.
    pub revision: u64,
}

/// Fixed disclosure, independently bound from managed Skill supply.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderDisclosure {
    /// All reachable access/quota services, sorted and unique.
    pub providers: Vec<String>,
    /// Cloud plaintext visibility notice.
    pub notice: String,
}

/// Closed versioned record. Its digest names inputs, never grants authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionInputManifest {
    /// Wire schema.
    pub schema: String,
    /// Exact Agent registration, preserving argument and route order.
    pub agent: AgentRegistration,
    /// Measured runtime, with adapters and library baseline sorted.
    pub runtime: RuntimeMeasurement,
    /// Approved supply binding.
    pub skill_generation: SkillInputs,
    /// Project instructions, never hardware-admitted packages.
    pub project_instructions: Vec<MeasuredInput>,
    /// Measured tool schemas.
    pub tool_schemas: Vec<MeasuredInput>,
    /// Measured plugin schemas.
    pub plugin_schemas: Vec<MeasuredInput>,
    /// Exact immutable cache base used to seed the private Session overlay.
    pub cache_base_digest: String,
    /// Governing policy identity.
    pub policy_digest: String,
    /// Isolation evidence reference.
    pub isolation_receipt: String,
    /// Capability envelope identity.
    pub envelope: EnvelopeInput,
    /// Explicitly empty ACP MCP state.
    pub acp_mcp_servers: Vec<String>,
    /// Reachable services and fixed disclosure statement.
    pub provider_disclosure: ProviderDisclosure,
}

/// Stable refusals without payload or configuration contents.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum SessionManifestError {
    /// Required component supplied no input.
    #[error("missing Session input: {field}")]
    Missing {
        /// Absent field.
        field: &'static str,
    },
    /// Runtime measurement is absent or contradictory.
    #[error("runtime: {reason}")]
    UnmeasuredRuntime {
        /// Fixed refusal reason.
        reason: &'static str,
    },
    /// Current witnessed supply was not resolved.
    #[error("managed_supply: {reason}")]
    UnresolvedGeneration {
        /// Fixed refusal reason.
        reason: &'static str,
    },
    /// Materialized view was not resolved.
    #[error("managed_supply: {reason}")]
    UnresolvedView {
        /// Fixed refusal reason.
        reason: &'static str,
    },
    /// A field violates the canonical contract.
    #[error("invalid Session input {field}: {reason}")]
    Malformed {
        /// Invalid field.
        field: &'static str,
        /// Fixed reason without input data.
        reason: &'static str,
    },
    /// A set contains a repeated or colliding entry.
    #[error("duplicate Session input: {field}")]
    Duplicate {
        /// Invalid collection.
        field: &'static str,
    },
    /// Verified v1 has no MCP support.
    #[error("native_supply: verified v1 requires an empty ACP MCP list")]
    NonEmptyMcp {},
    /// Unknown record version.
    #[error("unsupported Session input manifest schema")]
    UnsupportedSchema(String),
    /// Invalid closed JSON; details deliberately omit potentially sensitive text.
    #[error("malformed Session input manifest JSON")]
    MalformedJson(()),
    /// Alternate encodings are not accepted.
    #[error("Session input manifest is not canonical")]
    NonCanonical,
    /// Input exceeds the fixed boundary.
    #[error("Session input manifest exceeds the size limit")]
    TooLarge,
}

impl SessionManifestError {
    /// Independent posture dimension affected, when this is a supply refusal.
    #[must_use]
    pub fn dimension(&self) -> Option<DimensionName> {
        match self {
            Self::UnmeasuredRuntime { .. } => Some(DimensionName::Runtime),
            Self::UnresolvedGeneration { .. } | Self::UnresolvedView { .. } => {
                Some(DimensionName::ManagedSupply)
            }
            Self::NonEmptyMcp {} => Some(DimensionName::NativeSupply),
            Self::Missing { field } | Self::Malformed { field, .. } | Self::Duplicate { field } => {
                if field.starts_with("runtime") {
                    Some(DimensionName::Runtime)
                } else {
                    match *field {
                        "skill_generation_id" | "view_digest" | "policy_digest" => {
                            Some(DimensionName::ManagedSupply)
                        }
                        "project_instructions"
                        | "tool_schemas"
                        | "plugin_schemas"
                        | "acp_mcp_servers" => Some(DimensionName::NativeSupply),
                        "provider_disclosure" => Some(DimensionName::ProviderDisclosure),
                        _ => None,
                    }
                }
            }
            _ => None,
        }
    }

    /// Stable code suitable for independent posture reporting.
    #[must_use]
    pub fn failure_code(&self) -> Option<FailureCode> {
        match self {
            Self::NonEmptyMcp {} => Some(FailureCode::NativeSupplyUncertain),
            Self::Malformed { .. } | Self::Duplicate { .. } => {
                self.dimension().map(|dimension| match dimension {
                    DimensionName::Runtime => FailureCode::RuntimeDrift,
                    DimensionName::ManagedSupply => FailureCode::RootTrustFailed,
                    DimensionName::NativeSupply => FailureCode::NativeSupplyUncertain,
                    DimensionName::ProviderDisclosure => FailureCode::ProviderDisclosureMissing,
                    _ => FailureCode::UnknownFailure,
                })
            }
            _ => self.dimension().map(|_| FailureCode::EvidenceMissing),
        }
    }
}

impl SessionInputManifest {
    /// Binds resolved inputs, sorting unordered file sets only.
    ///
    /// # Errors
    /// Refuses absent, malformed, contradictory, duplicate, or oversized inputs
    /// and nonempty ACP MCP state. Does not verify the caller's provenance.
    pub fn build(inputs: SessionInputs) -> Result<Self, SessionManifestError> {
        let agent = required(inputs.agent, "agent")?;
        let runtime = inputs
            .runtime
            .ok_or(SessionManifestError::UnmeasuredRuntime {
                reason: "no measured runtime is bound",
            })?;
        let generation_digest =
            inputs
                .skill_generation_id
                .ok_or(SessionManifestError::UnresolvedGeneration {
                    reason: "no Skill Generation is bound",
                })?;
        let view_digest = inputs
            .view_digest
            .ok_or(SessionManifestError::UnresolvedView {
                reason: "no materialized Instruction view is bound",
            })?;
        let provider_disclosure = ProviderDisclosure {
            providers: agent
                .reachable_providers()
                .into_iter()
                .map(str::to_owned)
                .collect(),
            notice: PROVIDER_DISCLOSURE_NOTICE.to_owned(),
        };
        let mut manifest = Self {
            schema: INPUT_MANIFEST_SCHEMA.to_owned(),
            agent,
            runtime,
            skill_generation: SkillInputs {
                generation_digest,
                view_digest,
            },
            project_instructions: required(inputs.project_instructions, "project_instructions")?,
            tool_schemas: required(inputs.tool_schemas, "tool_schemas")?,
            plugin_schemas: required(inputs.plugin_schemas, "plugin_schemas")?,
            cache_base_digest: required(inputs.cache_base_digest, "cache_base_digest")?,
            policy_digest: required(inputs.policy_digest, "policy_digest")?,
            isolation_receipt: required(inputs.isolation_receipt, "isolation_receipt")?,
            envelope: EnvelopeInput {
                id: required(inputs.envelope_id, "envelope_id")?,
                revision: required(inputs.envelope_revision, "envelope_revision")?,
            },
            acp_mcp_servers: required(inputs.acp_mcp_servers, "acp_mcp_servers")?,
            provider_disclosure,
        };
        manifest.normalize();
        manifest.validate()?;
        Ok(manifest)
    }

    fn normalize(&mut self) {
        for entries in [
            &mut self.project_instructions,
            &mut self.tool_schemas,
            &mut self.plugin_schemas,
        ] {
            entries.sort_by(|a, b| a.path.cmp(&b.path));
        }
        self.runtime.adapters.sort_by(|a, b| a.path.cmp(&b.path));
        self.runtime.library_baseline.sort();
    }

    /// Serializes in fixed field order, without host metadata or a trailing newline.
    ///
    /// # Panics
    /// Only if a future schema introduces a fallible serializer. All current
    /// fields are JSON-native values and string-keyed maps.
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "Closed derived schema has only JSON-native values and string-keyed maps."
    )]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("Session input manifest is serializable")
    }

    /// Identity accepted by the existing launch request digest contract.
    #[must_use]
    pub fn digest(&self) -> Digest {
        Digest::of(&self.canonical_bytes())
    }

    /// Reads exact canonical bytes. Successful parsing is not trusted evidence.
    ///
    /// # Errors
    /// Rejects oversized, unknown, malformed, duplicate, noncanonical or
    /// semantically invalid records, including nested unknown fields.
    pub fn parse(bytes: &[u8]) -> Result<Self, SessionManifestError> {
        if bytes.len() > MAX_INPUT_MANIFEST_BYTES {
            return Err(SessionManifestError::TooLarge);
        }
        let mut manifest: Self =
            serde_json::from_slice(bytes).map_err(|_| SessionManifestError::MalformedJson(()))?;
        manifest.normalize();
        manifest.validate()?;
        if manifest.canonical_bytes() != bytes {
            return Err(SessionManifestError::NonCanonical);
        }
        Ok(manifest)
    }
}

fn required<T>(value: Option<T>, field: &'static str) -> Result<T, SessionManifestError> {
    value.ok_or(SessionManifestError::Missing { field })
}
