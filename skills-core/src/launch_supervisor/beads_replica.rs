//! Launch-time installation of broker-produced disposable tracker generations.

use super::SupervisorError;
use crate::{
    beads_replica::{self, InputBinding},
    launch::LaunchRequest,
    sandbox::{ConfinementPlan, IdentityPlan},
    workspace::{WorkspaceError, filesystem},
};
use std::{
    fs::{self, File},
    os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt, chown},
    path::Path,
};

pub(super) fn prepare(
    inputs: &Path,
    broker: (u32, u32),
    request: &LaunchRequest,
    plan: &mut ConfinementPlan,
) -> Result<(), SupervisorError> {
    install(inputs, broker, request, plan).map_err(|_| SupervisorError::ResolutionFailed)
}

fn install(
    inputs: &Path,
    broker: (u32, u32),
    request: &LaunchRequest,
    plan: &mut ConfinementPlan,
) -> Result<(), WorkspaceError> {
    let staging = inputs.join(&request.session_id);
    for path in [inputs, staging.as_path()] {
        let meta = match fs::symlink_metadata(path) {
            Ok(meta) => meta,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        if !meta.is_dir()
            || meta.uid() != broker.0
            || meta.gid() != broker.1
            || meta.mode() & 0o7777 != 0o700
        {
            return Err(WorkspaceError::Invalid("untrusted tracker replica input"));
        }
    }
    let input = filesystem::open_directory(&staging)?;
    let bytes = filesystem::read_source(&input, "binding.json", 4096)?
        .ok_or(WorkspaceError::Invalid("missing tracker replica binding"))?
        .bytes;
    let binding: InputBinding = serde_json::from_slice(&bytes)?;
    let IdentityPlan::HostIdentity { uid, gid } = plan.identity else {
        return Err(WorkspaceError::Invalid(
            "replicas require a private Session identity",
        ));
    };
    if binding.assigned_uid != uid
        || binding.assigned_gid != gid
        || binding.request_digest != request.digest().to_string()
    {
        return Err(WorkspaceError::Invalid("tracker replica binding mismatch"));
    }
    let files = beads_replica::read_generated(&staging.join("files"))?;
    let parent = plan
        .workspace
        .parent()
        .ok_or(WorkspaceError::Invalid("missing Session root"))?;
    let root = parent.join(beads_replica::DIRECTORY);
    fs::DirBuilder::new().mode(0o700).create(&root)?;
    chown(&root, Some(0), Some(gid))?;
    fs::set_permissions(&root, fs::Permissions::from_mode(0o2700))?;
    beads_replica::publish(&root, "initial", &files)?;
    // Parent remains root-only until every fresh inode has its final identity.
    // No Session process exists yet, and no Session-provided path is traversed.
    assign(&root, broker.0, gid)?;
    fs::set_permissions(&root, fs::Permissions::from_mode(0o2750))?;
    File::open(&root)?.sync_all()?;
    File::open(parent)?.sync_all()?;
    plan.environment.insert(
        "BEADS_DIR".into(),
        root.join("current/.beads").display().to_string(),
    );
    plan.beads_replica = Some(root);
    Ok(())
}

fn assign(path: &Path, uid: u32, gid: u32) -> Result<(), WorkspaceError> {
    let meta = fs::symlink_metadata(path)?;
    if meta.is_dir() {
        for entry in fs::read_dir(path)? {
            assign(&entry?.path(), uid, gid)?;
        }
        let mode = meta.mode();
        chown(path, Some(uid), Some(gid))?;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    } else {
        // lchown does not follow the one publisher-owned current symlink.
        std::os::unix::fs::lchown(path, Some(uid), Some(gid))?;
    }
    Ok(())
}
