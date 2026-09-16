//! Broker identity continuity, checked before opening any durable stores.

use super::{AuditDecision, BrokerError, sync_directory};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::Path,
};

const SCHEMA: &str = "louiselm.broker-identity/1";
const MAX_BYTES: u64 = 1024;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    schema: String,
    uid: u32,
    gid: u32,
}

#[derive(Serialize)]
struct Adoption {
    at_ms: u64,
    decision: AuditDecision,
}

/// Caller validates the private directory and installed process identity first.
pub(super) fn check(state: &Path, uid: u32, gid: u32) -> Result<(), BrokerError> {
    let (identity, file) = match read(state) {
        Ok(record) => record,
        Err(BrokerError::StateIdentityMissing) => {
            return initialize(state, &state.join("identity.json"), uid, gid);
        }
        Err(error) => return Err(error),
    };
    if identity.uid != uid || identity.gid != gid {
        return Err(BrokerError::StateIdentityMismatch);
    }
    // Re-establish durability on a retry after an uncertain initial fsync.
    file.sync_all().map_err(BrokerError::Storage)?;
    sync_directory(state)
}

fn read(state: &Path) -> Result<(Identity, fs::File), BrokerError> {
    let path = state.join("identity.json");
    match fs::symlink_metadata(&path) {
        Ok(metadata) if !metadata.is_file() || metadata.nlink() != 1 => {
            return Err(BrokerError::StateIdentityInvalid);
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(BrokerError::StateIdentityMissing);
        }
        Err(error) => return Err(BrokerError::Storage(error)),
    }
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(
            (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK)
                .bits()
                .cast_signed(),
        )
        .open(&path)
        .map_err(BrokerError::Storage)?;
    let metadata = file.metadata().map_err(BrokerError::Storage)?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.len() > MAX_BYTES
        || metadata.mode() & 0o777 != 0o600
    {
        return Err(BrokerError::StateIdentityInvalid);
    }
    let mut bytes = Vec::new();
    (&file)
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(BrokerError::Storage)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(BrokerError::StateIdentityInvalid);
    }
    let identity: Identity =
        serde_json::from_slice(&bytes).map_err(|_| BrokerError::StateIdentityInvalid)?;
    if identity.schema != SCHEMA {
        return Err(BrokerError::StateIdentityInvalid);
    }
    Ok((identity, file))
}

/// Lock the directory itself, leaving first-start empty-state detection intact.
pub(super) fn exclusive(state: &Path) -> Result<fs::File, BrokerError> {
    let directory = fs::OpenOptions::new()
        .read(true)
        .custom_flags(
            (rustix::fs::OFlags::DIRECTORY | rustix::fs::OFlags::NOFOLLOW)
                .bits()
                .cast_signed(),
        )
        .open(state)
        .map_err(BrokerError::Storage)?;
    match directory.try_lock() {
        Ok(()) => Ok(directory),
        Err(fs::TryLockError::WouldBlock) => Err(BrokerError::StateInUse),
        Err(fs::TryLockError::Error(error)) => Err(BrokerError::Storage(error)),
    }
}

/// Only the authenticated operator entrypoint calls this after identity/path checks.
pub(super) fn adopt(
    state: &Path,
    uid: u32,
    gid: u32,
    operator_uid: u32,
    at_ms: u64,
) -> Result<bool, BrokerError> {
    let exclusive = exclusive(state)?;
    let result = adopt_locked(state, uid, gid, operator_uid, at_ms);
    // Closing alone can leave flock held in a concurrently forked child until
    // exec. Explicit release covers successful no-op/publication and failures.
    exclusive.unlock().map_err(BrokerError::Storage)?;
    result
}

fn adopt_locked(
    state: &Path,
    uid: u32,
    gid: u32,
    operator_uid: u32,
    at_ms: u64,
) -> Result<bool, BrokerError> {
    let (previous, marker) = read(state)?;
    if previous.uid == uid && previous.gid == gid {
        // A retry after uncertain publication must establish durability without
        // rewriting the matching marker or recording a second decision.
        marker.sync_all().map_err(BrokerError::Storage)?;
        sync_directory(state)?;
        return Ok(false);
    }
    let directory = state.join("identity-adoptions");
    match fs::DirBuilder::new().mode(0o700).create(&directory) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(BrokerError::Storage(error)),
    }
    let metadata = fs::symlink_metadata(&directory).map_err(BrokerError::Storage)?;
    if !metadata.is_dir()
        || metadata.mode() & 0o777 != 0o700
        || metadata.uid() != rustix::process::geteuid().as_raw()
    {
        return Err(BrokerError::StateIdentityInvalid);
    }
    // This is a machine-scoped decision, with no invented Session/Run identifiers.
    // It records authorization before publication, not a claim that publication
    // succeeded. A crash may leave this record beside the unchanged old marker.
    let record = Adoption {
        at_ms,
        decision: AuditDecision::StateIdentityAdoption {
            operator_uid,
            previous_uid: previous.uid,
            previous_gid: previous.gid,
            new_uid: uid,
            new_gid: gid,
        },
    };
    let bytes = serde_json::to_vec(&record).map_err(|_| BrokerError::StateIdentityInvalid)?;
    let temporary = prepared(&directory, &bytes)?;
    temporary
        .persist_noclobber(directory.join(format!("{at_ms}.json")))
        .map_err(|error| BrokerError::Storage(error.error))?;
    sync_directory(&directory)?;
    sync_directory(state)?;
    let next = Identity {
        schema: SCHEMA.to_owned(),
        uid,
        gid,
    };
    let bytes = serde_json::to_vec(&next).map_err(|_| BrokerError::StateIdentityInvalid)?;
    prepared(state, &bytes)?
        .persist(state.join("identity.json"))
        .map_err(|error| BrokerError::Storage(error.error))?;
    sync_directory(state)?;
    Ok(true)
}

fn prepared(state: &Path, bytes: &[u8]) -> Result<tempfile::NamedTempFile, BrokerError> {
    let mut temporary = tempfile::NamedTempFile::new_in(state).map_err(BrokerError::Storage)?;
    temporary
        .write_all(bytes)
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(BrokerError::Storage)?;
    Ok(temporary)
}

fn initialize(state: &Path, path: &Path, uid: u32, gid: u32) -> Result<(), BrokerError> {
    if fs::read_dir(state)
        .map_err(BrokerError::Storage)?
        .next()
        .transpose()
        .map_err(BrokerError::Storage)?
        .is_some()
    {
        return Err(BrokerError::StateIdentityMissing);
    }
    let identity = Identity {
        schema: SCHEMA.to_owned(),
        uid,
        gid,
    };
    let bytes = serde_json::to_vec(&identity).map_err(|_| BrokerError::StateIdentityInvalid)?;
    // Same-directory, no-clobber publication: neither a partial marker nor a
    // concurrent creator can silently acquire existing state.
    prepared(state, &bytes)?
        .persist_noclobber(path)
        .map_err(|error| BrokerError::Storage(error.error))?;
    sync_directory(state)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "Filesystem fixtures assert setup and observable failures."
)]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::{MetadataExt, PermissionsExt, symlink},
    };

    #[test]
    fn first_open_is_durable_and_matching_reopen_does_not_rewrite() {
        let root = tempfile::tempdir().unwrap();
        check(root.path(), 1000, 1000).unwrap();
        let marker = root.path().join("identity.json");
        let bytes = fs::read(&marker).unwrap();
        let before = fs::metadata(&marker).unwrap();
        check(root.path(), 1000, 1000).unwrap();
        let after = fs::metadata(&marker).unwrap();
        assert_eq!(fs::read(&marker).unwrap(), bytes);
        assert_eq!(
            (before.ino(), before.mtime(), before.mtime_nsec()),
            (after.ino(), after.mtime(), after.mtime_nsec())
        );
        assert_eq!(after.mode() & 0o777, 0o600);
        assert!(matches!(
            check(root.path(), 1001, 1000),
            Err(BrokerError::StateIdentityMismatch)
        ));
        assert!(matches!(
            check(root.path(), 1000, 1001),
            Err(BrokerError::StateIdentityMismatch)
        ));
        assert_eq!(fs::read(&marker).unwrap(), bytes);
    }

    #[test]
    fn existing_unmarked_state_is_never_adopted_implicitly() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("receipts")).unwrap();
        assert!(matches!(
            check(root.path(), 1000, 1000),
            Err(BrokerError::StateIdentityMissing)
        ));
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[test]
    fn explicit_adoption_preserves_receipts_and_audits_before_restarting() {
        let root = tempfile::tempdir().unwrap();
        check(root.path(), 1000, 1000).unwrap();
        fs::write(root.path().join("receipt"), b"exact signed bytes").unwrap();
        assert!(adopt(root.path(), 2000, 2001, 100, 123).unwrap());
        check(root.path(), 2000, 2001).unwrap();
        assert_eq!(
            fs::read(root.path().join("receipt")).unwrap(),
            b"exact signed bytes"
        );
        let audit: serde_json::Value = serde_json::from_slice(
            &fs::read(root.path().join("identity-adoptions/123.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(audit["decision"]["operator_uid"], 100);
        assert_eq!(audit["decision"]["previous_uid"], 1000);
        assert_eq!(audit["decision"]["previous_gid"], 1000);
        assert_eq!(audit["decision"]["new_uid"], 2000);
        assert_eq!(audit["decision"]["new_gid"], 2001);
        let marker = root.path().join("identity.json");
        let before = fs::metadata(&marker).unwrap();
        assert!(!adopt(root.path(), 2000, 2001, 100, 124).unwrap());
        assert_eq!(fs::metadata(&marker).unwrap().ino(), before.ino());
        assert!(!root.path().join("identity-adoptions/124.json").exists());
        let marker_bytes = fs::read(&marker).unwrap();
        let audit_bytes = fs::read(root.path().join("identity-adoptions/123.json")).unwrap();
        assert!(adopt(root.path(), 3000, 3000, 100, 123).is_err());
        assert_eq!(fs::read(&marker).unwrap(), marker_bytes);
        assert_eq!(
            fs::read(root.path().join("identity-adoptions/123.json")).unwrap(),
            audit_bytes
        );
        assert!(adopt(root.path(), 3000, 3000, 100, 125).unwrap());
        check(root.path(), 3000, 3000).unwrap();
    }

    #[test]
    fn adoption_refuses_missing_invalid_busy_or_unauditable_state() {
        let root = tempfile::tempdir().unwrap();
        assert!(adopt(root.path(), 2000, 2000, 100, 123).is_err());
        assert!(!root.path().join("identity.json").exists());
        check(root.path(), 1000, 1000).unwrap();
        let marker = fs::read(root.path().join("identity.json")).unwrap();
        let held = exclusive(root.path()).unwrap();
        assert!(adopt(root.path(), 2000, 2000, 100, 123).is_err());
        drop(held);
        fs::write(root.path().join("identity-adoptions"), b"unavailable").unwrap();
        assert!(adopt(root.path(), 2000, 2000, 100, 123).is_err());
        assert_eq!(fs::read(root.path().join("identity.json")).unwrap(), marker);
        fs::write(root.path().join("identity.json"), b"invalid").unwrap();
        assert!(adopt(root.path(), 2000, 2000, 100, 123).is_err());
    }

    #[test]
    fn malformed_or_redirected_markers_fail_without_modification() {
        for bytes in [
            b"{".as_slice(),
            b"{}",
            b"null",
            br#"{"schema":"unknown","uid":1000,"gid":1000}"#,
            br#"{"schema":"louiselm.broker-identity/1","uid":1000,"gid":1000,"extra":true}"#,
        ] {
            let root = tempfile::tempdir().unwrap();
            let marker = root.path().join("identity.json");
            fs::write(&marker, bytes).unwrap();
            fs::set_permissions(&marker, fs::Permissions::from_mode(0o600)).unwrap();
            assert!(matches!(
                check(root.path(), 1000, 1000),
                Err(BrokerError::StateIdentityInvalid)
            ));
            assert_eq!(fs::read(&marker).unwrap(), bytes);
        }
        let root = tempfile::tempdir().unwrap();
        symlink(
            root.path().join("absent"),
            root.path().join("identity.json"),
        )
        .unwrap();
        assert!(matches!(
            check(root.path(), 1000, 1000),
            Err(BrokerError::StateIdentityInvalid)
        ));
        assert!(!root.path().join("absent").exists());
    }
}
