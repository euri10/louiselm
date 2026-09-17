//! Byte-only staging and actual export from a supervisor-owned frozen workspace.

use super::{
    Deserialize, Digest, JobPreview, MAX_PLAN_BYTES, Path, Serialize, WorkspaceError, bundle,
    filesystem, fs, prepare, read,
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Inputs {
    snapshot_digest: String,
    plan_digest: String,
}

pub(crate) fn stage_inputs(
    snapshot: &Path,
    snapshot_digest: &Digest,
    plan: &Path,
    plan_digest: &Digest,
    output: &Path,
) -> Result<Digest, WorkspaceError> {
    let snapshot = fs::canonicalize(snapshot)?;
    filesystem::validate_output(output, &snapshot)?;
    let (record, files) = super::super::load_snapshot(&snapshot, snapshot_digest)?;
    let plan_root = filesystem::open_directory(
        plan.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
    )?;
    let name = plan
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or(WorkspaceError::Invalid("invalid plan path"))?;
    let plan_bytes = read(&plan_root, name, MAX_PLAN_BYTES)?;
    if Digest::of(&plan_bytes) != *plan_digest {
        return Err(WorkspaceError::Invalid("verification plan digest mismatch"));
    }
    let inputs = serde_json::to_vec(&Inputs {
        snapshot_digest: snapshot_digest.to_string(),
        plan_digest: plan_digest.to_string(),
    })?;
    filesystem::publish(output, |staging| {
        filesystem::write_files(&staging.join("snapshot/files"), &files, true)?;
        filesystem::write_file(
            &staging.join("snapshot/snapshot.json"),
            &serde_json::to_vec(&record)?,
            0o400,
        )?;
        filesystem::write_file(&staging.join("plan.json"), &plan_bytes, 0o400)?;
        filesystem::write_file(&staging.join("inputs.json"), &inputs, 0o400)
    })?;
    Ok(Digest::of(&inputs))
}

pub(crate) fn export_job(
    input: &Path,
    expected: &Digest,
    workspace: &Path,
    output: &Path,
    source_snapshot: &Digest,
) -> Result<JobPreview, WorkspaceError> {
    let root = filesystem::open_directory(input)?;
    let bytes = read(&root, "inputs.json", MAX_PLAN_BYTES)?;
    if Digest::of(&bytes) != *expected {
        return Err(WorkspaceError::Invalid(
            "verification input digest mismatch",
        ));
    }
    let inputs: Inputs = serde_json::from_slice(&bytes)?;
    if serde_json::to_vec(&inputs)? != bytes {
        return Err(WorkspaceError::Invalid("noncanonical verification input"));
    }
    let snapshot_digest = Digest::parse(&inputs.snapshot_digest)
        .map_err(|_| WorkspaceError::Invalid("invalid snapshot digest"))?;
    if snapshot_digest != *source_snapshot {
        return Err(WorkspaceError::Invalid(
            "export baseline differs from the launched source",
        ));
    }
    let plan_digest = Digest::parse(&inputs.plan_digest)
        .map_err(|_| WorkspaceError::Invalid("invalid plan digest"))?;
    filesystem::validate_output(output, input)?;
    filesystem::validate_output(output, workspace)?;
    let mut preview = None;
    filesystem::publish(output, |staging| {
        let bundle = bundle::export(
            &input.join("snapshot"),
            &snapshot_digest,
            workspace,
            &staging.join("bundle"),
        )?;
        let bundle_digest = Digest::parse(&bundle.bundle_digest)
            .map_err(|_| WorkspaceError::Invalid("invalid bundle digest"))?;
        preview = Some(prepare(
            &input.join("snapshot"),
            &snapshot_digest,
            &staging.join("bundle"),
            &bundle_digest,
            &input.join("plan.json"),
            &plan_digest,
            &staging.join("job"),
        )?);
        Ok(())
    })?;
    preview.ok_or(WorkspaceError::Invalid(
        "verification export did not publish",
    ))
}
