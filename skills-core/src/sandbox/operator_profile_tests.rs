//! Offline fake-runtime observations through real Bubblewrap mounts.
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "Fixtures assert observed namespace behavior."
)]

use super::*;
use crate::{
    discovery::{INVENTORY_PATH, INVENTORY_SCHEMA},
    discovery_source::Source,
    registry::{MeasuredFile, NetworkPolicy},
    sandbox::{Backend, BubblewrapBackend, IdentityPlan, default_system_roots},
    session_manifest::MeasuredInput,
};
use std::{collections::BTreeMap, io::Read, os::unix::fs::PermissionsExt};

const PROFILE: &[u8] = b"---\ntype: louiselm/operator-profile\n---\nPrivate defaults.\n";

fn fixture() -> (tempfile::TempDir, RuntimePackage, ConfinementPlan) {
    let root = tempfile::tempdir().unwrap();
    for name in ["runtime", "home", "workspace"] {
        fs::create_dir(root.path().join(name)).unwrap();
    }
    let executable = root.path().join("runtime/agent");
    let script = r#"
set -eu
cat "$HOME/AGENTS.md"
test ! -s "$HOME/AGENTS.override.md"
if printf bad > "$HOME/AGENTS.md"; then exit 91; fi
if mv "$HOME/AGENTS.md" "$HOME/replaced"; then exit 92; fi
if rm "$HOME/AGENTS.override.md"; then exit 93; fi
if mv "$HOME" "$HOME-renamed"; then exit 94; fi
mkdir "$HOME/state"
printf writable > "$HOME/state/session"
"#;
    // HostIdentity pins the actual ELF executable, not an interpreted script.
    fs::copy("/bin/sh", &executable).unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
    let hash = Digest::of(&fs::read(&executable).unwrap()).hex().to_owned();
    let mut sources: Vec<_> = SourceKind::ALL
        .into_iter()
        .map(|kind| Source {
            id: kind.name().into(),
            kind,
            root: SourceRoot::Home,
            path: if kind == SourceKind::GlobalInstructions {
                "AGENTS.md".into()
            } else {
                format!("sources/{}", kind.name())
            },
        })
        .collect();
    sources.push(Source {
        id: "global_override".into(),
        kind: SourceKind::GlobalInstructions,
        root: SourceRoot::Home,
        path: "AGENTS.override.md".into(),
    });
    let inventory = Inventory {
        schema: INVENTORY_SCHEMA.into(),
        adapter_id: "fixture".into(),
        version: "1".into(),
        runtime_id: "fixture".into(),
        executable_sha256: hash.clone(),
        sources,
    };
    // Project instructions belong to the workspace in the measured inventory.
    let mut inventory = inventory;
    inventory
        .sources
        .iter_mut()
        .find(|source| source.kind == SourceKind::ProjectInstructions)
        .unwrap()
        .root = SourceRoot::Workspace;
    let bytes = inventory.canonical_bytes();
    fs::write(root.path().join("runtime").join(INVENTORY_PATH), &bytes).unwrap();
    let runtime = RuntimePackage {
        id: "fixture".into(),
        root: root.path().join("runtime"),
        executable: "agent".into(),
        executable_sha256: hash,
        adapters: vec![MeasuredFile {
            path: INVENTORY_PATH.into(),
            sha256: Digest::of(&bytes).hex().into(),
        }],
        version: "1".into(),
        origin: "offline profile fixture".into(),
    };
    let plan = ConfinementPlan {
        session_id: "profile-test".into(),
        runtime_root: runtime.root.clone(),
        executable,
        arguments: vec!["-c".into(), script.into()],
        environment: BTreeMap::from([
            (
                "HOME".into(),
                root.path().join("home").display().to_string(),
            ),
            ("PATH".into(), "/usr/bin:/bin".into()),
        ]),
        home: root.path().join("home"),
        workspace: root.path().join("workspace"),
        cache: None,
        beads_replica: None,
        skill_masks: None,
        operator_profile: None,
        system_roots: default_system_roots(),
        network: NetworkPolicy::Denied,
        identity: IdentityPlan::NamespaceOnly,
        channels: vec![],
    };
    (root, runtime, plan)
}

fn input() -> OperatorProfileInput {
    OperatorProfileInput {
        package_digest: Digest::of(b"admitted package fixture").to_string(),
        input: MeasuredInput::from_bytes("AGENTS.md", false, PROFILE).unwrap(),
    }
}

#[test]
fn fake_runtime_reads_profile_and_cannot_replace_it_or_unmask_other_globals() {
    let (root, runtime, mut plan) = fixture();
    plan.operator_profile = Some(
        OperatorProfileMount::prepare(
            &root.path().join("profile"),
            &input(),
            PROFILE,
            &runtime,
            &plan,
        )
        .unwrap(),
    );
    let bootstrap = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("louiselm-launch");
    let backend = BubblewrapBackend::at(Path::new("/usr/bin/bwrap"))
        .with_bootstrap(&bootstrap)
        .without_cgroup();
    let mut session = backend.spawn(&plan).unwrap();
    drop(session.take_stdin());
    let mut output = Vec::new();
    session
        .take_stdout()
        .unwrap()
        .read_to_end(&mut output)
        .unwrap();
    let mut errors = String::new();
    session
        .take_stderr()
        .unwrap()
        .read_to_string(&mut errors)
        .unwrap();
    let code = session.wait().unwrap();
    let report = session.dispose().unwrap();
    assert_eq!(code, 0, "{errors}");
    assert_eq!(output, PROFILE);
    assert_eq!(report.survivors, 0);
    assert_eq!(
        fs::read(plan.home.join("state/session")).unwrap(),
        b"writable"
    );
    assert!(!plan.home.join("replaced").exists());
}

#[test]
fn changed_profile_bytes_and_undeclared_slots_refuse_preparation() {
    let (root, runtime, plan) = fixture();
    assert!(matches!(
        OperatorProfileMount::prepare(
            &root.path().join("wrong"),
            &input(),
            b"unadmitted",
            &runtime,
            &plan
        ),
        Err(SandboxError::Refused(reason)) if reason == "contained Operator profile inventory or bytes unavailable"
    ));
    let mut other = input();
    other.input.path = "undeclared.md".into();
    assert!(matches!(
        OperatorProfileMount::prepare(
            &root.path().join("undeclared"),
            &other,
            PROFILE,
            &runtime,
            &plan
        ),
        Err(SandboxError::Refused(reason)) if reason == "contained Operator profile inventory or bytes unavailable"
    ));
    let mount = OperatorProfileMount::prepare(
        &root.path().join("profile"),
        &input(),
        PROFILE,
        &runtime,
        &plan,
    )
    .unwrap();
    fs::set_permissions(
        root.path().join("profile/profile"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    fs::write(root.path().join("profile/profile"), b"substituted").unwrap();
    fs::set_permissions(
        root.path().join("profile/profile"),
        fs::Permissions::from_mode(0o444),
    )
    .unwrap();
    assert!(
        matches!(mount.verify(&plan), Err(SandboxError::Refused(reason)) if reason == "contained Operator profile binding changed")
    );
}

#[test]
#[ignore = "requires root and cgroup v2 in the disposable launcher VM"]
fn privileged_profile_source_is_readable_only_by_its_assigned_identity() {
    assert!(rustix::process::geteuid().is_root());
    let (root, runtime, mut plan) = fixture();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o755)).unwrap();
    fs::set_permissions(&runtime.root, fs::Permissions::from_mode(0o755)).unwrap();
    let sessions = root.path().join("sessions");
    fs::create_dir(&sessions).unwrap();
    fs::set_permissions(&sessions, fs::Permissions::from_mode(0o711)).unwrap();
    let session_root = sessions.join("profile-test");
    fs::create_dir(&session_root).unwrap();
    fs::set_permissions(&session_root, fs::Permissions::from_mode(0o711)).unwrap();
    fs::rename(&plan.home, session_root.join("home")).unwrap();
    fs::rename(&plan.workspace, session_root.join("workspace")).unwrap();
    plan.home = session_root.join("home");
    plan.workspace = session_root.join("workspace");
    let (uid, gid) = (60_001, 60_001);
    for path in [&plan.home, &plan.workspace] {
        std::os::unix::fs::chown(path, Some(uid), Some(gid)).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    plan.identity = IdentityPlan::HostIdentity { uid, gid };
    plan.environment
        .insert("HOME".into(), plan.home.display().to_string());
    let profile_root = session_root.join("operator-profile");
    plan.operator_profile = Some(
        OperatorProfileMount::prepare(&profile_root, &input(), PROFILE, &runtime, &plan).unwrap(),
    );
    let assigned = read_as(uid, gid, &profile_root.join("profile"));
    assert!(
        assigned.status.success(),
        "{}",
        String::from_utf8_lossy(&assigned.stderr)
    );
    assert_eq!(assigned.stdout, PROFILE);
    let group = PathBuf::from(format!(
        "/sys/fs/cgroup/louiselm-profile-{}",
        std::process::id()
    ));
    fs::create_dir(&group).unwrap();
    fs::set_permissions(&group, fs::Permissions::from_mode(0o700)).unwrap();
    let bootstrap = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("louiselm-launch");
    let backend = BubblewrapBackend::for_launcher(
        Path::new("/usr/bin/bwrap"),
        &bootstrap,
        &group,
        "fixture".into(),
    );
    let mut session = backend.spawn(&plan).unwrap();
    drop(session.take_stdin());
    let mut output = Vec::new();
    session
        .take_stdout()
        .unwrap()
        .read_to_end(&mut output)
        .unwrap();
    let mut errors = String::new();
    session
        .take_stderr()
        .unwrap()
        .read_to_string(&mut errors)
        .unwrap();
    let code = session.wait().unwrap();
    let report = session.dispose().unwrap();
    fs::remove_dir(&group).unwrap();
    assert_eq!(code, 0, "{errors}");
    assert_eq!(output, PROFILE);
    assert_eq!(report.survivors, 0);
    let foreign = read_as(60_002, 60_002, &profile_root.join("profile"));
    assert_eq!(foreign.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&foreign.stderr).contains("Permission denied"));
    assert_eq!(foreign.stdout, b"");
}

fn read_as(uid: u32, gid: u32, path: &Path) -> std::process::Output {
    std::process::Command::new("/usr/bin/setpriv")
        .args([
            format!("--reuid={uid}"),
            format!("--regid={gid}"),
            "--clear-groups".into(),
            "/bin/cat".into(),
        ])
        .arg(path)
        .output()
        .unwrap()
}
