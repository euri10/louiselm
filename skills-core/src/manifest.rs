//! The canonical manifest: the portable identity of a Skill package.
//!
//! A manifest lists every regular file in the package, sorted by path, with the
//! content address and the single normalized mode bit each carries. It is the
//! Merkle root of the package: the package digest is the digest of the
//! manifest's canonical bytes, so two trees agree exactly when every path,
//! executable bit, size, and content hash agrees.
//!
//! Deliberately absent: modification times, ownership, permission bits beyond
//! `executable`, inode numbers, the source location the tree was captured from,
//! empty directories, and symlinks. Everything omitted here is either a local
//! fact (which belongs in Supply lineage) or a fact a reviewer cannot check.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::canonical::{CanonicalPath, Digest, PathError};

/// The manifest schema this build reads and writes.
pub const MANIFEST_SCHEMA: &str = "louiselm.skills.manifest/1";

/// One regular file inside a package.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestEntry {
    /// Canonical package-relative path.
    pub path: String,
    /// Whether the file is executable; every other mode bit is normalized away.
    pub executable: bool,
    /// Content length in bytes.
    pub size: u64,
    /// Content address as bare lowercase hex.
    pub sha256: String,
}

/// The complete, sorted description of a package's bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Schema identifier; a manifest with an unknown schema is never trusted.
    pub schema: String,
    /// Entries sorted by path, with no duplicates and no collisions.
    pub entries: Vec<ManifestEntry>,
}

/// A manifest that cannot be trusted to describe a package.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ManifestError {
    /// The manifest declares a schema this build does not implement.
    #[error("unsupported manifest schema '{0}'")]
    UnsupportedSchema(String),
    /// A path in the manifest violates the canonical path contract.
    #[error(transparent)]
    Path(#[from] PathError),
    /// Two entries share a path.
    #[error("duplicate path '{0}'")]
    DuplicatePath(String),
    /// The entries are not sorted, so the bytes are not the canonical order.
    #[error("entry '{0}' is out of sorted order")]
    Unsorted(String),
    /// Two distinct paths collide once a filesystem normalizes them.
    #[error("paths '{first}' and '{second}' collide when normalized")]
    CollidingPaths {
        /// The path seen first in sorted order.
        first: String,
        /// The path that collides with it.
        second: String,
    },
    /// An entry carries a content address that is not a SHA-256 digest.
    #[error("entry '{path}' has an invalid digest '{digest}'")]
    InvalidDigest {
        /// The entry's path.
        path: String,
        /// The rejected digest text.
        digest: String,
    },
    /// The bytes are not the canonical serialization of the manifest they hold.
    ///
    /// Re-serializing a parsed manifest must reproduce the input exactly.
    /// Anything else — reordered keys, added whitespace, an unknown field, a
    /// trailing newline — is a different byte sequence claiming a digest it
    /// does not have.
    #[error("manifest bytes are not in canonical form")]
    NonCanonical,
    /// The bytes are not valid manifest JSON at all.
    #[error("manifest is not valid JSON: {0}")]
    Malformed(String),
}

impl Manifest {
    /// Builds a manifest from unsorted entries, enforcing the path contract.
    pub fn new(
        mut entries: Vec<ManifestEntry>,
        allow_non_ascii: bool,
    ) -> Result<Self, ManifestError> {
        entries.sort_by(|left, right| left.path.cmp(&right.path));
        let manifest = Self {
            schema: MANIFEST_SCHEMA.to_owned(),
            entries,
        };
        manifest.validate(allow_non_ascii)?;
        Ok(manifest)
    }

    /// Serializes the manifest to the bytes its digest covers.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("manifest is always serializable")
    }

    /// Returns the package digest: the content address of the canonical bytes.
    pub fn digest(&self) -> Digest {
        Digest::of(&self.canonical_bytes())
    }

    /// Parses manifest bytes, rejecting anything not in canonical form.
    pub fn parse(bytes: &[u8], allow_non_ascii: bool) -> Result<Self, ManifestError> {
        let manifest: Self = serde_json::from_slice(bytes)
            .map_err(|error| ManifestError::Malformed(error.to_string()))?;
        if manifest.canonical_bytes() != bytes {
            return Err(ManifestError::NonCanonical);
        }
        manifest.validate(allow_non_ascii)?;
        Ok(manifest)
    }

    /// Finds an entry by exact path.
    pub fn entry(&self, path: &str) -> Option<&ManifestEntry> {
        self.entries.iter().find(|entry| entry.path == path)
    }

    /// Returns the total size of the package's content in bytes.
    pub fn total_size(&self) -> u64 {
        self.entries.iter().map(|entry| entry.size).sum()
    }

    fn validate(&self, allow_non_ascii: bool) -> Result<(), ManifestError> {
        if self.schema != MANIFEST_SCHEMA {
            return Err(ManifestError::UnsupportedSchema(self.schema.clone()));
        }
        let mut previous: Option<&str> = None;
        let mut keys: Vec<(String, String)> = Vec::with_capacity(self.entries.len());
        for entry in &self.entries {
            let path = CanonicalPath::parse(&entry.path, allow_non_ascii)?;
            Digest::parse(&entry.sha256).map_err(|_| ManifestError::InvalidDigest {
                path: entry.path.clone(),
                digest: entry.sha256.clone(),
            })?;
            if let Some(earlier) = previous {
                if earlier == entry.path {
                    return Err(ManifestError::DuplicatePath(entry.path.clone()));
                }
                if earlier > entry.path.as_str() {
                    return Err(ManifestError::Unsorted(entry.path.clone()));
                }
            }
            keys.push((path.collision_key(), entry.path.clone()));
            previous = Some(&entry.path);
        }
        keys.sort();
        for pair in keys.windows(2) {
            if pair[0].0 == pair[1].0 {
                return Err(ManifestError::CollidingPaths {
                    first: pair[0].1.clone(),
                    second: pair[1].1.clone(),
                });
            }
        }
        Ok(())
    }
}
