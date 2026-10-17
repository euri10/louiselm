//! Preserve old bytes behind the existing root-owned Session privacy barrier.
use super::{RecoveryError, io_error};
use crate::launch_recovery::RetainedStorage;
use std::{
    fs::{self, File},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Component, Path},
};

pub(super) fn seal_storage(root: &Path, session: &str) -> Result<RetainedStorage, RecoveryError> {
    crate::launch_protocol::validate_identifier(session).map_err(|_| RecoveryError::Conflict)?;
    if !root.is_absolute()
        || root
            .components()
            .any(|part| !matches!(part, Component::RootDir | Component::Normal(_)))
    {
        return Err(RecoveryError::Conflict);
    }
    let flags = rustix::fs::OFlags::RDONLY
        | rustix::fs::OFlags::DIRECTORY
        | rustix::fs::OFlags::NOFOLLOW
        | rustix::fs::OFlags::CLOEXEC;
    let mut directory = File::from(
        rustix::fs::open("/", flags, rustix::fs::Mode::empty())
            .map_err(|error| io_error("recovery storage root", error.into()))?,
    );
    // Descriptor-relative traversal never follows a symlink, including ancestors.
    // Every opened directory remains root-owned and unwritable to other users.
    for part in root
        .components()
        .skip(1)
        .map(Component::as_os_str)
        .chain(std::iter::once(std::ffi::OsStr::new(session)))
    {
        let metadata = directory
            .metadata()
            .map_err(|error| io_error("recovery storage authority", error))?;
        if metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
            return Err(RecoveryError::Conflict);
        }
        let child = match rustix::fs::openat(&directory, part, flags, rustix::fs::Mode::empty()) {
            Ok(fd) => File::from(fd),
            Err(rustix::io::Errno::NOENT) => {
                directory
                    .sync_all()
                    .map_err(|error| io_error("recovery storage absence durability", error))?;
                return Ok(RetainedStorage::Absent);
            }
            Err(error) => return Err(io_error("recovery storage directory", error.into()).into()),
        };
        directory = child;
    }
    let metadata = directory
        .metadata()
        .map_err(|error| io_error("recovery Session storage", error))?;
    if metadata.uid() != 0 || !matches!(metadata.mode() & 0o7777, 0o700 | 0o711) {
        return Err(RecoveryError::Conflict);
    }
    directory
        .set_permissions(fs::Permissions::from_mode(0o700))
        .and_then(|()| directory.sync_all())
        .map_err(|error| io_error("recovery storage privacy durability", error))?;
    Ok(RetainedStorage::Sealed)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "Tests abort on fixture failures.")]
    use super::*;
    use std::os::unix::fs::{chown, symlink};

    #[test]
    fn privileged_recovery_storage_preserves_bytes_and_refuses_untrusted_paths() {
        if std::env::var_os("LOUISELM_REQUIRE_CONSUMED_RECOVERY").is_none() {
            return;
        }
        assert!(rustix::process::geteuid().is_root());
        let root = tempfile::tempdir_in("/var/lib").unwrap();
        assert_eq!(
            seal_storage(root.path(), "absent").unwrap(),
            RetainedStorage::Absent
        );
        let session = root.path().join("session");
        fs::create_dir(&session).unwrap();
        fs::set_permissions(&session, fs::Permissions::from_mode(0o711)).unwrap();
        fs::write(session.join("retained"), b"unchanged").unwrap();
        assert_eq!(
            seal_storage(root.path(), "session").unwrap(),
            RetainedStorage::Sealed
        );
        assert_eq!(
            seal_storage(root.path(), "session").unwrap(),
            RetainedStorage::Sealed
        );
        assert_eq!(fs::read(session.join("retained")).unwrap(), b"unchanged");
        assert_eq!(fs::metadata(&session).unwrap().mode() & 0o7777, 0o700);
        symlink(&session, root.path().join("linked")).unwrap();
        assert!(seal_storage(root.path(), "linked").is_err());
        assert!(seal_storage(&root.path().join("linked"), "absent").is_err());
        assert!(seal_storage(root.path(), "..").is_err());
        assert!(seal_storage(root.path(), "alias/session").is_err());
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o777)).unwrap();
        assert!(seal_storage(root.path(), "absent").is_err());
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        chown(&session, Some(12345), None).unwrap();
        assert!(seal_storage(root.path(), "session").is_err());
        chown(&session, Some(0), None).unwrap();
        println!("RECOVERY_STORAGE_TRUST_AND_PRESERVATION");
    }
}
