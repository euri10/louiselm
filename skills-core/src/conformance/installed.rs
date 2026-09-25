//! Installed-host evidence. Only the fixed privileged certifier produces it.
//!
//! Parsing proves structure, not authority. Consumers must read protected
//! storage and compare fresh measurements; guest reports cannot be promoted.

use std::{collections::BTreeMap, io};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::preparation::uuid;
use super::{Report, ReportError, ReportResult, Scope};
use crate::Digest;

mod guard_probe;
mod measurement;
#[cfg(test)]
pub(crate) use guard_probe::REFUSE_GUARD_LOAD;
pub use guard_probe::serve_peer as serve_guard_probe;
#[doc(hidden)]
pub mod probes;
mod runner;
pub(super) mod storage;

pub use measurement::measure;
pub use probes::serve_probe;
#[cfg(test)]
pub(crate) use runner::{CANCEL_AFTER_FIRST_GROUP, FORCE_UNCONFIRMED_CLEANUP};
pub use runner::{certify, certify_isolated};
pub use storage::{CertificateStatus, CertificateStore};

/// Deliberately bounded initial supported host/dependency profile.
pub const PROFILE: &str = "debian13-x86_64-glibc/1";
/// Canonical installed certificate schema.
pub const CERTIFICATE_SCHEMA: &str = "louiselm.conformance.certificate/1";
pub(super) const MAX_CERTIFICATE_BYTES: usize = 256 * 1024;

/// Exact host, boot, implementation, library, loader and policy measurements.
/// No raw configuration or machine identifier is disclosed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostSnapshot {
    /// Fixed supported measurement profile.
    pub profile: String,
    /// Hash of the host machine identifier.
    pub machine_digest: String,
    /// Kernel-provided boot UUID.
    pub boot_id: String,
    /// Exact release that requires its own certification.
    pub release_digest: String,
    /// Sorted bounded map of trusted input identifiers to byte digests.
    pub inputs: BTreeMap<String, String>,
}

impl HostSnapshot {
    fn validate(&self) -> Result<(), CertificationError> {
        if self.profile != PROFILE
            || Digest::parse(&self.machine_digest).is_err()
            || Digest::parse(&self.release_digest).is_err()
            || !uuid(&self.boot_id)
            || self.inputs.len() > 128
            || ["launcher", "backend", "policy", "kernel", "loader"]
                .iter()
                .any(|key| !self.inputs.contains_key(*key))
            || self.inputs.iter().any(|(key, digest)| {
                key.is_empty()
                    || key.len() > 256
                    || key.chars().any(char::is_control)
                    || Digest::parse(digest).is_err()
            })
        {
            return Err(CertificationError::Invalid);
        }
        Ok(())
    }
}

/// Measured installed observations. A valid certificate can report failure.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Certificate {
    /// Must be [`CERTIFICATE_SCHEMA`].
    pub schema: String,
    /// Inputs measured both before and after the owned test run.
    pub host: HostSnapshot,
    /// Actual observations, never a caller-selected passing boolean.
    pub observations: Report,
}

impl Certificate {
    /// Construct bounded installed evidence without granting admission.
    ///
    /// # Errors
    /// Rejects guest scope, malformed measurements or observations.
    pub fn new(host: HostSnapshot, observations: Report) -> Result<Self, CertificationError> {
        let value = Self {
            schema: CERTIFICATE_SCHEMA.into(),
            host,
            observations,
        };
        value.canonical_bytes()?;
        Ok(value)
    }

    /// Encode exact validated bytes for protected retention.
    ///
    /// # Errors
    /// Rejects invalid schema, profile, scope, observations or size.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, CertificationError> {
        self.host.validate()?;
        if self.schema != CERTIFICATE_SCHEMA || self.observations.scope != Scope::InstalledHost {
            return Err(CertificationError::Invalid);
        }
        self.observations.canonical_bytes()?;
        let bytes = serde_json::to_vec(self).map_err(|_| CertificationError::Invalid)?;
        if bytes.len() > MAX_CERTIFICATE_BYTES {
            return Err(CertificationError::Invalid);
        }
        Ok(bytes)
    }

    /// Parse exact canonical bytes; this alone does not authenticate evidence.
    ///
    /// # Errors
    /// Rejects unbounded, malformed, noncanonical or contradictory input.
    pub fn parse_canonical(bytes: &[u8]) -> Result<Self, CertificationError> {
        if bytes.len() > MAX_CERTIFICATE_BYTES {
            return Err(CertificationError::Invalid);
        }
        let value: Self = serde_json::from_slice(bytes).map_err(|_| CertificationError::Invalid)?;
        if value.canonical_bytes()? != bytes {
            return Err(CertificationError::Invalid);
        }
        Ok(value)
    }

    /// Whether passing evidence exactly matches freshly measured inputs.
    /// Persistent failure state must independently permit its use.
    #[must_use]
    pub fn is_current(&self, measured: &HostSnapshot) -> bool {
        self.canonical_bytes().is_ok()
            && &self.host == measured
            && self.observations.result() == Ok(ReportResult::Passed)
    }
}

/// Expected certification failure, without exposing measured file contents.
#[derive(Debug, Error)]
pub enum CertificationError {
    /// Invalid, contradictory, missing or untrusted evidence.
    #[error("invalid or untrusted host certification evidence")]
    Invalid,
    /// The fixed initial platform/dependency profile cannot be measured.
    #[error("unsupported or unmeasurable host certification profile")]
    Unsupported,
    /// Production Sender guard inputs or its system loader are unavailable.
    #[error(
        "Sender guard unavailable; restore BPF LSM, kernel BTF and system libbpf, then certify"
    )]
    GuardUnavailable,
    /// Another attempt owns the store, or an old attempt has unresolved cleanup.
    #[error("certification busy or interrupted; inspect retained attempt and identity leases")]
    Pending,
    /// A required I/O, deadline or cleanup operation failed.
    #[error("host certification I/O failed")]
    Io(#[from] io::Error),
    /// Observation schema failed, not an observed containment failure.
    #[error(transparent)]
    Report(#[from] ReportError),
    /// Installed authority or identity leasing failed.
    #[error(transparent)]
    Launcher(#[from] crate::launcher_install::LauncherError),
    /// A production confinement or lifecycle operation failed.
    #[error(transparent)]
    Sandbox(#[from] crate::sandbox::SandboxError),
}
