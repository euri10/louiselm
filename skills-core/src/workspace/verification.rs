//! Prepared verification inputs, not verification or promotion authority.
//!
//! Blocking operator filesystem APIs; keep input stores and output parents
//! protected from Session writers. No command is executed here. A future
//! confined verifier must consume the exact job under separate authorization.

use std::{fs, path::Path};

use serde::{Deserialize, Serialize};

use super::{
    MAX_RECORD_BYTES, WorkspaceError, bundle, entries, filesystem, tree, validate_inventory,
    validate_path,
};
use crate::{Digest, ManifestEntry};

#[cfg(test)]
#[path = "verification_tests.rs"]
mod tests;

mod inputs;
pub(crate) use inputs::{export_job, stage_inputs};

const SCHEMA: &str = "louiselm.workspace.verification-job/1";
const MAX_PLAN_BYTES: usize = 64 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    schema: String,
    commands: Vec<Command>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Command {
    pub(crate) argv: Vec<String>,
    pub(crate) cwd: String,
    pub(crate) timeout_ms: u64,
}

impl Plan {
    fn parse(bytes: &[u8], files: &[ManifestEntry]) -> Result<Self, WorkspaceError> {
        let plan: Self = serde_json::from_slice(bytes)?;
        if plan.schema != "louiselm.workspace.verification-plan/1"
            || plan.commands.is_empty()
            || plan.commands.len() > 32
        {
            return Err(WorkspaceError::Invalid(
                "unsupported or unbounded verification plan",
            ));
        }
        let mut total_ms = 0_u64;
        for command in &plan.commands {
            if command.argv.is_empty()
                || command.argv.len() > 128
                || command.argv[0].is_empty()
                || command.argv[0].starts_with('-')
                || command
                    .argv
                    .iter()
                    .any(|arg| arg.len() > 4096 || arg.contains('\0'))
                || command.timeout_ms == 0
            {
                return Err(WorkspaceError::Invalid("invalid verification command"));
            }
            total_ms = total_ms
                .checked_add(command.timeout_ms)
                .filter(|total| *total <= 3_600_000)
                .ok_or(WorkspaceError::Invalid(
                    "verification plan exceeds one-hour budget",
                ))?;
            if command.cwd != "." {
                validate_path(&command.cwd)?;
                let prefix = format!("{}/", command.cwd);
                if !files.iter().any(|file| file.path.starts_with(&prefix)) {
                    return Err(WorkspaceError::Invalid(
                        "verification directory is absent from source inventory",
                    ));
                }
            }
        }
        Ok(plan)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Job {
    schema: String,
    snapshot_digest: String,
    bundle_digest: String,
    base_digest: String,
    plan_digest: String,
    files: Vec<ManifestEntry>,
}

/// Payload-free identities shared by human and robot inspection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobPreview {
    /// Versioned preview contract.
    pub schema: String,
    /// Always prepared; this command never executes a plan or grants authority.
    pub state: String,
    /// Digest of the canonical job binding all inputs below.
    pub job_digest: String,
    /// Exact source snapshot record digest.
    pub snapshot_digest: String,
    /// Exact proposed byte bundle digest.
    pub bundle_digest: String,
    /// Normalized baseline source inventory digest.
    pub base_digest: String,
    /// Normalized proposed source inventory digest, including executable bits.
    pub result_digest: String,
    /// Digest of the exact plan bytes, including whitespace.
    pub plan_digest: String,
    /// Number of required commands; their contents are deliberately omitted.
    pub command_count: usize,
}

impl Job {
    fn preview(&self, plan: &Plan) -> Result<JobPreview, WorkspaceError> {
        Ok(JobPreview {
            schema: "louiselm.workspace.verification-preview/1".into(),
            state: "prepared".into(),
            job_digest: Digest::of(&serde_json::to_vec(self)?).to_string(),
            snapshot_digest: self.snapshot_digest.clone(),
            bundle_digest: self.bundle_digest.clone(),
            base_digest: self.base_digest.clone(),
            result_digest: Digest::of(&serde_json::to_vec(&self.files)?).to_string(),
            plan_digest: self.plan_digest.clone(),
            command_count: plan.commands.len(),
        })
    }
}

/// Prepares a fresh read-only source tree and exact bounded verification plan.
///
/// Requires three independently selected digests; a digest is identity, not
/// approval. Never executes Git, hooks or the plan. This blocks on filesystem
/// I/O. Caller protects inputs and the output parent from Session writers.
///
/// # Errors
/// Refuses substituted/invalid inputs, unsafe payloads, unbounded plans,
/// existing/nested outputs and I/O failures. A post-rename sync failure may
/// leave the complete job present; inspect before retrying.
pub fn prepare(
    snapshot: &Path,
    snapshot_digest: &Digest,
    bundle: &Path,
    bundle_digest: &Digest,
    plan: &Path,
    plan_digest: &Digest,
    output: &Path,
) -> Result<JobPreview, WorkspaceError> {
    let snapshot = fs::canonicalize(snapshot)?;
    let bundle = fs::canonicalize(bundle)?;
    filesystem::validate_output(output, &snapshot)?;
    filesystem::validate_output(output, &bundle)?;
    let (preview, files) = bundle::load(&snapshot, snapshot_digest, &bundle, bundle_digest)?;
    let plan_root = filesystem::open_directory(
        plan.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
    )?;
    let plan_name = plan
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(WorkspaceError::Invalid("invalid verification plan path"))?;
    let bytes = read(&plan_root, plan_name, MAX_PLAN_BYTES)?;
    if Digest::of(&bytes) != *plan_digest {
        return Err(WorkspaceError::Invalid("verification plan digest mismatch"));
    }
    let inventory = entries(&files);
    let plan = Plan::parse(&bytes, &inventory)?;
    let job = Job {
        schema: SCHEMA.to_owned(),
        snapshot_digest: snapshot_digest.to_string(),
        bundle_digest: preview.bundle_digest,
        base_digest: preview.base_digest,
        plan_digest: plan_digest.to_string(),
        files: inventory,
    };
    let record = serde_json::to_vec(&job)?;
    if record.len() > MAX_RECORD_BYTES {
        return Err(WorkspaceError::Invalid(
            "verification record exceeds size limit",
        ));
    }
    filesystem::publish(output, |staging| {
        filesystem::write_files(&staging.join("source"), &files, true)?;
        filesystem::write_file(&staging.join("plan.json"), &bytes, 0o400)?;
        filesystem::write_file(&staging.join("job.json"), &record, 0o400)
    })?;
    job.preview(&plan)
}

/// Remeasures a prepared job's complete source inventory and exact plan bytes.
///
/// Blocks on filesystem I/O; caller excludes writers for the entire check.
/// Success means only that prepared bytes match the selected digest, not that
/// any command ran, confinement exists, or promotion is authorized.
///
/// # Errors
/// Refuses wrong/noncanonical records, extra/missing/changed source files or
/// executable bits, Git metadata, changed/invalid plans and filesystem failures.
pub fn inspect(job: &Path, expected: &Digest) -> Result<JobPreview, WorkspaceError> {
    Ok(load(job, expected)?.preview)
}

pub(crate) struct LoadedJob {
    pub(crate) preview: JobPreview,
    pub(crate) commands: Vec<Command>,
    pub(crate) files: super::SourceFiles,
}

pub(crate) fn load(job: &Path, expected: &Digest) -> Result<LoadedJob, WorkspaceError> {
    let root = filesystem::open_directory(job)?;
    let record = read(&root, "job.json", MAX_RECORD_BYTES)?;
    if Digest::of(&record) != *expected {
        return Err(WorkspaceError::Invalid("verification job digest mismatch"));
    }
    let job: Job = serde_json::from_slice(&record)?;
    if job.schema != SCHEMA || serde_json::to_vec(&job)? != record {
        return Err(WorkspaceError::Invalid(
            "unsupported or noncanonical verification job",
        ));
    }
    for digest in [
        &job.snapshot_digest,
        &job.bundle_digest,
        &job.base_digest,
        &job.plan_digest,
    ] {
        Digest::parse(digest)
            .map_err(|_| WorkspaceError::Invalid("invalid verification identity"))?;
    }
    validate_inventory(&job.files)?;
    let bytes = read(&root, "plan.json", MAX_PLAN_BYTES)?;
    if Digest::of(&bytes).to_string() != job.plan_digest {
        return Err(WorkspaceError::Invalid("verification plan digest mismatch"));
    }
    let plan = Plan::parse(&bytes, &job.files)?;
    let source = rustix::fs::openat(
        &root,
        "source",
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map(fs::File::from)
    .map_err(std::io::Error::from)?;
    match rustix::fs::statat(&source, ".git", rustix::fs::AtFlags::SYMLINK_NOFOLLOW) {
        Ok(_) => {
            return Err(WorkspaceError::Invalid(
                "verification source contains Git metadata",
            ));
        }
        Err(rustix::io::Errno::NOENT) => (),
        Err(error) => return Err(std::io::Error::from(error).into()),
    }
    let files = tree::capture(&source)?;
    if entries(&files) != job.files {
        return Err(WorkspaceError::Invalid(
            "verification source differs from its inventory",
        ));
    }
    Ok(LoadedJob {
        preview: job.preview(&plan)?,
        commands: plan.commands,
        files,
    })
}

fn read(root: &fs::File, path: &str, limit: usize) -> Result<Vec<u8>, WorkspaceError> {
    filesystem::read_source(root, path, limit)?
        .map(|file| file.bytes)
        .ok_or(WorkspaceError::Invalid("verification input is missing"))
}
