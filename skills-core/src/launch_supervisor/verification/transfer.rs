//! Root publishes only selected retained bytes, read-only to the installed broker group.

use super::{
    DirBuilderExt, File, MetadataExt, Path, Storage, SupervisorError, VerificationOperation,
    VerificationRequest, fs, protected, read_export,
};
use std::os::unix::fs::{PermissionsExt, chown};

impl Storage {
    pub(super) fn transfer(
        &self,
        request: &VerificationRequest,
    ) -> Result<String, SupervisorError> {
        let VerificationOperation::Transfer {
            export_request_id,
            export_digest,
            job_digest,
        } = &request.operation
        else {
            return Err(SupervisorError::AuthorizationRejected);
        };
        let source = self
            .directory
            .join("verification-exports")
            .join(export_request_id);
        let evidence = read_export(&source)?;
        if evidence
            .digest()
            .map_err(|_| SupervisorError::ResolutionFailed)?
            .to_string()
            != *export_digest
            || evidence.job.job_digest != *job_digest
            || evidence.request.launch != request.launch
            || evidence.request.head != request.head
        {
            return Err(SupervisorError::AuthorizationRejected);
        }
        let VerificationOperation::Export { input_id, .. } = &evidence.request.operation else {
            return Err(SupervisorError::AuthorizationRejected);
        };
        protected(&self.input_root, self.broker_uid, true)?;
        protected(&self.input_root.join(input_id), self.broker_uid, true)?;
        let snapshot = self.input_root.join(input_id).join("snapshot");
        let parent = self.directory.join("promotion-transfers");
        match fs::DirBuilder::new().mode(0o700).create(&parent) {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(_) => return Err(SupervisorError::DurabilityUnavailable),
        }
        protected(&parent, 0, false)?;
        let output = parent.join(&request.request_id);
        crate::workspace::promotion::copy_transfer(
            &snapshot,
            &source.join("job"),
            &evidence.job,
            &output,
        )
        .map_err(|_| SupervisorError::ResolutionFailed)?;
        // Files are already durable. Grant only group read/search; root retains
        // every write permission. No broker-selected path or ownership is used.
        readable(&output, self.broker_gid).map_err(|_| SupervisorError::DurabilityUnavailable)?;
        chown(&parent, None, Some(self.broker_gid))
            .map_err(|_| SupervisorError::DurabilityUnavailable)?;
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o750))
            .map_err(|_| SupervisorError::DurabilityUnavailable)?;
        File::open(&parent)
            .and_then(|p| p.sync_all())
            .map_err(|_| SupervisorError::DurabilityUnavailable)?;
        output
            .to_str()
            .map(str::to_owned)
            .ok_or(SupervisorError::ResolutionFailed)
    }
}

fn readable(path: &Path, gid: u32) -> Result<(), std::io::Error> {
    let meta = fs::symlink_metadata(path)?;
    if meta.is_dir() {
        for entry in fs::read_dir(path)? {
            readable(&entry?.path(), gid)?;
        }
    } else if !meta.is_file() || meta.nlink() != 1 {
        return Err(std::io::Error::other("invalid transfer file"));
    }
    chown(path, None, Some(gid))?;
    let mode = if meta.is_dir() || meta.mode() & 0o111 != 0 {
        0o550
    } else {
        0o440
    };
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    File::open(path)?.sync_all()
}
