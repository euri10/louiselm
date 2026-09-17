//! Private source/cache construction owned by the existing Launch supervisor.

use super::SupervisorError;

#[cfg(test)]
#[path = "workspace_tests.rs"]
mod tests;
use crate::{
    Digest,
    cache::CacheOverlay,
    launch::LaunchRequest,
    registry::Registry,
    sandbox::{ConfinementPlan, IdentityPlan},
    workspace::{
        WorkspaceError, filesystem,
        launch_inputs::{self, LoadedInputs},
    },
};
use std::{
    fs::{self, File},
    os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt, chown},
    path::Path,
};

pub(super) struct SessionWorkspace {
    root: File,
    cache: CacheOverlay,
}

pub(super) fn load(
    input_root: &Path,
    broker: (u32, u32),
    request: &LaunchRequest,
    registry: &Registry,
) -> Result<LoadedInputs, SupervisorError> {
    let expected = Digest::parse(&request.session_input_manifest_id)
        .map_err(|_| SupervisorError::ResolutionFailed)?;
    let input = input_root.join(expected.hex());
    for path in [input_root, input.as_path()] {
        let root =
            filesystem::open_directory(path).map_err(|_| SupervisorError::ResolutionFailed)?;
        let meta = root
            .metadata()
            .map_err(|_| SupervisorError::ResolutionFailed)?;
        if meta.uid() != broker.0 || meta.gid() != broker.1 || meta.mode() & 0o7777 != 0o700 {
            return Err(SupervisorError::ResolutionFailed);
        }
    }
    let inputs =
        launch_inputs::load(&input, &expected).map_err(|_| SupervisorError::ResolutionFailed)?;
    let manifest = &inputs.manifest;
    let agent = registry
        .agent(&request.agent_id)
        .map_err(|_| SupervisorError::ResolutionFailed)?;
    let mut runtime = registry
        .runtime(&agent.runtime_id)
        .and_then(|r| r.measure())
        .map_err(|_| SupervisorError::ResolutionFailed)?;
    runtime.adapters.sort_by(|a, b| a.path.cmp(&b.path));
    if manifest.agent != agent
        || manifest.runtime != runtime
        || manifest.skill_generation.generation_digest != request.skill_generation_id
        || manifest.envelope.id != request.envelope_id
        || manifest.envelope.revision != request.envelope_revision
    {
        return Err(SupervisorError::ResolutionFailed);
    }
    Ok(inputs)
}

impl SessionWorkspace {
    pub(super) fn prepare(
        inputs: &LoadedInputs,
        plan: &ConfinementPlan,
    ) -> Result<Self, SupervisorError> {
        let IdentityPlan::HostIdentity { uid, gid } = plan.identity else {
            return Err(SupervisorError::IsolationRejected);
        };
        let directory = plan
            .home
            .parent()
            .filter(|p| Some(*p) == plan.workspace.parent())
            .ok_or(SupervisorError::ResolutionFailed)?;
        let parent = directory
            .parent()
            .ok_or(SupervisorError::ResolutionFailed)?;
        let meta = fs::symlink_metadata(parent).map_err(|_| SupervisorError::ResolutionFailed)?;
        if !meta.is_dir() || meta.uid() != 0 || meta.mode() & 0o7777 != 0o711 {
            return Err(SupervisorError::ResolutionFailed);
        }
        // A fresh barrier excludes every Session UID while copies are built.
        // Existing partial or retained storage is never silently reused.
        fs::DirBuilder::new()
            .mode(0o700)
            .create(directory)
            .map_err(|_| SupervisorError::DurabilityUnavailable)?;
        let root =
            filesystem::open_directory(directory).map_err(|_| SupervisorError::CleanupUnproven)?;
        let result = (|| -> Result<CacheOverlay, WorkspaceError> {
            inputs.retain_source(&directory.join("inputs"))?;
            inputs.materialize_source(&plan.workspace)?;
            assign_tree(&plan.workspace, uid, gid)?;
            fs::DirBuilder::new().mode(0o700).create(&plan.home)?;
            chown(&plan.home, Some(uid), Some(gid))?;
            // Later tools bind this path while the Agent is running. Its
            // parent must become immutable before publishing the Session root.
            let cache_home = directory.join("cache-home");
            fs::DirBuilder::new().mode(0o700).create(&cache_home)?;
            chown(&cache_home, Some(uid), Some(gid))?;
            let cache = inputs
                .cache
                .materialize(&cache_home, &plan.session_id, plan.identity)?;
            chown(&cache_home, Some(0), Some(0))?;
            fs::set_permissions(&cache_home, fs::Permissions::from_mode(0o711))?;
            File::open(&cache_home)?.sync_all()?;
            root.set_permissions(fs::Permissions::from_mode(0o711))?;
            root.sync_all()?;
            File::open(parent)?.sync_all()?;
            Ok(cache)
        })();
        if let Ok(cache) = result {
            Ok(Self { root, cache })
        } else {
            seal(&root)?;
            Err(SupervisorError::DurabilityUnavailable)
        }
    }

    pub(super) fn cache_path(&self) -> &Path {
        self.cache.path()
    }

    pub(super) fn seal(&self) -> Result<(), SupervisorError> {
        seal(&self.root)
    }
}

fn seal(root: &File) -> Result<(), SupervisorError> {
    root.set_permissions(fs::Permissions::from_mode(0o700))
        .and_then(|()| root.sync_all())
        .map_err(|_| SupervisorError::CleanupUnproven)
}

// Only the newly generated tree is traversed, behind the root-owned 0700
// barrier, before any Session process exists. No candidate Git metadata enters.
fn assign_tree(path: &Path, uid: u32, gid: u32) -> Result<(), WorkspaceError> {
    let meta = fs::symlink_metadata(path)?;
    if meta.is_dir() {
        for entry in fs::read_dir(path)? {
            assign_tree(&entry?.path(), uid, gid)?;
        }
    } else if !meta.is_file() || meta.nlink() != 1 {
        return Err(WorkspaceError::Invalid(
            "generated workspace contains an unsafe entry",
        ));
    }
    chown(path, Some(uid), Some(gid))?;
    fs::set_permissions(
        path,
        fs::Permissions::from_mode(if meta.is_dir() || meta.mode() & 0o111 != 0 {
            0o700
        } else {
            0o600
        }),
    )?;
    File::open(path)?.sync_all()?;
    Ok(())
}
