//! The production launch registry is pathname authority, not merely JSON.

mod support;

use std::{fs, os::unix::fs::PermissionsExt};

use louiselm_skills::registry::{Registry, RegistryError};
use support::{write_file, write_registry};

#[test]
fn trusted_registry_requires_root_owned_immutable_documents_and_runtime() {
    if !rustix::process::geteuid().is_root() {
        eprintln!("skipping: trusted registry ownership requires root");
        return;
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
