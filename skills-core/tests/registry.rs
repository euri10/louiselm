//! Behavioral coverage for registry.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Test fixtures abort on setup failure and assert failures directly."
)]

//! The production launch registry is pathname authority, not merely JSON.

mod support;

use std::{
    fs,
    os::unix::{
        fs::{PermissionsExt, chown, symlink},
        net::UnixListener,
        process::CommandExt,
    },
    path::Path,
    process::Command,
};

use louiselm_skills::registry::{Registry, RegistryError};
use support::{write_file, write_registry};

fn root_fixture() -> Option<tempfile::TempDir> {
    if !rustix::process::geteuid().is_root() {
        assert!(
            std::env::var_os("LOUISELM_REQUIRE_ROOT_REGISTRY").is_none(),
            "the required registry acceptance must run as root",
        );
        eprintln!("skipping: trusted registry ownership requires root");
        return None;
    }
    if std::env::var_os("LOUISELM_REQUIRE_ROOT_REGISTRY").is_some() {
        let map = fs::read_to_string("/proc/self/uid_map").expect("the UID map is readable");
        assert_eq!(
            map.split_whitespace().collect::<Vec<_>>(),
            ["0", "0", "4294967295"],
            "required registry acceptance needs the initial user namespace",
        );
    }

    let fixture = tempfile::Builder::new()
        .prefix("louiselm-registry-")
        .tempdir_in("/var/lib")
        .expect("a root-owned registry fixture is creatable");
    let registry_root = fixture.path().join("registry");
    let runtime_root = fixture.path().join("runtime");
    let executable = runtime_root.join("bin/agent");
    write_file(&executable, "#!/bin/sh\nexec cat\n");
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))
        .expect("runtime is executable");
    write_file(&runtime_root.join("lib/adapter.js"), "// adapter\n");
    write_registry(&registry_root, &runtime_root);
    for directory in [
        fixture.path(),
        &registry_root,
        &runtime_root,
        &runtime_root.join("bin"),
        &runtime_root.join("lib"),
    ] {
        fs::set_permissions(directory, fs::Permissions::from_mode(0o755))
            .expect("the unprivileged probe can traverse the fixture");
    }
    for document in ["agents.json", "runtimes.json", "envelopes.json"] {
        fs::set_permissions(
            registry_root.join(document),
            fs::Permissions::from_mode(0o644),
        )
        .expect("the unprivileged probe can read the registry");
    }
    Some(fixture)
}

#[test]
fn trusted_registry_requires_root_owned_immutable_documents_and_runtime() {
    let Some(fixture) = root_fixture() else {
        return;
    };
    let registry_root = fixture.path().join("registry");
    let executable = fixture.path().join("runtime/bin/agent");

    Registry::open_trusted(&registry_root).expect("fixed root authority opens");

    let agents = registry_root.join("agents.json");
    fs::set_permissions(&agents, fs::Permissions::from_mode(0o666))
        .expect("registry document becomes writable");
    assert!(matches!(
        Registry::open_trusted(&registry_root),
        Err(RegistryError::Untrusted { path, .. }) if path == agents.display().to_string()
    ));
    fs::set_permissions(&agents, fs::Permissions::from_mode(0o600))
        .expect("registry document is restored");

    fs::set_permissions(&executable, fs::Permissions::from_mode(0o777))
        .expect("runtime becomes writable");
    assert!(matches!(
        Registry::open_trusted(&registry_root),
        Err(RegistryError::Untrusted { path, .. }) if path == executable.display().to_string()
    ));
}

fn operator_write(path: &Path) -> std::process::Output {
    Command::new("/bin/sh")
        .args([
            "-c",
            "printf 'operator-changed\\n' > \"$1\"",
            "registry-probe",
        ])
        .arg(path)
        .uid(1000)
        .gid(1000)
        .output()
        .expect("the unprivileged writer runs")
}

#[test]
fn trusted_registry_rejects_operator_mutation_of_unlisted_runtime_content() {
    let Some(fixture) = root_fixture() else {
        return;
    };
    let registry_root = fixture.path().join("registry");
    let runtime_root = fixture.path().join("runtime");
    let configuration = runtime_root.join("lib/provider-config.json");
    write_file(&configuration, "original\n");
    fs::set_permissions(&configuration, fs::Permissions::from_mode(0o666)).unwrap();
    // Observed by the ln30 guest diagnostic on 2026-09-06: unlisted config
    // changed under UID 1000 while the executable/adapter measurement did not.
    let runtime = Registry::open(&registry_root)
        .unwrap()
        .runtime("demo-runtime")
        .unwrap();
    let before = runtime.measure().unwrap();
    assert!(operator_write(&configuration).status.success());
    assert_eq!(
        fs::read_to_string(&configuration).unwrap(),
        "operator-changed\n"
    );
    assert_eq!(runtime.measure().unwrap(), before);
    assert!(matches!(
        Registry::open_trusted(&registry_root),
        Err(RegistryError::Untrusted { path, .. }) if path == configuration.display().to_string()
    ));

    fs::set_permissions(&configuration, fs::Permissions::from_mode(0o644)).unwrap();
    Registry::open_trusted(&registry_root).expect("immutable unlisted content remains supported");
    assert!(!operator_write(&configuration).status.success());
    assert_eq!(
        fs::read_to_string(&configuration).unwrap(),
        "operator-changed\n"
    );
    Registry::open_trusted(&registry_root).expect("denied mutation leaves the runtime trusted");
}

#[test]
fn trusted_registry_rejects_untrusted_unlisted_files_and_directories() {
    for directory in [false, true] {
        for (mode, owner) in [(0o620, 0), (0o602, 0), (0o644, 1000)] {
            let Some(fixture) = root_fixture() else {
                return;
            };
            let path = fixture.path().join("runtime/unlisted");
            if directory {
                fs::create_dir(&path).unwrap();
            } else {
                write_file(&path, "unlisted\n");
            }
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
            chown(&path, Some(owner), Some(owner)).unwrap();
            assert!(
                matches!(
                    Registry::open_trusted(&fixture.path().join("registry")),
                    Err(RegistryError::Untrusted { path: rejected, .. }) if rejected == path.display().to_string()
                ),
                "directory={directory}, mode={mode:o}, owner={owner}"
            );
        }
    }
}

#[test]
fn trusted_registry_rejects_unlisted_symlinks_and_special_files() {
    for kind in [
        "file-link",
        "directory-link",
        "dangling-link",
        "cycle",
        "fifo",
        "socket",
    ] {
        let Some(fixture) = root_fixture() else {
            return;
        };
        let path = fixture.path().join("runtime/unlisted");
        match kind {
            "file-link" => symlink("bin/agent", &path).unwrap(),
            "directory-link" => symlink("lib", &path).unwrap(),
            "dangling-link" => symlink("missing", &path).unwrap(),
            "cycle" => symlink("unlisted", &path).unwrap(),
            "fifo" => rustix::fs::mkfifoat(rustix::fs::CWD, &path, rustix::fs::Mode::RUSR).unwrap(),
            "socket" => drop(UnixListener::bind(&path).unwrap()),
            _ => unreachable!("the test lists every kind"),
        }
        assert!(
            matches!(
                Registry::open_trusted(&fixture.path().join("registry")),
                Err(RegistryError::Untrusted { path: rejected, .. }) if rejected == path.display().to_string()
            ),
            "kind={kind}"
        );
    }
}

#[test]
fn trusted_registry_propagates_an_unreadable_runtime_directory() {
    let Some(fixture) = root_fixture() else {
        return;
    };
    let hidden = fixture.path().join("runtime/hidden");
    fs::create_dir(&hidden).unwrap();
    fs::set_permissions(&hidden, fs::Permissions::from_mode(0o700)).unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "unreadable_runtime_helper", "--nocapture"])
        .env("LOUISELM_REGISTRY_READ_FAILURE_ROOT", fixture.path())
        .uid(1000)
        .gid(1000)
        .output()
        .expect("the unprivileged reader runs");
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn unreadable_runtime_helper() {
    let Some(root) = std::env::var_os("LOUISELM_REGISTRY_READ_FAILURE_ROOT") else {
        return;
    };
    let root = Path::new(&root);
    assert_eq!(rustix::process::geteuid().as_raw(), 1000);
    assert!(matches!(
        Registry::open_trusted(&root.join("registry")),
        Err(RegistryError::Io { path, source })
            if path == root.join("runtime/hidden").display().to_string()
                && source.kind() == std::io::ErrorKind::PermissionDenied
    ));
}
