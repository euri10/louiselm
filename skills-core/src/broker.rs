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

pub mod attention;
pub mod audit;
pub mod authorization;
pub mod commands;
pub mod delegation;
pub mod installed;
pub mod lifecycle;
pub mod receipts;
pub mod recovery;
pub mod service;

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
    /// Projection delivery failed; canonical authorization/receipt state is unchanged.
    #[error("Attention projection is unavailable")]
    Attention(#[source] io::Error),
    /// A lifecycle request failed authorization or compare-and-swap validation.
    #[error("{0}")]
    Policy(#[from] crate::launch_protocol::ProtocolError),
    /// Public installation or dedicated broker identity is not trustworthy.
    #[error("installed broker authority is invalid")]
    Installation,
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

/// Recovers a poisoned lock: the guarded sections own no invariant beyond
/// ordering, and every durable record is written before they unlock.
fn lock(mutex: &Mutex<()>) -> MutexGuard<'_, ()> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
