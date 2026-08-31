//! Shared owner-only Unix socket capability helpers.

use std::{
    fs::{self, OpenOptions},
    io,
    io::Write,
    path::Path,
};

use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use uuid::Uuid;

use crate::permissions::set_private_permissions;

pub(crate) fn load_or_create_capability(path: &Path) -> io::Result<String> {
    if path.exists() {
        if fs::symlink_metadata(path)?.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "operator capability is a symlink",
            ));
        }
        set_private_permissions(path, false)?;
        return valid_capability(fs::read_to_string(path)?.trim());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let token = Uuid::new_v4().to_string();
    let temporary = path.with_extension(format!("tmp-{}", Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)?;
    set_private_permissions(&temporary, false)?;
    file.write_all(token.as_bytes())?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temporary, path)?;
    Ok(token)
}

pub(crate) fn token_sha256(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}

pub(crate) fn verify_capability(expected_hash: &str, supplied: &str) -> bool {
    expected_hash
        .as_bytes()
        .ct_eq(token_sha256(supplied).as_bytes())
        .into()
}

#[cfg(unix)]
pub(crate) fn remove_stale_socket(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::FileTypeExt;
    if !fs::symlink_metadata(path)?.file_type().is_socket() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "operator socket path is not a socket",
        ));
    }
    fs::remove_file(path)
}

#[cfg(unix)]
pub(crate) fn set_owner_only(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

fn valid_capability(value: &str) -> io::Result<String> {
    let parsed = Uuid::parse_str(value).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidData, "operator capability is invalid")
    })?;
    if parsed.to_string() != value {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "operator capability is not canonical",
        ));
    }
    Ok(value.to_owned())
}
