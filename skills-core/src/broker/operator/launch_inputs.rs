//! Authenticated source/cache preparation, separate from launch authority.

use super::{InspectError, Request, exchange};
use crate::{session_manifest::SessionInputManifest, workspace::launch_inputs::InputPreview};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

/// Exact operator-selected inputs for one immutable broker publication.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchInputsRequest {
    /// Complete resolved manifest; never log its Agent configuration.
    pub manifest: Box<SessionInputManifest>,
    /// Operator-controlled, broker-readable snapshot directory.
    pub snapshot: PathBuf,
    /// Operator-controlled, broker-readable immutable cache directory.
    pub cache: PathBuf,
    /// Exact current Run HEAD that the snapshot must retain.
    pub expected_base_commit: String,
}

impl LaunchInputsRequest {
    /// Parses a closed, bounded staging selection without opening any input path.
    /// # Errors
    /// Refuses malformed JSON, invalid manifests/paths/HEAD and oversized input.
    pub fn parse(bytes: &[u8]) -> Result<Self, InspectError> {
        if bytes.is_empty() || bytes.len() > crate::launch_protocol::MAX_PROTOCOL_MESSAGE_BYTES {
            return Err(InspectError::InvalidRequest);
        }
        let request: Self =
            serde_json::from_slice(bytes).map_err(|_| InspectError::InvalidRequest)?;
        request.validate()?;
        Ok(request)
    }

    pub(crate) fn validate(&self) -> Result<(), InspectError> {
        if !self.snapshot.is_absolute()
            || !self.cache.is_absolute()
            || !matches!(self.expected_base_commit.len(), 40 | 64)
            || !self
                .expected_base_commit
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            || SessionInputManifest::parse(&self.manifest.canonical_bytes()).is_err()
        {
            return Err(InspectError::InvalidRequest);
        }
        Ok(())
    }
}

/// Payload-free publication bindings. Staging grants no Session authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchInputBinding {
    /// Versioned binding response.
    pub schema: String,
    /// Exact manifest the later launch must name.
    pub manifest_digest: String,
    /// Retained snapshot identity.
    pub source_snapshot_digest: String,
    /// Retained source inventory identity.
    pub source_base_digest: String,
    /// Retained cache inventory identity.
    pub cache_base_digest: String,
    /// Exact source commit captured by the snapshot.
    pub base_commit: String,
}

impl LaunchInputBinding {
    pub(crate) fn from_preview(preview: InputPreview) -> Self {
        Self {
            schema: "louiselm.launch-inputs.staged/1".into(),
            manifest_digest: preview.manifest_digest,
            source_snapshot_digest: preview.source.snapshot_digest,
            source_base_digest: preview.source.base_digest,
            cache_base_digest: preview.cache_base_digest,
            base_commit: preview.source.base_commit,
        }
    }

    fn matches(&self, request: &LaunchInputsRequest) -> bool {
        self.schema == "louiselm.launch-inputs.staged/1"
            && self.manifest_digest == request.manifest.digest().to_string()
            && self.source_snapshot_digest == request.manifest.source_snapshot_digest
            && self.source_base_digest == request.manifest.source_base_digest
            && self.cache_base_digest == request.manifest.cache_base_digest
            && self.base_commit == request.expected_base_commit
    }
}

/// Stages exact inputs through the authenticated broker, without authorizing a launch.
/// Run this blocking exchange outside an editor/event-loop callback.
/// # Errors
/// Refuses malformed or oversized requests, foreign peers, changed input bytes,
/// duplicate publication and replies not binding every selected input.
pub fn stage_launch_inputs(
    path: &Path,
    broker_uid: u32,
    request: &LaunchInputsRequest,
    timeout: Duration,
) -> Result<LaunchInputBinding, InspectError> {
    request.validate()?;
    let wire_request = Request::LaunchInputs {
        request: Box::new(request.clone()),
    };
    if serde_json::to_vec(&wire_request)
        .map_err(|_| InspectError::InvalidRequest)?
        .len()
        > crate::launch_protocol::MAX_PROTOCOL_MESSAGE_BYTES
    {
        return Err(InspectError::InvalidRequest);
    }
    let bytes = exchange(path, broker_uid, &wire_request, timeout)?;
    let response: LaunchInputBinding =
        serde_json::from_slice(&bytes).map_err(|_| InspectError::StatusUnavailable)?;
    if serde_json::to_vec(&response).map_err(|_| InspectError::StatusUnavailable)? != bytes
        || !response.matches(request)
    {
        return Err(InspectError::StatusUnavailable);
    }
    Ok(response)
}
