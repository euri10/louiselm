//! Bounded no-replace restoration; the owning supervisor keeps the target frozen.
use super::{DIRECTORY, RecoveryError, material, read, retained};
use crate::launch_protocol::RecoveryRestoreRequest;
use crate::workspace::filesystem;
use std::io::Write;
use std::{
    fs::{self, File},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::Path,
};

pub(super) fn restore(
    source: &Path,
    target: &Path,
    request: &RecoveryRestoreRequest,
    now_ms: u64,
) -> Result<(), RecoveryError> {
    request.validate().map_err(|_| RecoveryError::Invalid)?;
    if now_ms >= request.source.request.expires_at_ms {
        return Err(RecoveryError::Expired);
    }
    let source_root = filesystem::open_directory(source)?;
    let metadata = source_root.metadata()?;
    if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o7777 != 0o700 {
        return Err(RecoveryError::Invalid);
    }
    let retained_path = source.join(DIRECTORY);
    let evidence = retained(
        &retained_path,
        &request.source.launch,
        &request.source.integration_digest,
        &request.source.request,
    )?;
    if evidence != request.source {
        return Err(RecoveryError::Conflict);
    }
    let retained_root = filesystem::open_directory(&retained_path)?;
    let (checkpoint, workspace) = material(&retained_root, &request.source.request.acp_session_id)?;
    let target_root = filesystem::open_directory(target)?;
    // Root-owned intent precedes publication. Partial/corrupt output refuses
    // retry rather than overwriting possible Agent data or creating a new point.
    copy_exact(
        &target_root,
        "cold-restore.json",
        &request.canonical_bytes(),
        None,
    )?;
    for (directory, name, bytes) in [
        ("home", "recovery.json", checkpoint),
        ("workspace", "recovery-counter.json", workspace),
    ] {
        let directory = File::from(
            rustix::fs::openat(
                &target_root,
                directory,
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::DIRECTORY
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
            )
            .map_err(std::io::Error::from)?,
        );
        let owner = directory.metadata()?;
        copy_exact(&directory, name, &bytes, Some((owner.uid(), owner.gid())))?;
    }
    target_root.sync_all()?;
    Ok(())
}

fn copy_exact(
    directory: &File,
    name: &str,
    bytes: &[u8],
    owner: Option<(u32, u32)>,
) -> Result<(), RecoveryError> {
    let opened = rustix::fs::openat(
        directory,
        name,
        rustix::fs::OFlags::WRONLY
            | rustix::fs::OFlags::CREATE
            | rustix::fs::OFlags::EXCL
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
    );
    match opened {
        Ok(fd) => {
            let mut file = File::from(fd);
            file.write_all(bytes)?;
            if let Some((uid, gid)) = owner {
                rustix::fs::fchown(
                    &file,
                    Some(rustix::fs::Uid::from_raw(uid)),
                    Some(rustix::fs::Gid::from_raw(gid)),
                )
                .map_err(std::io::Error::from)?;
            }
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
            file.sync_all()?;
        }
        Err(rustix::io::Errno::EXIST) => {
            if read(directory, name)? != bytes {
                return Err(RecoveryError::Conflict);
            }
            let fd = rustix::fs::openat(
                directory,
                name,
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
            )
            .map_err(std::io::Error::from)?;
            let file = File::from(fd);
            let metadata = file.metadata()?;
            let (uid, gid) = owner.unwrap_or((
                rustix::process::geteuid().as_raw(),
                rustix::process::getegid().as_raw(),
            ));
            if metadata.uid() != uid || metadata.gid() != gid || metadata.mode() & 0o7777 != 0o600 {
                return Err(RecoveryError::Invalid);
            }
            file.sync_all()?;
        }
        Err(error) => return Err(std::io::Error::from(error).into()),
    }
    directory.sync_all()?;
    Ok(())
}
