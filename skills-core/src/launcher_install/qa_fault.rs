//! Reviewed permissions-only fault of a retired QA pin; bytes/history survive.
use super::{
    LAUNCHER_RELATIVE_PATH, LauncherConfig, LauncherError, LauncherPaths, MAX_STATE_BYTES, io_error,
};
use crate::broker::{AuthorizationStore, run_envelope::RunEnvelopeStore};
use crate::{
    Digest,
    release::{MANIFEST_SCHEMA, ReleaseManifest},
};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const SCHEMA: &str = "louiselm.qa.retired-release-fault/1";
const BROKER_STATE: &str = "/var/lib/louiselm/broker";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Plan {
    schema: String,
    baseline_release_id: String,
    retired_release_id: String,
    default_release_id: String,
    run_id: String,
    session_id: String,
    operator_uid: u32,
    launcher_digest: String,
    size: u64,
    device: u64,
    inode: u64,
    original_mode: u32,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    plan: Plan,
    restored: bool,
}

fn refused() -> LauncherError {
    LauncherError::Invalid("retired QA release fault review or retained authority mismatch".into())
}

fn check_ids(baseline: &str, retired: &str, current: &str) -> Result<(), LauncherError> {
    for id in [baseline, retired, current] {
        if !Digest::parse(id).is_ok_and(|digest| digest.to_string() == id) {
            return Err(refused());
        }
    }
    if baseline == retired || baseline == current || retired == current {
        return Err(refused());
    }
    Ok(())
}

fn trusted_directory(path: &Path, owner: u32) -> Result<(), LauncherError> {
    for parent in path.ancestors() {
        let metadata = fs::symlink_metadata(parent).map_err(|e| io_error("QA authority", e))?;
        if !metadata.is_dir()
            || ![0, owner].contains(&metadata.uid())
            || metadata.mode() & 0o022 != 0
        {
            return Err(refused());
        }
    }
    Ok(())
}

fn measured_file(path: &Path, owner: u32, limit: u64) -> Result<(File, Vec<u8>), LauncherError> {
    trusted_directory(path.parent().ok_or_else(refused)?, owner)?;
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(
            rustix::fs::OFlags::NOFOLLOW.bits().cast_signed()
                | rustix::fs::OFlags::NONBLOCK.bits().cast_signed(),
        )
        .open(path)
        .map_err(|e| io_error("QA measured file", e))?;
    let before = file
        .metadata()
        .map_err(|e| io_error("QA file metadata", e))?;
    if !before.is_file()
        || before.uid() != owner
        || before.nlink() != 1
        || before.mode() & 0o022 != 0
        || before.len() > limit
    {
        return Err(refused());
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| io_error("QA file read", e))?;
    let after = file
        .metadata()
        .map_err(|e| io_error("QA file metadata", e))?;
    if bytes.len() as u64 != before.len()
        || before.ctime() != after.ctime()
        || before.ctime_nsec() != after.ctime_nsec()
        || before.mode() != after.mode()
        || after.nlink() != 1
    {
        return Err(refused());
    }
    Ok((file, bytes))
}

fn component_path(paths: &LauncherPaths, retired: &str) -> PathBuf {
    paths
        .release_prefix
        .join("releases")
        .join(retired)
        .join(LAUNCHER_RELATIVE_PATH)
}

fn manifest(paths: &LauncherPaths, id: &str) -> Result<ReleaseManifest, LauncherError> {
    let path = paths
        .release_prefix
        .join("releases")
        .join(id)
        .join("manifest.json");
    let (_, bytes) = measured_file(&path, 0, 1024 * 1024)?;
    let value: ReleaseManifest = serde_json::from_slice(&bytes).map_err(|_| refused())?;
    if value.schema != MANIFEST_SCHEMA || value.release_id != id || value.digest().to_string() != id
    {
        return Err(refused());
    }
    Ok(value)
}

fn context(
    paths: &LauncherPaths,
    operator: u32,
    running: &str,
) -> Result<LauncherConfig, LauncherError> {
    super::require_system_install_context(paths)?;
    super::require_system_state_dirs(paths)?;
    let current =
        super::runtime_config_with_deadline(paths, Instant::now() + Duration::from_secs(5))?;
    if operator == 0
        || current.operator_uid != operator
        || current.release_id != running
        || super::public_runtime_config(paths)? != current
    {
        return Err(refused());
    }
    Ok(current)
}

fn binding(paths: &LauncherPaths, plan: &Plan) -> Result<(), LauncherError> {
    let anchor = super::history::load(paths, &plan.session_id)?.anchor();
    if anchor.run_id != plan.run_id || anchor.release_id != plan.retired_release_id {
        return Err(refused());
    }
    Ok(())
}

fn qa_approval(config: &LauncherConfig, plan: &Plan, now: u64) -> Result<(), LauncherError> {
    use crate::{conformance::admission::Attendance, launch_protocol::LaunchRole};
    let state = Path::new(BROKER_STATE);
    let authorizations = state.join("authorizations");
    let consumed = authorizations.join("consumed");
    trusted_directory(&consumed, config.broker_uid)?;
    // Validate bounded regular records before asking their existing read-only owner.
    let entries = fs::read_dir(&consumed).map_err(|e| io_error("QA consumed authority", e))?;
    for (index, entry) in entries.enumerate() {
        if index >= 4096 {
            return Err(refused());
        }
        measured_file(
            &entry.map_err(|e| io_error("QA consumed record", e))?.path(),
            config.broker_uid,
            MAX_STATE_BYTES,
        )?;
    }
    let grant = AuthorizationStore::read_consumed_for_session(&authorizations, &plan.session_id)
        .map_err(|_| refused())?
        .ok_or_else(refused)?;
    let registry = crate::registry::Registry::open_trusted(Path::new("/var/lib/louiselm/registry"))
        .map_err(|_| refused())?;
    let agent = registry.agent(&grant.agent_id).map_err(|_| refused())?;
    let runtime = registry.runtime(&agent.runtime_id).map_err(|_| refused())?;
    let registered = registry
        .envelope("desktop-qa-denied")
        .map_err(|_| refused())?;
    if agent.provider != crate::registry::Provider::Fixed("offline-test".into())
        || agent.tool_integration.as_deref() != Some("louiselm.test-tool-integration/1")
        || !agent.arguments.is_empty()
        || !agent.environment.is_empty()
        || runtime.id != grant.agent_id
        || runtime.origin != format!("release:{}", plan.retired_release_id)
        || runtime.executable != "louiselm-tool-test-agent"
        || registered.network != crate::registry::NetworkPolicy::Denied
    {
        return Err(refused());
    }
    let run_root = state.join("run-envelopes");
    let run = run_root.join(&plan.run_id);
    // The fixed QA coordinator approves exactly revision 1 and never renews it.
    trusted_directory(&run, config.broker_uid)?;
    let entries = fs::read_dir(&run)
        .map_err(|e| io_error("QA Run authority", e))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| io_error("QA Run records", e))?;
    if entries.len() != 1 || entries[0].file_name() != "1.json" {
        return Err(refused());
    }
    measured_file(&run.join("1.json"), config.broker_uid, MAX_STATE_BYTES)?;
    let envelope = RunEnvelopeStore::read_current(&run_root, &plan.run_id)
        .map_err(|_| refused())?
        .ok_or_else(refused)?;
    let exact_commands = crate::broker::ApprovedCommands {
        command_digest: Digest::of(b"printf QA_OK").to_string(),
        timeout_ms: 5000,
        uses: Some(2),
        allow_delegation: false,
        expires_at_ms: envelope.expires_at_ms,
    };
    if config.conformance != crate::conformance::admission::Enforcement::Enforced
        || grant.run_id != plan.run_id
        || grant.controller_uid != config.operator_uid
        || grant.agent_id != format!("qa-{}", &plan.retired_release_id[7..31])
        || grant.role != LaunchRole::Agent
        || grant.envelope_revision != 1
        || grant.conformance.attendance != Attendance::Interactive
        || grant.conformance.waiver.is_some()
        || grant.dependencies.is_some()
        || grant.skill_requests.is_some()
        || grant.beads_mutations.is_some()
        || grant.provider_requests.is_some()
        || envelope.controller_uid != config.operator_uid
        || envelope.envelope_id != "desktop-qa-denied"
        || envelope.max_sessions != 12
        || envelope.verification_plan_digest != Digest::of(br#"{"schema":"louiselm.workspace.verification-plan/1","commands":[{"argv":["/usr/bin/printf","QA_OK"],"cwd":".","timeout_ms":5000}]}"#).to_string()
        || envelope.bead_scope.is_some()
        || envelope.provider_requests.is_some()
        || grant.expires_at_ms != envelope.expires_at_ms
        || now >= envelope.expires_at_ms
        || envelope.expires_at_ms - now > 3_600_000
        || envelope.commands != Some(exact_commands.clone())
        || grant.commands != Some(exact_commands)
    {
        return Err(refused());
    }
    Ok(())
}

pub(crate) fn plan(
    paths: &LauncherPaths,
    operator: u32,
    running: &str,
    releases: [&str; 2],
    run: &str,
    session: &str,
    now: u64,
) -> Result<Plan, LauncherError> {
    let [baseline, retired] = releases;
    let _installation = super::acquire_install_lock(paths)?;
    let current = context(paths, operator, running)?;
    check_ids(baseline, retired, &current.release_id)?;
    if run.is_empty()
        || run.len() > 128
        || !run
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        return Err(refused());
    }
    let old = manifest(paths, baseline)?;
    let retained = manifest(paths, retired)?;
    let newer = manifest(paths, &current.release_id)?;
    for value in [&retained, &newer] {
        if !value.source.clean
            || value.source.commit.len() != 40
            || !value
                .source
                .commit
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            || value.component("louiselm-tool-test-agent").is_none()
        {
            return Err(refused());
        }
    }
    if old.built_at_ms >= retained.built_at_ms || retained.built_at_ms >= newer.built_at_ms {
        return Err(refused());
    }
    let retained = super::load_release(paths, retired)?;
    let (file, bytes) = measured_file(&component_path(paths, retired), 0, 128 * 1024 * 1024)?;
    let metadata = file
        .metadata()
        .map_err(|e| io_error("QA component metadata", e))?;
    let result = Plan {
        schema: SCHEMA.into(),
        baseline_release_id: baseline.into(),
        retired_release_id: retired.into(),
        default_release_id: current.release_id.clone(),
        run_id: run.into(),
        session_id: session.into(),
        operator_uid: operator,
        launcher_digest: Digest::of(&bytes).to_string(),
        size: metadata.len(),
        device: metadata.dev(),
        inode: metadata.ino(),
        original_mode: 0o555,
    };
    if metadata.mode() & 0o7777 != result.original_mode
        || result.launcher_digest != retained.launcher_digest
    {
        return Err(refused());
    }
    binding(paths, &result)?;
    qa_approval(&current, &result, now)?;
    Ok(result)
}

pub(crate) fn digest(plan: &Plan) -> Result<Digest, LauncherError> {
    serde_json::to_vec(plan)
        .map(|bytes| Digest::of(&bytes))
        .map_err(|_| refused())
}

fn record_path(paths: &LauncherPaths, reviewed: &Digest) -> PathBuf {
    paths
        .private()
        .join(format!("qa-fault-{}.json", reviewed.hex()))
}

pub(crate) fn apply(
    paths: &LauncherPaths,
    plan: &Plan,
    reviewed: &Digest,
    running: &str,
    now: u64,
) -> Result<(), LauncherError> {
    let _installation = super::acquire_install_lock(paths)?;
    let current = context(paths, plan.operator_uid, running)?;
    if &digest(plan)? != reviewed || current.release_id != plan.default_release_id {
        return Err(refused());
    }
    check_ids(
        &plan.baseline_release_id,
        &plan.retired_release_id,
        &plan.default_release_id,
    )?;
    binding(paths, plan)?;
    qa_approval(&current, plan, now)?;
    let file = retained_component(paths, plan, &[0o555])?;
    let record = Record {
        plan: plan.clone(),
        restored: false,
    };
    let path = record_path(paths, reviewed);
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .map_err(|e| io_error("QA fault recovery record (retain any prior record)", e))?;
    output
        .write_all(&serde_json::to_vec(&record).map_err(|_| refused())?)
        .and_then(|()| output.sync_all())
        .map_err(|e| io_error("QA fault recovery durability", e))?;
    super::sync_dir(&paths.private())?;
    // The durable original hash/mode and unchanged bytes exist before the only effect.
    change_mode(&file, 0o555, 0o444)
}

fn retained_component(
    paths: &LauncherPaths,
    plan: &Plan,
    modes: &[u32],
) -> Result<File, LauncherError> {
    let (file, bytes) = measured_file(
        &component_path(paths, &plan.retired_release_id),
        0,
        128 * 1024 * 1024,
    )?;
    let metadata = file
        .metadata()
        .map_err(|e| io_error("QA component metadata", e))?;
    if metadata.dev() != plan.device
        || metadata.ino() != plan.inode
        || metadata.len() != plan.size
        || !modes.contains(&(metadata.mode() & 0o7777))
        || Digest::of(&bytes).to_string() != plan.launcher_digest
    {
        return Err(refused());
    }
    Ok(file)
}

pub(crate) fn restore(
    paths: &LauncherPaths,
    operator: u32,
    running: &str,
    reviewed: &Digest,
) -> Result<(), LauncherError> {
    let _installation = super::acquire_install_lock(paths)?;
    let current = context(paths, operator, running)?;
    let path = record_path(paths, reviewed);
    let (file, bytes) = measured_file(&path, 0, 65536)?;
    if file
        .metadata()
        .map_err(|e| io_error("QA recovery metadata", e))?
        .mode()
        & 0o7777
        != 0o600
    {
        return Err(refused());
    }
    let mut record: Record = serde_json::from_slice(&bytes).map_err(|_| refused())?;
    let plan = &record.plan;
    if plan.schema != SCHEMA
        || plan.operator_uid != operator
        || plan.original_mode != 0o555
        || digest(plan)? != *reviewed
        || plan.default_release_id != current.release_id
    {
        return Err(refused());
    }
    check_ids(
        &plan.baseline_release_id,
        &plan.retired_release_id,
        &plan.default_release_id,
    )?;
    binding(paths, plan)?;
    // Restore is allowed after expiry/disposal: it restores bytes, never Session authority.
    let component = retained_component(paths, plan, &[0o444, 0o555])?;
    let mode = component
        .metadata()
        .map_err(|e| io_error("QA restore metadata", e))?
        .mode()
        & 0o7777;
    change_mode(&component, mode, 0o555)?;
    super::load_release(paths, &plan.retired_release_id)?;
    record.restored = true;
    super::write_json_atomic(&path, &record, 0o600)
}

fn change_mode(file: &File, expected: u32, target: u32) -> Result<(), LauncherError> {
    let metadata = file
        .metadata()
        .map_err(|e| io_error("QA permission metadata", e))?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.mode() & 0o7777 != expected
        || ![0o444, 0o555].contains(&expected)
        || ![0o444, 0o555].contains(&target)
    {
        return Err(refused());
    }
    file.set_permissions(fs::Permissions::from_mode(target))
        .and_then(|()| file.sync_all())
        .map_err(|e| io_error("QA permission change (retain recovery record)", e))
}

#[cfg(test)]
#[path = "qa_fault_tests.rs"]
mod tests;
