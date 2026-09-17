//! Operator-owned byte application; the broker supplies authorization separately.

mod client;
mod filesystem;
pub(crate) mod transfer;
pub use client::PromotionClient;
pub(crate) use transfer::copy_transfer;

use super::{SourceFiles, WorkspaceError, entries, tree, validate_inventory};
use crate::{Digest, ManifestEntry};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, MetadataExt},
    path::Path,
};

/// Identity of the opened operator destination, never an arbitrary privileged path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DestinationIdentity {
    /// Filesystem device containing the pinned directory.
    pub device: u64,
    /// Directory inode pinned for the entire application.
    pub inode: u64,
    /// Required non-root owner of checkout writes.
    pub uid: u32,
}

impl DestinationIdentity {
    /// Opens and identifies an operator-owned destination for an exact promotion preview.
    /// This blocks on filesystem I/O and grants no mutation authority.
    /// # Errors
    /// Refuses root application, foreign/writable directories, contention and filesystem failure.
    pub fn inspect(path: &Path) -> Result<Self, WorkspaceError> {
        Ok(Destination::open(path)?.identity)
    }
}

/// Exact normalized source changes displayed before the operator commits.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangePreview {
    /// Paths absent from the baseline.
    pub added: Vec<String>,
    /// Paths with different bytes or executable intent.
    pub modified: Vec<String>,
    /// Baseline paths absent from the result.
    pub deleted: Vec<String>,
}

pub(crate) struct Changes {
    pub(crate) baseline: Vec<ManifestEntry>,
    pub(crate) files: SourceFiles,
    pub(crate) preview: ChangePreview,
}

impl Changes {
    pub(crate) fn new(
        baseline: Vec<ManifestEntry>,
        files: SourceFiles,
    ) -> Result<Self, WorkspaceError> {
        validate_inventory(&baseline)?;
        let result = entries(&files);
        validate_inventory(&result)?;
        let old: std::collections::BTreeMap<_, _> = baseline.iter().map(|f| (&f.path, f)).collect();
        let new: std::collections::BTreeMap<_, _> = result.iter().map(|f| (&f.path, f)).collect();
        let mut preview = ChangePreview {
            added: Vec::new(),
            modified: Vec::new(),
            deleted: Vec::new(),
        };
        for file in &result {
            match old.get(&file.path) {
                None => preview.added.push(file.path.clone()),
                Some(previous) if **previous != *file => preview.modified.push(file.path.clone()),
                Some(_) => (),
            }
        }
        preview.deleted = baseline
            .iter()
            .filter(|f| !new.contains_key(&f.path))
            .map(|f| f.path.clone())
            .collect();
        Ok(Self {
            baseline,
            files,
            preview,
        })
    }

    pub(crate) fn steps(&self) -> usize {
        self.preview.deleted.len() + self.preview.modified.len() + self.preview.added.len()
    }
}

/// Durable local observations; an unfinished step may already have changed bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationResult {
    /// All writes, readback and final directory synchronization completed.
    pub complete: bool,
    /// Fully synchronized file effects acknowledged locally.
    pub completed_steps: usize,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "phase", deny_unknown_fields)]
pub(crate) enum StepEvent {
    Begin { index: usize },
    Done { index: usize },
}

pub(crate) struct Destination {
    root: fs::File,
    pub(crate) identity: DestinationIdentity,
}

impl Drop for Destination {
    // As in retention storage, closing alone can leave the flock held in a
    // concurrently forked child until it execs, refusing the next promotion as
    // busy (louiselm-xx07b). A failed release only delays the next open; the
    // applied bytes and their receipt are already durable.
    fn drop(&mut self) {
        let _ = rustix::fs::flock(&self.root, rustix::fs::FlockOperation::Unlock);
    }
}

impl Destination {
    pub(crate) fn open(path: &Path) -> Result<Self, WorkspaceError> {
        let root = super::filesystem::open_directory(path)?;
        let metadata = root.metadata()?;
        let uid = rustix::process::geteuid().as_raw();
        if uid == 0 || metadata.uid() != uid || metadata.mode() & 0o022 != 0 {
            return Err(WorkspaceError::Invalid(
                "promotion requires an operator-owned private-write destination",
            ));
        }
        // Serializes cooperating promotion clients. The operator must also stop
        // editor/build writers; flock cannot freeze arbitrary same-UID processes.
        rustix::fs::flock(&root, rustix::fs::FlockOperation::NonBlockingLockExclusive)
            .map_err(std::io::Error::from)?;
        Ok(Self {
            identity: DestinationIdentity {
                device: metadata.dev(),
                inode: metadata.ino(),
                uid,
            },
            root,
        })
    }

    pub(crate) fn check(&self, expected: &[ManifestEntry]) -> Result<(), WorkspaceError> {
        if entries(&tree::capture(&self.root)?) != expected {
            return Err(WorkspaceError::Invalid(
                "destination differs from the approved source; prepare and verify the combined result",
            ));
        }
        Ok(())
    }

    pub(crate) fn apply(
        &self,
        changes: &Changes,
        journal: &Path,
        mut authorize: impl FnMut(StepEvent) -> Result<(), WorkspaceError>,
    ) -> Result<ApplicationResult, WorkspaceError> {
        self.check(&changes.baseline)?;
        fs::DirBuilder::new().mode(0o700).create(journal)?;
        fs::File::open(
            journal
                .parent()
                .ok_or(WorkspaceError::Invalid("journal has no parent"))?,
        )?
        .sync_all()?;
        let mut expected = changes.baseline.clone();
        let paths = changes
            .preview
            .deleted
            .iter()
            .chain(&changes.preview.modified)
            .chain(&changes.preview.added);
        for (index, path) in paths.enumerate() {
            self.check(&expected)?;
            super::filesystem::write_file(
                &journal.join(format!("{index}.intent")),
                b"may have applied",
                0o400,
            )?;
            fs::File::open(journal)?.sync_all()?;
            authorize(StepEvent::Begin { index })?;
            filesystem::change(&self.root, path, changes.files.get(path))?;
            expected.retain(|entry| entry.path != *path);
            if let Some(file) = changes.files.get(path) {
                expected.push(ManifestEntry {
                    path: path.clone(),
                    executable: file.executable,
                    size: file.bytes.len() as u64,
                    sha256: Digest::of(&file.bytes).hex().to_owned(),
                });
                expected.sort_by(|a, b| a.path.cmp(&b.path));
            }
            self.check(&expected)?;
            super::filesystem::write_file(
                &journal.join(format!("{index}.done")),
                b"synchronized",
                0o400,
            )?;
            fs::File::open(journal)?.sync_all()?;
            authorize(StepEvent::Done { index })?;
        }
        self.check(&entries(&changes.files))?;
        self.root.sync_all()?;
        let result = ApplicationResult {
            complete: true,
            completed_steps: changes.steps(),
        };
        super::filesystem::write_file(
            &journal.join("complete.json"),
            &serde_json::to_vec(&result)?,
            0o400,
        )?;
        fs::File::open(journal)?.sync_all()?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests;
