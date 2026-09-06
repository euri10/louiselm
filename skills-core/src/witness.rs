//! Remote witnessing of signed Generations.
//!
//! A signature proves who approved a set. It does not prove that the approval
//! was ever seen outside this machine, which is what makes silent local
//! rollback possible. Witnessing publishes the exact signed bytes to a
//! protected Git branch and then reads them back from the remote, so a
//! Generation only becomes current after it exists somewhere the operator does
//! not solely control.
//!
//! Two properties matter more than the transport:
//!
//! * The witness ledger is append-only per Generation. A digest already
//!   published with different bytes is a refusal, never an overwrite.
//! * The commit is an ordinary commit. Branch protection on the remote is the
//!   control; a second hardware signature here would add a touch and prove
//!   nothing the first one did not.

use std::{
    fs, io,
    path::{Path, PathBuf},
    process::Command,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::canonical::Digest;

/// Evidence that a remote holds the exact signed bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WitnessEvidence {
    /// The remote the bytes were read back from.
    pub remote: String,
    /// The branch they live on.
    pub branch: String,
    /// The commit the bytes were read from.
    pub commit: String,
    /// The path within the witness repository.
    pub path: String,
    /// When the read-back succeeded.
    pub confirmed_at_ms: u64,
}

/// A witnessing operation that failed.
#[derive(Debug, Error)]
pub enum WitnessError {
    /// A local filesystem operation failed.
    #[error("witness I/O failed at '{path}': {source}")]
    Io {
        /// Path being operated on.
        path: String,
        /// Underlying failure.
        source: io::Error,
    },
    /// `git` is not installed.
    #[error("git is required to witness Generations: {0}")]
    ToolMissing(String),
    /// A git command failed.
    #[error("git {command} failed: {reason}")]
    Git {
        /// The subcommand that failed.
        command: String,
        /// What git reported.
        reason: String,
    },
    /// The remote holds different bytes for this Generation.
    #[error("witness holds different bytes for {digest}")]
    Mismatch {
        /// The contested Generation.
        digest: String,
    },
}

/// A place that can hold and return signed Generation bytes.
pub trait Witness {
    /// Returns the bytes the witness holds for `digest`, when it holds any.
    ///
    /// # Errors
    /// Returns backend-specific setup or lookup errors; no record is `Ok(None)`.
    /// Currently [`GitWitness`] also reports unsuccessful branch fetches as absence,
    /// including transport failures (tracked by louiselm-7k9w).
    fn fetch(&self, digest: &Digest) -> Result<Option<(Vec<u8>, WitnessEvidence)>, WitnessError>;

    /// Publishes `bytes` for `digest`.
    ///
    /// # Errors
    /// Returns transport/persistence failures or refuses conflicting bytes already published for this digest.
    fn publish(&self, digest: &Digest, bytes: &[u8]) -> Result<WitnessEvidence, WitnessError>;

    /// Describes the witness for robot output.
    fn describe(&self) -> String;
}

/// A witness backed by a protected Git branch.
pub struct GitWitness {
    remote: String,
    branch: String,
    workdir: PathBuf,
}

impl GitWitness {
    /// Witnesses on `branch` of `remote`, working inside `workdir`.
    #[must_use]
    pub fn new(remote: &Path, branch: &str, workdir: &Path) -> Self {
        Self {
            remote: remote.display().to_string(),
            branch: branch.to_owned(),
            workdir: workdir.to_path_buf(),
        }
    }

    fn record_path(digest: &Digest) -> String {
        format!("generations/{}.json", digest.directory_name())
    }

    fn scratch(&self) -> Result<PathBuf, WitnessError> {
        let path = self.workdir.clone();
        if path.exists() {
            fs::remove_dir_all(&path).map_err(|source| WitnessError::Io {
                path: path.display().to_string(),
                source,
            })?;
        }
        fs::create_dir_all(&path).map_err(|source| WitnessError::Io {
            path: path.display().to_string(),
            source,
        })?;
        Self::git(&path, &["init", "-q", "-b", "witness"])?;
        Self::git(&path, &["config", "user.name", "louiselm-skills"])?;
        Self::git(
            &path,
            &["config", "user.email", "louiselm-skills@localhost"],
        )?;
        Ok(path)
    }

    fn git(directory: &Path, arguments: &[&str]) -> Result<String, WitnessError> {
        let output = Command::new("git")
            .current_dir(directory)
            .args(arguments)
            .output()
            .map_err(|error| WitnessError::ToolMissing(error.to_string()))?;
        if !output.status.success() {
            return Err(WitnessError::Git {
                command: arguments.first().copied().unwrap_or("").to_owned(),
                reason: crate::scan::escape(String::from_utf8_lossy(&output.stderr).trim()),
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    fn fetch_branch(&self, directory: &Path) -> bool {
        Self::git(directory, &["fetch", "-q", &self.remote, &self.branch]).is_ok()
    }
}

impl Witness for GitWitness {
    fn fetch(&self, digest: &Digest) -> Result<Option<(Vec<u8>, WitnessEvidence)>, WitnessError> {
        let directory = self.scratch()?;
        if !self.fetch_branch(&directory) {
            return Ok(None);
        }
        let path = Self::record_path(digest);
        let Ok(commit) = Self::git(&directory, &["rev-parse", "FETCH_HEAD"]) else {
            return Ok(None);
        };
        let object = format!("FETCH_HEAD:{path}");
        let output = Command::new("git")
            .current_dir(&directory)
            .args(["cat-file", "blob", &object])
            .output()
            .map_err(|error| WitnessError::ToolMissing(error.to_string()))?;
        if !output.status.success() {
            return Ok(None);
        }
        Ok(Some((
            output.stdout,
            WitnessEvidence {
                remote: self.remote.clone(),
                branch: self.branch.clone(),
                commit,
                path,
                confirmed_at_ms: 0,
            },
        )))
    }

    fn publish(&self, digest: &Digest, bytes: &[u8]) -> Result<WitnessEvidence, WitnessError> {
        let directory = self.scratch()?;
        if self.fetch_branch(&directory) {
            Self::git(
                &directory,
                &["checkout", "-q", "-B", "witness", "FETCH_HEAD"],
            )?;
        }
        let path = Self::record_path(digest);
        let absolute = directory.join(&path);
        if let Some(parent) = absolute.parent() {
            fs::create_dir_all(parent).map_err(|source| WitnessError::Io {
                path: parent.display().to_string(),
                source,
            })?;
        }
        fs::write(&absolute, bytes).map_err(|source| WitnessError::Io {
            path: absolute.display().to_string(),
            source,
        })?;
        Self::git(&directory, &["add", &path])?;
        Self::git(
            &directory,
            &["commit", "-q", "-m", &format!("witness {digest}")],
        )?;
        let commit = Self::git(&directory, &["rev-parse", "HEAD"])?;
        Self::git(
            &directory,
            &[
                "push",
                "-q",
                &self.remote,
                &format!("witness:refs/heads/{}", self.branch),
            ],
        )?;
        Ok(WitnessEvidence {
            remote: self.remote.clone(),
            branch: self.branch.clone(),
            commit,
            path,
            confirmed_at_ms: 0,
        })
    }

    fn describe(&self) -> String {
        format!("git {} branch {}", self.remote, self.branch)
    }
}
