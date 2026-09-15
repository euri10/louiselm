//! Root-registered genesis bindings survive rotation and release replacement.
//!
//! Registration is serialized with installation/rotation and completes before a
//! genesis signature leaves the signer. It proves historical admission, never
//! current launch permission or that the broker acknowledged the receipt.

use super::{
    InstallLock, LauncherConfig, LauncherError, LauncherPaths, acquire_install_lock, io_error,
    public_keyring, read_required_json, require_configured_release, write_json_atomic,
};
use crate::{
    Digest,
    launch_receipt::{ChainAnchor, ReceiptPayload},
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const SCHEMA: &str = "louiselm.launch.receipt-binding/1";

pub(super) fn signing_lock(
    paths: &LauncherPaths,
    deadline: Instant,
) -> Result<InstallLock, LauncherError> {
    loop {
        match acquire_install_lock(paths) {
            Err(LauncherError::InstallBusy) if Instant::now() < deadline => {
                std::thread::sleep(
                    Duration::from_millis(5)
                        .min(deadline.saturating_duration_since(Instant::now())),
                );
            }
            result => return result,
        }
    }
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReceiptBinding {
    schema: String,
    session_id: String,
    run_id: String,
    release_id: String,
    signing_key_id: String,
    genesis_payload_digest: String,
}

impl ReceiptBinding {
    pub(super) fn anchor(&self) -> ChainAnchor {
        ChainAnchor {
            session_id: self.session_id.clone(),
            run_id: self.run_id.clone(),
            release_id: self.release_id.clone(),
            signing_key_id: self.signing_key_id.clone(),
        }
    }

    pub(super) fn check(&self, receipt: &ReceiptPayload) -> Result<(), LauncherError> {
        if self.schema != SCHEMA
            || self.session_id != receipt.session_id
            || self.run_id != receipt.run_id
            || self.release_id != receipt.release_id
            || self.signing_key_id != receipt.signing_key_id
            || (receipt.sequence == 0
                && self.genesis_payload_digest
                    != Digest::of(&receipt.canonical_bytes()).to_string())
        {
            return Err(LauncherError::Invalid(
                "receipt does not match registered genesis authority".to_owned(),
            ));
        }
        Ok(())
    }
}

fn directory(paths: &LauncherPaths) -> PathBuf {
    paths.state_root.join("receipt-bindings")
}

fn path(paths: &LauncherPaths, session_id: &str) -> PathBuf {
    directory(paths).join(format!("{}.json", Digest::of(session_id.as_bytes()).hex()))
}

fn require_binding(path: &Path, owner: u32) -> Result<(), LauncherError> {
    let metadata = fs::symlink_metadata(path).map_err(|e| io_error("receipt binding", e))?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.uid() != owner
        || metadata.mode() & 0o777 != 0o444
    {
        return Err(LauncherError::Invalid(
            "untrusted receipt binding ownership or file kind".to_owned(),
        ));
    }
    Ok(())
}

/// Public verification must establish ownership of the full authority path.
pub(super) fn load(
    paths: &LauncherPaths,
    session_id: &str,
) -> Result<ReceiptBinding, LauncherError> {
    let directory = directory(paths);
    for parent in directory.ancestors() {
        let metadata =
            fs::symlink_metadata(parent).map_err(|e| io_error("receipt authority", e))?;
        if !metadata.is_dir() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
            return Err(LauncherError::Invalid(
                "receipt authority has an untrusted ancestor".to_owned(),
            ));
        }
    }
    let path = path(paths, session_id);
    require_binding(&path, 0)?;
    let binding: ReceiptBinding = read_required_json(&path)?;
    if binding.schema != SCHEMA || binding.session_id != session_id {
        return Err(LauncherError::Invalid(
            "receipt binding identity mismatch".to_owned(),
        ));
    }
    for digest in [
        &binding.release_id,
        &binding.signing_key_id,
        &binding.genesis_payload_digest,
    ] {
        if Digest::parse(digest)
            .map_err(|e| LauncherError::Malformed(e.to_string()))?
            .to_string()
            != *digest
        {
            return Err(LauncherError::Invalid(
                "noncanonical receipt binding digest".to_owned(),
            ));
        }
    }
    Ok(binding)
}

/// Caller holds the install lock across this check, signing and registration.
pub(super) fn prepare(
    paths: &LauncherPaths,
    receipt: &ReceiptPayload,
) -> Result<Option<ReceiptBinding>, LauncherError> {
    let path = path(paths, &receipt.session_id);
    let exists = match fs::symlink_metadata(&path) {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(io_error("receipt binding", error)),
    };
    if exists {
        // Inspect before opening: a special file must refuse without blocking.
        require_binding(&path, rustix::process::geteuid().as_raw())?;
        let binding: ReceiptBinding = read_required_json(&path)?;
        binding.check(receipt)?;
        // Registration can reach rename but fail fsync. Exact retries must
        // establish durability before returning another signature.
        fs::File::open(&path)
            .and_then(|file| file.sync_all())
            .map_err(|e| io_error("receipt binding durability", e))?;
        fs::File::open(directory(paths))
            .and_then(|file| file.sync_all())
            .map_err(|e| io_error("receipt authority durability", e))?;
        return Ok(None);
    }
    let config: LauncherConfig = read_required_json(&paths.config())?;
    require_configured_release(paths, &config)?;
    let keys = public_keyring(paths)?;
    if receipt.sequence != 0
        || receipt.release_id != config.release_id
        || receipt.signing_key_id != keys.active_key_id
    {
        return Err(LauncherError::Invalid(
            "new receipt chain requires current release and active key".to_owned(),
        ));
    }
    Ok(Some(ReceiptBinding {
        schema: SCHEMA.to_owned(),
        session_id: receipt.session_id.clone(),
        run_id: receipt.run_id.clone(),
        release_id: receipt.release_id.clone(),
        signing_key_id: receipt.signing_key_id.clone(),
        genesis_payload_digest: Digest::of(&receipt.canonical_bytes()).to_string(),
    }))
}

/// Publish only after signing/verification succeeds, before returning a signature.
pub(super) fn register(
    paths: &LauncherPaths,
    binding: &ReceiptBinding,
) -> Result<(), LauncherError> {
    let directory = directory(paths);
    match fs::DirBuilder::new().mode(0o711).create(&directory) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(io_error("receipt authority", error)),
    }
    let metadata =
        fs::symlink_metadata(&directory).map_err(|e| io_error("receipt authority", e))?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o022 != 0
    {
        return Err(LauncherError::Invalid(
            "unsafe receipt authority directory".to_owned(),
        ));
    }
    // mkdir's mode is filtered by umask. An interrupted mkdir/chmod sequence
    // may leave a narrower owned directory, which the same registration resumes.
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o711))
        .map_err(|e| io_error("receipt authority permissions", e))?;
    fs::File::open(&paths.state_root)
        .and_then(|file| file.sync_all())
        .map_err(|e| io_error("receipt authority durability", e))?;
    write_json_atomic(&path(paths, &binding.session_id), binding, 0o444)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "Temporary lock fixtures assert synchronization and setup directly."
)]
mod tests {
    use super::*;

    #[test]
    fn signing_waits_for_rotation_and_respects_its_deadline() {
        let root = tempfile::TempDir::new().unwrap();
        let paths = super::super::tests::signer_paths(root.path());
        fs::create_dir(&paths.release_prefix).unwrap();
        super::super::ensure_state_dirs(&paths).unwrap();
        let rotation = acquire_install_lock(&paths).unwrap();
        assert!(matches!(
            signing_lock(&paths, Instant::now()),
            Err(LauncherError::InstallBusy)
        ));
        let (started, start) = std::sync::mpsc::channel();
        let (finished, finish) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            started.send(()).unwrap();
            let lock = signing_lock(&paths, Instant::now() + Duration::from_secs(2)).unwrap();
            finished.send(()).unwrap();
            drop(lock);
        });
        start.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(matches!(
            finish.recv_timeout(Duration::from_millis(20)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        drop(rotation);
        finish.recv_timeout(Duration::from_secs(2)).unwrap();
        worker.join().unwrap();
    }
}
