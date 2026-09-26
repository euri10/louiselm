//! Unprivileged Control broker: the durable half of one authorized launch.
//!
//! The broker decides Run policy and owns canonical launch bytes. It records a
//! short-lived single-use authorization before a privileged launch runs,
//! consumes that authorization exactly once when the [Launch supervisor] asks
//! for it, and durably stores the exact signed receipts the supervisor returns.
//! Nothing here performs privileged mechanics, signs an outcome, or rewrites a
//! supervisor result.
//!
//! Durability is the point of this module. An authorization is handed out only
//! after its consumption is durable, and a receipt is acknowledged only after
//! its exact bytes are durable, so a crash between the two loses the launch
//! rather than permitting a second one.
//!
//! [Launch supervisor]: crate::launch_supervisor

pub mod admission_source;
pub mod attention;
mod attention_config;
pub mod audit;
pub mod authorization;
mod beads_mutation;
mod beads_mutation_service;
mod beads_replica;
pub mod cold_resume;
pub mod commands;
pub mod conformance_inspection;
mod current_conformance;
pub mod delegation;
mod dependencies;
mod dependency_service;
mod history;
pub mod installed;
pub mod lifecycle;
pub mod operator;
mod posture;
mod posture_attention;
pub mod promotion;
pub mod provider_credentials;
pub mod provider_endpoint;
pub mod provider_extension;
mod provider_handoff;
mod provider_listener;
mod provider_ownership;
mod provider_socket;
mod provider_worker;
pub use provider_socket::GuardedUpstream;
pub mod provider_requests;
mod provider_service;
pub mod provider_transport;
pub mod receipts;
pub mod recovery;
pub mod service;
pub mod skill_quarantine;
mod skill_request_service;
mod skill_requests;
mod state_identity;
mod tracker_runner;
pub mod verification;
pub mod waiver;
mod workspace;
mod workspace_retention;

#[cfg(test)]
mod delegation_tests;

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::Path,
    sync::{Mutex, MutexGuard, PoisonError},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    launch_protocol::IdentityExhaustion, launch_receipt::ReceiptError,
    launch_transport::TransportError,
};

pub use audit::{AuditDecision, AuditEntry, AuditLog};
pub use authorization::{ApprovedCommands, AuthorizationStore, GrantRequest, PendingAuthorization};
pub use dependency_service::{DependencyInspection, PendingDependency};
pub use installed::InstalledBroker;
pub use receipts::{ReceiptStore, TrustedRelease};
pub use service::{BrokerService, BrokerSession, LaunchObservation, SessionInspection};

/// Longest durable broker record this build reads back.
const MAX_RECORD_BYTES: u64 = 64 * 1024;

/// Directory holding authorizations that have not been consumed.
const PENDING_DIRECTORY: &str = "pending";

/// Directory holding the durable proof that an authorization was consumed.
const CONSUMED_DIRECTORY: &str = "consumed";

/// A launch transaction the broker refused or could not make durable.
#[derive(Debug, Error)]
pub enum BrokerError {
    /// An exact operator waiver request was refused.
    #[error("{0}")]
    Waiver(#[from] waiver::WaiverError),
    /// An operator Provider extension was refused by policy.
    #[error("{0}")]
    ProviderExtension(#[from] provider_extension::ExtensionError),
    /// Installed tracker provisioning is present but cannot safely enable writes.
    #[error("broker tracker configuration: {0}")]
    TrackerConfiguration(&'static str),
    /// Signed skill evidence could not be verified; details stay inside the broker.
    #[error("Admission evidence is unavailable")]
    AdmissionEvidence(#[from] crate::admission::AdmissionError),
    /// Supplemental observations are missing or contradict signed admission.
    #[error("broker conformance report: {0}")]
    ConformanceReport(&'static str),
    /// A supply producer returned an observation outside its permitted ordering.
    #[error("broker supply posture: {0}")]
    SupplyPosture(&'static str),
    /// Retained trusted evidence could not form a consistent posture.
    #[error("broker posture evidence is invalid")]
    Posture(#[from] crate::posture::PostureError),
    /// Approved verification inputs could not be validated or durably copied.
    #[error("verification workspace is unavailable")]
    Workspace(#[from] crate::workspace::WorkspaceError),
    /// Projection delivery failed; canonical authorization/receipt state is unchanged.
    #[error("Attention projection is unavailable")]
    Attention(#[source] io::Error),
    /// A lifecycle request failed authorization or compare-and-swap validation.
    #[error("{0}")]
    Policy(#[from] crate::launch_protocol::ProtocolError),
    /// Public installation or dedicated broker identity is not trustworthy.
    #[error("installed broker authority is invalid")]
    Installation,
    /// Another installed broker or explicit adoption owns this state directory.
    #[error("broker state is in use; stop the broker before adopting state")]
    StateInUse,
    /// Durable state records a different broker UID or GID.
    #[error(
        "broker state identity changed; preserve state and explicitly run louiselm-control adopt-state"
    )]
    StateIdentityMismatch,
    /// Existing durable state has no identity continuity record.
    #[error(
        "broker state identity is missing; preserve state and restore or inspect the identity record"
    )]
    StateIdentityMissing,
    /// The continuity record is malformed or is not a private regular file.
    #[error(
        "broker state identity is invalid; preserve state and restore or inspect the identity record"
    )]
    StateIdentityInvalid,
    /// Installed public authority could not be validated; the cause stays internal.
    #[error("installed broker public authority is unavailable")]
    InstallationAuthority(#[source] crate::launcher_install::LauncherError),
    /// Installed public-key verification failed; no durable ACK was sent.
    #[error("launcher receipt verification failed")]
    Verification(#[source] crate::launcher_install::LauncherError),
    /// No pending authorization answers this identity; it never existed,
    /// expired out of the store, or was already consumed.
    #[error("authorization is unknown or already consumed")]
    UnknownAuthorization,
    /// The authorization exists but its exclusive expiry has passed.
    #[error("authorization has expired")]
    Expired,
    /// The presented request is not the one the authorization binds.
    #[error("request does not match its authorization")]
    RequestMismatch,
    /// The presenting controller is not the authorized one.
    #[error("controller does not match its authorization")]
    ControllerMismatch,
    /// An authorization already holds this identity.
    #[error("authorization identity is already in use")]
    DuplicateAuthorization,
    /// Every installed identity slot is held by a live Session.
    #[error("installed identity pool is exhausted")]
    IdentityExhausted(Box<IdentityExhaustion>),
    /// The grant or its request is not one this broker can authorize.
    #[error("launch grant is not well formed")]
    InvalidGrant,
    /// The installed identity pool cannot be used as configured.
    #[error("installed identity pool is unusable")]
    InvalidPool,
    /// The offered receipt is not one this chain accepts. The exact bytes were
    /// not stored, and the supervisor learns only that they were refused.
    #[error("receipt is not acceptable for this chain")]
    ReceiptRefused(#[source] ReceiptError),
    /// The offered receipt does not answer the authorization it claims.
    #[error("receipt does not answer its authorization")]
    ReceiptUnauthorized,
    /// Durable broker state could not be read or written. No acknowledgement
    /// follows a failure here: the exact bytes are not known to be durable.
    #[error("broker state is unavailable")]
    Storage(#[source] io::Error),
    /// The authenticated rendezvous could not carry the transaction.
    #[error("broker rendezvous is unavailable")]
    Transport(#[source] TransportError),
    /// The pinned `br` binary could not be run, or did not exit within its deadline.
    #[error("canonical Beads tracker invocation is unavailable")]
    TrackerInvocation(#[source] io::Error),
    /// Every approved Beads attempt has already been spent.
    #[error("Beads mutation budget exhausted")]
    BeadsBudgetExhausted,
    /// Every approved Provider request attempt of the Run has already been spent.
    #[error("Provider request budget exhausted")]
    ProviderBudgetExhausted,
    /// The Provider upstream could not be reached or its stream failed.
    /// The attempt's outcome is unknown and its unit stays spent.
    #[error("Provider upstream is unavailable")]
    ProviderUnavailable,
}

/// Reads one bounded durable record, or `None` when it is absent.
fn read_record<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Option<T>, BrokerError> {
    let Some(bytes) = read_bounded(path)? else {
        return Ok(None);
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| corrupt("broker record is malformed"))
}

/// Reads one bounded durable file, or `None` when it is absent.
fn read_bounded(path: &Path) -> Result<Option<Vec<u8>>, BrokerError> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(BrokerError::Storage(error)),
    };
    if metadata.len() > MAX_RECORD_BYTES {
        return Err(corrupt("broker record exceeds its bound"));
    }
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(BrokerError::Storage(error)),
    }
}

/// Writes one durable record that must not already exist.
///
/// The bytes and their directory entry are both durable before this returns,
/// which is what lets a caller answer with an acknowledgement.
fn write_new_record<T: Serialize>(path: &Path, record: &T) -> Result<(), BrokerError> {
    let bytes = serde_json::to_vec(record).map_err(|_| BrokerError::InvalidGrant)?;
    write_new_bytes(path, &bytes)
}

/// Writes exact bytes to a durable record that must not already exist.
fn write_new_bytes(path: &Path, bytes: &[u8]) -> Result<(), BrokerError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| {
            if error.kind() == io::ErrorKind::AlreadyExists {
                BrokerError::DuplicateAuthorization
            } else {
                BrokerError::Storage(error)
            }
        })?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(BrokerError::Storage)?;
    match path.parent() {
        Some(directory) => sync_directory(directory),
        None => Err(corrupt("durable record has no directory")),
    }
}

/// Returns the record file name for an identifier, refusing anything that
/// could name a path rather than a record.
fn record_name(identifier: &str) -> Result<String, BrokerError> {
    if is_record_identifier(identifier) {
        Ok(format!("{identifier}.json"))
    } else {
        Err(BrokerError::InvalidGrant)
    }
}

/// Whether an identifier is safe to spell as a durable record name.
fn is_record_identifier(identifier: &str) -> bool {
    !identifier.is_empty()
        && identifier.len() <= 128
        && identifier
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

/// Makes a directory entry change durable.
fn sync_directory(path: &Path) -> Result<(), BrokerError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(BrokerError::Storage)
}

/// Reports durable state this broker wrote but can no longer read as its own.
fn corrupt(reason: &'static str) -> BrokerError {
    BrokerError::Storage(io::Error::new(io::ErrorKind::InvalidData, reason))
}

fn now_ms() -> Result<u64, BrokerError> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .ok_or(BrokerError::InvalidGrant)
}

/// Recovers a poisoned lock: the guarded sections own no invariant beyond
/// ordering, and every durable record is written before they unlock.
fn lock(mutex: &Mutex<()>) -> MutexGuard<'_, ()> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
