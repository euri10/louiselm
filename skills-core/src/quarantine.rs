//! Emergency quarantine: narrowing authority without the token.
//!
//! Quarantine exists for the moment between noticing something and being able
//! to run a ceremony. It can only take authority away — exclude packages from
//! the current Generation, or exclude all of them — and it takes effect the
//! moment it is written, with no hardware present.
//!
//! Giving authority back is the other direction, so it is not offered here at
//! all: widening requires a newly admitted Generation, which requires the
//! token. A `clear` that merely deleted this file would be a way to re-enable
//! quarantined supply without a touch, which is the whole thing quarantine is
//! protecting.

use std::{collections::BTreeSet, fs, io, path::PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::store::Store;

/// The quarantine schema this build reads and writes.
pub const QUARANTINE_SCHEMA: &str = "louiselm.skills.quarantine/1";

/// The active narrowing of the current Generation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Quarantine {
    /// Schema identifier.
    pub schema: String,
    /// Package digests excluded from the current Generation.
    pub excluded: Vec<String>,
    /// Whether every member is excluded, however the Generation changes.
    pub excludes_everything: bool,
    /// Why, recorded for the operator who has to undo it deliberately.
    pub reasons: Vec<String>,
    /// When the narrowing was last widened in scope.
    pub declared_at_ms: u64,
}

/// A quarantine operation that was refused.
#[derive(Debug, Error)]
pub enum QuarantineError {
    /// The quarantine file could not be read or written.
    #[error("quarantine I/O failed at '{path}': {source}")]
    Io {
        /// Path being operated on.
        path: String,
        /// Underlying failure.
        source: io::Error,
    },
    /// The quarantine file on disk is unreadable.
    #[error("quarantine is malformed: {0}")]
    Malformed(String),
    /// The caller asked to widen authority.
    #[error(
        "quarantine only narrows authority; admit a new Skill Generation to restore {0} package(s)"
    )]
    WouldWiden(usize),
}

/// Reads the active quarantine, when there is one.
///
/// # Errors
/// Returns read/JSON errors; a missing quarantine file is `Ok(None)`.
pub fn load(store: &Store) -> Result<Option<Quarantine>, QuarantineError> {
    let path = path(store);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(QuarantineError::Io {
                path: path.display().to_string(),
                source,
            });
        }
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| QuarantineError::Malformed(error.to_string()))
}

/// Excludes `packages` from the current Generation, immediately.
///
/// # Errors
/// Returns quarantine read/JSON or persistence errors.
pub fn exclude(
    store: &Store,
    packages: &[String],
    reason: &str,
    declared_at_ms: u64,
) -> Result<Quarantine, QuarantineError> {
    let existing = load(store)?;
    let mut excluded = existing
        .as_ref()
        .map(|quarantine| quarantine.excluded.iter().cloned().collect::<BTreeSet<_>>())
        .unwrap_or_default();
    excluded.extend(packages.iter().cloned());
    let mut reasons = existing
        .as_ref()
        .map(|quarantine| quarantine.reasons.clone())
        .unwrap_or_default();
    let reason = crate::scan::escape(reason);
    if !reasons.contains(&reason) {
        reasons.push(reason);
    }
    let quarantine = Quarantine {
        schema: QUARANTINE_SCHEMA.to_owned(),
        excluded: excluded.into_iter().collect(),
        excludes_everything: existing.is_some_and(|quarantine| quarantine.excludes_everything),
        reasons,
        declared_at_ms,
    };
    write(store, &quarantine)?;
    Ok(quarantine)
}

/// Excludes every member of whatever Generation is current.
///
/// # Errors
/// Returns quarantine read/JSON or persistence errors.
pub fn exclude_everything(
    store: &Store,
    reason: &str,
    declared_at_ms: u64,
) -> Result<Quarantine, QuarantineError> {
    let mut quarantine = exclude(store, &[], reason, declared_at_ms)?;
    quarantine.excludes_everything = true;
    write(store, &quarantine)?;
    Ok(quarantine)
}

/// Always refuses: restoring authority is a Skill Admission, not a delete.
///
/// # Errors
/// Returns a quarantine-loading error or always `WouldWiden`; authority cannot be restored by deleting quarantine.
pub fn clear(store: &Store, _at_ms: u64) -> Result<(), QuarantineError> {
    let quarantine = load(store)?;
    let count = quarantine
        .as_ref()
        .map(|quarantine| quarantine.excluded.len())
        .unwrap_or_default();
    Err(QuarantineError::WouldWiden(count))
}

/// Splits `members` into what the quarantine still allows and what it excludes.
#[must_use]
pub fn partition(
    quarantine: Option<&Quarantine>,
    members: &[String],
) -> (Vec<String>, Vec<String>) {
    let Some(quarantine) = quarantine else {
        return (members.to_vec(), Vec::new());
    };
    if quarantine.excludes_everything {
        return (Vec::new(), members.to_vec());
    }
    let excluded = quarantine.excluded.iter().collect::<BTreeSet<_>>();
    members
        .iter()
        .cloned()
        .partition(|member| !excluded.contains(member))
}

fn write(store: &Store, quarantine: &Quarantine) -> Result<(), QuarantineError> {
    let path = path(store);
    let bytes = serde_json::to_vec(quarantine)
        .map_err(|error| QuarantineError::Malformed(error.to_string()))?;
    fs::write(&path, bytes).map_err(|source| QuarantineError::Io {
        path: path.display().to_string(),
        source,
    })
}

fn path(store: &Store) -> PathBuf {
    store.root().join("quarantine.json")
}
