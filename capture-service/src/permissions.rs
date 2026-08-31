use std::{fs, io, path::Path};

#[cfg(unix)]
pub(crate) fn set_private_permissions(path: &Path, directory: bool) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(
        path,
        fs::Permissions::from_mode(if directory { 0o700 } else { 0o600 }),
    )
}

#[cfg(not(unix))]
pub(crate) fn set_private_permissions(_path: &Path, _directory: bool) -> io::Result<()> {
    Ok(())
}
