//! Broker-side custody of reusable Provider credentials.
//!
//! The maintainer's decision on `louiselm-qbr.5.1.3` is that the broker performs
//! Provider calls itself, so a Session never needs the secret and never receives
//! one. This module is the half that holds the material: it hands out a
//! [`CredentialHandle`] that names a configured Provider and carries nothing
//! else, and exposes the secret only through a crate-internal accessor that
//! borrows the material rather than returning it.
//!
//! Nothing here reaches the network. Performing the request is
//! `louiselm-qbr.5.1.3.2`, which is blocked on this module.
//!
//! Redaction is a type property, not a convention: the private secret type has
//! a hand-written `Debug` and no `Serialize`, so a secret cannot reach a
//! receipt, audit record, or log by being formatted into one.

use std::{
    collections::BTreeMap,
    fmt,
    fs::{self, File},
    io::Read,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::launch_protocol::{ErrorCode, ProtocolError};

/// Largest credential file the broker will read, so a wrong path cannot
/// exhaust memory before the content is rejected as unusable.
const MAX_CREDENTIAL_BYTES: u64 = 16 * 1024;

/// Directory under the broker's machine-lifetime state holding credentials.
const CREDENTIAL_DIRECTORY: &str = "provider-credentials";

/// Reusable Provider credential material.
///
/// Deliberately not `Serialize`, not `Clone`, and not `Display`. Its [`Debug`]
/// renders a fixed placeholder, so the secret cannot reach a durable record or
/// an Agent-visible log through ordinary formatting.
struct ProviderSecret(String);

impl fmt::Debug for ProviderSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ProviderSecret(<redacted>)")
    }
}

/// A Session-visible reference to broker-held credential material.
///
/// Naming a Provider is all the authority a Session has here: the handle
/// carries no secret, and resolving it requires the store that actually holds
/// the material.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialHandle {
    /// Configured Provider this handle names.
    provider: String,
}

impl CredentialHandle {
    /// Returns the configured Provider this handle names.
    #[must_use]
    pub fn provider(&self) -> &str {
        &self.provider
    }
}

/// Ownership and mode facts about one path, separated from the filesystem so
/// the trust rules stay testable without a privileged fixture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileFacts {
    /// Owning user.
    pub uid: u32,
    /// Owning group.
    pub gid: u32,
    /// Permission bits.
    pub mode: u32,
    /// Whether the path is a directory.
    pub is_dir: bool,
    /// Whether the path itself is a symbolic link.
    pub is_symlink: bool,
}

impl FileFacts {
    fn read(path: &Path) -> Result<Self, ProtocolError> {
        let metadata = fs::symlink_metadata(path).map_err(|_| unavailable())?;
        Ok(Self {
            uid: metadata.uid(),
            gid: metadata.gid(),
            mode: metadata.permissions().mode() & 0o777,
            is_dir: metadata.is_dir(),
            is_symlink: metadata.file_type().is_symlink(),
        })
    }
}

/// Whether a credential file is private to exactly the broker identity.
///
/// A symlink is refused outright rather than followed: the target's own facts
/// say nothing about who can replace the link.
#[must_use]
pub fn credential_file_trusted(facts: &FileFacts, uid: u32, gid: u32) -> bool {
    !facts.is_symlink
        && !facts.is_dir
        && facts.uid == uid
        && facts.gid == gid
        && facts.mode == 0o600
}

/// Whether the credential directory is private to exactly the broker identity.
#[must_use]
pub fn credential_root_trusted(facts: &FileFacts, uid: u32, gid: u32) -> bool {
    !facts.is_symlink && facts.is_dir && facts.uid == uid && facts.gid == gid && facts.mode == 0o700
}

fn unavailable() -> ProtocolError {
    ProtocolError::new(ErrorCode::CredentialUnavailable, None, None)
}

fn invalid() -> ProtocolError {
    ProtocolError::new(ErrorCode::InvalidRequest, None, None)
}

/// Rejects a Provider id that could name something other than one configured
/// entry, before it ever reaches a path.
fn configured_name(provider: &str) -> Result<&str, ProtocolError> {
    let usable = !provider.is_empty()
        && provider.len() <= 64
        && provider
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    if usable { Ok(provider) } else { Err(invalid()) }
}

/// Broker-side custody of every configured Provider credential.
///
/// The store is the only thing in the process that holds secrets, and it never
/// yields one by value.
pub struct ProviderCredentialStore {
    secrets: BTreeMap<String, ProviderSecret>,
}

impl fmt::Debug for ProviderCredentialStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderCredentialStore")
            .field("providers", &self.secrets.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl ProviderCredentialStore {
    /// Builds a store from already-held material, for tests and for callers
    /// that loaded configuration through their own audited path.
    #[must_use]
    pub fn in_memory<'a>(entries: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        Self {
            secrets: entries
                .into_iter()
                .map(|(provider, secret)| (provider.to_owned(), ProviderSecret(secret.to_owned())))
                .collect(),
        }
    }

    /// Loads every credential under the broker's private state directory.
    ///
    /// Each entry is one file named for its Provider. The directory and every
    /// file must be private to exactly the broker identity; anything wider,
    /// foreign, or reached through a symlink is refused rather than read.
    ///
    /// # Errors
    /// Returns [`ErrorCode::CredentialUnavailable`] when the directory or any
    /// entry is missing, untrusted, oversized, or unreadable, and
    /// [`ErrorCode::InvalidRequest`] when a filename could not name a
    /// configured Provider.
    pub fn open(state: &Path, uid: u32, gid: u32) -> Result<Self, ProtocolError> {
        let root = state.join(CREDENTIAL_DIRECTORY);
        if !credential_root_trusted(&FileFacts::read(&root)?, uid, gid) {
            return Err(unavailable());
        }
        let mut secrets = BTreeMap::new();
        for entry in fs::read_dir(&root).map_err(|_| unavailable())? {
            let entry = entry.map_err(|_| unavailable())?;
            let name = entry.file_name().into_string().map_err(|_| invalid())?;
            let provider = configured_name(&name)?.to_owned();
            let path = entry.path();
            if !credential_file_trusted(&FileFacts::read(&path)?, uid, gid) {
                return Err(unavailable());
            }
            secrets.insert(provider, ProviderSecret(read_secret(&path)?));
        }
        Ok(Self { secrets })
    }

    /// Returns the private directory a broker state root must provide.
    #[must_use]
    pub fn root_in(state: &Path) -> PathBuf {
        state.join(CREDENTIAL_DIRECTORY)
    }

    /// Returns a handle naming a configured Provider.
    ///
    /// # Errors
    /// Returns [`ErrorCode::InvalidRequest`] when no such Provider is
    /// configured, and [`ErrorCode::CredentialUnavailable`] when the configured
    /// entry holds no usable material.
    pub fn handle(&self, provider: &str) -> Result<CredentialHandle, ProtocolError> {
        let name = configured_name(provider)?;
        let secret = self.secrets.get(name).ok_or_else(invalid)?;
        if secret.0.is_empty() {
            return Err(unavailable());
        }
        Ok(CredentialHandle {
            provider: name.to_owned(),
        })
    }

    /// Borrows the secret for one broker-side operation.
    ///
    /// Crate-internal on purpose: this is how the Provider request path
    /// (`louiselm-qbr.5.1.3.2`) authenticates a call the broker itself makes.
    /// It lends the material for the duration of `use_secret` instead of
    /// returning it, so no caller can store or forward it.
    ///
    /// # Errors
    /// Returns [`ErrorCode::InvalidRequest`] when the handle names a Provider
    /// this store does not hold, and [`ErrorCode::CredentialUnavailable`] when
    /// the entry holds no usable material.
    // Configuration-specific: the tests below exercise this accessor, so an
    // expectation would be unfulfilled in the test build and fire there instead.
    #[allow(
        dead_code,
        reason = "The only production caller is the Provider request path in louiselm-qbr.5.1.3.2, which is blocked on this task; custody is useless without a way for the broker to use what it holds, and widening this to `pub` to satisfy the lint would hand the secret to every crate consumer."
    )]
    pub(crate) fn with_secret<T>(
        &self,
        handle: &CredentialHandle,
        use_secret: impl FnOnce(&str) -> T,
    ) -> Result<T, ProtocolError> {
        let secret = self.secrets.get(handle.provider()).ok_or_else(invalid)?;
        if secret.0.is_empty() {
            return Err(unavailable());
        }
        Ok(use_secret(&secret.0))
    }

    /// Returns every configured Provider name, which is not secret.
    #[must_use]
    pub fn providers(&self) -> Vec<&str> {
        self.secrets.keys().map(String::as_str).collect()
    }
}

fn read_secret(path: &Path) -> Result<String, ProtocolError> {
    let mut file = File::open(path).map_err(|_| unavailable())?;
    let mut content = String::new();
    file.by_ref()
        .take(MAX_CREDENTIAL_BYTES + 1)
        .read_to_string(&mut content)
        .map_err(|_| unavailable())?;
    if content.len() as u64 > MAX_CREDENTIAL_BYTES {
        return Err(unavailable());
    }
    let trimmed = content.trim().to_owned();
    if trimmed.is_empty() {
        return Err(unavailable());
    }
    Ok(trimmed)
}

#[cfg(test)]
#[path = "provider_credentials_tests.rs"]
mod tests;
