//! Deterministic byte bundles from an exact snapshot to an untrusted final tree.
//!
//! Export and apply perform blocking filesystem I/O only. They neither run Git
//! nor grant verification or promotion authority. Callers protect snapshot,
//! bundle storage and output parents from Session writers, and freeze workspace
//! writers for export. Observed races are refused, not silently retried.

use std::{collections::BTreeMap, fs, path::Path};

use serde::{Deserialize, Serialize};

use super::{
    MAX_FILE_BYTES, MAX_RECORD_BYTES, SnapshotRecord, SourceFiles, WorkspaceError, entries,
    filesystem, load_snapshot, tree, validate_inventory,
};
use crate::workspace::provenance::{OutputProvenance, OutputProvenanceCode};
use crate::{Digest, ManifestEntry};

const SCHEMA: &str = "louiselm.workspace.bundle/1";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BundleRecord {
    schema: String,
    base_digest: String,
    output_provenance: OutputProvenance,
    files: Vec<ManifestEntry>,
}

/// Payload-free result shared by export, apply and human/robot output.
#[derive(Debug, Serialize)]
pub struct BundlePreview {
    /// Versioned preview contract.
    pub schema: &'static str,
    /// Digest of the canonical bundle record, binding the base and final tree.
    pub bundle_digest: String,
    /// Required normalized baseline inventory digest.
    pub base_digest: String,
    /// Normalized final inventory digest; no verification authority.
    pub result_digest: String,
    /// Portable provenance is unknown until a trusted broker checks its producer.
    pub output_provenance: OutputProvenance,
    /// Final regular file count.
    pub file_count: usize,
    /// Final content byte count.
    pub total_bytes: u64,
    /// Sorted paths absent from the baseline.
    pub added: Vec<String>,
    /// Sorted paths whose bytes or executable bit changed.
    pub modified: Vec<String>,
    /// Sorted baseline paths absent from the final tree.
    pub deleted: Vec<String>,
}

impl BundleRecord {
    fn validate(&self, base: &SnapshotRecord) -> Result<(), WorkspaceError> {
        if self.schema != SCHEMA || self.base_digest != base.preview()?.base_digest {
            return Err(WorkspaceError::Invalid(
                "unsupported bundle or baseline digest mismatch",
            ));
        }
        self.output_provenance.validate()?;
        if self.output_provenance.code == OutputProvenanceCode::Untainted {
            return Err(WorkspaceError::Invalid(
                "bundle cannot claim clean provenance",
            ));
        }
        validate_inventory(&self.files)
    }

    fn preview(&self, base: &SnapshotRecord) -> Result<BundlePreview, WorkspaceError> {
        let old: BTreeMap<_, _> = base
            .files
            .iter()
            .map(|file| (file.path.as_str(), file))
            .collect();
        let new: BTreeMap<_, _> = self
            .files
            .iter()
            .map(|file| (file.path.as_str(), file))
            .collect();
        let mut preview = BundlePreview {
            schema: "louiselm.workspace.bundle-preview/1",
            bundle_digest: Digest::of(&serde_json::to_vec(self)?).to_string(),
            base_digest: self.base_digest.clone(),
            result_digest: Digest::of(&serde_json::to_vec(&self.files)?).to_string(),
            output_provenance: self.output_provenance.clone(),
            file_count: self.files.len(),
            total_bytes: self.files.iter().map(|file| file.size).sum(),
            added: Vec::new(),
            modified: Vec::new(),
            deleted: Vec::new(),
        };
        for file in &self.files {
            match old.get(file.path.as_str()) {
                None => preview.added.push(file.path.clone()),
                Some(previous) if **previous != *file => preview.modified.push(file.path.clone()),
                Some(_) => (),
            }
        }
        preview.deleted = base
            .files
            .iter()
            .filter(|file| !new.contains_key(file.path.as_str()))
            .map(|file| file.path.clone())
            .collect();
        Ok(preview)
    }
}

/// Exports a workspace's actual bytes relative to the exact approved snapshot.
///
/// The complete tree is read through pinned descriptors. Root `.git` is ignored
/// without reading it; nested Git metadata, links and special files are refused.
/// Inventory binds regular paths, executable bits, lengths and content digests;
/// empty directories and all other permission bits are normalized away.
/// `output` must be new and outside both inputs with an operator-owned parent.
/// Callers freeze writers; two content/metadata scans additionally reject observed
/// races. These checks do not create an atomic filesystem snapshot.
///
/// # Errors
/// Refuses snapshot substitution, unsafe/colliding/oversized or changing trees,
/// existing/nested outputs, and read or persistence failures. A directory-sync
/// failure after publication may leave the complete output present.
pub fn export(
    snapshot: &Path,
    expected_snapshot: &Digest,
    workspace: &Path,
    output: &Path,
) -> Result<BundlePreview, WorkspaceError> {
    let snapshot = fs::canonicalize(snapshot)?;
    let workspace = fs::canonicalize(workspace)?;
    filesystem::validate_output(output, &snapshot)?;
    filesystem::validate_output(output, &workspace)?;
    let (base, _) = load_snapshot(&snapshot, expected_snapshot)?;
    let root = filesystem::open_directory(&workspace)?;
    let mut files = tree::capture(&root)?;
    let record = BundleRecord {
        schema: SCHEMA.to_owned(),
        base_digest: base.preview()?.base_digest,
        output_provenance: OutputProvenance::unknown(),
        files: entries(&files),
    };
    record.validate(&base)?;
    let bytes = serde_json::to_vec(&record)?;
    if bytes.len() > MAX_RECORD_BYTES {
        return Err(WorkspaceError::Invalid("bundle record exceeds size limit"));
    }
    let preview = record.preview(&base)?;
    let baseline: BTreeMap<_, _> = base
        .files
        .iter()
        .map(|file| (file.path.as_str(), file))
        .collect();
    for file in &record.files {
        if baseline
            .get(file.path.as_str())
            .is_some_and(|old| **old == *file)
        {
            files.remove(&file.path);
        }
    }
    filesystem::publish(output, |staging| {
        filesystem::write_files(&staging.join("files"), &files, true)?;
        filesystem::write_file(&staging.join("bundle.json"), &bytes, 0o400)
    })?;
    Ok(preview)
}

/// Applies an exact bundle to an exact snapshot in a fresh integration tree.
///
/// Only validated raw bytes are written; no Git metadata, hooks, filters or
/// candidate programs execute. Extra files outside the bundle inventory have
/// no effect. Cross-Session bundles remain untrusted until separate confined
/// verification and exact-digest promotion. All I/O is blocking.
/// Caller protects both input stores and the output parent from Session writers.
///
/// # Errors
/// Refuses either digest mismatch, a different base, noncanonical or malformed
/// records, unsafe/tampered payloads, existing/nested outputs and I/O failures.
/// Pre-publication failures leave no output; post-rename sync failures may leave
/// the complete output present and require inspection before retrying.
pub fn apply(
    snapshot: &Path,
    expected_snapshot: &Digest,
    bundle: &Path,
    expected_bundle: &Digest,
    output: &Path,
) -> Result<BundlePreview, WorkspaceError> {
    let snapshot = fs::canonicalize(snapshot)?;
    let bundle = fs::canonicalize(bundle)?;
    filesystem::validate_output(output, &snapshot)?;
    filesystem::validate_output(output, &bundle)?;
    let (preview, files) = load(&snapshot, expected_snapshot, &bundle, expected_bundle)?;
    filesystem::publish(output, |staging| {
        filesystem::write_files(staging, &files, false)
    })?;
    Ok(preview)
}

pub(super) fn load(
    snapshot: &Path,
    expected_snapshot: &Digest,
    bundle: &Path,
    expected_bundle: &Digest,
) -> Result<(BundlePreview, SourceFiles), WorkspaceError> {
    let (base, mut files) = load_snapshot(snapshot, expected_snapshot)?;
    let root = filesystem::open_directory(bundle)?;
    let bytes = filesystem::read_source(&root, "bundle.json", MAX_RECORD_BYTES)?
        .ok_or(WorkspaceError::Invalid("bundle record is missing"))?
        .bytes;
    if Digest::of(&bytes) != *expected_bundle {
        return Err(WorkspaceError::Invalid(
            "bundle digest mismatch; inspect the selected bundle",
        ));
    }
    let record: BundleRecord = serde_json::from_slice(&bytes)?;
    record.validate(&base)?;
    if serde_json::to_vec(&record)? != bytes {
        return Err(WorkspaceError::Invalid("bundle record is not canonical"));
    }
    reconstruct(&base, &record, &root, &mut files)?;
    Ok((record.preview(&base)?, files))
}

fn reconstruct(
    base: &SnapshotRecord,
    record: &BundleRecord,
    root: &fs::File,
    files: &mut SourceFiles,
) -> Result<(), WorkspaceError> {
    let baseline: BTreeMap<_, _> = base
        .files
        .iter()
        .map(|file| (file.path.as_str(), file))
        .collect();
    let final_paths: BTreeMap<_, _> = record
        .files
        .iter()
        .map(|file| (file.path.as_str(), file))
        .collect();
    files.retain(|path, _| final_paths.contains_key(path.as_str()));
    // Discard changed baseline bytes first, keeping memory bounded by the final
    // content limit while replacements are loaded.
    for entry in &record.files {
        if baseline
            .get(entry.path.as_str())
            .is_none_or(|old| **old != *entry)
        {
            files.remove(&entry.path);
        }
    }
    for entry in &record.files {
        if !files.contains_key(&entry.path) {
            let file =
                filesystem::read_source(root, &format!("files/{}", entry.path), MAX_FILE_BYTES)?
                    .ok_or(WorkspaceError::Invalid("bundle payload is missing"))?;
            if file.bytes.len() as u64 != entry.size
                || file.executable != entry.executable
                || Digest::of(&file.bytes).hex() != entry.sha256
            {
                return Err(WorkspaceError::Invalid(
                    "bundle payload differs from its inventory",
                ));
            }
            files.insert(entry.path.clone(), file);
        }
    }
    Ok(())
}
