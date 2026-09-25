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
pub const QUARANTINE_SCHEMA: &str = "louiselm.skills.quarantine/2";

/// The durable emergency narrowing of admitted supply.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Quarantine {
    /// Schema identifier.
    pub schema: String,
    /// Package digests excluded wherever they are admitted.
    pub excluded: Vec<String>,
    /// Generations whose complete membership is excluded, sorted and unique.
    pub excluded_generations: Vec<String>,
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
    /// The current Generation could not be resolved before quarantining it.
    #[error("current Skill Generation could not be read: {source}")]
    CurrentGeneration {
        /// Admission failure that prevented a trustworthy binding.
        #[source]
        source: Box<crate::admission::AdmissionError>,
    },
    /// There is no Generation for an all-members quarantine to bind.
    #[error("quarantine all requires a current Skill Generation")]
    NoCurrentGeneration,
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
    let quarantine: Quarantine = serde_json::from_slice(&bytes)
        .map_err(|error| QuarantineError::Malformed(error.to_string()))?;
    if quarantine.schema != QUARANTINE_SCHEMA {
        return Err(QuarantineError::Malformed(format!(
            "unknown schema '{}'",
            quarantine.schema
        )));
    }
    Ok(Some(quarantine))
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
    let excluded_generations = existing
        .as_ref()
        .map(|quarantine| quarantine.excluded_generations.clone())
        .unwrap_or_default();
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
        excluded_generations,
        reasons,
        declared_at_ms,
    };
    write(store, &quarantine)?;
    Ok(quarantine)
}

/// Excludes every member of the current Generation.
///
/// # Errors
/// Returns Admission, quarantine read/JSON or persistence errors. Refuses when
/// no Generation is current.
pub fn exclude_everything(
    store: &Store,
    reason: &str,
    declared_at_ms: u64,
) -> Result<Quarantine, QuarantineError> {
    let (_locked, current) = crate::admission::locked_current(store).map_err(|source| {
        QuarantineError::CurrentGeneration {
            source: Box::new(source),
        }
    })?;
    let generation = current
        .ok_or(QuarantineError::NoCurrentGeneration)?
        .generation;
    let existing = load(store)?;
    let excluded = existing
        .as_ref()
        .map(|quarantine| quarantine.excluded.clone())
        .unwrap_or_default();
    let mut excluded_generations = existing
        .as_ref()
        .map(|quarantine| {
            quarantine
                .excluded_generations
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    excluded_generations.insert(generation);
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
        excluded,
        excluded_generations: excluded_generations.into_iter().collect(),
        reasons,
        declared_at_ms,
    };
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
        .map(|quarantine| quarantine.excluded.len() + quarantine.excluded_generations.len())
        .unwrap_or_default();
    Err(QuarantineError::WouldWiden(count))
}

/// Splits `members` into what the quarantine still allows and what it excludes.
#[must_use]
pub fn partition(
    quarantine: Option<&Quarantine>,
    generation: &str,
    members: &[String],
) -> (Vec<String>, Vec<String>) {
    let Some(quarantine) = quarantine else {
        return (members.to_vec(), Vec::new());
    };
    if quarantine
        .excluded_generations
        .iter()
        .any(|excluded| excluded == generation)
    {
        return (Vec::new(), members.to_vec());
    }
    let excluded = quarantine.excluded.iter().collect::<BTreeSet<_>>();
    members
        .iter()
        .cloned()
        .partition(|member| !excluded.contains(member))
}

/// Replaces the quarantine atomically and durably.
///
/// The Control broker reads this file from running Sessions' workers and treats
/// unreadable content as reaching every Session, so a reader must never see a
/// half-written file (`louiselm-d6fv.6.5.1`).
fn write(store: &Store, quarantine: &Quarantine) -> Result<(), QuarantineError> {
    use std::{io::Write, os::unix::fs::PermissionsExt};
    let path = path(store);
    let bytes = serde_json::to_vec(quarantine)
        .map_err(|error| QuarantineError::Malformed(error.to_string()))?;
    let failed = |source| QuarantineError::Io {
        path: path.display().to_string(),
        source,
    };
    let root = store.root();
    let mut staged = tempfile::Builder::new()
        .prefix(".quarantine-")
        .tempfile_in(root)
        .map_err(failed)?;
    // The mode a plain write gave it; a shared store narrows it to 0640.
    staged
        .as_file()
        .set_permissions(fs::Permissions::from_mode(0o644))
        .map_err(failed)?;
    crate::store::share_evidence(&fs::File::open(root).map_err(failed)?, staged.as_file())
        .map_err(failed)?;
    staged
        .write_all(&bytes)
        .and_then(|()| staged.as_file().sync_all())
        .map_err(failed)?;
    staged.persist(&path).map_err(|error| failed(error.error))?;
    fs::File::open(root)
        .and_then(|directory| directory.sync_all())
        .map_err(failed)
}

fn path(store: &Store) -> PathBuf {
    store.root().join("quarantine.json")
}
