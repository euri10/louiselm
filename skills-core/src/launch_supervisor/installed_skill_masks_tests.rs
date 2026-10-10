//! Mandatory VM gate for actual skills-off mounts and signed launch history.

use super::*;
use crate::{
    discovery::{INVENTORY_PATH, INVENTORY_SCHEMA, Inventory},
    discovery_source::{Source, SourceKind, SourceRoot},
    registry::MeasuredFile,
    session_manifest::{MeasuredInput, SkillInputs},
};

const MASK_COMMAND: &str = "test -z \"$(ls -A .agents/skills)\" || exit 70; if mv .agents moved 2>/dev/null; then exit 71; fi; if { printf forbidden > .agents/skills/forbidden; } 2>/dev/null; then exit 72; fi; test -f AGENTS.md || exit 73; printf authorized > effect; printf done";

pub(super) fn command(root: &Path) -> &'static str {
    if root.join("skills-off").exists() {
        MASK_COMMAND
    } else {
        COMMAND
    }
}

pub(super) fn request(root: &Path) -> LaunchRequest {
    LaunchRequest::parse_canonical(&fs::read(root.join("skills-off.json")).unwrap()).unwrap()
}

#[expect(
    clippy::too_many_lines,
    reason = "One protected fixture transaction binds inventory, real source/cache and off-mode proposal."
)]
pub(super) fn prepare(root: &Path, config: &LauncherConfig, registry_root: &Path) {
    let mut runtime = Registry::open(registry_root)
        .unwrap()
        .runtime("runtime")
        .unwrap();
    let sources = SourceKind::ALL
        .into_iter()
        .map(|kind| {
            let (namespace, path) = match kind {
                SourceKind::ManagedSkills => (SourceRoot::Home, ".managed/skills".into()),
                SourceKind::NativeSkills => (SourceRoot::Home, ".agents/skills".into()),
                SourceKind::RepositorySkills => (SourceRoot::Workspace, ".agents/skills".into()),
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
        .collect();
    let inventory = Inventory {
        schema: INVENTORY_SCHEMA.into(),
        adapter_id: "offline-test-agent".into(),
        version: "1".into(),
        runtime_id: runtime.id.clone(),
        executable_sha256: runtime.executable_sha256.clone(),
        sources,
    };
    let bytes = inventory.canonical_bytes();
    Inventory::parse(&bytes).unwrap();
    fs::create_dir_all(runtime.root.join("builtin/skills")).unwrap();
    fs::write(
        runtime.root.join("builtin/skills/ambient"),
        b"must not be visible",
    )
    .unwrap();
    fs::write(runtime.root.join(INVENTORY_PATH), &bytes).unwrap();
    runtime.adapters = vec![MeasuredFile {
        path: INVENTORY_PATH.into(),
        sha256: Digest::of(&bytes).hex().into(),
    }];
    registry_record(
        &registry_root.join("runtimes.json"),
        &serde_json::json!([runtime]),
    );

    let staging = tempfile::tempdir().unwrap();
    let repo = staging.path().join("source");
    fs::create_dir_all(repo.join(".agents/skills")).unwrap();
    let instructions = b"project instructions survive skills-off\n";
    fs::write(repo.join("AGENTS.md"), instructions).unwrap();
    fs::write(repo.join(".agents/skills/ambient"), b"must not be visible").unwrap();
    for arguments in [
        vec!["init", "--quiet"],
        vec!["add", "AGENTS.md", ".agents"],
        vec![
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "-m",
            "fixture",
        ],
    ] {
        assert!(
            Command::new("git")
                .args(arguments)
                .current_dir(&repo)
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .status()
                .unwrap()
                .success()
        );
    }
    let snapshot = staging.path().join("snapshot");
    let preview = crate::workspace::prepare(&repo, &[], &snapshot).unwrap();
    let cache = staging.path().join("cache");
    fs::create_dir(&cache).unwrap();
    let mut manifest = workspace::fixture_manifest();
    manifest.runtime = runtime.measure().unwrap();
    manifest.skills = SkillInputs::Off {
        view_digest: crate::instruction_view::empty_digest().to_string(),
    };
    manifest.project_instructions =
        vec![MeasuredInput::from_bytes("AGENTS.md", false, instructions).unwrap()];
    manifest.source_snapshot_digest = preview.snapshot_digest;
    manifest.source_base_digest = preview.base_digest;
    let input_root = config
        .broker_socket_path
        .parent()
        .unwrap()
        .join("workspace-inputs");
    crate::workspace::launch_inputs::stage(
        &manifest,
        &snapshot,
        &cache,
        &input_root.join(manifest.digest().hex()),
    )
    .unwrap();
    workspace::own(&input_root, config.broker_uid, config.broker_gid);
    let mut launch = super::request();
    launch.skills = manifest.skills.clone();
    launch.session_input_manifest_id = manifest.digest().to_string();
    fs::write(root.join("skills-off.json"), launch.canonical_bytes()).unwrap();
    fs::write(root.join("skills-off"), b"").unwrap();
}

#[test]
fn privileged_installed_skills_off_masks_all_native_roots() {
    installed_effects(false, None, None, false, true);
}
