//! Broker identity continuity, checked before opening any durable stores.

use super::{BrokerError, sync_directory};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
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

/// Caller validates the private directory and installed process identity first.
pub(super) fn check(state: &Path, uid: u32, gid: u32) -> Result<(), BrokerError> {
    let path = state.join("identity.json");
    match fs::symlink_metadata(&path) {
        Ok(metadata) if !metadata.is_file() || metadata.nlink() != 1 => {
            return Err(BrokerError::StateIdentityInvalid);
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return initialize(state, &path, uid, gid);
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
    if identity.uid != uid || identity.gid != gid {
        return Err(BrokerError::StateIdentityMismatch);
    }
    // Re-establish durability on a retry after an uncertain initial fsync.
    file.sync_all().map_err(BrokerError::Storage)?;
    sync_directory(state)
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
    let mut temporary = tempfile::NamedTempFile::new_in(state).map_err(BrokerError::Storage)?;
    temporary
        .write_all(&bytes)
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(BrokerError::Storage)?;
    temporary
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
