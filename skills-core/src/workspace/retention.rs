//! Durable, payload-free workspace evidence and broker-owned retention policy.

pub(crate) mod cleanup;
mod storage;
pub use cleanup::CleanupReport;
pub(crate) use storage::Store;

/// Removes only proven disposed, sealed, expired and unpinned installed storage.
/// Fixed roots only; call from the installed root launcher. Blocks for a bounded
/// pass and keeps a durable unavailable marker before the first unlink.
/// # Errors
/// Refuses wrong authority, unsafe roots, busy policy or unavailable state.
/// Per-Session failures are reported in `failed` and must produce a failing exit.
pub fn cleanup_expired(
    broker_identity: (u32, u32),
    now_ms: u64,
) -> Result<CleanupReport, super::WorkspaceError> {
    cleanup::system(broker_identity, now_ms)
}

use super::WorkspaceError;
use crate::workspace::provenance::OutputProvenance;
use crate::{Digest, launch::LaunchRequest, session_manifest::SessionInputManifest};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Default unpinned expiry measured from launch authorization, never renewed on restart.
pub const DEFAULT_RETENTION_MS: u64 = 7 * 24 * 60 * 60 * 1000;
const SCHEMA: &str = "louiselm.workspace.retention/1";
const MAX_RECORD_BYTES: usize = 64 * 1024;

/// Availability claims deliberately distinguish durable pointers from source bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrimaryEvidence {
    /// No deletion was recorded; existence/readability has not been checked.
    NotChecked,
    /// Deletion started or failed; some or all primary bytes may be unavailable.
    CleanupIncomplete,
    /// The recorded Session tree was removed; this is not a secure-erasure claim.
    Removed,
}

/// Exact launch input references, without configuration, environment or source payloads.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputReferences {
    /// Canonical Session input manifest digest.
    pub manifest: String,
    /// Selected source snapshot digest.
    pub snapshot: String,
    /// Normalized source inventory digest.
    pub base: String,
    /// Immutable cache base digest.
    pub cache: String,
    /// Exact Skill Generation digest.
    pub generation: String,
    /// Digest of the full runtime measurement, including adapters.
    pub runtime: String,
    /// Declared isolation evidence reference, not an assertion of verification.
    pub isolation: String,
}

impl InputReferences {
    pub(crate) fn from_manifest(manifest: &SessionInputManifest) -> Result<Self, WorkspaceError> {
        Ok(Self {
            manifest: manifest.digest().to_string(),
            snapshot: manifest.source_snapshot_digest.clone(),
            base: manifest.source_base_digest.clone(),
            cache: manifest.cache_base_digest.clone(),
            generation: manifest.skill_generation.generation_digest.clone(),
            runtime: Digest::of(&serde_json::to_vec(&manifest.runtime)?).to_string(),
            isolation: manifest.isolation_receipt.clone(),
        })
    }

    fn validate(&self, launch: &LaunchRequest) -> Result<(), WorkspaceError> {
        for value in [
            &self.manifest,
            &self.snapshot,
            &self.base,
            &self.cache,
            &self.generation,
            &self.runtime,
        ] {
            Digest::parse(value).map_err(|_| invalid())?;
        }
        if self.manifest != launch.session_input_manifest_id
            || self.generation != launch.skill_generation_id
            || self.isolation.is_empty()
            || self.isolation.len() > 256
        {
            return Err(invalid());
        }
        Ok(())
    }
}

/// Durable pointers remain historical evidence after primary storage expires.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceReferences {
    /// Present once exact staged launch inputs were retained by the broker.
    pub inputs: Option<InputReferences>,
    /// Initial signed launch/start receipt digests retained by the broker.
    pub launch_receipts: BTreeSet<String>,
    /// Actual Agent/tool isolation measurement from the signed Start receipt.
    pub isolation: BTreeSet<String>,
    /// Authenticated export evidence digests.
    pub exports: BTreeSet<String>,
    /// Exact exported byte-bundle digests.
    pub bundles: BTreeSet<String>,
    /// Complete verification-record digests; quarantine can invalidate applicability.
    pub verifications: BTreeSet<String>,
    /// Exact promotion-request digests, not claims that every effect completed.
    pub promotions: BTreeSet<String>,
}

/// Broker policy and durable evidence for exactly one launch; never a live grant.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetentionRecord {
    /// Versioned closed schema.
    pub schema: String,
    /// Exact launch identity and input/Generation references.
    pub launch: LaunchRequest,
    /// Initial policy clock reading, preserved on retries and restart.
    pub recorded_at_ms: u64,
    /// Exclusive absolute retention deadline; expiry alone never proves Disposal.
    pub expires_at_ms: u64,
    /// Explicit operator retention until unpinned; grants no execution authority.
    pub pinned: bool,
    /// Conservative primary-evidence availability, independent of pointer durability.
    pub primary_evidence: PrimaryEvidence,
    /// Payload-free exact input and subsequent work references.
    pub evidence: EvidenceReferences,
}

/// Operator view keeps historical pointers separate from current quarantine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetentionInspection {
    /// Durable retention policy and historical evidence references.
    pub record: RetentionRecord,
    /// Current broker quarantine; retained evidence does not clear this state.
    pub quarantined: bool,
    /// Current output provenance, recomputed from canonical broker state.
    pub output_provenance: OutputProvenance,
}

impl RetentionRecord {
    pub(crate) fn validate(&self) -> Result<(), WorkspaceError> {
        self.launch.validate().map_err(|_| invalid())?;
        if self.schema != SCHEMA || self.expires_at_ms <= self.recorded_at_ms {
            return Err(invalid());
        }
        if let Some(inputs) = &self.evidence.inputs {
            inputs.validate(&self.launch)?;
        }
        for set in [
            &self.evidence.launch_receipts,
            &self.evidence.isolation,
            &self.evidence.exports,
            &self.evidence.bundles,
            &self.evidence.verifications,
            &self.evidence.promotions,
        ] {
            if set.len() > 128 {
                return Err(invalid());
            }
            for digest in set {
                Digest::parse(digest).map_err(|_| invalid())?;
            }
        }
        Ok(())
    }
}

fn invalid() -> WorkspaceError {
    WorkspaceError::Invalid("retention evidence is invalid or unavailable")
}

#[cfg(test)]
mod tests;
