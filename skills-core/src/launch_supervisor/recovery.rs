//! Bounded recovery retention, separate from broker eligibility and Run policy.
//!
//! Only the installed deterministic Agent has a supported layout. Its two
//! closed counter records contain no credentials or capability material. This
//! is not a vendor ACP recovery contract. The broker must authenticate the
//! supervisor response; serialized metadata alone grants no authority.

use std::{
    fs::{self, File},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[path = "recovery_restore.rs"]
mod restoration;
use restoration::restore;

pub use crate::launch_protocol::{RetentionEvidence, RetentionRequest};

use crate::{
    Digest,
    launch::LaunchRequest,
    workspace::{WorkspaceError, filesystem},
};

const CONTRACT: &str = "louiselm.test-recovery/1";
const EVIDENCE_SCHEMA: &str = crate::launch_protocol::RETENTION_EVIDENCE_SCHEMA;
const CHECKPOINT: &str = "home/recovery.json";
const WORKSPACE: &str = "workspace/recovery-counter.json";
const DIRECTORY: &str = "retained-recovery";
const MAX_BYTES: usize = 16 * 1024;

#[cfg(test)]
std::thread_local! {
    pub(super) static FAIL_SEAL_SYNC: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static FAIL_PUBLICATION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Expected retention failures, without exposing file contents or credentials.
#[derive(Debug, Error)]
pub enum RecoveryError {
    /// No verified recovery contract exists for the actual launched Agent.
    #[error("Agent recovery layout is unverified")]
    Unsupported,
    /// The process tree is not frozen, or an owned asynchronous operation is active.
    #[error("recovery retention requires an idle frozen Session")]
    NotParked,
    /// Metadata, layout, content or storage ownership violated the closed contract.
    #[error("recovery material or binding is invalid")]
    Invalid,
    /// This Session already retained a different operation; do not replace it.
    #[error("Session already has a different retained recovery point")]
    Conflict,
    /// The original Park retention deadline has passed.
    #[error("recovery retention expired")]
    Expired,
    /// A filesystem operation failed; its cause is retained for trusted diagnostics.
    #[error("recovery storage unavailable")]
    Io(#[from] std::io::Error),
    /// A bounded descriptor-relative read or durable publication failed.
    #[error("recovery storage validation or persistence failed")]
    Storage(#[from] WorkspaceError),
}

/// Exactly-once worker completion; callbacks must not block the owning worker.
pub type RecoveryCompletion = Box<dyn FnOnce(Result<RetentionEvidence, RecoveryError>) + Send>;
/// Exactly-once durable-copy completion; never proof of a successful ACP load.
pub type RestoreCompletion =
    Box<dyn FnOnce(Result<crate::launch_protocol::RecoveryRestoreRequest, RecoveryError>) + Send>;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Checkpoint {
    schema: String,
    acp_session_id: String,
    counter: u64,
}

/// Pinned privileged storage, owned by the same adapter as the process tree.
pub(super) struct SessionStorage {
    root: File,
    directory: PathBuf,
    launch: LaunchRequest,
    integration_digest: String,
}

impl SessionStorage {
    pub(super) fn restore(
        &self,
        request: &crate::launch_protocol::RecoveryRestoreRequest,
        now_ms: u64,
    ) -> Result<(), RecoveryError> {
        if request.target != self.launch || !valid_id(&request.source.launch.session_id) {
            return Err(RecoveryError::Invalid);
        }
        let parent = self.directory.parent().ok_or(RecoveryError::Invalid)?;
        restore(
            &parent.join(&request.source.launch.session_id),
            &self.directory,
            request,
            now_ms,
        )
    }
    pub(super) fn open(
        directory: &Path,
        launch: &LaunchRequest,
        integration: &super::ToolIsolationEvidence,
    ) -> Result<Self, RecoveryError> {
        launch.validate().map_err(|_| RecoveryError::Invalid)?;
        let root = filesystem::open_directory(directory)?;
        let metadata = root.metadata()?;
        if metadata.uid() != 0 || metadata.mode() & 0o7777 != 0o711 {
            return Err(RecoveryError::Invalid);
        }
        Ok(Self {
            root,
            directory: directory.to_owned(),
            launch: launch.clone(),
            integration_digest: Digest::of(&integration.canonical_bytes()).to_string(),
        })
    }

    pub(super) fn retain(
        &self,
        request: &RetentionRequest,
        now_ms: u64,
    ) -> Result<RetentionEvidence, RecoveryError> {
        retain(
            &self.root,
            &self.directory,
            &self.launch,
            &self.integration_digest,
            request,
            now_ms,
        )
    }

    pub(super) fn seal(&self) -> Result<(), RecoveryError> {
        // Old UID ownership of home/workspace must not expose the source copy
        // when the identity pool reuses that UID. Never chmod an unpinned path.
        self.root
            .set_permissions(fs::Permissions::from_mode(0o700))?;
        #[cfg(test)]
        if FAIL_SEAL_SYNC.replace(false) {
            return Err(std::io::Error::other("injected directory sync failure").into());
        }
        self.root.sync_all()?;
        Ok(())
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn read(root: &File, name: &str) -> Result<Vec<u8>, RecoveryError> {
    filesystem::read_source(root, name, MAX_BYTES)?
        .map(|file| file.bytes)
        .ok_or(RecoveryError::Invalid)
}

fn material(root: &File, acp_id: &str) -> Result<(Vec<u8>, Vec<u8>), RecoveryError> {
    let checkpoint = read(root, CHECKPOINT)?;
    let parsed: Checkpoint =
        serde_json::from_slice(&checkpoint).map_err(|_| RecoveryError::Invalid)?;
    let workspace = read(root, WORKSPACE)?;
    if parsed.schema != CONTRACT
        || parsed.acp_session_id != acp_id
        || serde_json::to_vec(&parsed).map_err(|_| RecoveryError::Invalid)? != checkpoint
        || parsed.counter.to_string().as_bytes() != workspace
    {
        return Err(RecoveryError::Invalid);
    }
    Ok((checkpoint, workspace))
}

fn material_digest(checkpoint: &[u8], workspace: &[u8]) -> String {
    // Length-prefix the first blob so no two pairs have the same concatenation.
    let mut bytes = (checkpoint.len() as u64).to_be_bytes().to_vec();
    bytes.extend_from_slice(checkpoint);
    bytes.extend_from_slice(workspace);
    Digest::of(&bytes).to_string()
}

fn retained(
    path: &Path,
    launch: &LaunchRequest,
    integration_digest: &str,
    request: &RetentionRequest,
) -> Result<RetentionEvidence, RecoveryError> {
    let root = filesystem::open_directory(path)?;
    let meta = root.metadata()?;
    if meta.uid() != rustix::process::geteuid().as_raw() || meta.mode() & 0o7777 != 0o700 {
        return Err(RecoveryError::Invalid);
    }
    let bytes = read(&root, "evidence.json")?;
    let evidence: RetentionEvidence =
        serde_json::from_slice(&bytes).map_err(|_| RecoveryError::Invalid)?;
    if evidence.launch != *launch
        || evidence.request != *request
        || evidence.integration_digest != integration_digest
    {
        return Err(RecoveryError::Conflict);
    }
    let (checkpoint, workspace) = material(&root, &request.acp_session_id)?;
    if evidence.schema != EVIDENCE_SCHEMA
        || evidence.contract != CONTRACT
        || evidence.material_digest != material_digest(&checkpoint, &workspace)
        || serde_json::to_vec(&evidence).map_err(|_| RecoveryError::Invalid)? != bytes
    {
        return Err(RecoveryError::Invalid);
    }
    // A retry can follow a rename that succeeded before directory fsync failed.
    // Re-establish durability before returning the original evidence.
    root.sync_all()?;
    Ok(evidence)
}

fn retain(
    root: &File,
    directory: &Path,
    launch: &LaunchRequest,
    integration_digest: &str,
    request: &RetentionRequest,
    now_ms: u64,
) -> Result<RetentionEvidence, RecoveryError> {
    launch.validate().map_err(|_| RecoveryError::Invalid)?;
    if !valid_id(&request.request_id)
        || !valid_id(&request.acp_session_id)
        || Digest::parse(integration_digest).is_err()
    {
        return Err(RecoveryError::Invalid);
    }
    if now_ms >= request.expires_at_ms {
        return Err(RecoveryError::Expired);
    }
    let path = directory.join(DIRECTORY);
    match fs::symlink_metadata(&path) {
        Ok(_) => {
            let evidence = retained(&path, launch, integration_digest, request)?;
            root.sync_all()?;
            return Ok(evidence);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let (checkpoint, workspace) = material(root, &request.acp_session_id)?;
    let evidence = RetentionEvidence {
        schema: EVIDENCE_SCHEMA.into(),
        launch: launch.clone(),
        request: request.clone(),
        contract: CONTRACT.into(),
        integration_digest: integration_digest.to_owned(),
        material_digest: material_digest(&checkpoint, &workspace),
    };
    let bytes = serde_json::to_vec(&evidence).map_err(|_| RecoveryError::Invalid)?;
    // ponytail: one immutable point per Session, two bounded files. Replacement
    // histories and vendor layouts need their own verified contracts, not flags.
    filesystem::publish(&path, |staging| {
        fs::set_permissions(staging, fs::Permissions::from_mode(0o700))?;
        fs::create_dir(staging.join("home"))?;
        fs::create_dir(staging.join("workspace"))?;
        filesystem::write_file(&staging.join(CHECKPOINT), &checkpoint, 0o600)?;
        filesystem::write_file(&staging.join(WORKSPACE), &workspace, 0o600)?;
        #[cfg(test)]
        if FAIL_PUBLICATION.replace(false) {
            return Err(std::io::Error::other("injected interrupted publication").into());
        }
        filesystem::write_file(&staging.join("evidence.json"), &bytes, 0o600)
    })?;
    Ok(evidence)
}

#[cfg(test)]
#[path = "recovery_tests.rs"]
mod tests;
