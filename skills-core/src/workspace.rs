//! Local source snapshots for later private Session launches.
//!
//! These blocking operator APIs freeze HEAD plus explicitly selected working
//! files. Snapshot storage must be protected from Session writers. Digests bind
//! bytes, not approval, confinement, or verification; launch integration owns
//! those decisions. No source Git metadata is copied into a workspace.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{CanonicalPath, Digest, Manifest, ManifestEntry};

pub mod bundle;
pub(crate) mod filesystem;
mod git;
pub mod launch_inputs;
pub mod promotion;
mod tree;
pub mod verification;

/// Maximum number of source files or reported changes.
pub const MAX_FILES: usize = 10_000;
/// Maximum bytes in one source file.
pub const MAX_FILE_BYTES: usize = 16 * 1024 * 1024;
/// Maximum total captured source bytes.
pub const MAX_TOTAL_BYTES: usize = 128 * 1024 * 1024;
const MAX_RECORD_BYTES: usize = 8 * 1024 * 1024;
const SNAPSHOT_SCHEMA: &str = "louiselm.workspace.snapshot/1";

/// A refused source snapshot or workspace operation. Messages omit input payloads.
#[derive(Debug, Error)]
pub enum WorkspaceError {
    /// An input violates the closed source contract.
    #[error("workspace input refused: {0}")]
    Invalid(&'static str),
    /// A filesystem operation failed; the underlying cause is retained.
    #[error("workspace filesystem operation failed; check source and destination access")]
    Io(#[from] std::io::Error),
    /// A fixed local Git operation failed; raw Git diagnostics are not displayed.
    #[error("workspace Git operation failed; check the local repository and installed Git")]
    Git,
    /// A record could not be serialized or parsed.
    #[error("invalid workspace record")]
    Record(#[from] serde_json::Error),
    /// Cache capture or private overlay validation failed.
    #[error("workspace cache input refused")]
    Cache(#[from] crate::cache::CacheError),
    /// A required Session input binding is invalid.
    #[error("workspace Session input manifest refused")]
    Input(#[from] crate::session_manifest::SessionManifestError),
}

/// A working-copy difference from the captured commit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    /// A committed file differs in bytes, mode, or kind.
    Modified,
    /// A committed file is absent from the working copy.
    Deleted,
    /// A file is absent from HEAD, including staged additions.
    Untracked,
    /// An ignored file or directory is excluded unless explicitly selected.
    Ignored,
}

/// One explicit inclusion or exclusion shown before workspace materialization.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceChange {
    /// Relative path; an excluded ignored directory ends with `/`.
    pub path: String,
    /// How the current source differs from HEAD.
    pub kind: ChangeKind,
    /// Whether this exact working-copy path was explicitly selected.
    pub included: bool,
}

/// Payload-free inspection of a frozen source snapshot, not launch authority.
#[derive(Clone, Debug, Serialize)]
pub struct SnapshotPreview {
    /// Versioned preview contract.
    pub schema: &'static str,
    /// Digest binding the canonical record and its complete source inventory.
    pub snapshot_digest: String,
    /// Digest of normalized source paths, executable bits, sizes, and hashes.
    pub base_digest: String,
    /// Exact local commit from which the baseline was read.
    pub base_commit: String,
    /// Number of captured regular files.
    pub file_count: usize,
    /// Total captured content bytes.
    pub total_bytes: u64,
    /// Included and excluded dirty, untracked, and ignored paths.
    pub changes: Vec<SourceChange>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotRecord {
    schema: String,
    base_commit: String,
    files: Vec<ManifestEntry>,
    selected: Vec<String>,
    changes: Vec<SourceChange>,
}

pub(crate) struct SourceFile {
    pub(crate) bytes: Vec<u8>,
    executable: bool,
}

type SourceFiles = BTreeMap<String, SourceFile>;

impl SnapshotRecord {
    fn preview(&self) -> Result<SnapshotPreview, WorkspaceError> {
        Ok(SnapshotPreview {
            schema: "louiselm.workspace.preview/1",
            snapshot_digest: Digest::of(&serde_json::to_vec(self)?).to_string(),
            base_digest: Digest::of(&serde_json::to_vec(&self.files)?).to_string(),
            base_commit: self.base_commit.clone(),
            file_count: self.files.len(),
            total_bytes: self.files.iter().map(|file| file.size).sum(),
            changes: self.changes.clone(),
        })
    }

    fn validate(&self) -> Result<(), WorkspaceError> {
        if self.schema != SNAPSHOT_SCHEMA || !git::valid_oid(&self.base_commit) {
            return Err(WorkspaceError::Invalid("unsupported snapshot record"));
        }
        validate_inventory(&self.files)?;
        if self.selected.len() > MAX_FILES || self.changes.len() > MAX_FILES {
            return Err(WorkspaceError::Invalid(
                "too many selected paths or changes",
            ));
        }
        for path in &self.selected {
            validate_path(path)?;
        }
        for change in &self.changes {
            let directory = change.path.ends_with('/');
            validate_path(change.path.trim_end_matches('/'))?;
            if directory && (change.kind != ChangeKind::Ignored || change.included) {
                return Err(WorkspaceError::Invalid("invalid directory selection"));
            }
            if change.included != self.selected.contains(&change.path) {
                return Err(WorkspaceError::Invalid("contradictory source selection"));
            }
        }
        if !strictly_sorted(self.selected.iter().map(String::as_str))
            || !strictly_sorted(self.changes.iter().map(|change| change.path.as_str()))
        {
            return Err(WorkspaceError::Invalid(
                "duplicate or unsorted source selection",
            ));
        }
        Ok(())
    }
}

fn strictly_sorted<'a>(items: impl Iterator<Item = &'a str>) -> bool {
    let mut previous = None;
    for item in items {
        if previous.is_some_and(|previous| previous >= item) {
            return false;
        }
        previous = Some(item);
    }
    true
}

fn validate_path(path: &str) -> Result<(), WorkspaceError> {
    CanonicalPath::parse(path, false)
        .map_err(|_| WorkspaceError::Invalid("noncanonical source path"))?;
    if path.contains('\\')
        || path
            .split('/')
            .any(|part| part.eq_ignore_ascii_case(".git"))
    {
        return Err(WorkspaceError::Invalid(
            "Git metadata or ambiguous source path",
        ));
    }
    Ok(())
}

fn validate_inventory(files: &[ManifestEntry]) -> Result<(), WorkspaceError> {
    let normalized = Manifest::new(files.to_vec(), false)
        .map_err(|_| WorkspaceError::Invalid("invalid or colliding source inventory"))?;
    if normalized.entries != files || files.len() > MAX_FILES {
        return Err(WorkspaceError::Invalid(
            "unsorted or oversized source inventory",
        ));
    }
    let mut total = 0_u64;
    let mut prefixes = BTreeMap::new();
    let names: BTreeSet<_> = files.iter().map(|file| file.path.as_str()).collect();
    for file in files {
        validate_path(&file.path)?;
        total = total
            .checked_add(file.size)
            .filter(|total| *total <= MAX_TOTAL_BYTES as u64)
            .ok_or(WorkspaceError::Invalid("source exceeds total size limit"))?;
        if file.size > MAX_FILE_BYTES as u64 {
            return Err(WorkspaceError::Invalid("source file exceeds size limit"));
        }
        let mut prefix = String::new();
        for part in file.path.split('/') {
            if !prefix.is_empty() {
                if names.contains(prefix.as_str()) {
                    return Err(WorkspaceError::Invalid("source file is also a directory"));
                }
                prefix.push('/');
            }
            prefix.push_str(part);
            if prefixes
                .insert(prefix.to_ascii_lowercase(), prefix.clone())
                .is_some_and(|old| old != prefix)
            {
                return Err(WorkspaceError::Invalid("source directory names collide"));
            }
        }
    }
    Ok(())
}

fn entries(files: &SourceFiles) -> Vec<ManifestEntry> {
    files
        .iter()
        .map(|(path, file)| ManifestEntry {
            path: path.clone(),
            executable: file.executable,
            size: file.bytes.len() as u64,
            sha256: Digest::of(&file.bytes).hex().to_owned(),
        })
        .collect()
}

/// Freezes HEAD plus selected working-copy files into a new local snapshot.
///
/// `repository` must name the top-level local checkout. `selected` contains
/// exact relative files (or tracked deletions), never glob patterns. Unselected
/// tracked files use committed bytes; untracked and ignored files need explicit
/// selection. Output parents are operator-owned and must exclude Session writers.
/// This blocks on local filesystem and Git I/O and never runs candidate code.
///
/// # Errors
/// Refuses unsupported paths/kinds, changing selected files, missing selections,
/// size limits, an existing/nested output, Git failures, and persistence failures.
pub fn prepare(
    repository: &Path,
    selected: &[String],
    output: &Path,
) -> Result<SnapshotPreview, WorkspaceError> {
    let repository = fs::canonicalize(repository)?;
    filesystem::validate_output(output, &repository)?;
    let selected_set: BTreeSet<_> = selected.iter().cloned().collect();
    if selected_set.len() != selected.len() || selected.len() > MAX_FILES {
        return Err(WorkspaceError::Invalid(
            "duplicate or excessive source selections",
        ));
    }
    for path in selected {
        validate_path(path)?;
    }
    let (base_commit, mut files) = git::baseline(&repository)?;
    let root = filesystem::open_directory(&repository)?;
    let changes = capture_changes(&repository, &root, &mut files, &selected_set)?;
    let record = SnapshotRecord {
        schema: SNAPSHOT_SCHEMA.to_owned(),
        base_commit,
        files: entries(&files),
        selected: selected_set.into_iter().collect(),
        changes,
    };
    record.validate()?;
    let bytes = serde_json::to_vec(&record)?;
    if bytes.len() > MAX_RECORD_BYTES {
        return Err(WorkspaceError::Invalid(
            "snapshot record exceeds size limit",
        ));
    }
    let preview = record.preview()?;
    filesystem::publish(output, |staging| {
        filesystem::write_files(&staging.join("files"), &files, true)?;
        filesystem::write_file(&staging.join("snapshot.json"), &bytes, 0o400)?;
        Ok(())
    })?;
    Ok(preview)
}

fn capture_changes(
    repository: &Path,
    root: &fs::File,
    files: &mut SourceFiles,
    selected: &BTreeSet<String>,
) -> Result<Vec<SourceChange>, WorkspaceError> {
    let (listed, ignored) = git::working_paths(repository)?;
    let paths: BTreeSet<_> = files
        .keys()
        .cloned()
        .chain(listed)
        .chain(selected.iter().cloned())
        .collect();
    if paths.len() + ignored.len() > MAX_FILES {
        return Err(WorkspaceError::Invalid("too many working-copy paths"));
    }
    let mut changes = BTreeMap::new();
    for path in paths {
        validate_path(&path)?;
        let included = selected.contains(&path);
        let baseline = files.get(&path);
        let kind = if baseline.is_some() {
            match filesystem::read_source(root, &path, MAX_FILE_BYTES) {
                Ok(Some(current)) => {
                    let differs = baseline.is_some_and(|base| {
                        base.bytes != current.bytes || base.executable != current.executable
                    });
                    if included {
                        files.insert(path.clone(), current);
                    }
                    differs.then_some(ChangeKind::Modified)
                }
                Ok(None) => {
                    if included {
                        files.remove(&path);
                    }
                    Some(ChangeKind::Deleted)
                }
                Err(WorkspaceError::Invalid(_)) if !included => Some(ChangeKind::Modified),
                Err(error) => return Err(error),
            }
        } else {
            if included {
                let current = filesystem::read_source(root, &path, MAX_FILE_BYTES)?
                    .ok_or(WorkspaceError::Invalid("selected source file is missing"))?;
                files.insert(path.clone(), current);
            }
            Some(
                if ignored.iter().any(|entry| {
                    entry == &path || (entry.ends_with('/') && path.starts_with(entry))
                }) {
                    ChangeKind::Ignored
                } else {
                    ChangeKind::Untracked
                },
            )
        };
        if let Some(kind) = kind {
            changes.insert(
                path.clone(),
                SourceChange {
                    path,
                    kind,
                    included,
                },
            );
        }
        // Check incrementally: many selected additions must not exhaust memory
        // before the final inventory validation can enforce the total bound.
        if files.values().map(|file| file.bytes.len()).sum::<usize>() > MAX_TOTAL_BYTES {
            return Err(WorkspaceError::Invalid("source exceeds total size limit"));
        }
    }
    for path in ignored {
        changes.entry(path.clone()).or_insert(SourceChange {
            path,
            kind: ChangeKind::Ignored,
            included: false,
        });
    }
    Ok(changes.into_values().collect())
}

/// Creates independent writable source and Git metadata from an exact snapshot.
///
/// Re-reads and hashes snapshot files through descriptors before publication.
/// The new Git baseline uses raw blobs and a fresh index, with no inherited
/// hooks, filters, remotes, alternates, or object sharing. The returned preview
/// identifies the copied bytes; it does not claim a verified Session exists.
/// Caller must protect both output parent and snapshot storage from Session writers.
///
/// # Errors
/// Refuses digest substitution, malformed/tampered snapshots, unsafe files,
/// an existing/nested output, Git failures, and persistence failures.
pub fn materialize(
    snapshot: &Path,
    expected: &Digest,
    output: &Path,
) -> Result<SnapshotPreview, WorkspaceError> {
    let snapshot = fs::canonicalize(snapshot)?;
    filesystem::validate_output(output, &snapshot)?;
    let (record, files) = load_snapshot(&snapshot, expected)?;
    filesystem::publish(output, |staging| {
        filesystem::write_files(staging, &files, false)?;
        git::initialize(staging, &files)?;
        Ok(())
    })?;
    record.preview()
}

fn load_snapshot(
    snapshot: &Path,
    expected: &Digest,
) -> Result<(SnapshotRecord, SourceFiles), WorkspaceError> {
    let root = filesystem::open_directory(snapshot)?;
    let bytes = filesystem::read_source(&root, "snapshot.json", MAX_RECORD_BYTES)?
        .ok_or(WorkspaceError::Invalid("snapshot record is missing"))?
        .bytes;
    if Digest::of(&bytes) != *expected {
        return Err(WorkspaceError::Invalid(
            "snapshot digest mismatch; inspect the selected snapshot",
        ));
    }
    let record: SnapshotRecord = serde_json::from_slice(&bytes)?;
    record.validate()?;
    if serde_json::to_vec(&record)? != bytes {
        return Err(WorkspaceError::Invalid("snapshot record is not canonical"));
    }
    let mut files = SourceFiles::new();
    for entry in &record.files {
        let file =
            filesystem::read_source(&root, &format!("files/{}", entry.path), MAX_FILE_BYTES)?
                .ok_or(WorkspaceError::Invalid("snapshot file is missing"))?;
        if file.bytes.len() as u64 != entry.size
            || file.executable != entry.executable
            || Digest::of(&file.bytes).hex() != entry.sha256
        {
            return Err(WorkspaceError::Invalid(
                "snapshot file differs from its inventory",
            ));
        }
        files.insert(entry.path.clone(), file);
    }
    Ok((record, files))
}
