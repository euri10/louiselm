//! Append-only storage of imported Endorsement records.
//!
//! Records are kept exactly as received, named by the digest of their bytes,
//! and written without clobbering, so importing is idempotent and any number
//! of local sources can feed the store with none of them authoritative.
//! Storing decides nothing about trust: that is recomputed at every use from
//! the effective endorser set.
//!
//! A withdrawal, once imported, is never removed by any path here: deleting
//! one would quietly restore focus for bytes its signer took back. Only
//! expired positive Endorsements are prunable, and pruning them only ever
//! means more review.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

use serde::Serialize;
use thiserror::Error;

use super::{EndorsementError, MAX_RECORD_BYTES, Record, StatementKind, parse};
use crate::{
    canonical::Digest,
    store::{Store, share_evidence},
};

/// Directory under the store root holding imported records.
pub const DIRECTORY: &str = "endorsements";

/// Most files one directory import reads.
pub const MAX_IMPORT_FILES: usize = 4096;

/// Whether an import stored new bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportOutcome {
    /// The record was not present and is now stored.
    Imported,
    /// The exact record was already stored.
    Existing,
}

/// One stored record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ImportedRecord {
    /// Digest of the record bytes, its storage name.
    #[serde(serialize_with = "as_text")]
    pub digest: Digest,
    /// Whether it endorses or withdraws.
    pub kind: StatementKind,
    /// Whether this import stored it.
    pub outcome: ImportOutcome,
}

/// A file a directory import refused, and why.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RefusedFile {
    /// The file refused.
    pub path: String,
    /// Why.
    pub reason: String,
}

/// The result of importing a file or directory.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ImportReport {
    /// Records now present in the store.
    pub imported: Vec<ImportedRecord>,
    /// Files that were not stored.
    pub refused: Vec<RefusedFile>,
}

impl ImportReport {
    /// Counts records this import newly stored.
    #[must_use]
    pub fn newly_imported(&self) -> usize {
        self.imported
            .iter()
            .filter(|record| record.outcome == ImportOutcome::Imported)
            .count()
    }
}

/// The readable stored records, and stored files that are not.
#[derive(Debug, Default)]
pub struct StoredRecords {
    /// Records whose bytes parse and match their storage name.
    pub records: Vec<Record>,
    /// Stored files that could not be used, with the reason.
    pub unreadable: Vec<RefusedFile>,
}

/// Why records could not be imported, read or pruned.
#[derive(Debug, Error)]
pub enum ImportError {
    /// The input is not a supported, well-formed record.
    #[error(transparent)]
    Record(#[from] EndorsementError),
    /// A file or directory could not be accessed.
    #[error("cannot access {path}: {source}")]
    Io {
        /// The path involved.
        path: String,
        /// The underlying error.
        source: io::Error,
    },
    /// A stored file holds different bytes than its name promises.
    #[error("stored endorsement {0} does not match its digest")]
    Corrupt(String),
    /// A directory holds more files than one import reads.
    #[error("directory holds more than {MAX_IMPORT_FILES} files")]
    TooManyFiles,
}

/// Imports one record's bytes.
///
/// # Errors
/// Returns [`ImportError::Record`] for input that does not parse as a
/// supported canonical record (nothing is stored), [`ImportError::Corrupt`]
/// when different bytes already occupy its name, and I/O errors.
pub fn import_bytes(store: &Store, bytes: &[u8]) -> Result<ImportedRecord, ImportError> {
    let record = parse(bytes)?;
    let digest = record.digest();
    let directory = store.root().join(DIRECTORY);
    fs::create_dir_all(&directory).map_err(|source| io_error(&directory, source))?;
    let path = record_path(store, &digest);
    let mut staged = tempfile::Builder::new()
        .prefix(".endorsement-")
        .tempfile_in(&directory)
        .map_err(|source| io_error(&directory, source))?;
    share_evidence(
        &File::open(&directory).map_err(|source| io_error(&directory, source))?,
        staged.as_file(),
    )
    .map_err(|source| io_error(&path, source))?;
    staged
        .write_all(bytes)
        .and_then(|()| staged.as_file().sync_all())
        .map_err(|source| io_error(&path, source))?;
    let outcome = match staged.persist_noclobber(&path) {
        Ok(_) => ImportOutcome::Imported,
        Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
            if read_bounded(&path)? != bytes {
                return Err(ImportError::Corrupt(digest.to_string()));
            }
            ImportOutcome::Existing
        }
        Err(error) => return Err(io_error(&path, error.error)),
    };
    File::open(&directory)
        .and_then(|handle| handle.sync_all())
        .map_err(|source| io_error(&directory, source))?;
    Ok(ImportedRecord {
        digest,
        kind: record.kind(),
        outcome,
    })
}

/// Imports a record file, or every `*.json` regular file directly inside a
/// directory. Each refused file is reported; the others are still imported.
///
/// # Errors
/// Returns I/O errors for the path itself, [`ImportError::TooManyFiles`], and
/// for a single file every [`import_bytes`] error.
pub fn import_path(store: &Store, path: &Path) -> Result<ImportReport, ImportError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| io_error(path, source))?;
    if !metadata.is_dir() {
        let record = import_bytes(store, &read_bounded(path)?)?;
        return Ok(ImportReport {
            imported: vec![record],
            refused: Vec::new(),
        });
    }
    let mut files = Vec::new();
    for entry in fs::read_dir(path).map_err(|source| io_error(path, source))? {
        let entry = entry.map_err(|source| io_error(path, source))?;
        let file = entry.path();
        let is_json = file
            .extension()
            .is_some_and(|extension| extension == "json");
        let regular = entry
            .file_type()
            .map_err(|source| io_error(&file, source))?
            .is_file();
        if is_json && regular {
            files.push(file);
        }
        if files.len() > MAX_IMPORT_FILES {
            return Err(ImportError::TooManyFiles);
        }
    }
    files.sort();
    let mut report = ImportReport::default();
    for file in files {
        match read_bounded(&file).and_then(|bytes| import_bytes(store, &bytes)) {
            Ok(record) => report.imported.push(record),
            Err(error) => report.refused.push(RefusedFile {
                path: file.display().to_string(),
                reason: error.to_string(),
            }),
        }
    }
    Ok(report)
}

/// Returns the sibling `endorsements/` directory of a candidate inside a
/// skills tree (`<tree>/<skill>/` beside `<tree>/endorsements/`), if any.
#[must_use]
pub fn sibling_directory(candidate: &Path) -> Option<PathBuf> {
    let directory = candidate.parent()?.join(DIRECTORY);
    fs::symlink_metadata(&directory)
        .ok()
        .filter(fs::Metadata::is_dir)
        .map(|_| directory)
}

/// Reads every stored record, checking each against its storage name.
///
/// # Errors
/// Returns I/O errors for the directory itself; unusable files are reported
/// in [`StoredRecords::unreadable`].
pub fn records(store: &Store) -> Result<StoredRecords, ImportError> {
    let directory = store.root().join(DIRECTORY);
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(StoredRecords::default());
        }
        Err(source) => return Err(io_error(&directory, source)),
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| io_error(&directory, source))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        // Exact, case-sensitive: this store only ever writes these names.
        if name
            .strip_prefix("sha256-")
            .and_then(|rest| rest.strip_suffix(".json"))
            .is_some()
        {
            names.push(name);
        }
    }
    names.sort();
    let mut stored = StoredRecords::default();
    for name in names {
        let path = directory.join(&name);
        match read_stored(&path, &name) {
            Ok(record) => stored.records.push(record),
            Err(error) => stored.unreadable.push(RefusedFile {
                path: path.display().to_string(),
                reason: error.to_string(),
            }),
        }
    }
    Ok(stored)
}

/// Removes stored positive Endorsements that have expired at `now_ms`.
/// Withdrawals are never removed, expired or not.
///
/// # Errors
/// Returns read and removal I/O errors.
pub fn prune_expired(store: &Store, now_ms: u64) -> Result<Vec<Digest>, ImportError> {
    let mut pruned = Vec::new();
    for record in records(store)?.records {
        if record.kind() == StatementKind::Endorsement && now_ms >= record.signing().expires_at_ms {
            let digest = record.digest();
            let path = record_path(store, &digest);
            fs::remove_file(&path).map_err(|source| io_error(&path, source))?;
            pruned.push(digest);
        }
    }
    if !pruned.is_empty() {
        let directory = store.root().join(DIRECTORY);
        File::open(&directory)
            .and_then(|handle| handle.sync_all())
            .map_err(|source| io_error(&directory, source))?;
    }
    Ok(pruned)
}

fn read_stored(path: &Path, name: &str) -> Result<Record, ImportError> {
    let bytes = read_bounded(path)?;
    let record = parse(&bytes)?;
    if format!("{}.json", record.digest().directory_name()) != name {
        return Err(ImportError::Corrupt(name.to_owned()));
    }
    Ok(record)
}

fn record_path(store: &Store, digest: &Digest) -> PathBuf {
    store
        .root()
        .join(DIRECTORY)
        .join(format!("{}.json", digest.directory_name()))
}

fn read_bounded(path: &Path) -> Result<Vec<u8>, ImportError> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(
            (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK)
                .bits()
                .cast_signed(),
        )
        .open(path)
        .map_err(|source| io_error(path, source))?;
    let metadata = file.metadata().map_err(|source| io_error(path, source))?;
    if !metadata.is_file() || metadata.nlink() > 1 {
        return Err(io_error(
            path,
            io::Error::new(io::ErrorKind::InvalidData, "not a single regular file"),
        ));
    }
    let limit = u64::try_from(MAX_RECORD_BYTES).unwrap_or(u64::MAX);
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| io_error(path, source))?;
    if bytes.len() > MAX_RECORD_BYTES {
        return Err(EndorsementError::Oversized {
            size: bytes.len(),
            limit: MAX_RECORD_BYTES,
        }
        .into());
    }
    Ok(bytes)
}

fn as_text<S: serde::Serializer>(digest: &Digest, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_str(digest)
}

fn io_error(path: &Path, source: io::Error) -> ImportError {
    ImportError::Io {
        path: path.display().to_string(),
        source,
    }
}
