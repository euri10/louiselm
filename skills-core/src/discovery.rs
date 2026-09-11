//! Authenticated adapter discovery inventories and per-source control checks.
//!
//! These checks consume observations authenticated by the existing sequence-zero
//! launcher receipt. They do not perform mounts or trust an Agent's description
//! of its own controls. A backend must establish the observations before signing;
//! omitted evidence refuses native verification. Vendor integration and live
//! mount/probe production belong to the installed-launch cutover.

mod authentication;
mod inventory;
mod proof;

pub use authentication::AuthenticatedInputs;
pub use inventory::Inventory;
pub use proof::DiscoveryProof;

use crate::{
    Digest,
    posture::{DimensionName, FailureCode},
};
use serde::Serialize;
use thiserror::Error;

/// Versioned inventory packaged as a measured adapter file.
pub const INVENTORY_SCHEMA: &str = "louiselm.discovery.inventory/1";
/// Required registered path inside each participating immutable runtime.
pub const INVENTORY_PATH: &str = "louiselm-discovery.json";
/// Versioned source observations covered by the launcher receipt.
pub const SOURCE_EVIDENCE_SCHEMA: &str = "louiselm.discovery.evidence/1";
/// Maximum encoded inventory or source observation record.
pub const MAX_DISCOVERY_BYTES: usize = 64 * 1024;

/// A failed trust check, with fixed display text and retained internal causes.
#[derive(Debug, Error)]
pub enum DiscoveryError {
    /// Launcher receipt could not be authenticated.
    #[error("native_supply: launcher receipt verification failed")]
    Receipt(#[from] crate::launch_receipt::ReceiptError),
    /// Canonical manifest validation failed.
    #[error("native_supply: input manifest validation failed")]
    Manifest(#[from] crate::session_manifest::SessionManifestError),
    /// Registered runtime could not be measured.
    #[error("runtime: measurement failed")]
    Runtime(#[from] crate::registry::RegistryError),
    /// General confinement prerequisites were not established.
    #[error("native_supply: confinement evidence failed")]
    Isolation(#[from] crate::isolation::IsolationFailure),
    /// Registered inventory could not be read.
    #[error("native_supply: inventory read failed")]
    Io(#[from] std::io::Error),
    /// Closed record encoding failed.
    #[error("native_supply: malformed discovery record")]
    Encoding(#[from] serde_json::Error),
    /// Fixed domain refusal, never caller-authored text.
    #[error("discovery: {0}")]
    Refused(&'static str),
}

impl DiscoveryError {
    /// Stable detailed refusal without paths, prompts, or configuration content.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Receipt(_) => "receipt_unverified",
            Self::Manifest(_) => "manifest_invalid",
            Self::Runtime(_) => "runtime_unmeasured",
            Self::Isolation(_) => "confinement_unverified",
            Self::Io(_) => "inventory_unreadable",
            Self::Encoding(_) => "malformed_discovery_record",
            Self::Refused(code) => code,
        }
    }

    /// Independently affected supply dimension.
    #[must_use]
    pub fn dimension(&self) -> DimensionName {
        match self {
            Self::Runtime(_)
            | Self::Refused(
                "live_executable_lookup" | "self_update_enabled" | "runtime_mismatch",
            ) => DimensionName::Runtime,
            _ => DimensionName::NativeSupply,
        }
    }

    /// Code accepted by the normalized posture contract.
    #[must_use]
    pub fn failure_code(&self) -> FailureCode {
        if self.dimension() == DimensionName::Runtime {
            FailureCode::RuntimeDrift
        } else {
            FailureCode::NativeSupplyUncertain
        }
    }
}

pub(crate) fn json_digest(value: &impl Serialize) -> Result<Digest, DiscoveryError> {
    Ok(Digest::of(&serde_json::to_vec(value)?))
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}
