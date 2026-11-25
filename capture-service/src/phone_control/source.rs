//! Owner-established source freeze identities, not Run or phone authority.

use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    PhoneProjectError, PhoneProjectStore,
    hex::lower_hex,
    permissions::set_private_permissions,
    phone_projects::{Directory, directory},
};

const SCHEMA: &str = "louiselm.phone-run-source/1";
const MAX_RECORD: u64 = 65_536;
const MAX_SNAPSHOT: u64 = 8 * 1024 * 1024;
const MAX_PREVIEW: usize = 16 * 1024 * 1024;
const SKILLS: &str = "/usr/local/lib/louiselm/current/bin/louiselm-skills";

#[derive(Debug, Error)]
pub(super) enum PhoneSourceError {
    #[error("source freeze refused: {0}")]
    Invalid(&'static str),
    #[error("source freeze integration is unsupported")]
    Unsupported,
    #[error("Run source identity already exists; inspect before retrying")]
    Conflict,
    #[error("source freeze storage unavailable")]
    Io(#[from] io::Error),
    #[error("source freeze record is invalid")]
    Json(#[from] serde_json::Error),
    #[error("source project identity is unavailable")]
    Project(#[from] PhoneProjectError),
}

pub(super) struct SourcePreview {
    pub(super) snapshot_digest: String,
    pub(super) base_digest: String,
    pub(super) base_commit: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspacePreview {
    schema: String,
    snapshot_digest: String,
    base_digest: String,
    base_commit: String,
    file_count: u64,
    total_bytes: u64,
    changes: Vec<WorkspaceChange>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceChange {
    path: String,
    kind: String,
    included: bool,
}

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SourceProof {
    schema: String,
    pub(super) run_id: String,
    pub(super) project_id: String,
    pub(super) source_id: String,
    snapshot: Directory,
    pub(super) snapshot_digest: String,
    pub(super) base_digest: String,
    pub(super) base_commit: String,
    sha256: String,
}

/// Private source records only establish an owner-observed freeze event.
/// A separate checkout review, Run admission, and current phone scope are required.
pub(super) struct PhoneSourceStore {
    root: PathBuf,
}

impl PhoneSourceStore {
    pub(super) fn open(root: impl AsRef<Path>) -> Result<Self, PhoneSourceError> {
        let root = root.as_ref();
        match fs::create_dir(root) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        if !fs::symlink_metadata(root)?.file_type().is_dir() {
            return Err(PhoneSourceError::Invalid("source store is not a directory"));
        }
        set_private_permissions(root, true)?;
        Ok(Self { root: root.into() })
    }

    pub(super) fn read(&self, run_id: &str) -> Result<SourceProof, PhoneSourceError> {
        self.locked(run_id, |_| self.load(run_id))
    }

    pub(super) fn freeze(
        &self,
        projects: &PhoneProjectStore,
        project_id: &str,
        run_id: &str,
        output: &Path,
        includes: &[String],
    ) -> Result<SourceProof, PhoneSourceError> {
        self.freeze_with(
            projects,
            project_id,
            run_id,
            output,
            includes,
            |project, snapshot, selected| {
                run_skills(Path::new(SKILLS), project, snapshot, selected)
            },
        )
    }

    pub(super) fn freeze_with(
        &self,
        projects: &PhoneProjectStore,
        project_id: &str,
        run_id: &str,
        output: &Path,
        includes: &[String],
        freeze: impl FnOnce(&Path, &Path, &[String]) -> Result<SourcePreview, PhoneSourceError>,
    ) -> Result<SourceProof, PhoneSourceError> {
        if !valid_id(project_id)
            || includes.len() > 10_000
            || includes
                .iter()
                .any(|value| value.is_empty() || value.len() > 1024 || value.contains('\0'))
            || includes.iter().collect::<BTreeSet<_>>().len() != includes.len()
        {
            return Err(PhoneSourceError::Invalid("invalid selected source inputs"));
        }
        self.locked(run_id, |_| {
            let proof_path = self.path(run_id);
            match fs::symlink_metadata(&proof_path) {
                Ok(_) => return Err(PhoneSourceError::Conflict),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            let project = projects.check_current_project(project_id)?;
            let (output, parent) = self.private_output(output)?;
            let preview = freeze(&project, &output, includes)?;
            if projects.check_current_project(project_id)? != project
                || directory(Path::new(&parent.path))? != parent
            {
                return Err(PhoneSourceError::Invalid(
                    "selected source identity changed",
                ));
            }
            if !fs::symlink_metadata(&output)?.file_type().is_dir() {
                return Err(PhoneSourceError::Invalid("snapshot output was replaced"));
            }
            let snapshot = directory(&output)?;
            if Path::new(&snapshot.path) != output {
                return Err(PhoneSourceError::Invalid("snapshot output was replaced"));
            }
            verify_snapshot(&snapshot, &preview, fs::metadata(&self.root)?.uid())?;
            if projects.check_current_project(project_id)? != project
                || directory(Path::new(&parent.path))? != parent
                || !fs::symlink_metadata(&output)?.file_type().is_dir()
                || directory(&output)? != snapshot
            {
                return Err(PhoneSourceError::Invalid(
                    "selected source identity changed",
                ));
            }
            let mut proof = SourceProof {
                schema: SCHEMA.into(),
                run_id: run_id.into(),
                project_id: project_id.into(),
                source_id: Uuid::new_v4().simple().to_string(),
                snapshot,
                snapshot_digest: preview.snapshot_digest,
                base_digest: preview.base_digest,
                base_commit: preview.base_commit,
                sha256: String::new(),
            };
            proof.sha256 = proof_hash(&proof)?;
            let bytes = serde_json::to_vec(&proof)?;
            if bytes.len() as u64 > MAX_RECORD {
                return Err(PhoneSourceError::Invalid("source proof is too large"));
            }
            // Create-new is the permanent fence, including if publication is interrupted.
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&proof_path)?;
            set_private_permissions(&proof_path, false)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            File::open(&self.root)?.sync_all()?;
            Ok(proof)
        })
    }

    fn private_output(&self, output: &Path) -> Result<(PathBuf, Directory), PhoneSourceError> {
        if !output.is_absolute() {
            return Err(PhoneSourceError::Invalid("snapshot path must be absolute"));
        }
        let parent = output
            .parent()
            .ok_or(PhoneSourceError::Invalid("snapshot has no parent"))?;
        let leaf = output
            .file_name()
            .ok_or(PhoneSourceError::Invalid("snapshot has no name"))?;
        let parent = fs::canonicalize(parent)?;
        let metadata = fs::metadata(&parent)?;
        if !metadata.is_dir()
            || metadata.uid() != fs::metadata(&self.root)?.uid()
            || metadata.mode() & 0o022 != 0
        {
            return Err(PhoneSourceError::Invalid(
                "snapshot parent is not owner-controlled",
            ));
        }
        let output = parent.join(leaf);
        match fs::symlink_metadata(&output) {
            Ok(_) => return Err(PhoneSourceError::Invalid("snapshot output already exists")),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        Ok((output, directory(&parent)?))
    }

    fn path(&self, run_id: &str) -> PathBuf {
        self.root.join(format!("{run_id}.json"))
    }

    fn locked<T>(
        &self,
        run_id: &str,
        action: impl FnOnce(&File) -> Result<T, PhoneSourceError>,
    ) -> Result<T, PhoneSourceError> {
        if Uuid::parse_str(run_id).map(|id| id.to_string()) != Ok(run_id.to_owned()) {
            return Err(PhoneSourceError::Invalid("Run UUID is not canonical"));
        }
        let path = self.root.join(format!(".{run_id}.lock"));
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)?;
        set_private_permissions(&path, false)?;
        lock.lock_exclusive()?;
        let result = action(&lock);
        FileExt::unlock(&lock)?;
        result
    }

    fn load(&self, run_id: &str) -> Result<SourceProof, PhoneSourceError> {
        let mut bytes = Vec::new();
        File::open(self.path(run_id))?
            .take(MAX_RECORD + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_RECORD {
            return Err(PhoneSourceError::Invalid("source proof is too large"));
        }
        let proof: SourceProof = serde_json::from_slice(&bytes)?;
        if proof.schema != SCHEMA
            || proof.run_id != run_id
            || !valid_id(&proof.project_id)
            || !valid_id(&proof.source_id)
            || !valid_digest(&proof.snapshot_digest)
            || !valid_digest(&proof.base_digest)
            || !valid_commit(&proof.base_commit)
            || !Path::new(&proof.snapshot.path).is_absolute()
            || proof.sha256 != proof_hash(&proof)?
        {
            return Err(PhoneSourceError::Invalid("source proof integrity failed"));
        }
        Ok(proof)
    }
}

pub(super) fn run_skills(
    executable: &Path,
    project: &Path,
    output: &Path,
    includes: &[String],
) -> Result<SourcePreview, PhoneSourceError> {
    let executable = trusted_executable(executable)?;
    let timeout = trusted_executable(Path::new("/usr/bin/timeout"))?;
    let mut command = Command::new(timeout);
    command
        .args(["--signal=TERM", "--kill-after=5s", "120s"])
        .arg(executable)
        .args(["workspace", "prepare", "--repository"])
        .arg(project)
        .arg("--output")
        .arg(output)
        .arg("--robot-json");
    for selected in includes {
        command.arg("--include").arg(selected);
    }
    // The installed integration bounds source/change counts and path lengths.
    // Keep its diagnostics and content out of logs and the local response.
    let response = command
        .current_dir("/")
        .env_clear()
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()?;
    if !response.status.success() || response.stdout.len() > MAX_PREVIEW {
        return Err(PhoneSourceError::Invalid(
            "source freeze failed or is uncertain; inspect the output",
        ));
    }
    let preview: WorkspacePreview = serde_json::from_slice(&response.stdout)?;
    if preview.schema != "louiselm.workspace.preview/1"
        || !valid_digest(&preview.snapshot_digest)
        || !valid_digest(&preview.base_digest)
        || !valid_commit(&preview.base_commit)
        || preview.file_count > 10_000
        || preview.total_bytes > 128 * 1024 * 1024
        || preview.changes.len() > 10_000
        || preview.changes.iter().any(|change| {
            change.path.is_empty()
                || change.path.len() > 1025
                || change.path.contains('\0')
                || !matches!(
                    (change.kind.as_str(), change.included),
                    ("modified" | "deleted" | "untracked" | "ignored", _)
                )
        })
    {
        return Err(PhoneSourceError::Invalid("source preview is invalid"));
    }
    Ok(SourcePreview {
        snapshot_digest: preview.snapshot_digest,
        base_digest: preview.base_digest,
        base_commit: preview.base_commit,
    })
}

fn trusted_executable(path: &Path) -> Result<PathBuf, PhoneSourceError> {
    let resolved = match fs::canonicalize(path) {
        Ok(path) => path,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(PhoneSourceError::Unsupported);
        }
        Err(error) => return Err(error.into()),
    };
    let file = fs::metadata(&resolved)?;
    if !file.is_file() || file.uid() != 0 || file.mode() & 0o022 != 0 || file.mode() & 0o111 == 0 {
        return Err(PhoneSourceError::Invalid(
            "installed source integration is not trusted",
        ));
    }
    for ancestor in resolved.ancestors().skip(1) {
        let metadata = fs::metadata(ancestor)?;
        if !metadata.is_dir() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
            return Err(PhoneSourceError::Invalid(
                "installed source integration is not trusted",
            ));
        }
    }
    Ok(resolved)
}

fn verify_snapshot(
    snapshot: &Directory,
    preview: &SourcePreview,
    owner: u32,
) -> Result<(), PhoneSourceError> {
    if !valid_digest(&preview.snapshot_digest)
        || !valid_digest(&preview.base_digest)
        || !valid_commit(&preview.base_commit)
    {
        return Err(PhoneSourceError::Invalid("source preview is invalid"));
    }
    let root = Path::new(&snapshot.path);
    let root_metadata = fs::metadata(root)?;
    if root_metadata.uid() != owner || root_metadata.mode() & 0o077 != 0 {
        return Err(PhoneSourceError::Invalid(
            "snapshot is not owner-controlled",
        ));
    }
    let path = root.join("snapshot.json");
    let metadata = fs::symlink_metadata(&path)?;
    if !metadata.file_type().is_file() || metadata.uid() != owner || metadata.mode() & 0o077 != 0 {
        return Err(PhoneSourceError::Invalid(
            "snapshot record is not owner-controlled",
        ));
    }
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_SNAPSHOT + 1)
        .read_to_end(&mut bytes)?;
    let record: serde_json::Value = serde_json::from_slice(&bytes)?;
    if bytes.len() as u64 > MAX_SNAPSHOT
        || format!("sha256:{}", lower_hex(&Sha256::digest(&bytes))) != preview.snapshot_digest
        || record.get("schema").and_then(serde_json::Value::as_str)
            != Some("louiselm.workspace.snapshot/1")
        || record
            .get("base_commit")
            .and_then(serde_json::Value::as_str)
            != Some(preview.base_commit.as_str())
        || directory(root)? != *snapshot
    {
        return Err(PhoneSourceError::Invalid("snapshot does not match freeze"));
    }
    Ok(())
}

fn valid_id(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        && value.bytes().any(|byte| byte != b'0')
}

fn valid_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn valid_commit(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn proof_hash(proof: &SourceProof) -> Result<String, serde_json::Error> {
    let bytes = serde_json::to_vec(&(
        &proof.schema,
        &proof.run_id,
        &proof.project_id,
        &proof.source_id,
        &proof.snapshot,
        &proof.snapshot_digest,
        &proof.base_digest,
        &proof.base_commit,
    ))?;
    Ok(lower_hex(&Sha256::digest(bytes)))
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        reason = "test fixtures abort on setup or assertion failure"
    )]

    use super::*;

    #[test]
    fn source_lock_releases_even_with_a_cloned_descriptor_open() {
        let temporary = tempfile::tempdir().unwrap();
        let store = PhoneSourceStore::open(temporary.path().join("sources")).unwrap();
        let id = "11111111-1111-4111-8111-111111111111";
        let mut inherited = None;
        store
            .locked(id, |lock| {
                inherited = Some(lock.try_clone()?);
                Ok(())
            })
            .unwrap();
        let competing = OpenOptions::new()
            .read(true)
            .write(true)
            .open(store.root.join(format!(".{id}.lock")))
            .unwrap();
        competing.try_lock_exclusive().unwrap();
        FileExt::unlock(&competing).unwrap();
        drop(inherited);
    }
}
