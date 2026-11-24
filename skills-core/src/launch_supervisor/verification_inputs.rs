//! Current qualified verifier tools and installed package set, never candidate code.

use super::SupervisorError;
use crate::Digest;
use std::{
    fs::File,
    io::Read,
    os::unix::fs::MetadataExt,
    path::Path,
    process::{Command, Stdio},
};

/// Measures the root-owned qualified toolchain and installed package versions.
/// Runs only the fixed installed toolchain checker, with no candidate arguments,
/// environment, Provider access or verification-plan execution. Blocking I/O.
/// # Errors
/// Refuses absent, writable, changed or unqualified tools and package records.
pub fn current() -> Result<Digest, SupervisorError> {
    measure(
        &[
            Path::new("/usr/lib/louiselm/verifier/SHA256SUMS"),
            Path::new("/usr/lib/louiselm/verifier/bin/verifier-toolchain"),
            Path::new("/var/lib/dpkg/status"),
        ],
        Path::new("/usr/lib/louiselm/verifier/bin/verifier-toolchain"),
        0,
    )
}

fn measure(files: &[&Path], checker: &Path, owner: u32) -> Result<Digest, SupervisorError> {
    let before = fingerprint(files, owner)?;
    check_toolchain(checker)?;
    let after = fingerprint(files, owner)?;
    if before != after {
        return Err(SupervisorError::VerifierToolchainUnavailable);
    }
    Ok(after)
}

fn fingerprint(files: &[&Path], owner: u32) -> Result<Digest, SupervisorError> {
    let mut inputs = Vec::new();
    for path in files {
        let file = File::from(
            rustix::fs::open(
                *path,
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
            )
            .map_err(|_| SupervisorError::VerifierToolchainUnavailable)?,
        );
        let meta = file
            .metadata()
            .map_err(|_| SupervisorError::VerifierToolchainUnavailable)?;
        if !meta.is_file()
            || meta.uid() != owner
            || meta.mode() & 0o022 != 0
            || meta.len() > 32 * 1024 * 1024
        {
            return Err(SupervisorError::VerifierToolchainUnavailable);
        }
        let mut bytes = Vec::new();
        file.take(32 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| SupervisorError::VerifierToolchainUnavailable)?;
        if bytes.len() as u64 != meta.len() {
            return Err(SupervisorError::VerifierToolchainUnavailable);
        }
        inputs.push(Digest::of(&bytes).to_string());
    }
    Ok(Digest::of(&serde_json::to_vec(&inputs).map_err(|_| {
        SupervisorError::VerifierToolchainUnavailable
    })?))
}

pub(crate) fn check_toolchain(program: &Path) -> Result<(), SupervisorError> {
    let status = Command::new(program)
        .arg("check")
        .current_dir("/")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    if status.is_ok_and(|status| status.success()) {
        Ok(())
    } else {
        Err(SupervisorError::VerifierToolchainUnavailable)
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        reason = "Private disposable measurement fixtures assert input and permission changes."
    )]
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn checker(root: &Path, script: &str) -> std::path::PathBuf {
        let checker = root.join("checker");
        // A sibling spawn can inherit a writable script FD until exec, making
        // our own exec fail with ETXTBSY even after fs::write closes it. Keep
        // that FD in a waited child instead of this shared test process.
        assert!(
            Command::new("/bin/sh")
                .args([
                    "-c",
                    "umask 077; printf '%s' \"$1\" > \"$2\" && chmod 700 \"$2\"",
                    "checker-fixture",
                    script,
                ])
                .arg(&checker)
                .status()
                .unwrap()
                .success()
        );
        checker
    }

    #[test]
    fn checker_creation_is_safe_during_concurrent_spawns() {
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    for _ in 0..16 {
                        let root = tempfile::tempdir().unwrap();
                        let checker = checker(root.path(), "#!/bin/sh\nexit 0\n");
                        assert_eq!(check_toolchain(&checker), Ok(()));
                    }
                });
            }
        });
    }

    #[test]
    fn toolchain_check_does_not_inherit_operator_private_working_directory() {
        let root = tempfile::tempdir().unwrap();
        let checker = checker(root.path(), "#!/bin/sh\n[ \"$PWD\" = / ]\n");
        assert_eq!(check_toolchain(&checker), Ok(()));
    }

    #[test]
    fn qualified_input_changes_require_new_evidence_and_unsafe_inputs_are_refused() {
        let root = tempfile::tempdir().unwrap();
        let manifest = root.path().join("manifest");
        std::fs::write(&manifest, b"tool-v1").unwrap();
        std::fs::set_permissions(&manifest, std::fs::Permissions::from_mode(0o600)).unwrap();
        let checker = checker(root.path(), "#!/bin/sh\nexit 0\n");
        let owner = rustix::process::geteuid().as_raw();
        let first = measure(&[&manifest, &checker], &checker, owner).unwrap();
        assert_eq!(
            first,
            measure(&[&manifest, &checker], &checker, owner).unwrap()
        );
        std::fs::write(&manifest, b"tool-v2").unwrap();
        assert_ne!(
            first,
            measure(&[&manifest, &checker], &checker, owner).unwrap()
        );
        std::fs::set_permissions(&manifest, std::fs::Permissions::from_mode(0o666)).unwrap();
        assert!(measure(&[&manifest, &checker], &checker, owner).is_err());
        std::fs::remove_file(&manifest).unwrap();
        std::os::unix::fs::symlink(&checker, &manifest).unwrap();
        assert!(measure(&[&manifest, &checker], &checker, owner).is_err());
    }
}
