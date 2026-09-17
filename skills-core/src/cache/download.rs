//! Prepare anonymous cache bytes before the short, revocation-serialized publication.

use super::{CacheError, MAX_BYTES, filesystem};
use crate::{Digest, sandbox::IdentityPlan};
use rustix::fs::{AtFlags, Mode, OFlags};
use std::{
    fs::{self, File},
    io::Write as _,
    os::{fd::AsRawFd as _, unix::fs::PermissionsExt as _},
};

pub(crate) struct DownloadWriter {
    pub(super) directory: File,
    pub(super) identity: IdentityPlan,
}

pub(crate) struct PreparedDownload {
    directory: File,
    file: File,
    name: String,
}

impl DownloadWriter {
    pub(crate) fn prepare(
        self,
        expected: &Digest,
        bytes: &[u8],
    ) -> Result<PreparedDownload, CacheError> {
        if bytes.len() > MAX_BYTES || Digest::of(bytes) != *expected {
            return Err(CacheError::Refused("download digest or size mismatch"));
        }
        let mut file = File::from(
            rustix::fs::openat(
                &self.directory,
                ".",
                OFlags::TMPFILE | OFlags::WRONLY | OFlags::CLOEXEC,
                Mode::RUSR | Mode::WUSR,
            )
            .map_err(std::io::Error::from)?,
        );
        file.write_all(bytes)?;
        filesystem::set_owner(&file, self.identity)?;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        file.sync_all()?;
        Ok(PreparedDownload {
            directory: self.directory,
            file,
            name: format!("artifact-{}", expected.hex()),
        })
    }
}

impl PreparedDownload {
    pub(crate) fn publish(self) -> Result<String, CacheError> {
        // Only the source follows procfs, resolving our still-open anonymous
        // inode. The destination is descriptor-relative and never replaced.
        rustix::fs::linkat(
            rustix::fs::CWD,
            format!("/proc/self/fd/{}", self.file.as_raw_fd()),
            &self.directory,
            self.name.as_str(),
            AtFlags::SYMLINK_FOLLOW,
        )
        .map_err(std::io::Error::from)?;
        self.directory.sync_all()?;
        Ok(self.name)
    }
}
