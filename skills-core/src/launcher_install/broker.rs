//! Public installed authority and bounded, measured receipt verification.

use super::{
    CommandInvocation, LauncherConfig, LauncherError, LauncherPaths, io_error, public_keyring,
    read_required_json, require_configured_release, require_measured_tool, require_root_metadata,
    require_secure_tool, run_signing_command, validate_config, validate_paths,
};
use crate::{
    launch_receipt::{ChainAnchor, MAX_RECEIPT_BYTES, RECEIPT_SCHEMA, ReceiptPayload},
    sshsig,
};
use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// Reads the root-owned public installation without accessing private keys.
///
/// Alternative paths must meet the same root ownership boundary as production.
/// This snapshot establishes installed identities and measured verification
/// tools; it does not authorize any launch or claim a Verified Session.
///
/// # Errors
/// Refuses writable/symlinked authority, invalid configuration or changed release/tool bytes.
pub fn public_runtime_config(paths: &LauncherPaths) -> Result<LauncherConfig, LauncherError> {
    validate_paths(paths)?;
    for parent in paths.state_root.ancestors() {
        let metadata =
            fs::symlink_metadata(parent).map_err(|source| io_error("public authority", source))?;
        if !metadata.is_dir() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
            return Err(LauncherError::Invalid(
                "public authority has an untrusted ancestor".to_owned(),
            ));
        }
    }
    require_root_metadata(
        &paths.public_config(),
        0o444,
        false,
        "public launcher config",
    )?;
    let config: LauncherConfig = read_required_json(&paths.public_config())?;
    validate_config(&config, paths)?;
    require_configured_release(paths, &config)?;
    require_secure_tool(&config.ssh_keygen_path, "ssh-keygen")?;
    require_measured_tool(&config)?;
    Ok(config)
}

/// Public-key verifier consuming root-registered historical Session authority.
///
/// Verification uses the existing bounded process runner with an empty
/// environment and measured absolute OpenSSH executable. No signing key is read.
#[derive(Debug)]
pub struct LauncherVerifier {
    config: LauncherConfig,
    paths: LauncherPaths,
    scratch: PathBuf,
}

impl LauncherVerifier {
    /// Inspects compromise independently of receipt signatures or claimed times.
    /// # Errors
    /// Refuses unavailable shared authority or malformed local containment evidence.
    pub fn key_revocation(
        &self,
        session_id: &str,
    ) -> Result<Option<super::SessionKeyRevocation>, LauncherError> {
        self.check_authority()?;
        super::revocation::session_revocation(&self.paths, session_id)
    }
    /// Opens public authority; `scratch` must be the broker's private state directory.
    ///
    /// # Errors
    /// Refuses untrusted public configuration/keyring or unreadable state.
    pub fn open(paths: &LauncherPaths, scratch: &Path) -> Result<Self, LauncherError> {
        let config = public_runtime_config(paths)?;
        require_root_metadata(&paths.keyring(), 0o444, false, "launcher keyring")?;
        public_keyring(paths)?;
        Ok(Self {
            config,
            paths: paths.clone(),
            scratch: scratch.to_owned(),
        })
    }

    /// Validated installed authority; contains no secrets.
    #[must_use]
    pub const fn config(&self) -> &LauncherConfig {
        &self.config
    }

    /// Rechecks shared public trust separately from an individual chain.
    pub(crate) fn check_authority(&self) -> Result<(), LauncherError> {
        require_root_metadata(&self.paths.keyring(), 0o444, false, "launcher keyring")?;
        public_keyring(&self.paths)?;
        require_measured_tool(&self.config)
    }

    /// Reads the trusted original identity without authorizing a new launch.
    pub(crate) fn receipt_anchor(&self, session_id: &str) -> Result<ChainAnchor, LauncherError> {
        let anchor = super::history::load(&self.paths, session_id)?.anchor();
        super::revocation::require_key(&self.paths, &anchor.signing_key_id)?;
        Ok(anchor)
    }

    /// Verifies exact signed payload bytes with the fixed receipt namespace.
    ///
    /// # Errors
    /// Refuses a foreign key/namespace, malformed signature, changed tool,
    /// verification failure, deadline expiry or unproved verifier cleanup.
    pub fn verify(
        &self,
        key_id: &str,
        payload: &[u8],
        signature: &str,
    ) -> Result<(), LauncherError> {
        let receipt = ReceiptPayload::parse_canonical(payload)
            .map_err(|_| LauncherError::Invalid("invalid launcher receipt payload".to_owned()))?;
        if signature.len() > MAX_RECEIPT_BYTES || receipt.signing_key_id != key_id {
            return Err(LauncherError::Invalid(
                "launcher receipt authority mismatch".to_owned(),
            ));
        }
        super::history::load(&self.paths, &receipt.session_id)?.check(&receipt)?;
        require_root_metadata(&self.paths.keyring(), 0o444, false, "launcher keyring")?;
        let keyring = public_keyring(&self.paths)?;
        let key = keyring
            .key(key_id)
            .ok_or_else(|| LauncherError::Invalid("unknown launcher key".to_owned()))?;
        super::revocation::require_key(&self.paths, key_id)?;
        let parsed = sshsig::parse(signature)
            .map_err(|_| LauncherError::Invalid("invalid launcher signature".to_owned()))?;
        if parsed.namespace != RECEIPT_SCHEMA || parsed.openssh_public_key() != key.public_key {
            return Err(LauncherError::Invalid(
                "launcher signature authority mismatch".to_owned(),
            ));
        }
        require_measured_tool(&self.config)?;
        let scratch = tempfile::Builder::new()
            .prefix("receipt-verify-")
            .tempdir_in(&self.scratch)
            .map_err(|source| io_error("receipt verification scratch", source))?;
        let signature_path = scratch.path().join("signature");
        let allowed_path = scratch.path().join("allowed_signers");
        fs::write(&signature_path, signature)
            .map_err(|source| io_error("receipt signature", source))?;
        fs::write(&allowed_path, format!("launcher {}\n", key.public_key))
            .map_err(|source| io_error("receipt public key", source))?;
        let invocation = CommandInvocation {
            program: self.config.ssh_keygen_path.clone(),
            arguments: vec![
                "-Y".into(),
                "verify".into(),
                "-f".into(),
                allowed_path.into_os_string(),
                "-I".into(),
                "launcher".into(),
                "-n".into(),
                RECEIPT_SCHEMA.into(),
                "-s".into(),
                signature_path.into_os_string(),
            ],
            stdin: payload.to_vec(),
            current_dir: Some(scratch.path().to_owned()),
        };
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(2))
            .ok_or_else(|| LauncherError::Invalid("verification clock unavailable".to_owned()))?;
        let result = run_signing_command(&invocation, deadline, None)
            .map_err(|source| io_error("receipt verifier", source))?;
        if !result.success {
            return Err(LauncherError::Invalid(
                "launcher signature verification failed".to_owned(),
            ));
        }
        super::revocation::require_key(&self.paths, key_id)
    }
}
