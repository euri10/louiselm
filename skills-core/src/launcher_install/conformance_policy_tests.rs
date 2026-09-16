//! A new default release cannot silently revoke an otherwise unchanged Session.
#![allow(
    clippy::unwrap_used,
    reason = "Fixture construction and assertions are test failures."
)]

use super::*;
use crate::release::{Component, PolicyIdentity, SourceIdentity, ToolchainIdentity};

fn retained_release(paths: &LauncherPaths) -> LauncherConfig {
    let bytes = b"pinned launcher";
    let mut manifest = ReleaseManifest {
        schema: MANIFEST_SCHEMA.into(),
        release_id: String::new(),
        version: "0.1.0".into(),
        built_at_ms: 1,
        source: SourceIdentity {
            commit: "0123456789abcdef0123456789abcdef01234567".into(),
            clean: true,
            describe: "fixture".into(),
            dependencies_digest: Digest::of(b"deps").to_string(),
        },
        toolchain: ToolchainIdentity {
            rustc: "fixture".into(),
            cargo: "fixture".into(),
            target: "x86_64-linux".into(),
        },
        policy: PolicyIdentity {
            version: "1".into(),
            digest: Digest::of(b"policy").to_string(),
        },
        schemas: Vec::new(),
        components: vec![Component {
            name: LAUNCHER_COMPONENT.into(),
            path: LAUNCHER_RELATIVE_PATH.into(),
            sha256: Digest::of(bytes).hex().into(),
            size: bytes.len() as u64,
            executable: true,
        }],
    };
    manifest.release_id = manifest.digest().to_string();
    let root = paths
        .release_prefix
        .join("releases")
        .join(&manifest.release_id);
    fs::create_dir_all(root.join("bin")).unwrap();
    fs::write(
        root.join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    fs::write(root.join(LAUNCHER_RELATIVE_PATH), bytes).unwrap();
    fs::set_permissions(
        root.join(LAUNCHER_RELATIVE_PATH),
        fs::Permissions::from_mode(0o555),
    )
    .unwrap();
    let mut config = super::tests::config_with_measured_bwrap(&paths.bwrap);
    config.release_id = manifest.release_id;
    config.launcher_digest = Digest::of(bytes).to_string();
    config.launcher_path = paths.launcher();
    config.broker_socket_path = paths.broker_socket.clone();
    config.conformance = crate::conformance::admission::Enforcement::Enforced;
    config
}

#[test]
fn default_release_changes_preserve_pins_but_policy_and_byte_changes_do_not() {
    let root = tempfile::tempdir().unwrap();
    let paths = LauncherPaths {
        release_prefix: root.path().join("releases"),
        state_root: root.path().join("authority"),
        bwrap: PathBuf::from("/usr/bin/true"),
        ..LauncherPaths::system()
    };
    fs::create_dir_all(&paths.state_root).unwrap();
    let pinned = retained_release(&paths);
    let mut current = pinned.clone();
    current.release_id = Digest::of(b"new default release").to_string();
    current.launcher_digest = Digest::of(b"new executable").to_string();
    fs::write(paths.config(), serde_json::to_vec(&current).unwrap()).unwrap();
    // No mutable current symlink is consulted by retained-release inspection.
    require_session_policy(&paths, &pinned).unwrap();
    for policy in [
        LauncherConfig {
            bwrap_digest: Digest::of(b"changed dependency").to_string(),
            ..current.clone()
        },
        LauncherConfig {
            conformance: crate::conformance::admission::Enforcement::PreCutover,
            ..current.clone()
        },
        LauncherConfig {
            operator_uid: 1_001,
            ..current.clone()
        },
    ] {
        fs::write(paths.config(), serde_json::to_vec(&policy).unwrap()).unwrap();
        assert!(require_session_policy(&paths, &pinned).is_err());
    }
    fs::write(paths.config(), serde_json::to_vec(&current).unwrap()).unwrap();
    let executable = paths
        .release_prefix
        .join("releases")
        .join(&pinned.release_id)
        .join(LAUNCHER_RELATIVE_PATH);
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(&executable, b"changed pinned bytes").unwrap();
    assert!(require_session_policy(&paths, &pinned).is_err());
}
