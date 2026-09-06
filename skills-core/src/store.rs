//! The immutable content-addressed package store.
//!
//! The store's one job is that a digest names exactly one sequence of bytes,
//! forever. Publishing is atomic — a package appears complete or not at all —
//! and a digest already present is never overwritten: identical bytes are a
//! no-op, different bytes are an error the caller cannot suppress.
//!
//! Nothing here trusts what is written next to the bytes. [`Store::verify`]
//! recomputes every content hash from the files on disk and compares them to
//! the manifest, and every read path that matters routes through it, so a
//! reviewer approving a Dossier is approving bytes that were re-read, not a
//! digest that was recorded.

use std::{
    fs,
    io::{self},
    path::{Path, PathBuf},
    process,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    assessment::Assessment,
    canonical::{Digest, DigestError, Hasher},
    capture::{self, CaptureError, StagedPackage},
    lineage::{CaptureRecord, LineageLink, SupplyLineage},
    manifest::{Manifest, ManifestError},
    policy::Policy,
};

static STAGING_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A store operation that failed.
#[derive(Debug, Error)]
pub enum StoreError {
    /// A filesystem operation failed.
    #[error("store I/O failed at '{path}': {source}")]
    Io {
        /// Path being operated on.
        path: String,
        /// Underlying failure.
        source: io::Error,
    },
    /// The candidate could not be captured.
    #[error(transparent)]
    Capture(#[from] CaptureError),
    /// A manifest in the store is not admissible.
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    /// A digest given by the caller is malformed.
    #[error(transparent)]
    Digest(#[from] DigestError),
    /// The requested package is not in the store.
    #[error("package {0} is not in the store")]
    UnknownPackage(String),
    /// The digest is already present with different bytes.
    ///
    /// Reaching this means either a SHA-256 collision or a store someone
    /// edited by hand. Both are refusals, never merges.
    #[error("package {digest} already exists with different bytes")]
    DigestConflict {
        /// The contested digest.
        digest: String,
    },
    /// The stored bytes no longer match the manifest that names them.
    #[error("package {digest} failed verification: {summary}")]
    Tampered {
        /// The package that failed.
        digest: String,
        /// What did not match.
        summary: String,
    },
    /// Stored local metadata is unreadable.
    #[error("cannot read {kind} for {digest}: {reason}")]
    Metadata {
        /// Which record failed.
        kind: String,
        /// The package it belongs to.
        digest: String,
        /// Why it failed.
        reason: String,
    },
}

/// Whether publishing created a package or found it already present.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublishOutcome {
    /// The package did not exist and was written.
    Created,
    /// The identical package was already present; nothing was written.
    Existing,
}

/// A published package, opened from the store.
#[derive(Clone, Debug)]
pub struct Package {
    /// Package digest.
    pub digest: Digest,
    /// Canonical manifest, re-parsed from the stored bytes.
    pub manifest: Manifest,
    /// Directory holding `manifest.json` and `files/`.
    pub root: PathBuf,
}

impl Package {
    /// Returns the absolute path of one packaged file.
    #[must_use]
    pub fn file_path(&self, package_relative: &str) -> PathBuf {
        self.root.join("files").join(package_relative)
    }

    /// Reads one packaged file's bytes.
    ///
    /// # Errors
    /// Returns file-open/read errors for the requested packaged path.
    pub fn read(&self, package_relative: &str) -> Result<Vec<u8>, StoreError> {
        let path = self.file_path(package_relative);
        fs::read(&path).map_err(|source| StoreError::Io {
            path: path.display().to_string(),
            source,
        })
    }
}

/// One way stored bytes disagreed with the manifest naming them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VerifyFailure {
    /// A manifest entry has no file on disk.
    Missing {
        /// Package-relative path.
        path: String,
    },
    /// A file on disk is absent from the manifest.
    Unexpected {
        /// Package-relative path.
        path: String,
    },
    /// A file's content hash differs from the manifest.
    ContentMismatch {
        /// Package-relative path.
        path: String,
        /// Digest the manifest records.
        expected: String,
        /// Digest the bytes actually have.
        found: String,
    },
    /// A file's executable bit differs from the manifest.
    ModeMismatch {
        /// Package-relative path.
        path: String,
        /// Executable bit the manifest records.
        expected: bool,
        /// Executable bit the file actually has.
        found: bool,
    },
}

impl VerifyFailure {
    /// Renders the failure as one line of reviewer-facing text.
    #[must_use]
    pub fn summary(&self) -> String {
        match self {
            Self::Missing { path } => format!("'{path}' is missing"),
            Self::Unexpected { path } => format!("'{path}' is not in the manifest"),
            Self::ContentMismatch {
                path,
                expected,
                found,
            } => format!("'{path}' hashes to {found}, manifest says {expected}"),
            Self::ModeMismatch {
                path,
                expected,
                found,
            } => format!("'{path}' executable bit is {found}, manifest says {expected}"),
        }
    }
}

/// The result of recomputing a package from its stored bytes.
#[derive(Clone, Debug)]
pub struct VerifyReport {
    /// Digest the package is stored under.
    pub digest: Digest,
    /// Digest recomputed from the stored manifest bytes.
    pub recomputed_digest: Digest,
    /// Every disagreement found; empty means the package verified.
    pub failures: Vec<VerifyFailure>,
}

impl VerifyReport {
    /// Reports whether the stored bytes are exactly what the digest names.
    #[must_use]
    pub fn is_intact(&self) -> bool {
        self.failures.is_empty() && self.digest == self.recomputed_digest
    }

    /// Renders every failure as one reviewer-facing line.
    pub fn summary(&self) -> String {
        if self.digest != self.recomputed_digest {
            return format!(
                "manifest bytes hash to {}, stored as {}",
                self.recomputed_digest, self.digest
            );
        }
        self.failures
            .iter()
            .map(VerifyFailure::summary)
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// The schema of a store's provenance record.
pub const PROVENANCE_SCHEMA: &str = "louiselm.skills.store-provenance/1";

/// Whether a store was created by a trusted release.
///
/// Recorded once, when the store is created, and never recomputed. A store
/// created by a development build is a development store forever: letting it
/// be promoted later would mean an Agent that could write the store could also
/// decide the store was trustworthy.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    /// Schema identifier.
    pub schema: String,
    /// Whether a verified release created this store.
    pub trusted: bool,
    /// The release that created it, when one did.
    pub created_by_release: Option<String>,
}

/// The immutable package store rooted at one directory.
#[derive(Clone, Debug)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    /// Opens, creating the store layout when it does not exist yet.
    ///
    /// # Errors
    /// Returns directory-creation or provenance-write errors.
    pub fn open(root: &Path) -> Result<Self, StoreError> {
        for directory in ["packages", "lineage", "staging", "assessments"] {
            let path = root.join(directory);
            fs::create_dir_all(&path).map_err(|source| StoreError::Io {
                path: path.display().to_string(),
                source,
            })?;
        }
        let store = Self {
            root: root.to_path_buf(),
        };
        store.record_provenance()?;
        Ok(store)
    }

    /// Returns the store's provenance, recording it if this is a new store.
    ///
    /// # Errors
    /// Returns provenance read/JSON errors or persistence errors while recording a new store.
    pub fn provenance(&self) -> Result<Provenance, StoreError> {
        let path = self.root.join("provenance.json");
        match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| StoreError::Metadata {
                kind: "provenance".to_owned(),
                digest: self.root.display().to_string(),
                reason: error.to_string(),
            }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                self.record_provenance()?;
                self.provenance()
            }
            Err(source) => Err(StoreError::Io {
                path: path.display().to_string(),
                source,
            }),
        }
    }

    /// Reports whether this store may hold supply a verified Session uses.
    #[must_use]
    pub fn is_trusted(&self) -> bool {
        self.provenance().is_ok_and(|provenance| provenance.trusted)
    }

    fn record_provenance(&self) -> Result<(), StoreError> {
        let path = self.root.join("provenance.json");
        if path.exists() {
            return Ok(());
        }
        let identity = crate::release::running_identity();
        let provenance = Provenance {
            schema: PROVENANCE_SCHEMA.to_owned(),
            trusted: identity.verified,
            created_by_release: identity.release_id,
        };
        let bytes = serde_json::to_vec(&provenance).map_err(|error| StoreError::Metadata {
            kind: "provenance".to_owned(),
            digest: self.root.display().to_string(),
            reason: error.to_string(),
        })?;
        fs::write(&path, bytes).map_err(|source| StoreError::Io {
            path: path.display().to_string(),
            source,
        })
    }

    /// Returns the store root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Captures `source` and publishes it, recording Supply lineage.
    ///
    /// `captured_at_ms` is supplied by the caller rather than read from the
    /// clock so that packaging is reproducible under test and the timestamp
    /// stays out of the package bytes.
    ///
    /// # Errors
    /// Returns staging/capture validation or I/O errors, or any publication failure from [`Self::publish`].
    pub fn capture(
        &self,
        source: &Path,
        policy: &Policy,
        captured_at_ms: u64,
    ) -> Result<(Package, PublishOutcome), StoreError> {
        let staging = self.staging_directory()?;
        let staged = match capture::stage(source, &staging, policy) {
            Ok(staged) => staged,
            Err(error) => {
                capture::discard_staging(&staging);
                return Err(error.into());
            }
        };
        let result = self.publish(staged, captured_at_ms, policy);
        capture::discard_staging(&staging);
        result
    }

    /// Publishes a staged package and appends its lineage record.
    ///
    /// # Errors
    /// Refuses a digest collision with different stored bytes; propagates verification, copy/rename, lineage-write, and manifest-loading errors.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "Publishing consumes the staged handle because its directory is moved or removed and must not be reused."
    )]
    pub fn publish(
        &self,
        staged: StagedPackage,
        captured_at_ms: u64,
        policy: &Policy,
    ) -> Result<(Package, PublishOutcome), StoreError> {
        let destination = self.package_path(&staged.digest);
        let outcome = if destination.exists() {
            let existing = self.open_package(&staged.digest, policy)?;
            if existing.manifest.canonical_bytes() != staged.manifest.canonical_bytes() {
                return Err(StoreError::DigestConflict {
                    digest: staged.digest.to_string(),
                });
            }
            let report = self.verify(&staged.digest, policy)?;
            if !report.is_intact() {
                return Err(StoreError::Tampered {
                    digest: staged.digest.to_string(),
                    summary: report.summary(),
                });
            }
            PublishOutcome::Existing
        } else {
            fs::rename(&staged.root, &destination).map_err(|source| StoreError::Io {
                path: destination.display().to_string(),
                source,
            })?;
            PublishOutcome::Created
        };

        let mut lineage = self
            .lineage(&staged.digest)?
            .unwrap_or_else(|| SupplyLineage::new(&staged.digest.to_string()));
        lineage.record(CaptureRecord {
            captured_at_ms,
            source_root: staged.source_root.display().to_string(),
            links: staged.link_origins.iter().map(LineageLink::from).collect(),
        });
        self.write_lineage(&staged.digest, &lineage)?;

        let package = self.open_package(&staged.digest, policy)?;
        Ok((package, outcome))
    }

    /// Opens a published package, re-parsing its manifest from stored bytes.
    ///
    /// # Errors
    /// Returns `UnknownPackage`, manifest-read errors, or canonical manifest validation errors.
    pub fn open_package(&self, digest: &Digest, policy: &Policy) -> Result<Package, StoreError> {
        let root = self.package_path(digest);
        let manifest_path = root.join("manifest.json");
        let bytes = fs::read(&manifest_path).map_err(|source| match source.kind() {
            io::ErrorKind::NotFound => StoreError::UnknownPackage(digest.to_string()),
            _ => StoreError::Io {
                path: manifest_path.display().to_string(),
                source,
            },
        })?;
        let manifest = Manifest::parse(&bytes, policy.allows_non_ascii_paths())?;
        Ok(Package {
            digest: digest.clone(),
            manifest,
            root,
        })
    }

    /// Recomputes a package from its stored bytes and reports every mismatch.
    ///
    /// # Errors
    /// Returns package-open, content-read, or directory-listing errors. Missing files and content/mode mismatches are findings in the returned report.
    pub fn verify(&self, digest: &Digest, policy: &Policy) -> Result<VerifyReport, StoreError> {
        let package = self.open_package(digest, policy)?;
        let recomputed_digest = Digest::of(&package.manifest.canonical_bytes());
        let mut failures = Vec::new();

        for entry in &package.manifest.entries {
            let path = package.file_path(&entry.path);
            let Ok(metadata) = fs::metadata(&path) else {
                failures.push(VerifyFailure::Missing {
                    path: entry.path.clone(),
                });
                continue;
            };
            let (found, size) = hash_file(&path)?;
            if found.hex() != entry.sha256 || size != entry.size {
                failures.push(VerifyFailure::ContentMismatch {
                    path: entry.path.clone(),
                    expected: format!("sha256:{}", entry.sha256),
                    found: found.to_string(),
                });
            }
            let executable = {
                use std::os::unix::fs::PermissionsExt;
                metadata.permissions().mode() & 0o111 != 0
            };
            if executable != entry.executable {
                failures.push(VerifyFailure::ModeMismatch {
                    path: entry.path.clone(),
                    expected: entry.executable,
                    found: executable,
                });
            }
        }

        for path in list_files(&package.root.join("files"))? {
            if package.manifest.entry(&path).is_none() {
                failures.push(VerifyFailure::Unexpected { path });
            }
        }

        Ok(VerifyReport {
            digest: digest.clone(),
            recomputed_digest,
            failures,
        })
    }

    /// Lists every published package digest, in store order.
    ///
    /// # Errors
    /// Returns package-directory read errors; non-digest filenames are ignored.
    pub fn list(&self) -> Result<Vec<Digest>, StoreError> {
        let packages = self.root.join("packages");
        let mut digests = Vec::new();
        let listing = fs::read_dir(&packages).map_err(|source| StoreError::Io {
            path: packages.display().to_string(),
            source,
        })?;
        for entry in listing {
            let entry = entry.map_err(|source| StoreError::Io {
                path: packages.display().to_string(),
                source,
            })?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Ok(digest) = Digest::parse(&name) {
                digests.push(digest);
            }
        }
        digests.sort();
        Ok(digests)
    }

    /// Reads the Supply lineage recorded for a package, when there is any.
    ///
    /// # Errors
    /// Returns lineage read/JSON errors; no lineage record is `Ok(None)`.
    pub fn lineage(&self, digest: &Digest) -> Result<Option<SupplyLineage>, StoreError> {
        let path = self.lineage_path(digest);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(StoreError::Io {
                    path: path.display().to_string(),
                    source,
                });
            }
        };
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| StoreError::Metadata {
                kind: "lineage".to_owned(),
                digest: digest.to_string(),
                reason: error.to_string(),
            })
    }

    /// Records an advisory Assessment for a package, replacing any earlier one.
    ///
    /// Assessments are local, advisory, and replaceable; unlike package bytes
    /// they carry no authority, so overwriting one is not a trust event.
    ///
    /// # Errors
    /// Rejects an invalid package digest; returns Assessment serialization or file-write errors.
    pub fn record_assessment(&self, assessment: &Assessment) -> Result<(), StoreError> {
        let digest = Digest::parse(&assessment.key.package_digest)?;
        let path = self.metadata_path("assessments", &digest);
        let bytes = serde_json::to_vec(assessment).map_err(|error| StoreError::Metadata {
            kind: "assessment".to_owned(),
            digest: digest.to_string(),
            reason: error.to_string(),
        })?;
        fs::write(&path, bytes).map_err(|source| StoreError::Io {
            path: path.display().to_string(),
            source,
        })
    }

    /// Reads the Assessment recorded for a package, when there is one.
    ///
    /// The caller still has to check it describes the Model and prompt in use;
    /// see [`Assessment::current_for`].
    ///
    /// # Errors
    /// Returns Assessment read/JSON errors; a missing record is `Ok(None)`.
    pub fn assessment(&self, digest: &Digest) -> Result<Option<Assessment>, StoreError> {
        let path = self.metadata_path("assessments", digest);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(StoreError::Io {
                    path: path.display().to_string(),
                    source,
                });
            }
        };
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| StoreError::Metadata {
                kind: "assessment".to_owned(),
                digest: digest.to_string(),
                reason: error.to_string(),
            })
    }

    /// Returns the path local records for `digest` are stored under.
    pub(crate) fn metadata_path(&self, kind: &str, digest: &Digest) -> PathBuf {
        self.root
            .join(kind)
            .join(format!("{}.json", digest.directory_name()))
    }

    fn write_lineage(&self, digest: &Digest, lineage: &SupplyLineage) -> Result<(), StoreError> {
        let path = self.lineage_path(digest);
        let bytes = serde_json::to_vec(lineage).map_err(|error| StoreError::Metadata {
            kind: "lineage".to_owned(),
            digest: digest.to_string(),
            reason: error.to_string(),
        })?;
        fs::write(&path, bytes).map_err(|source| StoreError::Io {
            path: path.display().to_string(),
            source,
        })
    }

    fn lineage_path(&self, digest: &Digest) -> PathBuf {
        self.metadata_path("lineage", digest)
    }

    fn package_path(&self, digest: &Digest) -> PathBuf {
        self.root.join("packages").join(digest.directory_name())
    }

    fn staging_directory(&self) -> Result<PathBuf, StoreError> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let counter = STAGING_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path =
            self.root
                .join("staging")
                .join(format!("{}-{}-{}", process::id(), nanos, counter));
        fs::create_dir_all(&path).map_err(|source| StoreError::Io {
            path: path.display().to_string(),
            source,
        })?;
        Ok(path)
    }
}

fn hash_file(path: &Path) -> Result<(Digest, u64), StoreError> {
    use std::io::Read;

    let mut file = fs::File::open(path).map_err(|source| StoreError::Io {
        path: path.display().to_string(),
        source,
    })?;
    let mut hasher = Hasher::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    let mut size = 0_u64;
    loop {
        let read = file.read(&mut buffer).map_err(|source| StoreError::Io {
            path: path.display().to_string(),
            source,
        })?;
        if read == 0 {
            break;
        }
        size += read as u64;
        hasher.update(&buffer[..read]);
    }
    Ok((hasher.finish(), size))
}

fn list_files(root: &Path) -> Result<Vec<String>, StoreError> {
    let mut found = Vec::new();
    collect_files(root, "", &mut found)?;
    found.sort();
    Ok(found)
}

fn collect_files(
    directory: &Path,
    prefix: &str,
    found: &mut Vec<String>,
) -> Result<(), StoreError> {
    let listing = match fs::read_dir(directory) {
        Ok(listing) => listing,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(StoreError::Io {
                path: directory.display().to_string(),
                source,
            });
        }
    };
    for entry in listing {
        let entry = entry.map_err(|source| StoreError::Io {
            path: directory.display().to_string(),
            source,
        })?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let relative = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let file_type = entry.file_type().map_err(|source| StoreError::Io {
            path: relative.clone(),
            source,
        })?;
        if file_type.is_dir() {
            collect_files(&entry.path(), &relative, found)?;
        } else {
            found.push(relative);
        }
    }
    Ok(())
}
