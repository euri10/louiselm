//! Exact source/cache staging for the existing privileged launcher.
//!
//! These blocking byte operations grant no authority. The broker owns staging;
//! the supervisor selects only the manifest digest in its authorized request,
//! validates all bytes, then copies them before starting the private Session.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{
    Digest,
    cache::CacheBase,
    session_manifest::{MAX_INPUT_MANIFEST_BYTES, SessionInputManifest},
};

use super::{SnapshotPreview, SnapshotRecord, SourceFiles, WorkspaceError, filesystem, git};

/// Payload-free launch input preview. Digests identify bytes, not approval.
#[derive(Serialize)]
pub struct InputPreview {
    /// Versioned preview contract.
    pub schema: &'static str,
    /// Exact manifest the launch request must name.
    pub manifest_digest: String,
    /// Included/excluded source changes and the immutable baseline identity.
    pub source: SnapshotPreview,
    /// Measured base from which each private cache overlay is copied.
    pub cache_base_digest: String,
}

pub(crate) struct LoadedInputs {
    pub(crate) manifest: SessionInputManifest,
    pub(crate) cache: CacheBase,
    record: SnapshotRecord,
    files: SourceFiles,
}

/// Payload-free binding retained beside the supervisor's protected baseline.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RetainedBinding {
    schema: String,
    pub(crate) manifest_digest: String,
    pub(crate) snapshot_digest: String,
    pub(crate) base_digest: String,
    pub(crate) cache_base_digest: String,
}

pub(crate) fn retained_binding(
    input: &Path,
    expected: &str,
) -> Result<RetainedBinding, WorkspaceError> {
    let root = filesystem::open_directory(input)?;
    let bytes = filesystem::read_source(&root, "binding.json", 4096)?
        .ok_or(WorkspaceError::Invalid("retained source binding missing"))?
        .bytes;
    let binding: RetainedBinding = serde_json::from_slice(&bytes)?;
    if binding.schema != "louiselm.workspace.launch-binding/1"
        || binding.manifest_digest != expected
        || serde_json::to_vec(&binding)? != bytes
    {
        return Err(WorkspaceError::Invalid("retained source binding mismatch"));
    }
    for value in [
        &binding.manifest_digest,
        &binding.snapshot_digest,
        &binding.base_digest,
        &binding.cache_base_digest,
    ] {
        if !Digest::parse(value).is_ok_and(|digest| digest.to_string() == *value) {
            return Err(WorkspaceError::Invalid("invalid retained source digest"));
        }
    }
    Ok(binding)
}

impl LoadedInputs {
    fn capture(
        manifest: SessionInputManifest,
        snapshot: &Path,
        cache: &Path,
    ) -> Result<Self, WorkspaceError> {
        let expected = Digest::parse(&manifest.source_snapshot_digest)
            .map_err(|_| WorkspaceError::Invalid("invalid source identity"))?;
        let (record, files) = super::load_snapshot(snapshot, &expected)?;
        let cache = CacheBase::capture(cache)?;
        if record.preview()?.base_digest != manifest.source_base_digest
            || cache.digest().to_string() != manifest.cache_base_digest
        {
            return Err(WorkspaceError::Invalid("source or cache binding mismatch"));
        }
        Ok(Self {
            manifest,
            cache,
            record,
            files,
        })
    }

    pub(crate) fn preview(&self) -> Result<InputPreview, WorkspaceError> {
        Ok(InputPreview {
            schema: "louiselm.workspace.launch-input-preview/1",
            manifest_digest: self.manifest.digest().to_string(),
            source: self.record.preview()?,
            cache_base_digest: self.cache.digest().to_string(),
        })
    }

    pub(crate) fn publish(&self, output: &Path) -> Result<(), WorkspaceError> {
        filesystem::publish(output, |staging| {
            filesystem::write_file(
                &staging.join("manifest.json"),
                &self.manifest.canonical_bytes(),
                0o400,
            )?;
            filesystem::write_files(&staging.join("snapshot/files"), &self.files, true)?;
            filesystem::write_file(
                &staging.join("snapshot/snapshot.json"),
                &serde_json::to_vec(&self.record)?,
                0o400,
            )?;
            self.cache.write_snapshot(&staging.join("cache"))
        })
    }

    pub(crate) fn retain_source(&self, output: &Path) -> Result<(), WorkspaceError> {
        let binding = RetainedBinding {
            schema: "louiselm.workspace.launch-binding/1".into(),
            manifest_digest: self.manifest.digest().to_string(),
            snapshot_digest: self.manifest.source_snapshot_digest.clone(),
            base_digest: self.manifest.source_base_digest.clone(),
            cache_base_digest: self.manifest.cache_base_digest.clone(),
        };
        filesystem::publish(output, |staging| {
            filesystem::write_file(
                &staging.join("binding.json"),
                &serde_json::to_vec(&binding)?,
                0o400,
            )?;
            filesystem::write_files(&staging.join("snapshot/files"), &self.files, true)?;
            filesystem::write_file(
                &staging.join("snapshot/snapshot.json"),
                &serde_json::to_vec(&self.record)?,
                0o400,
            )
        })
    }

    pub(crate) fn materialize_source(&self, output: &Path) -> Result<(), WorkspaceError> {
        filesystem::publish(output, |staging| {
            filesystem::write_files(staging, &self.files, false)?;
            git::initialize(staging, &self.files)
        })
    }
}

/// Copies a canonical manifest and its exact source/cache inputs into a new tree.
///
/// The output parent must be controlled by the operator or broker. No process is
/// launched, source Git metadata copied, or approval implied. All inputs are
/// measured into memory before publication; the original paths are not retained.
///
/// # Errors
/// Refuses malformed manifests, changed/unsafe input bytes, mismatched digests,
/// existing/nested output or failed durable publication.
pub fn stage(
    manifest: &SessionInputManifest,
    snapshot: &Path,
    cache: &Path,
    output: &Path,
) -> Result<InputPreview, WorkspaceError> {
    let manifest = SessionInputManifest::parse(&manifest.canonical_bytes())?;
    filesystem::validate_output(output, &std::fs::canonicalize(snapshot)?)?;
    filesystem::validate_output(output, &std::fs::canonicalize(cache)?)?;
    let inputs = LoadedInputs::capture(manifest, snapshot, cache)?;
    inputs.publish(output)?;
    inputs.preview()
}

pub(crate) fn load(input: &Path, expected: &Digest) -> Result<LoadedInputs, WorkspaceError> {
    let root = filesystem::open_directory(input)?;
    let bytes = filesystem::read_source(&root, "manifest.json", MAX_INPUT_MANIFEST_BYTES)?
        .ok_or(WorkspaceError::Invalid("Session input manifest missing"))?
        .bytes;
    let manifest = SessionInputManifest::parse(&bytes)?;
    if manifest.digest() != *expected {
        return Err(WorkspaceError::Invalid(
            "Session input manifest digest mismatch",
        ));
    }
    LoadedInputs::capture(manifest, &input.join("snapshot"), &input.join("cache"))
}

/// Remeasures a staged launch input and reports included/excluded source paths.
/// This is a local preview, not launch authority or live isolation evidence.
///
/// # Errors
/// Refuses missing, malformed, unsafe or substituted inputs and filesystem errors.
pub fn inspect(input: &Path, expected: &Digest) -> Result<InputPreview, WorkspaceError> {
    load(input, expected)?.preview()
}
