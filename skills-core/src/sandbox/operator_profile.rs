//! Private-home profile mounts, independent of writable Agent state.

#[cfg(test)]
#[path = "operator_profile_tests.rs"]
mod tests;

use super::{ConfinementPlan, SandboxError};
use crate::{
    Digest,
    discovery::Inventory,
    discovery_source::{OperatorProfileEvidence, SourceKind, SourceRoot},
    registry::RuntimePackage,
    session_manifest::OperatorProfileInput,
    workspace::filesystem,
};
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt, chown},
    path::{Path, PathBuf},
};

/// Launcher-owned immutable profile and empty global-instruction masks.
/// Only trusted preparation constructs this plan; it grants no supply authority.
#[derive(Clone, Debug)]
pub struct OperatorProfileMount {
    home: PathBuf,
    directory: PathBuf,
    evidence: OperatorProfileEvidence,
}

impl OperatorProfileMount {
    pub(crate) fn prepare(
        directory: &Path,
        input: &OperatorProfileInput,
        bytes: &[u8],
        runtime: &RuntimePackage,
        plan: &ConfinementPlan,
    ) -> Result<Self, SandboxError> {
        let refused = || {
            SandboxError::Refused(
                "contained Operator profile inventory or bytes unavailable".into(),
            )
        };
        runtime.measure().map_err(|_| refused())?;
        let inventory = Inventory::load(runtime).map_err(|_| refused())?;
        let sources: Vec<_> = inventory
            .sources
            .iter()
            .filter(|source| source.kind == SourceKind::GlobalInstructions)
            .cloned()
            .collect();
        if !input.matches_bytes(bytes)
            || sources
                .iter()
                .any(|source| source.root != SourceRoot::Home || source.path.contains('/'))
            || !sources.iter().any(|source| source.path == input.input.path)
            || inventory.sources.iter().any(|source| {
                source.kind != SourceKind::GlobalInstructions
                    && source.root == SourceRoot::Home
                    && sources.iter().any(|global| {
                        source.path == global.path
                            || source.path.starts_with(&format!("{}/", global.path))
                    })
            })
        {
            return Err(refused());
        }
        // A top-level home file has no writable discovery ancestor: the home
        // itself is a bind mount. Pinning the leaves leaves runtime state writable.
        filesystem::publish(directory, |staging| {
            filesystem::write_file(&staging.join("profile"), bytes, 0o444)?;
            filesystem::write_file(&staging.join("empty"), b"", 0o444)
        })
        .map_err(|_| refused())?;
        if let super::IdentityPlan::HostIdentity { gid, .. } = plan.identity {
            // Bubblewrap opens bind sources after dropping to the Session UID.
            // Its group may read; it never owns or writes the supervisor copy.
            chown(directory, Some(0), Some(gid)).map_err(|_| refused())?;
            fs::set_permissions(directory, fs::Permissions::from_mode(0o750))
                .map_err(|_| refused())?;
            fs::File::open(directory)
                .and_then(|file| file.sync_all())
                .map_err(|_| refused())?;
        }
        for source in &sources {
            filesystem::write_file(&plan.home.join(&source.path), b"", 0o444)
                .map_err(|_| refused())?;
        }
        let mounts = Self {
            home: plan.home.clone(),
            directory: directory.to_owned(),
            evidence: OperatorProfileEvidence {
                inventory_digest: Digest::of(&inventory.canonical_bytes()).to_string(),
                profile: input.clone(),
                sources,
            },
        };
        mounts.verify(plan)?;
        Ok(mounts)
    }

    pub(super) fn verify(&self, plan: &ConfinementPlan) -> Result<(), SandboxError> {
        let refused = || SandboxError::Refused("contained Operator profile binding changed".into());
        let root = filesystem::open_directory(&self.directory).map_err(|_| refused())?;
        let metadata = root.metadata().map_err(|_| refused())?;
        let directory_valid = match plan.identity {
            super::IdentityPlan::HostIdentity { gid, .. } => {
                metadata.uid() == 0 && metadata.gid() == gid && metadata.mode() & 0o7777 == 0o750
            }
            super::IdentityPlan::NamespaceOnly => {
                metadata.uid() == rustix::process::geteuid().as_raw()
                    && metadata.mode() & 0o7777 == 0o700
            }
        };
        let file = filesystem::read_source(&root, "profile", crate::inspect::MAX_PROFILE_BYTES)
            .map_err(|_| refused())?
            .ok_or_else(refused)?;
        let empty = filesystem::read_source(&root, "empty", 0)
            .map_err(|_| refused())?
            .ok_or_else(refused)?;
        if !directory_valid
            || self.home != plan.home
            || !self.evidence.profile.matches_bytes(&file.bytes)
            || !empty.bytes.is_empty()
            || ["profile", "empty"].iter().any(|name| {
                !fs::symlink_metadata(self.directory.join(name)).is_ok_and(|metadata| {
                    metadata.mode() & 0o7777 == 0o444
                        && metadata.uid() == rustix::process::geteuid().as_raw()
                })
            })
            || self.evidence.sources.iter().any(|source| {
                !fs::symlink_metadata(self.home.join(&source.path))
                    .is_ok_and(|metadata| metadata.is_file())
            })
        {
            return Err(refused());
        }
        Ok(())
    }

    pub(super) fn arguments(&self) -> Vec<String> {
        self.evidence
            .sources
            .iter()
            .flat_map(|source| {
                let file = if source.path == self.evidence.profile.input.path {
                    "profile"
                } else {
                    "empty"
                };
                [
                    "--ro-bind".into(),
                    self.directory.join(file).display().to_string(),
                    self.home.join(&source.path).display().to_string(),
                ]
            })
            .collect()
    }

    pub(super) fn evidence(&self) -> OperatorProfileEvidence {
        self.evidence.clone()
    }
}
