//! Explicit ordinary instruction-file delivery; no Agent-name or path guessing.

use std::{
    collections::BTreeMap,
    fs::File,
    io::{self, Read, Write},
    os::{fd::AsRawFd, unix::fs::MetadataExt},
    path::{Component, Path, PathBuf},
};

use rustix::fs::{AtFlags, FileType, Mode, OFlags};

use super::{Entry, ViewError, io_error, read_file, tree_error};
use crate::{
    Digest, GenerationRecord, Package,
    admission::ProfileDelivery,
    inspect::{MAX_PROFILE_BYTES, PROFILE_FILE, PackageKind},
    registry::Registry,
};

pub(super) fn delivery(
    record: &GenerationRecord,
    packages: &BTreeMap<&str, Package>,
    registry: &Registry,
    publish: bool,
) -> Result<ProfileDelivery, ViewError> {
    let mut profiles = record.payload.members.iter().filter(|member| {
        PackageKind::of(&packages[member.package_digest.as_str()].manifest)
            == PackageKind::OperatorProfile
    });
    let mut result = ProfileDelivery {
        profile: None,
        undelivered: registry.agent_ids(),
    };
    result.undelivered.sort();
    let Some(member) = profiles.next() else {
        return Ok(result);
    };
    if profiles.next().is_some() {
        return Err(ViewError::Refused("multiple_operator_profiles"));
    }
    let package = &packages[member.package_digest.as_str()];
    let entry = package
        .manifest
        .entry(PROFILE_FILE)
        .ok_or(ViewError::Refused("missing_profile_file"))?;
    if package.manifest.entries.len() != 1
        || entry.executable
        || entry.size > MAX_PROFILE_BYTES as u64
    {
        return Err(ViewError::Refused("invalid_profile_shape"));
    }
    let bytes = read_file(
        &package.file_path(PROFILE_FILE),
        &Entry::File {
            digest: Digest::parse(&entry.sha256).map_err(crate::StoreError::from)?,
            size: entry.size,
            executable: false,
        },
        false,
    )?;
    result.profile = Some(member.package_digest.clone());
    let mut undelivered = Vec::new();
    for id in &result.undelivered {
        let agent = registry
            .agent(id)
            .map_err(|_| ViewError::Refused("registered_agent_missing"))?;
        let Some(path) = agent.always_on_path.filter(|_| member.agents.contains(id)) else {
            undelivered.push(id.clone());
            continue;
        };
        if publish {
            replace(&path, &bytes)?;
        } else if !matches(&path, &bytes)? {
            undelivered.push(id.clone());
        }
    }
    result.undelivered = undelivered;
    Ok(result)
}

const DIRECTORY: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

// Pin every ancestor with no-follow opens so a renamed or swapped directory
// cannot redirect a later publication. Only explicitly declared paths reach here.
pub(super) fn parent(path: &Path, create: bool) -> Result<File, ViewError> {
    let parent = path
        .parent()
        .ok_or(ViewError::Refused("profile_destination_missing_parent"))?;
    let mut directory = File::from(
        rustix::fs::open("/", DIRECTORY, Mode::empty())
            .map_err(|source| io_error(path, source.into()))?,
    );
    for component in parent.components() {
        let Component::Normal(name) = component else {
            continue;
        };
        if create {
            match rustix::fs::mkdirat(&directory, name, Mode::RWXU) {
                Ok(()) => directory
                    .sync_all()
                    .map_err(|source| io_error(path, source))?,
                Err(rustix::io::Errno::EXIST) => (),
                Err(source) => return Err(io_error(path, source.into())),
            }
        }
        directory = rustix::fs::openat(&directory, name, DIRECTORY, Mode::empty())
            .map(File::from)
            .map_err(|source| io_error(path, source.into()))?;
    }
    Ok(directory)
}

fn name(path: &Path) -> Result<&std::ffi::OsStr, ViewError> {
    path.file_name()
        .ok_or(ViewError::Refused("profile_destination_missing_name"))
}

fn replace(path: &Path, bytes: &[u8]) -> Result<(), ViewError> {
    let directory = parent(path, true)?;
    let name = name(path)?;
    match rustix::fs::statat(&directory, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) if FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile => (),
        Ok(_) => return Err(tree_error(path, "profile_destination_not_regular")),
        Err(rustix::io::Errno::NOENT) => (),
        Err(source) => return Err(io_error(path, source.into())),
    }
    // tempfile supplies a private O_EXCL file; /proc keeps both its creation
    // and atomic rename relative to the pinned directory, not a mutable pathname.
    let pinned = PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd()));
    let mut staged = tempfile::Builder::new()
        .prefix(".louiselm-profile-")
        .tempfile_in(&pinned)
        .map_err(|source| io_error(path, source))?;
    rustix::fs::fchmod(staged.as_file(), Mode::RUSR | Mode::WUSR)
        .map_err(|source| io_error(path, source.into()))?;
    staged
        .write_all(bytes)
        .and_then(|()| staged.as_file().sync_all())
        .map_err(|source| io_error(path, source))?;
    staged
        .persist(pinned.join(name))
        .map_err(|error| io_error(path, error.error))?;
    directory
        .sync_all()
        .map_err(|source| io_error(path, source))
}

fn matches(path: &Path, expected: &[u8]) -> Result<bool, ViewError> {
    let directory = match parent(path, false) {
        Ok(directory) => directory,
        Err(ViewError::Io { source, .. }) if unavailable(&source) => return Ok(false),
        Err(error) => return Err(error),
    };
    let file = match rustix::fs::openat(
        &directory,
        name(path)?,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => File::from(fd),
        Err(error) if unavailable(&error.into()) => return Ok(false),
        Err(error) => return Err(io_error(path, error.into())),
    };
    let metadata = file.metadata().map_err(|source| io_error(path, source))?;
    if !metadata.is_file()
        || metadata.len() != expected.len() as u64
        || metadata.mode() & 0o077 != 0
    {
        return Ok(false);
    }
    let mut bytes = Vec::new();
    file.take(expected.len() as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| io_error(path, source))?;
    Ok(bytes == expected)
}

fn unavailable(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied | io::ErrorKind::NotADirectory
    ) || error.raw_os_error() == Some(rustix::io::Errno::LOOP.raw_os_error())
}
