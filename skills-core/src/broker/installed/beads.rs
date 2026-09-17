//! One administrator-selected canonical tracker for this machine-wide broker.

use std::{
    fs, io,
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
};

use serde::Deserialize;

use super::super::{BrokerError, BrokerService, attention_config::read_protected};
use crate::{Digest, launcher_install::LauncherConfig};

const CONFIG: &str = "/etc/louiselm-broker-beads.json";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Tracker {
    workspace: PathBuf,
    program: PathBuf,
    program_digest: String,
}

pub(super) fn configure(
    service: &mut BrokerService,
    installed: &LauncherConfig,
) -> Result<(), BrokerError> {
    let bytes = match read_protected(Path::new(CONFIG)) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(invalid("cannot read protected configuration")),
    };
    let tracker = parse(&bytes)?;
    validate(&tracker, installed)?;
    service
        .configure_beads_tracker(
            &tracker.workspace,
            &tracker.program,
            &Digest::parse(&tracker.program_digest)
                .map_err(|_| invalid("invalid executable digest"))?,
        )
        .map_err(|error| match error {
            BrokerError::InvalidGrant => invalid("configured executable digest does not match"),
            error => error,
        })
}

fn invalid(reason: &'static str) -> BrokerError {
    BrokerError::TrackerConfiguration(reason)
}

fn parse(bytes: &[u8]) -> Result<Tracker, BrokerError> {
    let tracker: Tracker =
        serde_json::from_slice(bytes).map_err(|_| invalid("invalid configuration schema"))?;
    for path in [&tracker.workspace, &tracker.program] {
        if !path.is_absolute()
            || path == Path::new("/")
            || path
                .components()
                .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
        {
            return Err(invalid("paths must be canonical and absolute"));
        }
    }
    if !Digest::parse(&tracker.program_digest)
        .is_ok_and(|digest| digest.to_string() == tracker.program_digest)
    {
        return Err(invalid("invalid executable digest"));
    }
    Ok(tracker)
}

fn validate(tracker: &Tracker, installed: &LauncherConfig) -> Result<(), BrokerError> {
    for path in [&tracker.workspace, &tracker.program] {
        if fs::canonicalize(path).map_err(|_| invalid("tracker path unavailable"))? != *path {
            return Err(invalid("paths must be canonical and absolute"));
        }
    }
    for ancestor in tracker.program.ancestors() {
        check(ancestor, ancestor != tracker.program, &[0], &[])?;
    }
    if fs::symlink_metadata(&tracker.program)
        .map_err(|_| invalid("executable unavailable"))?
        .mode()
        & 0o111
        == 0
    {
        return Err(invalid("pinned program is not executable"));
    }
    for ancestor in tracker.workspace.ancestors() {
        check(ancestor, true, &[0, installed.operator_uid], &[])?;
    }
    let beads = tracker.workspace.join(".beads");
    let owners = [0, installed.operator_uid, installed.broker_uid];
    let groups = [installed.broker_gid];
    let mut remaining = 65536;
    check_tree(&beads, &owners, &groups, &mut remaining)?;
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(beads.join("beads.db"))
        .map_err(|_| invalid("broker cannot read and write existing database"))?;
    let probe = tempfile::tempfile_in(&beads)
        .map_err(|_| invalid("broker cannot write tracker directory"))?;
    drop(probe);
    Ok(())
}

fn check(path: &Path, directory: bool, owners: &[u32], groups: &[u32]) -> Result<(), BrokerError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| invalid("tracker path unavailable"))?;
    if metadata.is_dir() != directory
        || (!directory && (!metadata.is_file() || metadata.nlink() != 1))
        || !owners.contains(&metadata.uid())
        || metadata.mode() & 0o002 != 0
        || (metadata.mode() & 0o020 != 0 && !groups.contains(&metadata.gid()))
    {
        return Err(invalid("untrusted tracker ownership, type or permissions"));
    }
    // Named ACL entries could grant a Session write access despite an allowed GID.
    for attribute in ["system.posix_acl_access", "system.posix_acl_default"] {
        match rustix::fs::getxattr(path, attribute, &mut [0_u8; 256]) {
            Err(rustix::io::Errno::NODATA | rustix::io::Errno::NOTSUP) => {}
            _ => return Err(invalid("extended tracker ACLs are unsupported")),
        }
    }
    Ok(())
}

fn check_tree(
    path: &Path,
    owners: &[u32],
    groups: &[u32],
    remaining: &mut usize,
) -> Result<(), BrokerError> {
    *remaining = remaining
        .checked_sub(1)
        .ok_or_else(|| invalid("tracker tree exceeds validation bound"))?;
    let metadata = fs::symlink_metadata(path).map_err(|_| invalid("tracker path unavailable"))?;
    check(path, metadata.is_dir(), owners, groups)?;
    if metadata.is_dir() {
        for entry in fs::read_dir(path).map_err(|_| invalid("tracker directory unavailable"))? {
            check_tree(
                &entry
                    .map_err(|_| invalid("tracker entry unavailable"))?
                    .path(),
                owners,
                groups,
                remaining,
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "Fixtures assert closed tracker configuration and filesystem boundaries."
)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    fn configuration_requires_exact_paths_fields_and_digest() {
        let record = serde_json::json!({"workspace":"/srv/project", "program":"/opt/br", "program_digest":Digest::of(b"br").to_string()});
        assert!(parse(&serde_json::to_vec(&record).unwrap()).is_ok());
        for field in ["workspace", "program", "program_digest"] {
            let mut missing = record.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert!(parse(&serde_json::to_vec(&missing).unwrap()).is_err());
            let mut invalid = record.clone();
            invalid[field] = "relative".into();
            assert!(parse(&serde_json::to_vec(&invalid).unwrap()).is_err());
        }
        for path in ["/", "/srv/../project"] {
            let mut invalid = record.clone();
            invalid["workspace"] = path.into();
            assert!(parse(&serde_json::to_vec(&invalid).unwrap()).is_err());
        }
        let mut extra = record;
        extra["grants"] = serde_json::json!([]);
        assert!(parse(&serde_json::to_vec(&extra).unwrap()).is_err());
    }

    #[test]
    fn tracker_boundary_rejects_links_untrusted_writers_and_special_files() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("db");
        fs::write(&path, b"fixture").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let uid = rustix::process::geteuid().as_raw();
        assert!(check(&path, false, &[uid], &[]).is_ok());
        assert!(check(&path, false, &[], &[]).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
        assert!(check(&path, false, &[uid], &[]).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let link = root.path().join("link");
        symlink(&path, &link).unwrap();
        assert!(check(&link, false, &[uid], &[]).is_err());
        fs::remove_file(&link).unwrap();
        fs::hard_link(&path, &link).unwrap();
        assert!(check(&path, false, &[uid], &[]).is_err());
        let fifo = root.path().join("fifo");
        rustix::fs::mkfifoat(rustix::fs::CWD, &fifo, rustix::fs::Mode::RUSR).unwrap();
        assert!(check(&fifo, false, &[uid], &[]).is_err());
    }
}
