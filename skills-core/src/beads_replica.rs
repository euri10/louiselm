//! Disposable tracker generations; published inputs never come from a Session.

use crate::workspace::WorkspaceError;
use crate::workspace::filesystem;
use std::{
    collections::BTreeMap,
    fs::{self, File},
    os::unix::fs::{MetadataExt, PermissionsExt, symlink},
    path::{Component, Path},
};

pub(crate) type Files = BTreeMap<String, Vec<u8>>;

pub(crate) const DIRECTORY: &str = "beads-replica";
/// Called only after the Session root is sealed and its processes are gone.
pub(crate) fn discard(root: &Path) -> Result<(), WorkspaceError> {
    match fs::remove_dir_all(root) {
        Ok(()) => (),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    }
    File::open(
        root.parent()
            .ok_or(WorkspaceError::Invalid("missing replica parent"))?,
    )?
    .sync_all()?;
    Ok(())
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InputBinding {
    pub(crate) request_digest: String,
    pub(crate) assigned_uid: u32,
    pub(crate) assigned_gid: u32,
}
const MAX_BYTES: usize = 256 * 1024 * 1024;
const MAX_FILES: usize = 256;

/// Reads only a private, completed upstream export, never a published replica.
pub(crate) fn read_generated(root: &Path) -> Result<Files, WorkspaceError> {
    let directory = filesystem::open_directory(root)?;
    let mut files = Files::new();
    let mut remaining = MAX_BYTES;
    let mut pending = vec![String::new()];
    let mut entries = 0;
    while let Some(prefix) = pending.pop() {
        for entry in fs::read_dir(root.join(&prefix))? {
            entries += 1;
            if entries > MAX_FILES {
                return Err(WorkspaceError::Invalid(
                    "tracker replica has too many entries",
                ));
            }
            let entry = entry?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| WorkspaceError::Invalid("tracker replica name is not UTF-8"))?;
            let name = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            if entry.file_type()?.is_dir() {
                pending.push(name);
            } else {
                let file = filesystem::read_source(&directory, &name, remaining)?.ok_or(
                    WorkspaceError::Invalid("tracker replica changed during capture"),
                )?;
                remaining -= file.bytes.len();
                files.insert(name, file.bytes);
            }
        }
    }
    Ok(files)
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

fn select(root: &Path, generation: &str) -> Result<(), WorkspaceError> {
    if !valid_name(generation) {
        return Err(WorkspaceError::Invalid(
            "invalid tracker replica generation",
        ));
    }
    let temporary = root.join(format!("current-{generation}"));
    // The publisher directory is not writable by the Session. A leftover
    // pointer from a crash can be removed without opening its target.
    match fs::remove_file(&temporary) {
        Ok(()) => (),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(error.into()),
    }
    symlink(generation, &temporary)?;
    fs::rename(temporary, root.join("current"))?;
    File::open(root)?.sync_all()?;
    Ok(())
}

/// Replays preserve a newer current generation. A crash after the pointer
/// rename is recovered without reading any Session-writable bytes.
pub(crate) fn completed(root: &Path, generation: &str) -> Result<bool, WorkspaceError> {
    if !valid_name(generation) {
        return Err(WorkspaceError::Invalid(
            "invalid tracker replica generation",
        ));
    }
    let marker = root.join(format!("ready-{generation}"));
    match fs::symlink_metadata(&marker) {
        Ok(meta)
            if meta.is_file()
                && meta.nlink() == 1
                && meta.uid() == fs::metadata(root)?.uid()
                && meta.mode() & 0o7777 == 0o600 =>
        {
            return Ok(true);
        }
        Ok(_) => {
            return Err(WorkspaceError::Invalid(
                "invalid tracker publication marker",
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(error.into()),
    }
    match fs::read_link(root.join("current")) {
        Ok(target) if target == Path::new(generation) => {
            filesystem::write_file(&marker, b"", 0o600)?;
            File::open(root)?.sync_all()?;
            Ok(true)
        }
        Ok(_) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn publish(root: &Path, generation: &str, files: &Files) -> Result<(), WorkspaceError> {
    if !valid_name(generation) || files.len() > MAX_FILES {
        return Err(WorkspaceError::Invalid(
            "invalid tracker replica generation",
        ));
    }
    let mut total = 0_usize;
    for (name, bytes) in files {
        if name.is_empty()
            || Path::new(name)
                .components()
                .any(|p| !matches!(p, Component::Normal(_)))
        {
            return Err(WorkspaceError::Invalid("invalid tracker replica path"));
        }
        total = total
            .checked_add(bytes.len())
            .filter(|total| *total <= MAX_BYTES)
            .ok_or(WorkspaceError::Invalid(
                "tracker replica exceeds size limit",
            ))?;
    }
    let output = root.join(generation);
    filesystem::publish(&output, |staging| {
        for (name, bytes) in files {
            let path = staging.join(name);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            filesystem::write_file(&path, bytes, 0o660)?;
        }
        Ok(())
    })?;
    // All bytes still sit behind the private 0700 generation barrier. No
    // Session-controlled path is traversed after making that barrier writable.
    let mut directories = vec![output.clone()];
    for name in files.keys() {
        let path = output.join(name);
        let mut parent = path.parent();
        while let Some(directory) = parent.filter(|directory| *directory != output) {
            directories.push(directory.to_owned());
            parent = directory.parent();
        }
    }
    directories.sort();
    directories.dedup();
    directories.reverse();
    let group = fs::metadata(root)?.gid();
    for directory in directories {
        let handle = filesystem::open_directory(&directory)?;
        if handle.metadata()?.gid() != group {
            return Err(WorkspaceError::Invalid(
                "tracker replica lost its private group",
            ));
        }
        handle.set_permissions(fs::Permissions::from_mode(0o2770))?;
        handle.sync_all()?;
    }
    select(root, generation)?;
    filesystem::write_file(&root.join(format!("ready-{generation}")), b"", 0o600)?;
    File::open(root)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        reason = "Fixtures assert replica publication invariants."
    )]
    use super::*;
    use std::{fs, os::unix::fs::symlink};

    #[test]
    fn capture_and_publication_refuse_aliases_and_escaping_paths() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        symlink(outside.path(), root.path().join("issues.jsonl")).unwrap();
        assert!(read_generated(root.path()).is_err());
        fs::remove_file(root.path().join("issues.jsonl")).unwrap();
        fs::hard_link(outside.path(), root.path().join("issues.jsonl")).unwrap();
        assert!(read_generated(root.path()).is_err());
        let files = Files::from([("../escape".into(), b"no".to_vec())]);
        assert!(publish(root.path(), "valid", &files).is_err());
        assert!(publish(root.path(), "../escape", &Files::new()).is_err());
        assert!(!root.path().join("current").exists());
    }

    #[test]
    fn publication_replaces_the_pointer_without_reading_session_edits() {
        let root = tempfile::tempdir().unwrap();
        let mut files = Files::new();
        files.insert("issues.jsonl".into(), b"first".to_vec());
        publish(root.path(), "first", &files).unwrap();
        assert_eq!(
            fs::read(root.path().join("current/issues.jsonl")).unwrap(),
            b"first"
        );
        fs::remove_file(root.path().join("first/issues.jsonl")).unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        fs::write(outside.path(), b"outside").unwrap();
        symlink(outside.path(), root.path().join("first/issues.jsonl")).unwrap();
        *files.get_mut("issues.jsonl").unwrap() = b"second".to_vec();
        publish(root.path(), "second", &files).unwrap();
        assert_eq!(
            fs::read(root.path().join("current/issues.jsonl")).unwrap(),
            b"second"
        );
        assert_eq!(fs::read(outside.path()).unwrap(), b"outside");
        assert!(
            fs::symlink_metadata(root.path().join("first/issues.jsonl"))
                .unwrap()
                .is_symlink()
        );
        assert!(publish(root.path(), "second", &files).is_err());
        assert!(completed(root.path(), "first").unwrap());
        assert_eq!(
            fs::read_link(root.path().join("current")).unwrap(),
            Path::new("second")
        );
        fs::remove_file(root.path().join("ready-second")).unwrap();
        assert!(completed(root.path(), "second").unwrap());
        assert!(root.path().join("ready-second").is_file());
        discard(root.path()).unwrap();
        assert_eq!(fs::read(outside.path()).unwrap(), b"outside");
    }
}
