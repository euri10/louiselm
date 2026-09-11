//! Descriptor-relative capture and pre-launch private copy publication.

use super::{CacheError, CacheFile, MAX_BYTES, MAX_ENTRIES};
use crate::{CanonicalPath, Digest, ManifestEntry, sandbox::IdentityPlan};
use rustix::fs::{Dir, Mode, OFlags};
use std::{
    collections::BTreeSet,
    fs::{self, File, Metadata, OpenOptions},
    io::{Read as _, Write as _},
    os::{
        fd::AsRawFd as _,
        unix::fs::{
            DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _,
        },
    },
    path::{Component, Path},
};

const DIRECTORY: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

fn open_directory(path: &Path) -> Result<File, CacheError> {
    if !path.is_absolute() {
        return Err(CacheError::Refused("cache path must be absolute"));
    }
    let mut directory =
        File::from(rustix::fs::open("/", DIRECTORY, Mode::empty()).map_err(std::io::Error::from)?);
    for part in path.components() {
        match part {
            Component::RootDir => (),
            Component::Normal(name) => {
                directory = File::from(
                    rustix::fs::openat(&directory, name, DIRECTORY, Mode::empty())
                        .map_err(std::io::Error::from)?,
                );
            }
            _ => return Err(CacheError::Refused("noncanonical cache root")),
        }
    }
    Ok(directory)
}

pub(super) fn capture(source: &Path) -> Result<Vec<CacheFile>, CacheError> {
    let directory = open_directory(source)?;
    let mut scan = Scan {
        files: Vec::new(),
        names: BTreeSet::new(),
        total: 0,
    };
    scan.walk(&directory, "", 0)?;
    scan.files.sort_by(|a, b| a.entry.path.cmp(&b.entry.path));
    Ok(scan.files)
}

struct Scan {
    files: Vec<CacheFile>,
    names: BTreeSet<String>,
    total: usize,
}

impl Scan {
    fn walk(&mut self, directory: &File, prefix: &str, depth: usize) -> Result<(), CacheError> {
        if depth > 64 {
            return Err(CacheError::Refused("cache exceeds directory depth limit"));
        }
        let before = directory.metadata()?;
        for entry in Dir::read_from(directory).map_err(std::io::Error::from)? {
            let entry = entry.map_err(std::io::Error::from)?;
            let name = entry
                .file_name()
                .to_str()
                .map_err(|_| CacheError::Refused("noncanonical cache path"))?;
            if matches!(name, "." | "..") {
                continue;
            }
            let path = if prefix.is_empty() {
                name.to_owned()
            } else {
                format!("{prefix}/{name}")
            };
            let canonical = CanonicalPath::parse(&path, false)
                .map_err(|_| CacheError::Refused("noncanonical cache path"))?;
            if path.contains('\\')
                || !self.names.insert(canonical.collision_key())
                || self.names.len() > MAX_ENTRIES
            {
                return Err(CacheError::Refused("colliding or excessive cache entries"));
            }
            // Pin the object without opening a device/FIFO; NOFOLLOW pins a
            // symlink itself, then metadata rejects it before any content read.
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
                    rustix::fs::openat(&handle, ".", DIRECTORY, Mode::empty())
                        .map_err(std::io::Error::from)?,
                );
                self.walk(&child, &path, depth + 1)?;
            } else {
                self.read_file(&handle, &metadata, path)?;
            }
        }
        unchanged(&before, &directory.metadata()?)
    }

    fn read_file(
        &mut self,
        handle: &File,
        before: &Metadata,
        path: String,
    ) -> Result<(), CacheError> {
        if !before.is_file() || before.nlink() != 1 {
            return Err(CacheError::Refused(
                "cache requires unaliased regular files and directories",
            ));
        }
        let limit = MAX_BYTES - self.total;
        if before.len() > limit as u64 {
            return Err(CacheError::Refused("cache exceeds byte limit"));
        }
        // This opens the pinned regular inode, never a source pathname that a
        // concurrent writer could substitute with a link/device after validation.
        let file = File::open(format!("/proc/self/fd/{}", handle.as_raw_fd()))?;
        let mut bytes = Vec::new();
        file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
        unchanged(before, &handle.metadata()?)?;
        if bytes.len() > limit || bytes.len() as u64 != before.len() {
            return Err(CacheError::Refused("cache changed during capture"));
        }
        self.total += bytes.len();
        self.files.push(CacheFile {
            entry: ManifestEntry {
                path,
                executable: before.mode() & 0o111 != 0,
                size: bytes.len() as u64,
                sha256: Digest::of(&bytes).hex().to_owned(),
            },
            bytes,
        });
        Ok(())
    }
}

fn unchanged(before: &Metadata, after: &Metadata) -> Result<(), CacheError> {
    if (
        before.dev(),
        before.ino(),
        before.mode(),
        before.nlink(),
        before.len(),
        before.mtime(),
        before.mtime_nsec(),
        before.ctime(),
        before.ctime_nsec(),
    ) != (
        after.dev(),
        after.ino(),
        after.mode(),
        after.nlink(),
        after.len(),
        after.mtime(),
        after.mtime_nsec(),
        after.ctime(),
        after.ctime_nsec(),
    ) {
        return Err(CacheError::Refused("cache changed during capture"));
    }
    Ok(())
}

pub(super) fn check_home(home: &Path, identity: IdentityPlan) -> Result<(), CacheError> {
    let metadata = open_directory(home)?.metadata()?;
    let (uid, gid) = match identity {
        IdentityPlan::NamespaceOnly => (
            rustix::process::geteuid().as_raw(),
            rustix::process::getegid().as_raw(),
        ),
        IdentityPlan::HostIdentity { uid, gid }
            if uid != 0 && gid != 0 && rustix::process::geteuid().is_root() =>
        {
            (uid, gid)
        }
        IdentityPlan::HostIdentity { .. } => {
            return Err(CacheError::Refused(
                "distinct host identity enforcement unavailable",
            ));
        }
    };
    if metadata.uid() != uid || metadata.gid() != gid || metadata.mode() & 0o7777 != 0o700 {
        return Err(CacheError::Refused(
            "home must be private to the Session identity",
        ));
    }
    Ok(())
}

pub(super) fn set_owner(file: &File, identity: IdentityPlan) -> Result<(), CacheError> {
    if let IdentityPlan::HostIdentity { uid, gid } = identity {
        rustix::fs::fchown(
            file,
            Some(rustix::fs::Uid::from_raw(uid)),
            Some(rustix::fs::Gid::from_raw(gid)),
        )
        .map_err(std::io::Error::from)?;
    }
    Ok(())
}

pub(super) fn materialize(
    files: &[CacheFile],
    home: &Path,
    output: &Path,
    identity: IdentityPlan,
) -> Result<File, CacheError> {
    let staging = tempfile::Builder::new()
        .prefix(".cache-")
        .tempdir_in(home)?;
    fs::set_permissions(staging.path(), fs::Permissions::from_mode(0o700))?;
    for cached in files {
        let path = staging.path().join(&cached.entry.path);
        if let Some(parent) = path.parent() {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)?;
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        file.write_all(&cached.bytes)?;
        set_owner(&file, identity)?;
        file.set_permissions(fs::Permissions::from_mode(if cached.entry.executable {
            0o700
        } else {
            0o600
        }))?;
        file.sync_all()?;
    }
    finish_directories(staging.path(), identity)?;
    let directory = open_directory(staging.path())?;
    rustix::fs::renameat_with(
        rustix::fs::CWD,
        staging.path(),
        rustix::fs::CWD,
        output,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(std::io::Error::from)?;
    File::open(home)?.sync_all()?;
    Ok(directory)
}

fn finish_directories(root: &Path, identity: IdentityPlan) -> Result<(), CacheError> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            finish_directories(&entry.path(), identity)?;
        }
    }
    let directory = File::open(root)?;
    set_owner(&directory, identity)?;
    directory.sync_all()?;
    Ok(())
}
