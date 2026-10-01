//! Trusted staging on the existing broker worker, before launch authorization.

use super::{BrokerError, BrokerService, InstalledBroker, sync_directory};
use crate::{
    session_manifest::SessionInputManifest,
    workspace::launch_inputs::{self, InputPreview},
};
use std::{fs, os::unix::fs::DirBuilderExt, path::Path};

impl BrokerService {
    /// Stages exact source/cache bytes named by a manifest, without authorizing launch.
    /// Run on the broker I/O worker with operator-selected inputs. Neither Agents
    /// nor capture-service may call this trusted controller composition API.
    ///
    /// # Errors
    /// Refuses changed inputs, duplicate publication and storage/validation failure.
    pub fn stage_launch_inputs(
        &self,
        manifest: &SessionInputManifest,
        snapshot: &Path,
        cache: &Path,
    ) -> Result<InputPreview, BrokerError> {
        let root = self
            .verification_inputs
            .parent()
            .ok_or(BrokerError::InvalidGrant)?
            .join("workspace-inputs");
        match fs::DirBuilder::new().mode(0o700).create(&root) {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(error) => return Err(BrokerError::Storage(error)),
        }
        sync_directory(root.parent().ok_or(BrokerError::InvalidGrant)?)?;
        Ok(launch_inputs::stage(
            manifest,
            snapshot,
            cache,
            &root.join(manifest.digest().hex()),
        )?)
    }
}

impl InstalledBroker {
    /// Validates the operator's current Run HEAD before immutable input staging.
    /// This copies only exact manifest-bound bytes and grants no launch authority.
    /// # Errors
    /// Refuses malformed requests, stale source HEAD, changed/unsafe inputs,
    /// duplicate publication and storage failure.
    pub fn stage_operator_launch_inputs(
        &self,
        request: &super::operator::LaunchInputsRequest,
    ) -> Result<super::operator::LaunchInputBinding, BrokerError> {
        request.validate().map_err(|_| BrokerError::InvalidGrant)?;
        let digest = crate::Digest::parse(&request.manifest.source_snapshot_digest)
            .map_err(|_| BrokerError::InvalidGrant)?;
        let actual = crate::workspace::snapshot_base_commit(&request.snapshot, &digest)?;
        if actual != request.expected_base_commit {
            return Err(BrokerError::InvalidGrant);
        }
        self.stage_launch_inputs(&request.manifest, &request.snapshot, &request.cache)
            .map(super::operator::LaunchInputBinding::from_preview)
    }

    /// Stages a manifest and exact source/cache bytes on the installed broker worker.
    /// This grants no launch authority; the operator reviews the returned preview
    /// before authorizing the exact request through the existing launch boundary.
    ///
    /// # Errors
    /// Refuses changed inputs, duplicate publication or unavailable storage.
    pub fn stage_launch_inputs(
        &self,
        manifest: &SessionInputManifest,
        snapshot: &Path,
        cache: &Path,
    ) -> Result<InputPreview, BrokerError> {
        self.service.stage_launch_inputs(manifest, snapshot, cache)
    }
}
