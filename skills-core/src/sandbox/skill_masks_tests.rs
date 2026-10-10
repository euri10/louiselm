//! Preparation refuses ambiguity before a confined process exists.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "Tests assert fixture setup."
)]

use super::*;
use crate::{
    discovery::{INVENTORY_PATH, INVENTORY_SCHEMA},
    registry::{MeasuredFile, NetworkPolicy},
    sandbox::IdentityPlan,
};
use std::{
    collections::BTreeMap,
    os::unix::fs::{PermissionsExt, symlink},
};

fn fixture(root: &Path) -> (RuntimePackage, ConfinementPlan, Inventory) {
    let runtime_root = root.join("runtime");
    fs::create_dir_all(runtime_root.join("builtin/skills")).unwrap();
    fs::write(runtime_root.join("agent"), b"measured fixture runtime").unwrap();
    let home = root.join("home");
    let workspace = root.join("workspace");
    fs::create_dir(&home).unwrap();
    fs::create_dir(&workspace).unwrap();
    let inventory = Inventory {
        schema: INVENTORY_SCHEMA.into(),
        adapter_id: "fixture".into(),
        version: "1".into(),
        runtime_id: "runtime".into(),
        executable_sha256: Digest::of(b"measured fixture runtime").hex().into(),
        sources: SourceKind::ALL
            .into_iter()
            .map(|kind| {
                let (namespace, path) = match kind {
                    SourceKind::ManagedSkills => (SourceRoot::Home, ".managed/skills".into()),
                    SourceKind::NativeSkills => (SourceRoot::Home, ".agents/skills".into()),
                    SourceKind::RepositorySkills => {
                        (SourceRoot::Workspace, ".agents/skills".into())
                    }
                    SourceKind::SystemSkills => (SourceRoot::Runtime, "builtin/skills".into()),
                    SourceKind::ProjectInstructions => (SourceRoot::Workspace, "AGENTS.md".into()),
                    _ => (SourceRoot::Home, format!("other/{}", kind.name())),
                };
                Source {
                    id: kind.name().into(),
                    kind,
                    root: namespace,
                    path,
                }
            })
            .collect(),
    };
    let mut runtime = RuntimePackage {
        id: "runtime".into(),
        root: runtime_root.clone(),
        executable: "agent".into(),
        executable_sha256: inventory.executable_sha256.clone(),
        adapters: vec![],
        version: "1".into(),
        origin: "fixture".into(),
    };
    register(&mut runtime, &inventory);
    let plan = ConfinementPlan {
        session_id: "fixture".into(),
        executable: runtime_root.join("agent"),
        runtime_root,
        arguments: vec![],
        environment: BTreeMap::new(),
        home,
        workspace,
        cache: None,
        beads_replica: None,
        skill_masks: None,
        system_roots: vec![root.join("system")],
        network: NetworkPolicy::Denied,
        identity: IdentityPlan::NamespaceOnly,
        channels: vec![],
    };
    (runtime, plan, inventory)
}

fn register(runtime: &mut RuntimePackage, inventory: &Inventory) {
    let bytes = inventory.canonical_bytes();
    fs::write(runtime.root.join(INVENTORY_PATH), &bytes).unwrap();
    runtime.adapters = vec![MeasuredFile {
        path: INVENTORY_PATH.into(),
        sha256: Digest::of(&bytes).hex().into(),
    }];
}

#[test]
fn masks_bind_registered_inventory_and_pin_writable_ancestors() {
    let root = tempfile::tempdir().unwrap();
    let (runtime, plan, _) = fixture(root.path());
    let store = Store::open(&root.path().join("empty-store")).unwrap();
    let masks = SkillMasks::prepare(&store, &runtime, &plan).unwrap();
    assert_eq!(masks.evidence.sources.len(), 4);
    assert!(crate::discovery_source::skill_masks_match(
        &crate::session_manifest::SkillInputs::Off {
            view_digest: instruction_view::empty_digest().to_string()
        },
        &runtime.measure().unwrap(),
        Some(&masks.evidence)
    ));
    assert_eq!(masks.parents.len(), 3);
    for arguments in masks.arguments().as_chunks::<3>().0 {
        assert_eq!(arguments[0], "--ro-bind");
    }
    assert!(!store.root().join("current").exists());
    let mut changed = plan.clone();
    changed.home = root.path().join("different-home");
    assert!(masks.verify(&changed).is_err());
    fs::set_permissions(&masks.view, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(masks.view.join("unexpected"), b"not empty").unwrap();
    assert!(masks.verify(&plan).is_err());
}

#[test]
fn changed_missing_ambiguous_or_symlinked_roots_are_refused() {
    for case in 0..6 {
        let root = tempfile::tempdir().unwrap();
        let (mut runtime, plan, mut inventory) = fixture(root.path());
        let store = Store::open(&root.path().join("empty-store")).unwrap();
        match case {
            0 => runtime.adapters.clear(),
            1 => fs::write(runtime.root.join(INVENTORY_PATH), b"changed").unwrap(),
            2 => {
                inventory
                    .sources
                    .iter_mut()
                    .find(|s| s.kind == SourceKind::ProjectInstructions)
                    .unwrap()
                    .path = ".agents/skills/rules.md".into();
                register(&mut runtime, &inventory);
            }
            3 => symlink(root.path(), plan.home.join(".agents")).unwrap(),
            4 => {
                inventory
                    .sources
                    .iter_mut()
                    .find(|s| s.kind == SourceKind::NativeSkills)
                    .unwrap()
                    .path = ".managed".into();
                register(&mut runtime, &inventory);
            }
            _ => {
                inventory
                    .sources
                    .iter_mut()
                    .find(|s| s.kind == SourceKind::SystemSkills)
                    .unwrap()
                    .root = SourceRoot::System;
                register(&mut runtime, &inventory);
            }
        }
        assert!(
            SkillMasks::prepare(&store, &runtime, &plan).is_err(),
            "case {case}"
        );
    }
}
