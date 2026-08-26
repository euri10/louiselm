use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufReader, BufWriter, Write},
    path::{Path, PathBuf},
    process::Command,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// Input needed to persist a cold-Parked Run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunDraft {
    /// Canonical Run UUID.
    pub id: String,
    /// Agent-scoped Session identity that claimed the work.
    pub session_id: String,
    /// Beads claims to release if this Park expires.
    pub claimed_issue_ids: Vec<String>,
    /// Unix epoch milliseconds when the Park expires.
    pub park_expires_at_ms: u64,
}

/// Durable Run state retained after cleanup for Forensics correlation.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct Run {
    /// Storage schema version.
    pub schema_version: u8,
    /// Run UUID.
    pub id: String,
    /// Session identity used to form a truthful reaper actor.
    pub session_id: String,
    /// One of `cold_parked` or `disposed`.
    pub state: String,
    /// Cold-Park expiry.
    pub park_expires_at_ms: u64,
    cleanup: Vec<CleanupEntry>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct CleanupEntry {
    issue_id: String,
    completed: bool,
}

/// One pre-authorized claim-release action for the Beads adapter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReapAction {
    pub issue_id: String,
    pub actor: String,
}

/// Narrow adapter that can only release a pre-recorded Beads claim.
#[derive(Clone, Debug)]
pub struct BeadsCleanup {
    workspace: PathBuf,
}

impl BeadsCleanup {
    /// Bind cleanup to one explicit Beads workspace.
    pub fn new(workspace: impl AsRef<Path>) -> Result<Self, RunStoreError> {
        let workspace = workspace.as_ref();
        if !workspace.join(".beads").is_dir() {
            return Err(RunStoreError::Invalid(
                "Beads workspace has no .beads directory".to_owned(),
            ));
        }
        Ok(Self {
            workspace: workspace.to_path_buf(),
        })
    }

    /// Release exactly the supplied claim; it cannot create or select work.
    pub fn release(&self, action: &ReapAction) -> Result<(), String> {
        let output = Command::new("br")
            .args(self.arguments(action))
            .current_dir(&self.workspace)
            .output()
            .map_err(|error| error.to_string())?;
        if output.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
        }
    }

    /// Exact constrained command shape used for one release.
    #[must_use]
    pub fn arguments<'a>(&self, action: &'a ReapAction) -> [&'a str; 7] {
        [
            "update",
            &action.issue_id,
            "--assignee",
            "",
            "--actor",
            &action.actor,
            "--json",
        ]
    }
}

/// Durable Run-store failure.
#[derive(Debug, Error)]
pub enum RunStoreError {
    /// Run data violates the local storage contract.
    #[error("invalid run: {0}")]
    Invalid(String),
    /// Run storage failed.
    #[error("run storage failed: {0}")]
    Io(#[from] io::Error),
    /// Persisted Run data is malformed.
    #[error("run data is malformed: {0}")]
    Json(#[from] serde_json::Error),
    /// Cleanup adapter declined an action; the journal remains retryable.
    #[error("run cleanup failed: {0}")]
    Cleanup(String),
}

/// Filesystem-backed Run records and their retry-safe cleanup journals.
#[derive(Clone, Debug)]
pub struct RunStore {
    root: PathBuf,
}

impl RunStore {
    /// Open or create the Run-record root.
    pub fn new(root: impl AsRef<Path>) -> Result<Self, RunStoreError> {
        fs::create_dir_all(root.as_ref())?;
        set_private_permissions(root.as_ref(), true)?;
        Ok(Self {
            root: root.as_ref().to_path_buf(),
        })
    }

    /// Persist one cold Park. Repeating the same record is safe.
    pub fn park_cold(&self, draft: RunDraft) -> Result<(), RunStoreError> {
        validate_draft(&draft)?;
        let path = self.path(&draft.id);
        let run = Run {
            schema_version: 1,
            id: draft.id,
            session_id: draft.session_id,
            state: "cold_parked".to_owned(),
            park_expires_at_ms: draft.park_expires_at_ms,
            cleanup: draft
                .claimed_issue_ids
                .into_iter()
                .map(|issue_id| CleanupEntry {
                    issue_id,
                    completed: false,
                })
                .collect(),
        };
        if path.exists() {
            return if self.run(&run.id)? == run {
                Ok(())
            } else {
                Err(RunStoreError::Invalid(
                    "Run UUID conflicts with existing record".to_owned(),
                ))
            };
        }
        write_atomic(&path, &run)
    }

    /// Load one retained Run record.
    pub fn run(&self, id: &str) -> Result<Run, RunStoreError> {
        validate_id(id)?;
        let path = self.path(id);
        if !path.is_file() {
            return Err(RunStoreError::Invalid("Run was not found".to_owned()));
        }
        let run: Run = serde_json::from_reader(BufReader::new(File::open(path)?))?;
        if run.schema_version != 1 || run.id != id {
            return Err(RunStoreError::Invalid("stored Run is invalid".to_owned()));
        }
        Ok(run)
    }

    /// Reap due cold Parks through a narrow, caller-supplied cleanup adapter.
    ///
    /// A failed action is left incomplete and retried later. An adapter must make
    /// releasing a claim idempotent because a crash after its external effect and
    /// before this journal is written can replay that one action.
    pub fn reap_expired(
        &self,
        now_ms: u64,
        mut cleanup: impl FnMut(&ReapAction) -> Result<(), String>,
    ) -> Result<usize, RunStoreError> {
        let mut reaped = 0;
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let Some(id) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.strip_suffix(".json"))
                .map(str::to_owned)
            else {
                continue;
            };
            if validate_id(&id).is_err() {
                continue;
            }
            let mut run = self.run(&id)?;
            if run.state != "cold_parked" || run.park_expires_at_ms > now_ms {
                continue;
            }
            for index in 0..run.cleanup.len() {
                if run.cleanup[index].completed {
                    continue;
                }
                let action = ReapAction {
                    issue_id: run.cleanup[index].issue_id.clone(),
                    actor: format!("reaper/{}", run.session_id),
                };
                cleanup(&action).map_err(RunStoreError::Cleanup)?;
                run.cleanup[index].completed = true;
                write_atomic(&self.path(&id), &run)?;
            }
            run.state = "disposed".to_owned();
            write_atomic(&self.path(&id), &run)?;
            reaped += 1;
        }
        Ok(reaped)
    }

    fn path(&self, id: &str) -> PathBuf {
        self.root.join(format!("{id}.json"))
    }
}

fn validate_draft(draft: &RunDraft) -> Result<(), RunStoreError> {
    validate_id(&draft.id)?;
    if draft.session_id.is_empty()
        || draft.park_expires_at_ms == 0
        || draft.claimed_issue_ids.iter().any(|id| id.is_empty())
    {
        return Err(RunStoreError::Invalid(
            "Run fields must be non-empty and expiry positive".to_owned(),
        ));
    }
    Ok(())
}
fn validate_id(id: &str) -> Result<(), RunStoreError> {
    let parsed = Uuid::parse_str(id)
        .map_err(|_| RunStoreError::Invalid("Run id must be a UUID".to_owned()))?;
    if parsed.to_string() != id.to_ascii_lowercase() {
        return Err(RunStoreError::Invalid(
            "Run id must use canonical UUID text".to_owned(),
        ));
    }
    Ok(())
}
fn write_atomic(path: &Path, value: &impl Serialize) -> Result<(), RunStoreError> {
    let temporary = path.with_file_name(format!(".run-{}", Uuid::new_v4()));
    let result = (|| {
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        let mut writer = BufWriter::new(file);
        serde_json::to_writer_pretty(&mut writer, value)?;
        writer.write_all(b"\n")?;
        writer.flush()?;
        writer.get_ref().sync_all()?;
        fs::rename(&temporary, path)?;
        set_private_permissions(path, false)?;
        File::open(path.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "Run path has no parent")
        })?)?
        .sync_all()?;
        Ok(())
    })();
    if temporary.exists() {
        let _ = fs::remove_file(temporary);
    }
    result
}

#[cfg(unix)]
fn set_private_permissions(path: &Path, directory: bool) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(
        path,
        fs::Permissions::from_mode(if directory { 0o700 } else { 0o600 }),
    )
}

#[cfg(not(unix))]
fn set_private_permissions(_path: &Path, _directory: bool) -> io::Result<()> {
    Ok(())
}
