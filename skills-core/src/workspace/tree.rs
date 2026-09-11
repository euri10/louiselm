//! Bounded descriptor-relative workspace capture; Git metadata has no authority.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{File, Metadata},
    io::Read as _,
    os::{fd::AsRawFd as _, unix::fs::MetadataExt as _},
};

use rustix::fs::{Dir, Mode, OFlags};

use super::{
    MAX_FILE_BYTES, MAX_FILES, MAX_TOTAL_BYTES, SourceFile, SourceFiles, WorkspaceError,
    validate_path,
};

const MAX_ENTRIES: usize = 20_000;
const MAX_DEPTH: usize = 64;
const CHANGED: WorkspaceError =
    WorkspaceError::Invalid("workspace changed during export; freeze writers and retry");

#[derive(PartialEq, Eq)]
struct Stamp {
    device: u64,
    inode: u64,
    mode: u32,
    links: u64,
    length: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

impl From<&Metadata> for Stamp {
    fn from(meta: &Metadata) -> Self {
        Self {
            device: meta.dev(),
            inode: meta.ino(),
            mode: meta.mode(),
            links: meta.nlink(),
            length: meta.len(),
            modified: (meta.mtime(), meta.mtime_nsec()),
            changed: (meta.ctime(), meta.ctime_nsec()),
        }
    }
}

#[derive(Default)]
struct Scan {
    stamps: BTreeMap<String, Stamp>,
    names: BTreeSet<String>,
    files: SourceFiles,
    file_count: usize,
    total: u64,
}

pub(super) fn capture(root: &File) -> Result<SourceFiles, WorkspaceError> {
    capture_with_probe(root, || {})
}

fn capture_with_probe(
    root: &File,
    between_scans: impl FnOnce(),
) -> Result<SourceFiles, WorkspaceError> {
    let mut captured = Scan::default();
    captured.walk(root, "", 0, true)?;
    between_scans();
    let mut checked = Scan::default();
    checked.walk(root, "", 0, false)?;
    if captured.stamps != checked.stamps {
        return Err(CHANGED);
    }
    Ok(captured.files)
}

impl Scan {
    fn walk(
        &mut self,
        directory: &File,
        prefix: &str,
        depth: usize,
        capture: bool,
    ) -> Result<(), WorkspaceError> {
        if depth > MAX_DEPTH {
            return Err(WorkspaceError::Invalid(
                "workspace exceeds directory depth limit",
            ));
        }
        let before = Stamp::from(&directory.metadata()?);
        let entries = Dir::read_from(directory).map_err(std::io::Error::from)?;
        for entry in entries {
            let entry = entry.map_err(std::io::Error::from)?;
            let name = entry
                .file_name()
                .to_str()
                .map_err(|_| WorkspaceError::Invalid("noncanonical workspace path"))?;
            if name == "." || name == ".." || (prefix.is_empty() && name == ".git") {
                continue;
            }
            let path = if prefix.is_empty() {
                name.to_owned()
            } else {
                format!("{prefix}/{name}")
            };
            validate_path(&path)?;
            if !self.names.insert(path.to_ascii_lowercase()) || self.names.len() > MAX_ENTRIES {
                return Err(WorkspaceError::Invalid(
                    "colliding or excessive workspace entries",
                ));
            }
            // O_PATH pins the object without opening a device or FIFO for I/O.
            // NOFOLLOW pins a symlink itself, which the metadata check rejects.
            let handle = File::from(
                rustix::fs::openat(
                    directory,
                    name,
                    OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .map_err(std::io::Error::from)?,
            );
            let metadata = handle.metadata()?;
            if metadata.is_dir() {
                let child = File::from(
                    rustix::fs::openat(
                        &handle,
                        ".",
                        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                        Mode::empty(),
                    )
                    .map_err(std::io::Error::from)?,
                );
                self.walk(&child, &path, depth + 1, capture)?;
            } else {
                self.file(&handle, &metadata, &path, capture)?;
            }
        }
        if before != Stamp::from(&directory.metadata()?) {
            return Err(CHANGED);
        }
        self.stamps.insert(prefix.to_owned(), before);
        Ok(())
    }

    fn file(
        &mut self,
        handle: &File,
        metadata: &Metadata,
        path: &str,
        capture: bool,
    ) -> Result<(), WorkspaceError> {
        if !metadata.is_file() || metadata.nlink() != 1 {
            return Err(WorkspaceError::Invalid(
                "workspace requires unaliased regular files and directories",
            ));
        }
        self.file_count += 1;
        self.total = self
            .total
            .checked_add(metadata.len())
            .filter(|total| *total <= MAX_TOTAL_BYTES as u64)
            .ok_or(WorkspaceError::Invalid(
                "workspace exceeds total size limit",
            ))?;
        if metadata.len() > MAX_FILE_BYTES as u64 || self.file_count > MAX_FILES {
            return Err(WorkspaceError::Invalid(
                "workspace exceeds file size or count limit",
            ));
        }
        let before = Stamp::from(metadata);
        if capture {
            // Linux procfs reopens this pinned, already-validated regular inode;
            // a concurrent rename cannot redirect the read to another object.
            let file = File::open(format!("/proc/self/fd/{}", handle.as_raw_fd()))?;
            let mut bytes = Vec::new();
            file.take(MAX_FILE_BYTES as u64 + 1)
                .read_to_end(&mut bytes)?;
            if bytes.len() as u64 != metadata.len() || before != Stamp::from(&handle.metadata()?) {
                return Err(CHANGED);
            }
            self.files.insert(
                path.to_owned(),
                SourceFile {
                    bytes,
                    executable: metadata.mode() & 0o111 != 0,
                },
            );
        }
        self.stamps.insert(path.to_owned(), before);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        reason = "Tests abort on disposable fixture failures."
    )]
    use super::*;
    use std::fs;

    #[test]
    fn mutation_replacement_addition_and_removal_between_scans_are_refused() {
        for operation in 0..4 {
            let temp = tempfile::tempdir().unwrap();
            let file = temp.path().join("file");
            fs::write(&file, b"before").unwrap();
            let root = super::super::filesystem::open_directory(temp.path()).unwrap();
            let result = capture_with_probe(&root, || match operation {
                0 => fs::write(&file, b"after!").unwrap(),
                1 => {
                    fs::rename(&file, temp.path().join("old")).unwrap();
                    fs::write(&file, b"before").unwrap();
                }
                2 => fs::write(temp.path().join("added"), b"new").unwrap(),
                _ => fs::remove_file(&file).unwrap(),
            });
            assert!(result.is_err(), "accepted mutation {operation}");
        }
    }
}
