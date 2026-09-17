//! Closed dependency grants, requests, results and bounded cache ingress frames.

use serde::{Deserialize, Serialize};

use super::{
    Artifact, Candidate, canonical_digest, identifier, invalid,
    transport::{HttpsTransport, RegistryEndpoint},
};
use crate::{CanonicalPath, launch_protocol::ProtocolError, launch_receipt::ReceiptHead};

/// Largest raw chunk within the existing 64-KiB authenticated command packet.
pub const CACHE_CHUNK_BYTES: usize = 8192;

/// Exact pre-start dependency scope. Absence from a launch grants no fetch authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovedDependencies {
    /// Exact immutable staged Session input manifest.
    pub input_manifest_digest: String,
    /// Selected Cargo.lock path inside that starting source snapshot.
    pub lockfile_path: String,
    /// Exact starting lockfile bytes, checked independently by the broker.
    pub lockfile_digest: String,
    /// Registry archive endpoints approved before start; no remote index is consulted.
    pub registries: Vec<RegistryEndpoint>,
    /// Exact explicit exceptions approved before start, including unattended scope.
    pub preapproved: Vec<Candidate>,
    /// Non-refundable maximum fetch attempts.
    pub max_fetches: u32,
    /// Non-refundable aggregate byte reservations.
    pub max_bytes: u64,
    /// Exclusive absolute expiry; approvals and retries never extend it.
    pub expires_at_ms: u64,
}

impl ApprovedDependencies {
    /// Validates bounded scope without reading a file or contacting a registry.
    /// # Errors
    /// Refuses malformed bindings, excessive grants, invalid endpoints or expired scope.
    pub fn validate(&self, now_ms: u64) -> Result<(), ProtocolError> {
        CanonicalPath::parse(&self.lockfile_path, false).map_err(|_| invalid())?;
        if !canonical_digest(&self.input_manifest_digest)
            || !canonical_digest(&self.lockfile_digest)
            || self.preapproved.len() > 32
            || self.max_fetches == 0
            || self.max_fetches > 4096
            || self.max_bytes == 0
            || self.max_bytes > 1024 * 1024 * 1024
            || now_ms >= self.expires_at_ms
            || std::path::Path::new(&self.lockfile_path)
                .file_name()
                .is_none_or(|name| name != "Cargo.lock")
        {
            return Err(invalid());
        }
        HttpsTransport::new(self.registries.clone(), std::time::Duration::from_secs(30))
            .map_err(|_| invalid())?;
        let mut ids = std::collections::BTreeSet::new();
        for candidate in &self.preapproved {
            if !ids.insert(candidate.id()?) {
                return Err(invalid());
            }
        }
        if serde_json::to_vec(self).map_err(|_| invalid())?.len() > 32 * 1024 {
            return Err(invalid());
        }
        Ok(())
    }
}

/// An Agent's proposal and bounded fetch request. There is no approval or output path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DependencyRequest {
    /// Stable retry identity; changed content needs a new identity.
    pub request_id: String,
    /// Exact typed coordinate and integrity.
    pub candidate: Candidate,
    /// Maximum bytes this attempt may consume from the approved aggregate scope.
    pub max_bytes: u64,
}

impl DependencyRequest {
    /// Checks the closed proposal before any side effect.
    /// # Errors
    /// Refuses invalid identities, unbounded reservations or malformed coordinates.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        self.candidate.validate()?;
        if !identifier(&self.request_id)
            || self.max_bytes == 0
            || self.max_bytes > crate::cache::MAX_BYTES as u64
        {
            return Err(invalid());
        }
        Ok(())
    }
}

/// Durable dependency request outcome. Uncertain fetches never retry automatically.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DependencyStatus {
    /// A typed local candidate awaits exact operator batch approval.
    Pending {
        /// Stable digest of the complete proposal.
        candidate_id: String,
    },
    /// Exact opaque archive is visible in the requesting Session's cache.
    Complete {
        /// Published content address, size and integrity assurance.
        artifact: Artifact,
    },
    /// The requested effect is outside unattended or current capability scope.
    Denied,
    /// An attempt was durably spent, but no successful publication was confirmed.
    Unknown,
}

impl DependencyStatus {
    /// Checks canonical identifiers and published-artifact claims.
    /// # Errors
    /// Rejects malformed candidate IDs or contradictory artifact fields.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        match self {
            Self::Pending { candidate_id } if !canonical_digest(candidate_id) => Err(invalid()),
            Self::Complete { artifact } => artifact.validate(),
            _ => Ok(()),
        }
    }
}

/// One broker-to-supervisor fragment. It conveys no executable command or file path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CacheChunk {
    /// Exact approved candidate being delivered to its original requester.
    pub candidate_id: String,
    /// Digest of the complete opaque archive.
    pub artifact_digest: String,
    /// Total bounded byte count.
    pub total_size: u64,
    /// Byte offset; the supervisor accepts only the next contiguous chunk.
    pub offset: u64,
    /// Opaque bytes; no archive names are interpreted.
    pub bytes: Vec<u8>,
    /// Exact pre-download lifecycle head. Park/Resume cannot revive a stale transfer.
    pub head: ReceiptHead,
    /// Original approved expiry, checked again before publication.
    pub expires_at_ms: u64,
}

impl CacheChunk {
    /// Checks individual chunk bounds and canonical digests before allocation.
    /// # Errors
    /// Refuses malformed identifiers, oversized chunks or impossible offsets.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if !canonical_digest(&self.candidate_id)
            || !canonical_digest(&self.artifact_digest)
            || !canonical_digest(&self.head.digest)
            || self.total_size > crate::cache::MAX_BYTES as u64
            || self.bytes.len() > CACHE_CHUNK_BYTES
            || self.bytes.is_empty()
            || self
                .offset
                .checked_add(self.bytes.len() as u64)
                .is_none_or(|end| end > self.total_size)
        {
            return Err(invalid());
        }
        Ok(())
    }
}
