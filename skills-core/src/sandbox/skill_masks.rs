//! Native Skill masks and their immutable mountpoint ancestors.

#[cfg(test)]
#[path = "skill_masks_tests.rs"]
mod tests;

use super::{Channel, ConfinementPlan, SandboxError};
use crate::{
    Digest, Store,
    discovery::Inventory,
    discovery_source::{SkillMaskEvidence, Source, SourceKind, SourceRoot},
    instruction_view,
    registry::RuntimePackage,
};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

/// Launcher-derived mount plan. Callers cannot substitute paths or view bytes.
#[derive(Clone, Debug)]
pub struct SkillMasks {
    store: PathBuf,
    view: PathBuf,
    evidence: SkillMaskEvidence,
    roots: Vec<PathBuf>,
    parents: BTreeSet<PathBuf>,
}

pub(crate) fn is_skill_root(kind: SourceKind) -> bool {
    matches!(
        kind,
        SourceKind::ManagedSkills
            | SourceKind::RepositorySkills
            | SourceKind::NativeSkills
            | SourceKind::SystemSkills
    )
}

impl SkillMasks {
    /// Helpers share only the Agent workspace; keep its masks and pinned parents.
    pub(crate) fn workspace_only(&self) -> Self {
        let mut masks = self.clone();
        (masks.evidence.sources, masks.roots) = self
            .evidence
            .sources
            .iter()
            .zip(&self.roots)
            .filter(|(source, _)| source.root == SourceRoot::Workspace)
            .map(|(source, root)| (source.clone(), root.clone()))
            .unzip();
        masks
            .parents
            .retain(|parent| masks.roots.iter().any(|root| root.starts_with(parent)));
        masks
    }

    pub(crate) fn prepare(
        store: &Store,
        runtime: &RuntimePackage,
        plan: &ConfinementPlan,
    ) -> Result<Self, SandboxError> {
        let refused =
            || SandboxError::Refused("skills-off inventory or empty view unavailable".into());
        runtime.measure().map_err(|_| refused())?;
        let inventory = Inventory::load(runtime).map_err(|_| refused())?;
        let view = instruction_view::empty(store).map_err(|_| refused())?;
        let sources: Vec<Source> = inventory
            .sources
            .iter()
            .filter(|s| is_skill_root(s.kind))
            .cloned()
            .collect();
        let roots = sources
            .iter()
            .map(|source| target(source, plan))
            .collect::<Result<Vec<_>, _>>()?;
        let mut parents = BTreeSet::new();
        for source in &sources {
            // Mounting just a leaf inside a writable ancestor permits renaming
            // that ancestor and rebuilding discovery at the original path.
            // Pin the containing top-level directory read-only as well.
            if matches!(source.root, SourceRoot::Home | SourceRoot::Workspace)
                && let Some((first, _)) = source.path.split_once('/')
            {
                let base = if source.root == SourceRoot::Home {
                    &plan.home
                } else {
                    &plan.workspace
                };
                parents.insert(base.join(first));
            }
        }
        for root in &roots {
            if roots
                .iter()
                .any(|other| root != other && (root.starts_with(other) || other.starts_with(root)))
            {
                return Err(refused());
            }
            // A mask must not erase instructions or tool/plugin schemas.
            for source in inventory.sources.iter().filter(|s| !is_skill_root(s.kind)) {
                let other = target(source, plan)?;
                if other.starts_with(root) || root.starts_with(&other) {
                    return Err(refused());
                }
            }
        }
        let masks = Self {
            store: store.root().into(),
            view: view.skills_root().into(),
            evidence: SkillMaskEvidence {
                inventory_digest: Digest::of(&inventory.canonical_bytes()).to_string(),
                view_digest: view.digest().to_string(),
                sources,
            },
            roots,
            parents,
        };
        for (source, root) in masks.evidence.sources.iter().zip(&masks.roots) {
            let base = match source.root {
                SourceRoot::Home => &plan.home,
                SourceRoot::Workspace => &plan.workspace,
                SourceRoot::Runtime => &plan.runtime_root,
                SourceRoot::System => Path::new("/"),
            };
            let mut current = base.to_path_buf();
            for component in Path::new(&source.path).components() {
                current.push(component);
                match fs::symlink_metadata(&current) {
                    Ok(meta) if meta.is_dir() => (),
                    Err(error)
                        if error.kind() == std::io::ErrorKind::NotFound
                            && matches!(source.root, SourceRoot::Home | SourceRoot::Workspace) =>
                    {
                        fs::create_dir(&current).map_err(|_| refused())?;
                    }
                    _ => return Err(refused()),
                }
            }
            if &current != root {
                return Err(refused());
            }
        }
        masks.verify(plan)?;
        Ok(masks)
    }

    pub(super) fn verify(&self, plan: &ConfinementPlan) -> Result<(), SandboxError> {
        let refused = || SandboxError::Refused("skills-off mask binding changed".into());
        let store = Store::open_existing(&self.store).map_err(|_| refused())?;
        let view = instruction_view::empty(&store).map_err(|_| refused())?;
        if view.skills_root() != self.view || view.digest().to_string() != self.evidence.view_digest
        {
            return Err(refused());
        }
        for (source, root) in self.evidence.sources.iter().zip(&self.roots) {
            if target(source, plan)? != *root
                || !fs::symlink_metadata(root).is_ok_and(|meta| meta.is_dir())
                || [&plan.executable].iter().any(|path| path.starts_with(root))
                || plan.channels.iter().any(|channel| matches!(channel, Channel::UnixSocket { guest_path, .. } if guest_path.starts_with(root)))
            {
                return Err(refused());
            }
        }
        Ok(())
    }

    pub(super) fn arguments(&self) -> Vec<String> {
        let mut arguments = vec![];
        for parent in &self.parents {
            arguments.extend([
                "--ro-bind".into(),
                parent.display().to_string(),
                parent.display().to_string(),
            ]);
        }
        for root in &self.roots {
            arguments.extend([
                "--ro-bind".into(),
                self.view.display().to_string(),
                root.display().to_string(),
            ]);
        }
        arguments
    }

    pub(super) fn evidence(&self) -> SkillMaskEvidence {
        self.evidence.clone()
    }
}

fn target(source: &Source, plan: &ConfinementPlan) -> Result<PathBuf, SandboxError> {
    let root = match source.root {
        SourceRoot::Home => plan.home.join(&source.path),
        SourceRoot::Workspace => plan.workspace.join(&source.path),
        SourceRoot::Runtime => plan.runtime_root.join(&source.path),
        SourceRoot::System => Path::new("/").join(&source.path),
    };
    if source.root == SourceRoot::System
        && !plan
            .system_roots
            .iter()
            .any(|system| root.starts_with(system) && root != *system)
    {
        return Err(SandboxError::Refused(
            "Skill mask outside declared system roots".into(),
        ));
    }
    Ok(root)
}
