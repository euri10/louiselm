//! Single-use fetch admission and final publication into the exact Session cache.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{Candidate, Decision, DependencySession, invalid};
use crate::{
    Digest,
    cache::{CacheError, CacheOverlay, MAX_BYTES},
    launch_protocol::ProtocolError,
};

/// A dependency fetch refused before publication. Display never includes payloads.
#[derive(Debug, Error)]
pub enum FetchError {
    /// Exact authority, lifetime or candidate binding no longer holds.
    #[error("dependency fetch authorization refused")]
    Policy(#[from] ProtocolError),
    /// Received bytes exceed their reservation or fail the approved integrity.
    #[error("dependency archive integrity or size mismatch")]
    Integrity,
    /// The pinned Session cache refused publication.
    #[error("dependency cache publication failed")]
    Cache(#[from] CacheError),
    /// Transport failed, redirected, or returned a non-success response.
    #[error("dependency download failed")]
    Transport,
    /// HTTP/TLS failure retained internally; Display does not include the URL.
    #[error("dependency HTTPS exchange failed")]
    Http(#[source] Box<ureq::Error>),
    /// Worker or stream I/O cause, without user payloads in Display.
    #[error("dependency download I/O failed")]
    Io(#[source] std::io::Error),
}

/// Non-cloneable permit minted before any external name disclosure.
///
/// The transport receives this only after the broker records its non-refundable
/// intent. Completion must return it to the same live owner before publishing.
pub struct FetchPermit {
    generation: Arc<AtomicBool>,
    candidate: Candidate,
    max_bytes: u64,
    deadline: Instant,
}

impl FetchPermit {
    pub(super) fn remaining(&self) -> Result<Duration, FetchError> {
        self.deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| invalid().into())
    }
    /// Rechecks queued work against revocation and its original monotonic expiry.
    /// # Errors
    /// Returns a policy refusal after revocation or expiry; transports check before I/O.
    pub fn check_active(&self) -> Result<(), FetchError> {
        if self.generation.load(Ordering::Acquire) && Instant::now() < self.deadline {
            Ok(())
        } else {
            Err(invalid().into())
        }
    }
    /// Exact approved proposal for the transport, with no new name resolution.
    #[must_use]
    pub const fn candidate(&self) -> &Candidate {
        &self.candidate
    }

    /// Maximum body bytes reserved from this Session's aggregate budget.
    #[must_use]
    pub const fn max_bytes(&self) -> u64 {
        self.max_bytes
    }
}

/// Published opaque bytes, never an extracted or executed package.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    /// Fixed content-addressed filename relative to the Session cache.
    pub name: String,
    /// Actual SHA-256 of the published archive.
    pub digest: String,
    /// Exact byte length.
    pub size: u64,
    /// Whether these bytes matched an integrity value known before fetching.
    pub integrity_verified: bool,
}

impl Artifact {
    /// Validates the fixed filename against the exact digest and size bound.
    /// # Errors
    /// Refuses malformed content addresses, alternate paths or excessive sizes.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        let digest = Digest::parse(&self.digest).map_err(|_| invalid())?;
        if digest.to_string() != self.digest
            || self.name != format!("artifact-{}", digest.hex())
            || self.size > MAX_BYTES as u64
        {
            return Err(invalid());
        }
        Ok(())
    }
}

impl DependencySession {
    /// Spends bounded fetch authority before a transport sees the candidate.
    ///
    /// Reservations are not refunded on error, cancellation or short responses.
    /// The broker must persist the reservation before dispatching the permit.
    /// # Errors
    /// Refuses unapproved candidates, inactive lifetimes, or exhausted/invalid budgets.
    pub fn begin_fetch(
        &mut self,
        candidate: &Candidate,
        max_bytes: u64,
        now_ms: u64,
    ) -> Result<FetchPermit, ProtocolError> {
        if self.consider(candidate, now_ms)? != Decision::Authorized
            || max_bytes == 0
            || max_bytes > MAX_BYTES as u64
            || max_bytes > self.remaining_bytes
            || self.remaining_fetches == 0
        {
            return Err(invalid());
        }
        self.remaining_fetches -= 1;
        self.remaining_bytes -= max_bytes;
        Ok(FetchPermit {
            generation: Arc::clone(&self.generation),
            candidate: candidate.clone(),
            max_bytes,
            deadline: Instant::now()
                .checked_add(Duration::from_millis(self.policy.expires_at_ms - now_ms))
                .ok_or_else(invalid)?,
        })
    }

    /// Rechecks a late completion, verifies exact bytes and publishes an opaque artifact.
    ///
    /// This blocking filesystem operation belongs on the owning cache worker.
    /// The installed supervisor must serialize final publication against local
    /// revocation. No archive parsing, paths inside archives or package scripts
    /// are interpreted. Missing integrity stays explicitly unverified.
    /// # Errors
    /// Refuses replaced owners, expired/revoked authority, foreign/disposed cache,
    /// oversized/corrupt bytes, or unsafe filesystem publication.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "Consumes the single-use fetch permit on every publication attempt, including failure."
    )]
    pub fn finish_fetch(
        &mut self,
        permit: FetchPermit,
        bytes: &[u8],
        overlay: &mut CacheOverlay,
        now_ms: u64,
    ) -> Result<Artifact, FetchError> {
        self.check_active(now_ms)?;
        permit.check_active()?;
        if !Arc::ptr_eq(&self.generation, &permit.generation)
            || overlay.session_id() != self.policy.session_id
        {
            return Err(invalid().into());
        }
        if bytes.len() as u64 > permit.max_bytes {
            return Err(FetchError::Integrity);
        }
        let digest = Digest::of(bytes);
        if permit
            .candidate
            .integrity
            .as_ref()
            .is_some_and(|expected| expected != &digest.to_string())
        {
            return Err(FetchError::Integrity);
        }
        let name = overlay.store_download(&digest, bytes)?;
        Ok(Artifact {
            name,
            digest: digest.to_string(),
            size: bytes.len() as u64,
            integrity_verified: permit.candidate.integrity.is_some(),
        })
    }
}
