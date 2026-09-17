//! Measured warm cache bytes and lifecycle-owned private Session overlays.
//!
//! The base is an immutable, bounded in-memory snapshot: no source path or open
//! source inode survives capture. Overlays are eager independent copies under
//! the existing private home mount. This trades memory/copy cost for no mutable
//! lower layer, shared hardlinks, or additional mount mechanism. Capture does
//! not approve the bytes; the operator must bind the reviewed digest to inputs.
//! All I/O is blocking. Final launch wiring and retention belong to the caller.

use std::{
    fs::{self, File},
    os::unix::fs::MetadataExt as _,
    path::{Path, PathBuf},
};

use thiserror::Error;

use crate::{
    Digest, Manifest, ManifestEntry,
    sandbox::{IdentityPlan, SandboxError, SandboxedSession},
    session_manifest::SessionInputManifest,
};

mod download;
mod filesystem;
pub(crate) use download::DownloadWriter;

/// Maximum captured bytes and maximum size of one broker download.
pub const MAX_BYTES: usize = 128 * 1024 * 1024;
/// Maximum cache entries, including directories.
pub const MAX_ENTRIES: usize = 20_000;

/// A refused cache operation. Display never includes cache paths or contents.
#[derive(Debug, Error)]
pub enum CacheError {
    /// Invalid input, ownership, or enforcement evidence.
    #[error("Session cache refused: {0}")]
    Refused(&'static str),
    /// Filesystem failure retained internally.
    #[error("Session cache filesystem operation failed")]
    Io(#[from] std::io::Error),
    /// Disposal could not establish that the Session tree is gone.
    #[error("Session cache retained: sandbox disposal failed")]
    Sandbox(#[from] SandboxError),
}

struct CacheFile {
    entry: ManifestEntry,
    bytes: Vec<u8>,
}

/// Immutable measured bytes, reusable across Sessions without filesystem aliases.
/// No mutable byte access or automatic publication from an overlay is provided.
pub struct CacheBase {
    files: Vec<CacheFile>,
    digest: Digest,
}

impl CacheBase {
    pub(crate) fn write_snapshot(
        &self,
        root: &Path,
    ) -> Result<(), crate::workspace::WorkspaceError> {
        fs::create_dir(root)?;
        for cached in &self.files {
            let path = root.join(&cached.entry.path);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            crate::workspace::filesystem::write_file(
                &path,
                &cached.bytes,
                if cached.entry.executable {
                    0o500
                } else {
                    0o400
                },
            )?;
        }
        Ok(())
    }

    /// Captures a complete bounded tree using descriptor-relative reads.
    ///
    /// # Errors
    /// Rejects links, special files, noncanonical/colliding paths, mutation
    /// observed during capture, size/depth limits, and filesystem failures.
    pub fn capture(source: &Path) -> Result<Self, CacheError> {
        let files = filesystem::capture(source)?;
        let manifest = Manifest::new(files.iter().map(|file| file.entry.clone()).collect(), false)
            .map_err(|_| CacheError::Refused("invalid base inventory"))?;
        let mut bytes = b"louiselm.cache.base/1\n".to_vec();
        bytes.extend(manifest.canonical_bytes());
        Ok(Self {
            files,
            digest: Digest::of(&bytes),
        })
    }

    /// Exact paths, executable bits, sizes and content hashes of captured bytes.
    #[must_use]
    pub fn digest(&self) -> &Digest {
        &self.digest
    }

    /// Creates a new writable overlay beneath the Session's existing private home.
    ///
    /// Call before starting the Session, with a home whose parent is protected
    /// by the existing launcher ownership boundary. Configure the Agent's cache
    /// path to the returned path. `NamespaceOnly` supports local conformance,
    /// but cannot pass [`CacheOverlay::check_verified`]. Never reuse an overlay
    /// for a new Session. No directory is removed when this handle is dropped.
    ///
    /// # Errors
    /// Refuses invalid Session ids, nonprivate homes, an existing destination,
    /// unavailable ownership enforcement, and failed copy/publication operations.
    pub fn materialize(
        &self,
        home: &Path,
        session_id: &str,
        identity: IdentityPlan,
    ) -> Result<CacheOverlay, CacheError> {
        if session_id.is_empty()
            || session_id.len() > 128
            || !session_id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
        {
            return Err(CacheError::Refused("invalid Session id"));
        }
        filesystem::check_home(home, identity)?;
        let path = home.join(format!("cache-{session_id}"));
        let directory = filesystem::materialize(&self.files, home, &path, identity)?;
        Ok(CacheOverlay {
            directory: Some(directory),
            path,
            session_id: session_id.to_owned(),
            base_digest: self.digest.clone(),
            identity,
        })
    }
}

/// One private writable cache. The Session lifecycle owns retention and disposal.
/// A pinned directory descriptor is the only broker download destination.
pub struct CacheOverlay {
    directory: Option<File>,
    path: PathBuf,
    session_id: String,
    base_digest: Digest,
    identity: IdentityPlan,
}

impl CacheOverlay {
    /// Session whose lifetime owns this pinned private overlay.
    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Path inside the existing private-home mount; never mount the source cache.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Checks the input binding and the existing backend's complete enforcement.
    ///
    /// Evidence must come from the trusted backend, not an Agent-supplied record.
    /// This is an additional cache check, not authorization to launch a Session.
    ///
    /// # Errors
    /// Missing/unknown/unsatisfied enforcement, namespace-only identity, input
    /// substitution, a disposed overlay, or a different Session refuses Verified use.
    pub fn check_verified(
        &self,
        session: &SandboxedSession,
        inputs: &SessionInputManifest,
    ) -> Result<(), CacheError> {
        if self.directory.is_none()
            || session.session_id != self.session_id
            || !matches!(self.identity, IdentityPlan::HostIdentity { .. })
            || inputs.cache_base_digest != self.base_digest.to_string()
            || session.evidence.check().is_err()
        {
            return Err(CacheError::Refused(
                "cache binding or isolation is unverified",
            ));
        }
        Ok(())
    }

    /// Publishes broker-fetched bytes only into this overlay, under a fixed name.
    ///
    /// The broker remains responsible for authorization, transport, aggregate
    /// quotas, and stopping ingress during Park. Bytes are verified before any
    /// write and atomically linked without replacing existing Agent-controlled
    /// entries. There is no caller-supplied destination path and no base writer.
    ///
    /// # Errors
    /// Rejects changed/oversized bytes, a disposed handle, existing destinations,
    /// or unavailable anonymous-file/ownership/publication enforcement.
    pub fn store_download(
        &mut self,
        expected: &Digest,
        bytes: &[u8],
    ) -> Result<String, CacheError> {
        self.download_writer()?.prepare(expected, bytes)?.publish()
    }

    pub(crate) fn download_writer(&self) -> Result<DownloadWriter, CacheError> {
        Ok(DownloadWriter {
            directory: self
                .directory
                .as_ref()
                .ok_or(CacheError::Refused("overlay disposed"))?
                .try_clone()?,
            identity: self.identity,
        })
    }

    /// Disposes the matching process tree before removing its retained cache.
    ///
    /// Park never invokes this operation. Retention/pin policy must approve it;
    /// failures retain the handle for retry. Parent paths must remain under the
    /// launcher's lifecycle ownership; dropping a handle does not delete bytes.
    ///
    /// # Errors
    /// Rejects a different Session, incomplete tree disposal, replaced paths,
    /// and filesystem cleanup failures. No physical-media erasure is promised.
    pub fn dispose(&mut self, session: &mut SandboxedSession) -> Result<(), CacheError> {
        if session.session_id != self.session_id {
            return Err(CacheError::Refused("different Session owns overlay"));
        }
        session.dispose()?;
        let Some(directory) = &self.directory else {
            return Ok(());
        };
        let pinned = directory.metadata()?;
        let named = fs::symlink_metadata(&self.path)?;
        if !named.is_dir() || (pinned.dev(), pinned.ino()) != (named.dev(), named.ino()) {
            return Err(CacheError::Refused(
                "overlay path replaced; retain for cleanup",
            ));
        }
        fs::remove_dir_all(&self.path)?;
        self.directory = None;
        Ok(())
    }
}
