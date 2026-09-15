//! Root-owned signing references and exact retired-private-key removal.
//!
//! The keyring is the exhaustive admission ledger, not a filesystem inventory.
//! All transitions share the install/signing lock. A missing ledger is unknown
//! authority, and a crashed supervisor leaves a live reference indefinitely.

use super::{
    LauncherError, LauncherKey, LauncherPaths, LauncherSigner, acquire_install_lock, history,
    io_error, private_key_path, public_keyring, write_json_atomic,
};
use crate::launch_receipt::{ReceiptPayload, SessionState};
use serde::{Deserialize, Serialize};
use std::{fs, io, os::unix::fs::MetadataExt, path::Path, time::Instant};

/// Signing lifetime recorded by trusted admission and the owning supervisor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionKeyState {
    /// The Session may still require lifecycle signatures, including while Parked.
    Live,
    /// Cleanup and all terminal receipt obligations have completed; no more signing.
    Complete,
}

/// Successful exact private-key cleanup; public authority remains installed.
#[derive(Debug, Serialize)]
pub struct KeyCleanup {
    /// Retired key whose private file is durably absent, including on retry.
    pub key_id: String,
}

pub(super) fn validate(key: &LauncherKey, active: &str) -> Result<(), LauncherError> {
    if key.private_key_cleanup_authorized
        && (key.key_id == active
            || key.signing_sessions.as_ref().is_none_or(|sessions| {
                sessions
                    .values()
                    .any(|state| *state == SessionKeyState::Live)
            }))
    {
        return Err(LauncherError::Invalid(
            "private-key cleanup contradicts active or live signing authority".into(),
        ));
    }
    Ok(())
}

/// Caller holds the signing lock through genesis registration and signature return.
pub(super) fn register_session(
    paths: &LauncherPaths,
    receipt: &ReceiptPayload,
    new_binding: bool,
) -> Result<(), LauncherError> {
    let mut ring = public_keyring(paths)?;
    let key = ring
        .keys
        .iter_mut()
        .find(|key| key.key_id == receipt.signing_key_id)
        .ok_or_else(|| LauncherError::Invalid("unknown signing key".into()))?;
    if key.private_key_cleanup_authorized {
        return Err(LauncherError::Invalid(
            "retired private-key signing is closed".into(),
        ));
    }
    let Some(sessions) = key.signing_sessions.as_mut() else {
        // Missing exhaustive authority cannot be reconstructed from bindings.
        // Historical signing remains possible, but cleanup is never authorized.
        return Ok(());
    };
    match sessions.get(&receipt.session_id) {
        Some(SessionKeyState::Complete) => {
            return Err(LauncherError::Invalid(
                "Session signing lifetime has completed".into(),
            ));
        }
        Some(SessionKeyState::Live) => {}
        None if new_binding => {
            sessions.insert(receipt.session_id.clone(), SessionKeyState::Live);
        }
        None => {
            return Err(LauncherError::Invalid(
                "registered Session has no authoritative signing reference".into(),
            ));
        }
    }
    // Also fsync exact retries: a prior rename may have outlived a failed fsync.
    write_json_atomic(&paths.keyring(), &ring, 0o444)
}

impl LauncherSigner {
    /// The owning supervisor calls only after proven cleanup and terminal ACK,
    /// with its event receiver disconnected. This is not a broker/operator claim.
    pub(crate) fn complete_session(
        &self,
        receipt: &ReceiptPayload,
        deadline: Instant,
    ) -> Result<(), LauncherError> {
        ReceiptPayload::parse_canonical(&receipt.canonical_bytes())
            .map_err(|error| LauncherError::Malformed(error.to_string()))?;
        if receipt.resulting_state != SessionState::Terminal
            || receipt.release_id != self.release_id
            || self.keyring.key(&receipt.signing_key_id).is_none()
        {
            return Err(LauncherError::Invalid("Session is not terminal".into()));
        }
        let _lock = history::signing_lock(&self.paths, deadline)?;
        // The signer was opened against trusted root authority. Use the same
        // binding validation as signing (including refusal of special files).
        history::prepare(&self.paths, receipt)?;
        let mut ring = public_keyring(&self.paths)?;
        let key = ring
            .keys
            .iter_mut()
            .find(|key| key.key_id == receipt.signing_key_id)
            .ok_or_else(|| LauncherError::Invalid("unknown completion key".into()))?;
        let state = key.signing_sessions.as_mut()
            .and_then(|sessions| sessions.get_mut(&receipt.session_id))
            .ok_or_else(|| LauncherError::Invalid(
                "missing authoritative Session reference; retain the private key and inspect launcher authority".into(),
            ))?;
        *state = SessionKeyState::Complete;
        write_json_atomic(&self.paths.keyring(), &ring, 0o444)?;
        cleanup_if_ready(&self.paths, &receipt.signing_key_id)
    }
}

/// Caller holds the install lock; live, revoked and unknown keys remain retained.
pub(super) fn cleanup_if_ready(paths: &LauncherPaths, key_id: &str) -> Result<(), LauncherError> {
    let ring = public_keyring(paths)?;
    let key = ring
        .key(key_id)
        .ok_or_else(|| LauncherError::Invalid("unknown cleanup key".into()))?;
    if key.retired_at_ms.is_some()
        && key.revoked_at_ms.is_none()
        && key.signing_sessions.as_ref().is_some_and(|sessions| {
            sessions
                .values()
                .all(|state| *state == SessionKeyState::Complete)
        })
    {
        cleanup_locked(paths, key_id, remove_private)?;
    }
    Ok(())
}

/// Retries removal of exactly one routinely retired private key.
///
/// This administrative maintenance action is outside the Agent protocol and
/// sudo command set. It never infers completion from age, process absence or an
/// empty directory. Root authority records every admitted Session and only its
/// supervisor can complete that reference. Public keys and bindings remain.
///
/// # Errors
/// Refuses active/revoked keys, live or missing references, unsafe installation,
/// lock contention, or failed deletion/durability. Retry this exact key after
/// inspecting the failure; never remove an outstanding reference manually.
pub fn cleanup_key(paths: &LauncherPaths, key_id: &str) -> Result<KeyCleanup, LauncherError> {
    super::validate_paths(paths)?;
    super::require_system_install_context(paths)?;
    super::inspect_system_existing_state_dirs(paths)?;
    super::require_system_state_dirs(paths)?;
    super::require_system_existing_state_files(paths)?;
    let _lock = acquire_install_lock(paths)?;
    cleanup_locked(paths, key_id, remove_private)
}

fn cleanup_locked(
    paths: &LauncherPaths,
    key_id: &str,
    remove: impl FnOnce(&Path) -> io::Result<()>,
) -> Result<KeyCleanup, LauncherError> {
    let mut ring = public_keyring(paths)?;
    let key = ring
        .keys
        .iter_mut()
        .find(|key| key.key_id == key_id)
        .ok_or_else(|| LauncherError::Invalid("unknown cleanup key".into()))?;
    if key.retired_at_ms.is_none() || key.revoked_at_ms.is_some() {
        return Err(LauncherError::Invalid(
            "cleanup requires an ordinarily retired key".into(),
        ));
    }
    let sessions = key.signing_sessions.as_ref().ok_or_else(|| LauncherError::Invalid(
        "missing authoritative signing ledger; retain private material and inspect launcher authority".into(),
    ))?;
    if sessions
        .values()
        .any(|state| *state == SessionKeyState::Live)
    {
        return Err(LauncherError::Invalid(
            "outstanding Session signing references; finish their lifecycle before retrying cleanup".into(),
        ));
    }
    let private = private_key_path(&paths.keys(), key_id)?;
    let parent = private
        .parent()
        .ok_or_else(|| LauncherError::Invalid("missing key directory".into()))?;
    for directory in [paths.keys(), parent.to_path_buf()] {
        let metadata = fs::symlink_metadata(directory)
            .map_err(|error| io_error("private-key directory", error))?;
        if !metadata.is_dir()
            || metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.mode() & 0o777 != 0o700
        {
            return Err(LauncherError::Invalid(
                "untrusted private-key directory".into(),
            ));
        }
    }
    match fs::symlink_metadata(&private) {
        Ok(metadata) if metadata.uid() == rustix::process::geteuid().as_raw() => {
            super::ensure_private_key(&private)?;
        }
        Err(error)
            if error.kind() == io::ErrorKind::NotFound && key.private_key_cleanup_authorized => {}
        _ => {
            return Err(LauncherError::Invalid(
                "private key missing or unsafe before authorized cleanup".into(),
            ));
        }
    }
    // Persist signing shutdown BEFORE unlink. A crash can leave the file, but
    // cannot permit signing again. Retry repeats both publication and dir fsync.
    key.private_key_cleanup_authorized = true;
    write_json_atomic(&paths.keyring(), &ring, 0o444)?;
    remove(&private).map_err(|error| {
        io_error(
            "retired private-key removal; retry cleanup-key with the same key ID",
            error,
        )
    })?;
    Ok(KeyCleanup {
        key_id: key_id.into(),
    })
}

fn remove_private(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("missing key parent"))?;
    fs::File::open(parent)?.sync_all()
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "Disposable key fixtures assert setup and fault boundaries directly."
)]
mod tests {
    use super::super::{KEYRING_SCHEMA, PublicKeyring, SystemCommandRunner};
    use super::*;

    #[test]
    fn oversized_authority_update_preserves_the_readable_record() {
        let root = tempfile::TempDir::new().unwrap();
        let path = root.path().join("authority.json");
        write_json_atomic(&path, &"original authority", 0o444).unwrap();
        let original = fs::read(&path).unwrap();
        let oversized = "x".repeat(usize::try_from(super::super::MAX_STATE_BYTES).unwrap());
        assert!(
            write_json_atomic(&path, &oversized, 0o444).is_err(),
            "oversized update must refuse before replacing readable authority"
        );
        assert_eq!(fs::read(path).unwrap(), original);
    }

    #[test]
    fn failed_unlink_or_fsync_never_reopens_signing_and_retries_exact_removal() {
        let root = tempfile::TempDir::new().unwrap();
        let paths = super::super::tests::signer_paths(root.path());
        fs::create_dir(&paths.release_prefix).unwrap();
        super::super::ensure_state_dirs(&paths).unwrap();
        let mut ring = PublicKeyring {
            schema: KEYRING_SCHEMA.into(),
            active_key_id: String::new(),
            keys: Vec::new(),
        };
        for retired in [true, false] {
            let generated = super::super::generate_key(&paths, &SystemCommandRunner).unwrap();
            if !retired {
                ring.active_key_id.clone_from(&generated.key_id);
            }
            ring.keys.push(LauncherKey {
                key_id: generated.key_id,
                public_key: generated.public_key,
                created_at_ms: 1,
                retired_at_ms: retired.then_some(2),
                revoked_at_ms: None,
                signing_sessions: Some(std::collections::BTreeMap::new()),
                private_key_cleanup_authorized: false,
                rotation_id: None,
                replaces: None,
            });
        }
        write_json_atomic(&paths.keyring(), &ring, 0o444).unwrap();
        let key_id = &ring.keys[0].key_id;
        let key = private_key_path(&paths.keys(), key_id).unwrap();
        let public_before = ring.keys[0].public_key.clone();
        let _lock = acquire_install_lock(&paths).unwrap();
        assert!(
            cleanup_locked(&paths, key_id, |_| {
                assert!(
                    public_keyring(&paths)
                        .unwrap()
                        .key(key_id)
                        .unwrap()
                        .private_key_cleanup_authorized
                );
                Err(io::Error::other("injected unlink failure"))
            })
            .is_err()
        );
        assert!(key.is_file());
        assert!(
            cleanup_locked(&paths, key_id, |path| {
                fs::remove_file(path)?;
                Err(io::Error::other("injected directory fsync failure"))
            })
            .is_err()
        );
        assert!(!key.exists());
        cleanup_locked(&paths, key_id, remove_private).unwrap();
        cleanup_locked(&paths, key_id, remove_private).unwrap();
        let after = public_keyring(&paths).unwrap();
        assert_eq!(after.key(key_id).unwrap().public_key, public_before);
        assert!(
            private_key_path(&paths.keys(), &after.active_key_id)
                .unwrap()
                .is_file()
        );
        let receipt =
            super::super::tests::receipt_bytes(&crate::Digest::of(b"release").to_string(), key_id);
        let payload = ReceiptPayload::parse_canonical(&receipt).unwrap();
        assert!(
            register_session(&paths, &payload, true).is_err(),
            "late signing cannot reopen a deleted key"
        );
    }
}
