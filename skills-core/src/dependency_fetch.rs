//! Exact dependency candidates and immutable, pre-start fetch policy.
//!
//! Policy evaluation performs no I/O. Construction and batch approval belong to
//! the trusted broker/controller, never to Agent deserialization. Candidate
//! submission conveys no approval and performs no registry lookup.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{
    Digest,
    launch_protocol::{ErrorCode, ProtocolError},
};

mod cargo;
pub use cargo::StartingLockfile;
mod download;
pub use download::{Artifact, FetchError, FetchPermit};
pub mod transport;
mod wire;
pub use crate::conformance::admission::Attendance;
pub use wire::{
    ApprovedDependencies, CACHE_CHUNK_BYTES, CacheChunk, DependencyRequest, DependencyStatus,
};

/// Maximum candidates retained for one Session, including starting dependencies.
pub const MAX_CANDIDATES: usize = 4096;

/// Where an exact dependency originates, before any external lookup.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Source {
    /// A configured registry identity; this string is never used as a URL.
    Registry {
        /// Exact configured registry, with `crates-io` reserved for Cargo's default.
        registry: String,
    },
    /// A Git dependency; approval never permits executing Git or repository code.
    Git {
        /// Repository locator shown locally for review.
        repository: String,
        /// Exact resolved commit, never a branch or tag.
        revision: String,
    },
    /// An explicitly proposed archive URL, never an implicit registry lookup.
    Url {
        /// Exact locator requiring explicit approval.
        url: String,
    },
    /// A non-registry source that cannot acquire automatic fetch authority.
    Other {
        /// Bounded local review text; never executed or resolved speculatively.
        locator: String,
    },
}

/// One exact package and its expected bytes. This type carries no authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    /// Cargo package name, preserved case-sensitively.
    pub name: String,
    /// Exact canonical `SemVer`, including build metadata when present.
    pub version: String,
    /// Typed source, never inferred from an Agent-selected name.
    pub source: Source,
    /// Canonical SHA-256 of the archive, or explicitly missing integrity.
    pub integrity: Option<String>,
}

impl Candidate {
    /// Validates exact coordinates without resolving names or reaching the network.
    /// # Errors
    /// Rejects malformed, unbounded, noncanonical or contradictory fields.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        let version = semver::Version::parse(&self.version).map_err(|_| invalid())?;
        if self.name.is_empty()
            || self.name.len() > 64
            || !self
                .name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
            || self.version.len() > 128
            || version.to_string() != self.version
            || self
                .integrity
                .as_ref()
                .is_some_and(|value| !canonical_digest(value))
        {
            return Err(invalid());
        }
        let valid = match &self.source {
            Source::Registry { registry } => text(registry),
            Source::Git {
                repository,
                revision,
            } => {
                text(repository)
                    && matches!(revision.len(), 40 | 64)
                    && revision
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            }
            Source::Url { url } => text(url),
            Source::Other { locator } => text(locator),
        };
        if valid { Ok(()) } else { Err(invalid()) }
    }

    /// Content address of the complete validated proposal, including its integrity.
    /// # Errors
    /// Rejects invalid candidate content before producing a review identity.
    pub fn id(&self) -> Result<String, ProtocolError> {
        self.validate()?;
        let bytes = serde_json::to_vec(self).map_err(|_| invalid())?;
        Ok(Digest::of(&bytes).to_string())
    }

    fn automatic(&self) -> bool {
        matches!(self.source, Source::Registry { .. }) && self.integrity.is_some()
    }
}

/// Trusted immutable input for one dependency authority lifetime.
///
/// Build this from approved launch inputs before starting the Run. Deserializing
/// an Agent proposal must never construct or replace the active policy.
#[derive(Clone, Debug)]
pub struct DependencyPolicy {
    /// Session bound by the authenticated supervisor.
    pub session_id: String,
    /// Owning Run, bound before launch.
    pub run_id: String,
    /// Exact capability envelope revision.
    pub envelope_revision: u64,
    /// Controller-selected attendance, never an Agent request flag.
    pub attendance: Attendance,
    /// Parsed from the trusted starting source snapshot, never the live workspace.
    pub starting: StartingLockfile,
    /// Exact explicit approvals fixed before start, including exceptional sources.
    pub preapproved: Vec<Candidate>,
    /// Exclusive absolute expiry, never renewed by retries or Resume.
    pub expires_at_ms: u64,
    /// Maximum non-refundable fetch attempts for this lifetime.
    pub max_fetches: u32,
    /// Aggregate maximum downloaded bytes for this lifetime.
    pub max_bytes: u64,
}

/// Result of local policy evaluation. None of these values performs a fetch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Decision {
    /// Exact starting registry/integrity match or an explicit approval exists.
    Authorized,
    /// Local typed proposal awaiting a trusted operator decision.
    Pending {
        /// Digest of the complete candidate to present for exact approval.
        candidate_id: String,
    },
    /// Unattended scope excludes this coordinate; no prompt is created.
    Denied,
}

/// Single-owner authority for one live Session; dropping it grants nothing.
pub struct DependencySession {
    policy: DependencyPolicy,
    approved: BTreeSet<String>,
    pending: BTreeMap<String, Candidate>,
    revoked: bool,
    generation: std::sync::Arc<std::sync::atomic::AtomicBool>,
    remaining_fetches: u32,
    remaining_bytes: u64,
}

impl DependencySession {
    /// Constructs a lifetime from trusted pre-start input and an actual clock.
    /// # Errors
    /// Rejects invalid identity, budgets, expired authority or duplicate approvals.
    pub fn new(policy: DependencyPolicy, now_ms: u64) -> Result<Self, ProtocolError> {
        if !identifier(&policy.session_id)
            || policy.envelope_revision == 0
            || !identifier(&policy.run_id)
            || policy.max_fetches == 0
            || policy.max_fetches > 4096
            || policy.max_bytes == 0
            || policy.max_bytes > 1024 * 1024 * 1024
            || policy.expires_at_ms <= now_ms
            || policy.preapproved.len() > MAX_CANDIDATES
        {
            return Err(invalid());
        }
        let mut approved = BTreeSet::new();
        for candidate in &policy.preapproved {
            if !approved.insert(candidate.id()?) {
                return Err(invalid());
            }
        }
        Ok(Self {
            remaining_fetches: policy.max_fetches,
            remaining_bytes: policy.max_bytes,
            policy,
            approved,
            pending: BTreeMap::new(),
            revoked: false,
            generation: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
        })
    }

    /// Checks the authenticated caller's Session, Run, revision and current lifetime.
    /// # Errors
    /// Refuses wrong subjects, changed revision, revoked or expired authority.
    pub fn check_binding(
        &self,
        session: &str,
        run: &str,
        revision: u64,
        now_ms: u64,
    ) -> Result<(), ProtocolError> {
        self.check_active(now_ms)?;
        if self.policy.session_id != session || self.policy.run_id != run {
            return Err(ProtocolError::new(ErrorCode::SubjectMismatch, None, None));
        }
        if self.policy.envelope_revision != revision {
            return Err(ProtocolError::new(
                ErrorCode::EnvelopeRevisionMismatch,
                None,
                None,
            ));
        }
        Ok(())
    }

    /// Classifies a validated proposal locally, before any external name disclosure.
    /// # Errors
    /// Refuses malformed input, inactive authority or exhaustion of local candidate storage.
    pub fn consider(
        &mut self,
        candidate: &Candidate,
        now_ms: u64,
    ) -> Result<Decision, ProtocolError> {
        self.check_active(now_ms)?;
        let id = candidate.id()?;
        if self.approved.contains(&id)
            || (candidate.automatic() && self.policy.starting.entries().contains(candidate))
        {
            return Ok(Decision::Authorized);
        }
        if self.policy.attendance == Attendance::Unattended {
            return Ok(Decision::Denied);
        }
        if !self.pending.contains_key(&id)
            && self.pending.len() + self.approved.len() >= MAX_CANDIDATES
        {
            return Err(invalid());
        }
        self.pending.insert(id.clone(), candidate.clone());
        Ok(Decision::Pending { candidate_id: id })
    }

    /// Pending typed proposals, for the authenticated operator's local review only.
    #[must_use]
    pub const fn pending(&self) -> &BTreeMap<String, Candidate> {
        &self.pending
    }

    /// Approves an exact batch after operator authentication, atomically in memory.
    ///
    /// The service must durably record the batch before calling this method or
    /// acknowledging approval. It cannot be called by an Agent request handler.
    /// # Errors
    /// Refuses unattended Runs, expired/revoked authority, empty/duplicate/unknown IDs.
    pub fn approve_batch(&mut self, ids: &[String], now_ms: u64) -> Result<(), ProtocolError> {
        self.check_active(now_ms)?;
        let unique: BTreeSet<_> = ids.iter().collect();
        if self.policy.attendance != Attendance::Interactive
            || ids.is_empty()
            || ids.len() > MAX_CANDIDATES
            || unique.len() != ids.len()
            || ids.iter().any(|id| !self.pending.contains_key(id))
        {
            return Err(invalid());
        }
        for id in ids {
            self.pending.remove(id);
            self.approved.insert(id.clone());
        }
        Ok(())
    }

    /// Permanently closes this lifetime; pending proposals confer no retained authority.
    pub fn revoke(&mut self) {
        self.generation
            .store(false, std::sync::atomic::Ordering::Release);
        self.revoked = true;
        self.pending.clear();
    }

    fn check_active(&self, now_ms: u64) -> Result<(), ProtocolError> {
        if self.revoked || now_ms >= self.policy.expires_at_ms {
            Err(invalid())
        } else {
            Ok(())
        }
    }
}

fn text(value: &str) -> bool {
    !value.is_empty() && value.len() <= 2048 && value.bytes().all(|b| b.is_ascii_graphic())
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

fn canonical_digest(value: &str) -> bool {
    Digest::parse(value).is_ok_and(|digest| digest.to_string() == value)
}

fn invalid() -> ProtocolError {
    ProtocolError::new(ErrorCode::InvalidRequest, None, None)
}
