//! Bounded exact-byte transfer; no serialized record grants application authority.

use super::Changes;
use crate::{
    Digest, ManifestEntry,
    workspace::{
        self, WorkspaceError,
        verification::{self, JobPreview},
    },
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
};

const MAX_HEADER: usize = 16 * 1024 * 1024;

pub(crate) fn configure_stream(
    stream: &std::os::unix::net::UnixStream,
) -> Result<(), WorkspaceError> {
    stream.set_read_timeout(Some(std::time::Duration::from_secs(30)))?;
    stream.set_write_timeout(Some(std::time::Duration::from_secs(30)))?;
    Ok(())
}

pub(crate) fn remaining(expires: u64) -> Result<std::time::Duration, WorkspaceError> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
        .ok_or(WorkspaceError::Invalid("promotion clock unavailable"))?;
    let remaining = expires
        .checked_sub(now)
        .filter(|ms| (1..=300_000).contains(ms))
        .ok_or(WorkspaceError::Invalid(
            "promotion authorization expired or exceeds five minutes",
        ))?;
    Ok(std::time::Duration::from_millis(remaining))
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    job: JobPreview,
    baseline: Vec<ManifestEntry>,
    files: Vec<ManifestEntry>,
}

pub(crate) fn send<T: Serialize>(stream: &mut impl Write, value: &T) -> Result<(), WorkspaceError> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > MAX_HEADER {
        return Err(WorkspaceError::Invalid("promotion frame exceeds bound"));
    }
    let length = u32::try_from(bytes.len())
        .map_err(|_| WorkspaceError::Invalid("promotion frame exceeds bound"))?;
    stream.write_all(&length.to_be_bytes())?;
    stream.write_all(&bytes)?;
    Ok(())
}

pub(crate) fn receive<T: DeserializeOwned>(stream: &mut impl Read) -> Result<T, WorkspaceError> {
    let mut size = [0_u8; 4];
    stream.read_exact(&mut size)?;
    let size = u32::from_be_bytes(size) as usize;
    if size > MAX_HEADER {
        return Err(WorkspaceError::Invalid("promotion frame exceeds bound"));
    }
    let mut bytes = vec![0_u8; size];
    stream.read_exact(&mut bytes)?;
    Ok(serde_json::from_slice(&bytes)?)
}

pub(crate) fn load(directory: &Path, job: &JobPreview) -> Result<Changes, WorkspaceError> {
    let (snapshot, _) =
        workspace::load_snapshot(&directory.join("snapshot"), &digest(&job.snapshot_digest)?)?;
    let loaded = verification::load(&directory.join("job"), &digest(&job.job_digest)?)?;
    if loaded.preview != *job || snapshot.preview()?.base_digest != job.base_digest {
        return Err(WorkspaceError::Invalid("promotion input identity mismatch"));
    }
    Changes::new(snapshot.files, loaded.files)
}

pub(crate) fn send_changes(
    stream: &mut impl Write,
    changes: &Changes,
    job: &JobPreview,
) -> Result<(), WorkspaceError> {
    send(
        stream,
        &Header {
            job: job.clone(),
            baseline: changes.baseline.clone(),
            files: workspace::entries(&changes.files),
        },
    )?;
    for file in changes.files.values() {
        stream.write_all(&file.bytes)?;
    }
    Ok(())
}

pub(crate) fn receive_changes(
    stream: &mut impl Read,
    job: &JobPreview,
) -> Result<Changes, WorkspaceError> {
    let header: Header = receive(stream)?;
    workspace::validate_inventory(&header.baseline)?;
    workspace::validate_inventory(&header.files)?;
    if header.job != *job
        || Digest::of(&serde_json::to_vec(&header.baseline)?).to_string() != job.base_digest
        || Digest::of(&serde_json::to_vec(&header.files)?).to_string() != job.result_digest
    {
        return Err(WorkspaceError::Invalid(
            "promotion transfer does not match approved inputs",
        ));
    }
    let mut files = workspace::SourceFiles::new();
    for file in header.files {
        let size = usize::try_from(file.size)
            .map_err(|_| WorkspaceError::Invalid("promotion file exceeds bound"))?;
        let mut bytes = vec![0_u8; size];
        stream.read_exact(&mut bytes)?;
        if Digest::of(&bytes).hex() != file.sha256 {
            return Err(WorkspaceError::Invalid("promotion payload digest mismatch"));
        }
        files.insert(
            file.path,
            workspace::SourceFile {
                bytes,
                executable: file.executable,
            },
        );
    }
    Changes::new(header.baseline, files)
}

pub(crate) fn copy_transfer(
    snapshot: &Path,
    job_path: &Path,
    job: &JobPreview,
    output: &Path,
) -> Result<(), WorkspaceError> {
    let (record, baseline) = workspace::load_snapshot(snapshot, &digest(&job.snapshot_digest)?)?;
    let loaded = verification::load(job_path, &digest(&job.job_digest)?)?;
    if loaded.preview != *job || record.preview()?.base_digest != job.base_digest {
        return Err(WorkspaceError::Invalid(
            "promotion transfer identity mismatch",
        ));
    }
    if output.try_exists()? {
        load(output, job)?;
        return Ok(());
    }
    let root = workspace::filesystem::open_directory(job_path)?;
    let read = |name| {
        workspace::filesystem::read_source(&root, name, workspace::MAX_RECORD_BYTES)?
            .ok_or(WorkspaceError::Invalid("missing promotion input"))
    };
    let job_bytes = read("job.json")?.bytes;
    let plan = read("plan.json")?.bytes;
    workspace::filesystem::publish(output, |staging| {
        workspace::filesystem::write_files(&staging.join("snapshot/files"), &baseline, true)?;
        workspace::filesystem::write_file(
            &staging.join("snapshot/snapshot.json"),
            &serde_json::to_vec(&record)?,
            0o400,
        )?;
        workspace::filesystem::write_files(&staging.join("job/source"), &loaded.files, true)?;
        workspace::filesystem::write_file(&staging.join("job/job.json"), &job_bytes, 0o400)?;
        workspace::filesystem::write_file(&staging.join("job/plan.json"), &plan, 0o400)
    })?;
    fs::File::open(output)?.sync_all()?;
    Ok(())
}

fn digest(value: &str) -> Result<Digest, WorkspaceError> {
    Digest::parse(value).map_err(|_| WorkspaceError::Invalid("invalid promotion digest"))
}
