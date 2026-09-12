//! Descriptor-relative reads and private, no-replace output publication.

use std::{
    fs::{self, File, OpenOptions},
    io::{Read as _, Write as _},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::Path,
};

use rustix::fs::{Mode, OFlags};

use super::{SourceFile, SourceFiles, WorkspaceError};

const DIRECTORY: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

pub(crate) fn open_directory(path: &Path) -> Result<File, WorkspaceError> {
    rustix::fs::open(path, DIRECTORY, Mode::empty())
        .map(File::from)
        .map_err(|error| std::io::Error::from(error).into())
}

pub(crate) fn read_source(
    root: &File,
    path: &str,
    limit: usize,
) -> Result<Option<SourceFile>, WorkspaceError> {
    read_source_with_probe(root, path, limit, || {})
}

// As in candidate capture, a private probe makes concurrent mutation observable
// deterministically in tests. Production never installs one.
fn read_source_with_probe(
    root: &File,
    path: &str,
    limit: usize,
    after_read: impl FnOnce(),
) -> Result<Option<SourceFile>, WorkspaceError> {
    let mut directory = root.try_clone()?;
    let mut parts = path.split('/').peekable();
    while let Some(part) = parts.next() {
        let flags = if parts.peek().is_some() {
            DIRECTORY
        } else {
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK
        };
        match rustix::fs::openat(&directory, part, flags, Mode::empty()) {
            Ok(fd) => directory = fd.into(),
            Err(rustix::io::Errno::NOENT) => return Ok(None),
            Err(rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR) => {
                return Err(WorkspaceError::Invalid(
                    "source contains a link or non-directory ancestor",
                ));
            }
            Err(error) => return Err(std::io::Error::from(error).into()),
        }
    }
    let before = directory.metadata()?;
    if !before.is_file() || before.nlink() != 1 {
        return Err(WorkspaceError::Invalid(
            "source must be an unaliased regular file",
        ));
    }
    if before.len() > limit as u64 {
        return Err(WorkspaceError::Invalid("source file exceeds size limit"));
    }
    let mut bytes = Vec::new();
    (&directory)
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    after_read();
    let after = directory.metadata()?;
    if bytes.len() > limit
        || before.len() != bytes.len() as u64
        || before.len() != after.len()
        || before.mode() != after.mode()
        || before.mtime() != after.mtime()
        || before.mtime_nsec() != after.mtime_nsec()
        || before.ctime() != after.ctime()
        || before.ctime_nsec() != after.ctime_nsec()
        || after.nlink() != 1
    {
        return Err(WorkspaceError::Invalid(
            "source changed during capture; prepare again",
        ));
    }
    Ok(Some(SourceFile {
        bytes,
        executable: before.mode() & 0o111 != 0,
    }))
}

pub(super) fn validate_output(output: &Path, source: &Path) -> Result<(), WorkspaceError> {
    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent = fs::canonicalize(parent)?;
    if parent.starts_with(source) || output.file_name().is_none() {
        return Err(WorkspaceError::Invalid(
            "output must be outside the source tree",
        ));
    }
    match fs::symlink_metadata(output) {
        Ok(_) => Err(WorkspaceError::Invalid(
            "output already exists; choose a new destination",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn write_file(path: &Path, bytes: &[u8], mode: u32) -> Result<(), WorkspaceError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.set_permissions(fs::Permissions::from_mode(mode))?;
    file.sync_all()?;
    Ok(())
}

pub(crate) fn write_files(
    root: &Path,
    files: &SourceFiles,
    readonly: bool,
) -> Result<(), WorkspaceError> {
    fs::create_dir_all(root)?;
    for (name, file) in files {
        let path = root.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mode = match (readonly, file.executable) {
            (true, false) => 0o400,
            (true, true) => 0o500,
            (false, false) => 0o600,
            (false, true) => 0o700,
        };
        write_file(&path, &file.bytes, mode)?;
    }
    Ok(())
}

pub(crate) fn publish(
    output: &Path,
    write: impl FnOnce(&Path) -> Result<(), WorkspaceError>,
) -> Result<(), WorkspaceError> {
    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let staging = tempfile::Builder::new()
        .prefix(".workspace-")
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir_in(parent)?;
    write(staging.path())?;
    sync_tree(staging.path())?;
    rustix::fs::renameat_with(
        rustix::fs::CWD,
        staging.path(),
        rustix::fs::CWD,
        output,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(std::io::Error::from)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn sync_tree(root: &Path) -> Result<(), WorkspaceError> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            sync_tree(&entry.path())?;
        } else {
            File::open(entry.path())?.sync_all()?;
        }
    }
    File::open(root)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "Tests abort on fixture failures.")]

    use super::*;

    #[test]
    fn publication_is_private_under_permissive_umask() {
        const CHILD: &str = "LOUISELM_WORKSPACE_MODE_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new("/bin/sh")
                .args(["-c", "umask 022; exec \"$@\"", "workspace-mode-test"])
                .arg(std::env::current_exe().unwrap())
                .args([
                    "workspace::filesystem::tests::publication_is_private_under_permissive_umask",
                    "--exact",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("published");
        publish(&output, |staging| {
            assert_eq!(
                fs::metadata(staging)?.mode() & 0o777,
                0o700,
                "the unpublished barrier must already be private"
            );
            write_file(&staging.join("record"), b"private", 0o400)
        })
        .unwrap();
        assert_eq!(fs::metadata(output).unwrap().mode() & 0o777, 0o700);
    }

    #[test]
    fn mutation_after_read_refuses_bytes_before_they_can_be_published() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("file");
        fs::write(&path, b"before").unwrap();
        let root = open_directory(temp.path()).unwrap();
        let result = read_source_with_probe(&root, "file", 100, || {
            fs::write(&path, b"a different length after read").unwrap();
        });
        assert!(matches!(
            result,
            Err(WorkspaceError::Invalid(
                "source changed during capture; prepare again"
            ))
        ));
    }

    #[test]
    fn failed_publication_removes_only_its_private_staging_directory() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("keep"), b"unrelated").unwrap();
        let output = temp.path().join("output");
        let result = publish(&output, |staging| {
            write_file(&staging.join("partial"), b"unpublished", 0o400)?;
            Err(WorkspaceError::Invalid("injected persistence failure"))
        });
        assert!(result.is_err());
        assert!(!output.exists());
        assert_eq!(fs::read(temp.path().join("keep")).unwrap(), b"unrelated");
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
    }
}
