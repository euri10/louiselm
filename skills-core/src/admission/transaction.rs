//! Activation's three files share one durable undo journal and the trust lock.
//! A present journal means rollback, including after a process crash. Its removal
//! commits only after every replacement is durable. A failed final directory sync
//! is an uncertain commit, never a claim that the old supply stayed current.

use std::{
    fs::{self, File},
    io::{self, Read, Write},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

use rustix::fs::{Mode, OFlags};
use serde::{Deserialize, Serialize};

use super::{AdmissionError, Digest, GenerationRecord, GenerationState, Store};
use crate::trust::persistence::LockedTrust;

const JOURNAL: &str = "activation.pending.json";

#[cfg(test)]
#[path = "transaction_tests.rs"]
mod tests;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Backup {
    generation: String,
    bytes: String,
}

impl Backup {
    fn capture(store: &Store, digest: &Digest) -> Result<Self, AdmissionError> {
        let path = super::record_path(store, digest);
        let bytes =
            read_optional(&path)?.ok_or_else(|| io_error(&path, io::ErrorKind::NotFound.into()))?;
        Ok(Self {
            generation: digest.to_string(),
            bytes,
        })
    }

    fn path(&self, store: &Store) -> Result<PathBuf, AdmissionError> {
        let digest = Digest::parse(&self.generation)?;
        let record: GenerationRecord = serde_json::from_str(&self.bytes)
            .map_err(|error| AdmissionError::Malformed(error.to_string()))?;
        if record.digest() != digest || record.generation != self.generation {
            return Err(AdmissionError::Malformed(
                "activation backup identity mismatch".to_owned(),
            ));
        }
        Ok(super::record_path(store, &digest))
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    version: u8,
    previous: Option<Backup>,
    candidate: Backup,
    pins: Option<String>,
}

// Checkpoints exercise actual publication order in fault/crash tests; production
// supplies a no-op, with no environment-driven fault injection in the tool.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Checkpoint {
    JournalDurable,
    PreviousWritten,
    CandidateWritten,
    PinsWritten,
    JournalRemoved,
}

pub(super) fn lock(store: &Store) -> Result<LockedTrust, AdmissionError> {
    let locked = LockedTrust::acquire(store)?;
    recover(store)?;
    Ok(locked)
}

pub(super) fn activate(
    store: &Store,
    previous: Option<&GenerationRecord>,
    current: &GenerationRecord,
    activated_at_ms: u64,
) -> Result<(), AdmissionError> {
    activate_with(store, previous, current, activated_at_ms, |_| Ok(()))
}

fn activate_with(
    store: &Store,
    previous: Option<&GenerationRecord>,
    current: &GenerationRecord,
    activated_at_ms: u64,
    mut checkpoint: impl FnMut(Checkpoint) -> Result<(), AdmissionError>,
) -> Result<(), AdmissionError> {
    let pins_path = store.root().join("pins.jsonl");
    let journal = Journal {
        version: 1,
        previous: previous
            .map(|record| Backup::capture(store, &record.digest()))
            .transpose()?,
        candidate: Backup::capture(store, &current.digest())?,
        pins: read_optional(&pins_path)?,
    };
    let mut pins = journal.pins.clone().unwrap_or_default();
    pins.push_str(&super::pin_line(current, activated_at_ms)?);
    let journal_path = store.root().join(JOURNAL);
    let bytes = serde_json::to_vec(&journal)
        .map_err(|error| AdmissionError::Malformed(error.to_string()))?;
    // No Generation or lineage write may precede durable rollback evidence.
    write_atomic(&journal_path, &bytes)?;
    let result = (|| {
        checkpoint(Checkpoint::JournalDurable)?;
        if let Some(previous) = previous {
            let mut superseded = previous.clone();
            superseded.state = GenerationState::Superseded;
            super::write_record(store, &superseded)?;
            checkpoint(Checkpoint::PreviousWritten)?;
        }
        super::write_record(store, current)?;
        checkpoint(Checkpoint::CandidateWritten)?;
        write_atomic(&pins_path, pins.as_bytes())?;
        checkpoint(Checkpoint::PinsWritten)?;
        fs::remove_file(&journal_path).map_err(|source| io_error(&journal_path, source))
    })();
    if let Err(failure) = result {
        return match recover(store) {
            Ok(()) => Err(failure),
            Err(source) => Err(AdmissionError::RecoveryRequired {
                failure: Box::new(failure),
                source: Box::new(source),
            }),
        };
    }
    checkpoint(Checkpoint::JournalRemoved)
        .and_then(|()| sync_directory(store.root()))
        .map_err(|source| AdmissionError::CommitUncertain {
            generation: current.generation.clone(),
            source: Box::new(source),
        })
}

pub(super) fn confirm(store: &Store, digest: &Digest) -> Result<(), AdmissionError> {
    sync_directory(store.root()).map_err(|source| AdmissionError::CommitUncertain {
        generation: digest.to_string(),
        source: Box::new(source),
    })
}

fn recover(store: &Store) -> Result<(), AdmissionError> {
    let path = store.root().join(JOURNAL);
    let Some(bytes) = read_optional(&path)? else {
        return Ok(());
    };
    let journal: Journal = serde_json::from_str(&bytes)
        .map_err(|error| AdmissionError::Malformed(error.to_string()))?;
    if journal.version != 1 {
        return Err(AdmissionError::Malformed(
            "unsupported activation journal".to_owned(),
        ));
    }
    // Validate every backup before restoring any file. Journal identities never
    // become arbitrary paths, even in a malformed or operator-edited store.
    let candidate_path = journal.candidate.path(store)?;
    let previous_path = journal
        .previous
        .as_ref()
        .map(|backup| backup.path(store))
        .transpose()?;
    if previous_path.as_ref() == Some(&candidate_path) {
        return Err(AdmissionError::Malformed(
            "activation backups name the same Generation".to_owned(),
        ));
    }
    if let (Some(previous), Some(path)) = (&journal.previous, previous_path) {
        write_atomic(&path, previous.bytes.as_bytes())?;
    }
    write_atomic(&candidate_path, journal.candidate.bytes.as_bytes())?;
    let pins_path = store.root().join("pins.jsonl");
    if let Some(pins) = &journal.pins {
        write_atomic(&pins_path, pins.as_bytes())?;
    } else if regular_file(&pins_path)?.is_some() {
        fs::remove_file(&pins_path).map_err(|source| io_error(&pins_path, source))?;
        sync_directory(store.root())?;
    }
    fs::remove_file(&path).map_err(|source| io_error(&path, source))?;
    sync_directory(store.root())
}

pub(super) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), AdmissionError> {
    let parent = path
        .parent()
        .ok_or_else(|| io_error(path, io::ErrorKind::InvalidInput.into()))?;
    regular_file(path)?;
    let mut staged = tempfile::Builder::new()
        .prefix(".admission-")
        .tempfile_in(parent)
        .map_err(|source| io_error(parent, source))?;
    crate::store::share_evidence(
        &File::open(parent).map_err(|source| io_error(parent, source))?,
        staged.as_file(),
    )
    .map_err(|source| io_error(path, source))?;
    staged
        .write_all(bytes)
        .and_then(|()| staged.as_file().sync_all())
        .map_err(|source| io_error(path, source))?;
    regular_file(path)?;
    staged
        .persist(path)
        .map_err(|error| io_error(path, error.error))?;
    sync_directory(parent)
}

fn read_optional(path: &Path) -> Result<Option<String>, AdmissionError> {
    let Some(mut file) = regular_file(path)? else {
        return Ok(None);
    };
    let mut bytes = String::new();
    file.read_to_string(&mut bytes)
        .map_err(|source| io_error(path, source))?;
    Ok(Some(bytes))
}

fn regular_file(path: &Path) -> Result<Option<File>, AdmissionError> {
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let file = match rustix::fs::open(path, flags, Mode::empty()) {
        Ok(file) => File::from(file),
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(source) => return Err(io_error(path, source.into())),
    };
    let metadata = file.metadata().map_err(|source| io_error(path, source))?;
    if !metadata.is_file() || metadata.nlink() > 1 {
        return Err(io_error(
            path,
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "expected an unaliased regular file",
            ),
        ));
    }
    Ok(Some(file))
}

fn sync_directory(path: &Path) -> Result<(), AdmissionError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| io_error(path, source))
}

fn io_error(path: &Path, source: io::Error) -> AdmissionError {
    AdmissionError::Io {
        path: path.display().to_string(),
        source,
    }
}
