//! Explicit administrative compromise decisions share the signing transaction lock.

use super::{
    LauncherError, LauncherPaths, LauncherSigner, acquire_install_lock,
    inspect_system_existing_state_dirs, public_keyring, require_system_existing_state_files,
    require_system_install_context, require_system_state_dirs, validate_paths, write_json_atomic,
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
};

/// Local supervisor observation after withdrawal of key authority.
/// This is not a Launcher receipt and does not restore historical trust.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyContainment {
    /// Capabilities revoked and process tree observed frozen; identity retained.
    Frozen,
    /// Required capability revocation or freeze could not be proved.
    Failed,
}

/// Root-owned local control evidence, separate from compromised signatures.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyContainmentReport {
    /// Exact Session bound by trusted genesis registration.
    pub session_id: String,
    /// Key named by that original registration.
    pub key_id: String,
    /// Observed outcome; neither variant authorizes continuation.
    pub containment: KeyContainment,
}

fn containment_path(paths: &LauncherPaths, session_id: &str) -> std::path::PathBuf {
    paths.state_root.join("key-containment").join(format!(
        "{}.json",
        crate::Digest::of(session_id.as_bytes()).hex()
    ))
}

pub(super) fn containment(
    paths: &LauncherPaths,
    session_id: &str,
    key_id: &str,
) -> Result<Option<KeyContainmentReport>, LauncherError> {
    let path = containment_path(paths, session_id);
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(super::io_error("key containment", error)),
        Ok(_) => {}
    }
    super::require_root_metadata(
        path.parent()
            .ok_or_else(|| LauncherError::Invalid("missing containment parent".into()))?,
        0o711,
        true,
        "key containment directory",
    )?;
    super::require_root_metadata(&path, 0o444, false, "key containment")?;
    let report: KeyContainmentReport = super::read_required_json(&path)?;
    if report.session_id != session_id || report.key_id != key_id {
        return Err(LauncherError::Invalid(
            "foreign key containment observation".into(),
        ));
    }
    Ok(Some(report))
}

/// Persisted compromise decision; does not claim mechanical containment.
#[derive(Debug, Serialize)]
pub struct KeyRevocation {
    /// Exact installed key whose authority was revoked.
    pub key_id: String,
    /// Original administrative revocation time, retained on retry.
    pub revoked_at_ms: u64,
}

/// Trusted inspection of a Session affected by administrative key revocation.
#[derive(Clone, Debug, Serialize)]
pub struct SessionKeyRevocation {
    /// Original Session identity from root-registered genesis authority.
    pub session_id: String,
    /// Compromised key, never inferred from untrusted receipt text.
    pub key_id: String,
    /// Administrative decision time, not a historical receipt trust cutoff.
    pub revoked_at_ms: u64,
    /// Last local supervisor observation; absent means containment is unconfirmed.
    pub observation: Option<KeyContainmentReport>,
    /// Safe operator action; this record grants no continuation authority.
    pub next_action: String,
}

pub(super) fn session_revocation(
    paths: &LauncherPaths,
    session_id: &str,
) -> Result<Option<SessionKeyRevocation>, LauncherError> {
    let binding = match super::history::load(paths, session_id) {
        Ok(binding) => binding.anchor(),
        Err(LauncherError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    let ring = public_keyring(paths)?;
    let key = ring
        .key(&binding.signing_key_id)
        .ok_or_else(|| LauncherError::Invalid("unknown historical launcher key".into()))?;
    let Some(revoked_at_ms) = key.revoked_at_ms else {
        return Ok(None);
    };
    let observation = containment(paths, session_id, &key.key_id)?;
    Ok(Some(SessionKeyRevocation {
        session_id: session_id.into(), key_id: key.key_id.clone(), revoked_at_ms, observation,
        next_action: "Keep this Session disabled; inspect local containment and controller/supervisor health. Original receipts remain untrusted; no automatic recovery is safe.".into(),
    }))
}

pub(super) fn affected_sessions(
    paths: &LauncherPaths,
) -> Result<Vec<SessionKeyRevocation>, LauncherError> {
    let directory = paths.state_root.join("receipt-bindings");
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(super::io_error("affected Session inspection", error)),
    };
    let mut affected = Vec::new();
    for entry in entries {
        let path = entry
            .map_err(|error| super::io_error("affected Session inspection", error))?
            .path();
        super::require_root_metadata(&path, 0o444, false, "receipt binding")?;
        let binding: super::history::ReceiptBinding = super::read_required_json(&path)?;
        if let Some(report) = session_revocation(paths, &binding.anchor().session_id)? {
            affected.push(report);
        }
    }
    affected.sort_by(|left, right| left.session_id.cmp(&right.session_id));
    Ok(affected)
}

/// Irreversibly revokes one known launcher key, including its entire history.
///
/// Production paths require root. This administrative entrypoint is never part
/// of the Agent protocol or launcher sudo command set. It preserves public and
/// private keys, receipt bytes and routine retirement metadata. Rotation does
/// not undo the decision. Success proves persistence only, not Session freeze.
///
/// # Errors
/// Refuses untrusted installation, unknown keys, concurrent installation/signing,
/// or failed durable publication. Retry the exact key after any uncertain error.
pub fn revoke_key(
    paths: &LauncherPaths,
    key_id: &str,
    now_ms: u64,
) -> Result<KeyRevocation, LauncherError> {
    validate_paths(paths)?;
    require_system_install_context(paths)?;
    inspect_system_existing_state_dirs(paths)?;
    require_system_state_dirs(paths)?;
    require_system_existing_state_files(paths)?;
    let _lock = acquire_install_lock(paths)?;
    let mut ring = public_keyring(paths)?;
    let key = ring
        .keys
        .iter_mut()
        .find(|key| key.key_id == key_id)
        .ok_or_else(|| LauncherError::Invalid("unknown launcher key".into()))?;
    let revoked_at_ms = *key.revoked_at_ms.get_or_insert(now_ms);
    // Exact retries repeat publication/fsync, including after rename succeeded
    // but its durability acknowledgement failed.
    write_json_atomic(&paths.keyring(), &ring, 0o444)?;
    Ok(KeyRevocation {
        key_id: key_id.into(),
        revoked_at_ms,
    })
}

pub(super) fn require_key(paths: &LauncherPaths, key_id: &str) -> Result<(), LauncherError> {
    let ring = public_keyring(paths)?;
    let key = ring
        .key(key_id)
        .ok_or_else(|| LauncherError::Invalid("unknown launcher key".into()))?;
    if key.revoked_at_ms.is_some() {
        return Err(LauncherError::Invalid(
            "launcher key revoked: all associated history is untrusted".into(),
        ));
    }
    Ok(())
}

impl LauncherSigner {
    /// Persists this supervisor's unsigned local containment result.
    /// The caller owns the process tree; this records an observation, not authority.
    /// # Errors
    /// Refuses a foreign Session/key binding or failed durable publication.
    pub fn record_containment(
        &self,
        key_id: &str,
        session_id: &str,
        containment: KeyContainment,
    ) -> Result<(), LauncherError> {
        let binding = super::history::load(&self.paths, session_id)?.anchor();
        if binding.signing_key_id != key_id {
            return Err(LauncherError::Invalid("foreign containment key".into()));
        }
        let path = containment_path(&self.paths, session_id);
        let directory = path
            .parent()
            .ok_or_else(|| LauncherError::Invalid("missing containment parent".into()))?;
        match fs::create_dir(directory) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(super::io_error("key containment directory", error)),
        }
        let metadata = fs::symlink_metadata(directory)
            .map_err(|error| super::io_error("key containment directory", error))?;
        if !metadata.is_dir() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
            return Err(LauncherError::Invalid(
                "untrusted key containment directory".into(),
            ));
        }
        fs::set_permissions(directory, fs::Permissions::from_mode(0o711))
            .map_err(|error| super::io_error("key containment mode", error))?;
        fs::File::open(&self.paths.state_root)
            .and_then(|file| file.sync_all())
            .map_err(|error| super::io_error("key containment durability", error))?;
        write_json_atomic(
            &path,
            &KeyContainmentReport {
                session_id: session_id.into(),
                key_id: key_id.into(),
                containment,
            },
            0o444,
        )
    }

    /// Rechecks current key authority even for an already-open signer.
    ///
    /// # Errors
    /// Refuses revoked/unknown keys or unreadable installed authority.
    pub fn require_key_authority(&self, key_id: &str) -> Result<(), LauncherError> {
        require_key(&self.paths, key_id)?;
        if public_keyring(&self.paths)?
            .key(key_id)
            .is_none_or(|key| key.private_key_cleanup_authorized)
        {
            return Err(LauncherError::Invalid(
                "retired private-key signing is closed".into(),
            ));
        }
        Ok(())
    }
}
