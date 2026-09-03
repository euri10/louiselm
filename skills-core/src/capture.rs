//! Capturing an untrusted Skill candidate into a staged immutable package.
//!
//! Capture is the only place a mutable tree becomes package bytes, so it is
//! written to fail closed. Anything it cannot describe unambiguously in a
//! canonical manifest — a device node, a hardlinked file, a directory cycle, a
//! file that changed while being read, a path that collides with another once
//! normalized — aborts the whole capture before any package is published.
//!
//! Symlinks are resolved and their content copied in as ordinary immutable
//! files, so a later edit to the link or its target cannot change bytes that
//! were already reviewed. Where the link pointed is a local fact with no
//! portable meaning, so it is recorded in Supply lineage and surfaced in the
//! Dossier, never in the manifest.

use std::{
    fs::{self, File},
    io::{self, Read, Write},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};

use thiserror::Error;

use crate::{
    canonical::{CanonicalPath, Digest, Hasher, PathError},
    manifest::{Manifest, ManifestEntry, ManifestError},
    policy::Policy,
};

const COPY_BUFFER_BYTES: usize = 64 * 1024;

/// A capture that was refused; no package is published when one is returned.
#[derive(Debug, Error)]
pub enum CaptureError {
    /// The candidate root is missing, unreadable, or not a directory.
    #[error("candidate root '{path}' is not a readable directory: {reason}")]
    Root {
        /// Root the caller named.
        path: String,
        /// Why it was refused.
        reason: String,
    },
    /// Reading the candidate failed.
    #[error("cannot read '{path}': {source}")]
    Read {
        /// Path being read.
        path: String,
        /// Underlying I/O failure.
        source: io::Error,
    },
    /// Writing the staged package failed.
    #[error("cannot stage '{path}': {source}")]
    Stage {
        /// Path being written.
        path: String,
        /// Underlying I/O failure.
        source: io::Error,
    },
    /// A path in the candidate violates the canonical path contract.
    #[error("{source} (at '{path}')")]
    Path {
        /// Candidate-relative location.
        path: String,
        /// The contract violation.
        source: PathError,
    },
    /// The entry is neither a regular file, a directory, nor a symlink.
    #[error("'{path}' is a {kind}, which cannot be packaged")]
    SpecialFile {
        /// Candidate-relative location.
        path: String,
        /// What the entry actually is.
        kind: String,
    },
    /// The file has more than one name, so its bytes have a second writer.
    #[error("'{path}' is hardlinked ({links} names), so its content is ambiguous")]
    HardlinkAmbiguity {
        /// Candidate-relative location.
        path: String,
        /// Observed link count.
        links: u64,
    },
    /// Following symlinks re-entered a directory already on the descent path.
    #[error("'{path}' completes a directory cycle through '{target}'")]
    Cycle {
        /// Candidate-relative location.
        path: String,
        /// Resolved target already being visited.
        target: String,
    },
    /// The file changed between the first and last observation of it.
    #[error("'{path}' changed while being captured")]
    MutationDuringCapture {
        /// Candidate-relative location.
        path: String,
    },
    /// A policy limit was exceeded.
    #[error("policy limit exceeded: {0}")]
    LimitExceeded(String),
    /// The manifest the capture would produce is not admissible.
    #[error(transparent)]
    Manifest(#[from] ManifestError),
}

/// Where one packaged file's bytes actually came from on this machine.
///
/// Purely local: two identical packages captured on different machines share a
/// digest and disagree on every field here, which is why this never enters the
/// manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkOrigin {
    /// Package-relative path the content was captured as.
    pub path: String,
    /// Symlink target as declared, before resolution.
    pub declared_target: String,
    /// Fully resolved target on this machine.
    pub resolved_target: String,
    /// Whether the resolved target lies outside the candidate root.
    pub escapes_root: bool,
}

/// A package written to a staging directory and not yet published.
#[derive(Debug)]
pub struct StagedPackage {
    /// Canonical manifest describing the staged bytes.
    pub manifest: Manifest,
    /// Package digest: the content address of the manifest's canonical bytes.
    pub digest: Digest,
    /// Directory holding `files/` and `manifest.json`.
    pub root: PathBuf,
    /// Local link origins, for Supply lineage.
    pub link_origins: Vec<LinkOrigin>,
    /// Absolute candidate root the capture read.
    pub source_root: PathBuf,
}

struct Capture<'a> {
    policy: &'a Policy,
    probe: &'a dyn Fn(&Path),
    source_root: PathBuf,
    files_root: PathBuf,
    entries: Vec<ManifestEntry>,
    link_origins: Vec<LinkOrigin>,
    total_bytes: u64,
}

/// Captures `source` into `staging`, which must be an empty directory.
///
/// On success `staging` holds `files/` and `manifest.json`, and every captured
/// file is read-only. On failure the caller is expected to discard `staging`
/// entirely: a partially captured tree is never a package.
pub fn stage(
    source: &Path,
    staging: &Path,
    policy: &Policy,
) -> Result<StagedPackage, CaptureError> {
    stage_with_probe(source, staging, policy, &|_| {})
}

/// Captures `source`, calling `probe` after each file is copied.
///
/// The probe exists so the mutation-during-capture and source-removal paths
/// can be exercised deterministically instead of by racing a writer thread: a
/// check whose failure path was never taken is indistinguishable from no
/// check. Production capture passes a no-op, and the seam stays crate-private.
pub(crate) fn stage_with_probe(
    source: &Path,
    staging: &Path,
    policy: &Policy,
    probe: &dyn Fn(&Path),
) -> Result<StagedPackage, CaptureError> {
    let source_root = fs::canonicalize(source).map_err(|error| CaptureError::Root {
        path: source.display().to_string(),
        reason: error.to_string(),
    })?;
    let metadata = fs::metadata(&source_root).map_err(|error| CaptureError::Root {
        path: source.display().to_string(),
        reason: error.to_string(),
    })?;
    if !metadata.is_dir() {
        return Err(CaptureError::Root {
            path: source.display().to_string(),
            reason: "not a directory".to_owned(),
        });
    }

    let files_root = staging.join("files");
    fs::create_dir_all(&files_root).map_err(|error| CaptureError::Stage {
        path: files_root.display().to_string(),
        source: error,
    })?;

    let mut capture = Capture {
        policy,
        source_root: source_root.clone(),
        files_root,
        entries: Vec::new(),
        link_origins: Vec::new(),
        total_bytes: 0,
        probe,
    };
    let mut visiting = vec![source_root.clone()];
    capture.walk(&source_root, "", 0, &mut visiting)?;

    let manifest = Manifest::new(
        std::mem::take(&mut capture.entries),
        policy.allows_non_ascii_paths(),
    )?;
    let digest = manifest.digest();
    let manifest_path = staging.join("manifest.json");
    write_immutable(&manifest_path, &manifest.canonical_bytes(), false)?;

    Ok(StagedPackage {
        manifest,
        digest,
        root: staging.to_path_buf(),
        link_origins: std::mem::take(&mut capture.link_origins),
        source_root,
    })
}

impl Capture<'_> {
    fn walk(
        &mut self,
        directory: &Path,
        prefix: &str,
        depth: usize,
        visiting: &mut Vec<PathBuf>,
    ) -> Result<(), CaptureError> {
        if depth > self.policy.limits().max_depth {
            return Err(CaptureError::LimitExceeded(format!(
                "directory depth {depth} exceeds {}",
                self.policy.limits().max_depth
            )));
        }
        let mut names = Vec::new();
        let listing = fs::read_dir(directory).map_err(|error| CaptureError::Read {
            path: directory.display().to_string(),
            source: error,
        })?;
        for entry in listing {
            let entry = entry.map_err(|error| CaptureError::Read {
                path: directory.display().to_string(),
                source: error,
            })?;
            names.push(entry.file_name());
        }
        names.sort();

        for name in names {
            let name = name.to_string_lossy().into_owned();
            let relative = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let absolute = directory.join(&name);
            self.visit(&absolute, &relative, depth, visiting)?;
        }
        Ok(())
    }

    fn visit(
        &mut self,
        absolute: &Path,
        relative: &str,
        depth: usize,
        visiting: &mut Vec<PathBuf>,
    ) -> Result<(), CaptureError> {
        let link_metadata = fs::symlink_metadata(absolute).map_err(|error| CaptureError::Read {
            path: relative.to_owned(),
            source: error,
        })?;

        if link_metadata.file_type().is_symlink() {
            return self.visit_symlink(absolute, relative, depth, visiting);
        }
        self.visit_resolved(absolute, relative, depth, visiting, None)
    }

    fn visit_symlink(
        &mut self,
        absolute: &Path,
        relative: &str,
        depth: usize,
        visiting: &mut Vec<PathBuf>,
    ) -> Result<(), CaptureError> {
        let declared = fs::read_link(absolute).map_err(|error| CaptureError::Read {
            path: relative.to_owned(),
            source: error,
        })?;
        let resolved = fs::canonicalize(absolute).map_err(|error| CaptureError::Read {
            path: relative.to_owned(),
            source: error,
        })?;
        let origin = LinkOrigin {
            path: relative.to_owned(),
            declared_target: declared.display().to_string(),
            resolved_target: resolved.display().to_string(),
            escapes_root: !resolved.starts_with(&self.source_root),
        };
        self.link_origins.push(origin);
        self.visit_resolved(&resolved, relative, depth, visiting, Some(&resolved))
    }

    fn visit_resolved(
        &mut self,
        absolute: &Path,
        relative: &str,
        depth: usize,
        visiting: &mut Vec<PathBuf>,
        resolved_link: Option<&Path>,
    ) -> Result<(), CaptureError> {
        let metadata = fs::metadata(absolute).map_err(|error| CaptureError::Read {
            path: relative.to_owned(),
            source: error,
        })?;
        let file_type = metadata.file_type();

        if file_type.is_dir() {
            let canonical = match resolved_link {
                Some(path) => path.to_path_buf(),
                None => fs::canonicalize(absolute).map_err(|error| CaptureError::Read {
                    path: relative.to_owned(),
                    source: error,
                })?,
            };
            if visiting.contains(&canonical) {
                return Err(CaptureError::Cycle {
                    path: relative.to_owned(),
                    target: canonical.display().to_string(),
                });
            }
            visiting.push(canonical);
            let result = self.walk(absolute, relative, depth + 1, visiting);
            visiting.pop();
            return result;
        }

        if !file_type.is_file() {
            return Err(CaptureError::SpecialFile {
                path: relative.to_owned(),
                kind: describe_file_type(&metadata),
            });
        }
        if metadata.nlink() > 1 {
            return Err(CaptureError::HardlinkAmbiguity {
                path: relative.to_owned(),
                links: metadata.nlink(),
            });
        }
        self.capture_file(absolute, relative, &metadata)
    }

    fn capture_file(
        &mut self,
        absolute: &Path,
        relative: &str,
        before: &fs::Metadata,
    ) -> Result<(), CaptureError> {
        let limits = self.policy.limits();
        let path = CanonicalPath::parse(relative, self.policy.allows_non_ascii_paths()).map_err(
            |source| CaptureError::Path {
                path: relative.to_owned(),
                source,
            },
        )?;
        if before.len() > limits.max_file_bytes {
            return Err(CaptureError::LimitExceeded(format!(
                "'{relative}' is {} bytes, over the {} byte file limit",
                before.len(),
                limits.max_file_bytes
            )));
        }
        if self.entries.len() + 1 > limits.max_entries {
            return Err(CaptureError::LimitExceeded(format!(
                "package holds more than {} files",
                limits.max_entries
            )));
        }

        let executable = before.permissions().mode() & 0o111 != 0;
        let destination = self.files_root.join(path.as_str());
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(|error| CaptureError::Stage {
                path: parent.display().to_string(),
                source: error,
            })?;
        }

        let (digest, size) =
            copy_and_hash(absolute, &destination, relative, limits.max_file_bytes)?;
        (self.probe)(absolute);

        let after = fs::metadata(absolute).map_err(|error| CaptureError::Read {
            path: relative.to_owned(),
            source: error,
        })?;
        if !unchanged(before, &after) || size != after.len() {
            return Err(CaptureError::MutationDuringCapture {
                path: relative.to_owned(),
            });
        }

        self.total_bytes = self.total_bytes.saturating_add(size);
        if self.total_bytes > limits.max_total_bytes {
            return Err(CaptureError::LimitExceeded(format!(
                "package exceeds the {} byte total limit",
                limits.max_total_bytes
            )));
        }

        set_read_only(&destination, executable)?;
        self.entries.push(ManifestEntry {
            path: path.as_str().to_owned(),
            executable,
            size,
            sha256: digest.hex().to_owned(),
        });
        Ok(())
    }
}

fn copy_and_hash(
    source: &Path,
    destination: &Path,
    relative: &str,
    max_bytes: u64,
) -> Result<(Digest, u64), CaptureError> {
    let mut reader = File::open(source).map_err(|error| CaptureError::Read {
        path: relative.to_owned(),
        source: error,
    })?;
    let mut writer = File::create(destination).map_err(|error| CaptureError::Stage {
        path: destination.display().to_string(),
        source: error,
    })?;
    let mut hasher = Hasher::new();
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
    let mut size = 0_u64;
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| CaptureError::Read {
                path: relative.to_owned(),
                source: error,
            })?;
        if read == 0 {
            break;
        }
        size += read as u64;
        if size > max_bytes {
            return Err(CaptureError::LimitExceeded(format!(
                "'{relative}' grew past the {max_bytes} byte file limit while being read"
            )));
        }
        hasher.update(&buffer[..read]);
        writer
            .write_all(&buffer[..read])
            .map_err(|error| CaptureError::Stage {
                path: destination.display().to_string(),
                source: error,
            })?;
    }
    writer.flush().map_err(|error| CaptureError::Stage {
        path: destination.display().to_string(),
        source: error,
    })?;
    Ok((hasher.finish(), size))
}

/// Writes `bytes` and drops write permission, so the store owns them next.
pub(crate) fn write_immutable(
    path: &Path,
    bytes: &[u8],
    executable: bool,
) -> Result<(), CaptureError> {
    fs::write(path, bytes).map_err(|error| CaptureError::Stage {
        path: path.display().to_string(),
        source: error,
    })?;
    set_read_only(path, executable)
}

fn set_read_only(path: &Path, executable: bool) -> Result<(), CaptureError> {
    let mode = if executable { 0o555 } else { 0o444 };
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(|error| {
        CaptureError::Stage {
            path: path.display().to_string(),
            source: error,
        }
    })
}

fn unchanged(before: &fs::Metadata, after: &fs::Metadata) -> bool {
    before.ino() == after.ino()
        && before.dev() == after.dev()
        && before.len() == after.len()
        && before.mtime() == after.mtime()
        && before.mtime_nsec() == after.mtime_nsec()
        && before.ctime() == after.ctime()
        && before.ctime_nsec() == after.ctime_nsec()
}

fn describe_file_type(metadata: &fs::Metadata) -> String {
    let file_type = metadata.file_type();
    let kind = {
        use std::os::unix::fs::FileTypeExt;
        if file_type.is_socket() {
            "socket"
        } else if file_type.is_fifo() {
            "fifo"
        } else if file_type.is_block_device() {
            "block device"
        } else if file_type.is_char_device() {
            "character device"
        } else {
            "special file"
        }
    };
    kind.to_owned()
}

/// Removes a staging directory whose files were made read-only.
pub(crate) fn discard_staging(staging: &Path) {
    let _ = restore_writability(staging);
    let _ = fs::remove_dir_all(staging);
}

fn restore_writability(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_dir() {
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
        for entry in fs::read_dir(path)? {
            restore_writability(&entry?.path())?;
        }
    } else {
        fs::set_permissions(path, fs::Permissions::from_mode(0o644))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{thread, time::Duration};

    use super::*;

    struct Scratch {
        root: PathBuf,
    }

    impl Scratch {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "louiselm-skills-capture-{}-{}",
                process_id(),
                name
            ));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(root.join("candidate")).expect("candidate is creatable");
            fs::create_dir_all(root.join("staging")).expect("staging is creatable");
            Self { root }
        }

        fn candidate(&self) -> PathBuf {
            self.root.join("candidate")
        }

        fn staging(&self) -> PathBuf {
            self.root.join("staging")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            discard_staging(&self.root);
        }
    }

    fn process_id() -> u32 {
        std::process::id()
    }

    #[test]
    fn a_file_rewritten_while_it_is_read_aborts_the_capture() {
        let scratch = Scratch::new("mutated");
        let target = scratch.candidate().join("SKILL.md");
        fs::write(&target, "original\n").expect("file is writable");

        let error = stage_with_probe(
            &scratch.candidate(),
            &scratch.staging(),
            &Policy::embedded(),
            &|path| {
                // Filesystem timestamps have finite resolution; sleeping first
                // makes the rewrite observable rather than assuming it is.
                thread::sleep(Duration::from_millis(20));
                fs::write(path, "rewritten mid-capture\n").expect("file is rewritable");
            },
        )
        .expect_err("a file that changed while being read is refused");

        assert!(
            matches!(error, CaptureError::MutationDuringCapture { ref path } if path == "SKILL.md"),
            "unexpected error: {error}",
        );
    }

    #[test]
    fn a_file_removed_while_it_is_read_aborts_the_capture() {
        let scratch = Scratch::new("removed");
        let target = scratch.candidate().join("SKILL.md");
        fs::write(&target, "original\n").expect("file is writable");

        let error = stage_with_probe(
            &scratch.candidate(),
            &scratch.staging(),
            &Policy::embedded(),
            &|path| {
                fs::remove_file(path).expect("file is removable");
            },
        )
        .expect_err("a file that vanished while being read is refused");

        assert!(
            matches!(error, CaptureError::Read { ref path, .. } if path == "SKILL.md"),
            "unexpected error: {error}",
        );
    }

    #[test]
    fn an_undisturbed_capture_passes_the_same_check() {
        let scratch = Scratch::new("undisturbed");
        fs::write(scratch.candidate().join("SKILL.md"), "original\n").expect("file is writable");

        let staged = stage(
            &scratch.candidate(),
            &scratch.staging(),
            &Policy::embedded(),
        )
        .expect("an undisturbed capture succeeds");

        assert_eq!(staged.manifest.entries.len(), 1);
    }
}
