//! Owner-created checkout evidence for an exact retained source freeze.

use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::source::{
    PhoneSourceError, PhoneSourceStore, SourcePreview, SourceProof, materialize_skills,
    run_trusted, valid_commit, valid_digest, valid_id, verify_snapshot,
};
use crate::{
    hex::lower_hex,
    permissions::set_private_permissions,
    phone_projects::{Directory, directory},
};

const SCHEMA: &str = "louiselm.phone-run-checkout/1";
const MAX_RECORD: u64 = 65_536;

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CheckoutProof {
    schema: String,
    pub(super) run_id: String,
    pub(super) project_id: String,
    pub(super) source_id: String,
    pub(super) checkout_id: String,
    pub(super) snapshot_digest: String,
    checkout: Directory,
    pub(super) head: String,
    pub(super) reviewed: bool,
    sha256: String,
}

impl PhoneSourceStore {
    pub(super) fn read_checkout(&self, run_id: &str) -> Result<CheckoutProof, PhoneSourceError> {
        self.locked(run_id, |_| self.load_checkout(run_id))
    }

    pub(super) fn prepare_checkout(
        &self,
        run_id: &str,
        source_id: &str,
        output: &Path,
    ) -> Result<CheckoutProof, PhoneSourceError> {
        self.prepare_checkout_with(run_id, source_id, output, materialize_checkout)
    }

    pub(super) fn prepare_checkout_with(
        &self,
        run_id: &str,
        source_id: &str,
        output: &Path,
        materialize: impl FnOnce(
            &Path,
            &str,
            &Path,
            &str,
        ) -> Result<(SourcePreview, String), PhoneSourceError>,
    ) -> Result<CheckoutProof, PhoneSourceError> {
        if !valid_id(source_id) {
            return Err(PhoneSourceError::Invalid("invalid source identity"));
        }
        self.locked(run_id, |_| {
            let proof_path = self.checkout_path(run_id);
            match fs::symlink_metadata(&proof_path) {
                Ok(_) => return Err(PhoneSourceError::Conflict),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            let source = self.load(run_id)?;
            if source.source_id != source_id {
                return Err(PhoneSourceError::Invalid("source identity is stale"));
            }
            let expected = source_preview(&source);
            self.check_snapshot(&source, &expected)?;
            let (output, parent) = self.private_output(output)?;
            if output.starts_with(&source.snapshot.path) {
                return Err(PhoneSourceError::Invalid(
                    "checkout overlaps source snapshot",
                ));
            }
            let (observed, head) = materialize(
                Path::new(&source.snapshot.path),
                &source.snapshot_digest,
                &output,
                run_id,
            )?;
            if observed != expected || !valid_commit(&head) {
                return Err(PhoneSourceError::Invalid("checkout source binding changed"));
            }
            self.check_snapshot(&source, &expected)?;
            if self.load(run_id)? != source {
                return Err(PhoneSourceError::Invalid(
                    "source or checkout parent changed",
                ));
            }
            self.check_parent(&parent)?;
            let checkout = checkout_directory(&output)?;
            let mut proof = CheckoutProof {
                schema: SCHEMA.into(),
                run_id: run_id.into(),
                project_id: source.project_id,
                source_id: source.source_id,
                checkout_id: Uuid::new_v4().simple().to_string(),
                snapshot_digest: source.snapshot_digest,
                checkout,
                head,
                reviewed: false,
                sha256: String::new(),
            };
            proof.sha256 = checkout_hash(&proof)?;
            let bytes = serde_json::to_vec(&proof)?;
            if bytes.len() as u64 > MAX_RECORD {
                return Err(PhoneSourceError::Invalid("checkout proof is too large"));
            }
            // A partial publication fences retries until explicit inspection.
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&proof_path)?;
            set_private_permissions(&proof_path, false)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            File::open(&self.root)?.sync_all()?;
            Ok(proof)
        })
    }

    fn checkout_path(&self, run_id: &str) -> PathBuf {
        self.root.join(format!("{run_id}.checkout.json"))
    }

    fn load_checkout(&self, run_id: &str) -> Result<CheckoutProof, PhoneSourceError> {
        let mut bytes = Vec::new();
        File::open(self.checkout_path(run_id))?
            .take(MAX_RECORD + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_RECORD {
            return Err(PhoneSourceError::Invalid("checkout proof is too large"));
        }
        let proof: CheckoutProof = serde_json::from_slice(&bytes)?;
        if proof.schema != SCHEMA
            || proof.run_id != run_id
            || !valid_id(&proof.project_id)
            || !valid_id(&proof.source_id)
            || !valid_id(&proof.checkout_id)
            || !valid_digest(&proof.snapshot_digest)
            || !valid_commit(&proof.head)
            || !Path::new(&proof.checkout.path).is_absolute()
            || proof.reviewed
            || proof.sha256 != checkout_hash(&proof)?
        {
            return Err(PhoneSourceError::Invalid("checkout proof integrity failed"));
        }
        Ok(proof)
    }

    fn check_snapshot(
        &self,
        source: &SourceProof,
        expected: &SourcePreview,
    ) -> Result<(), PhoneSourceError> {
        let path = Path::new(&source.snapshot.path);
        if !fs::symlink_metadata(path)?.file_type().is_dir() || directory(path)? != source.snapshot
        {
            return Err(PhoneSourceError::Invalid(
                "source snapshot identity changed",
            ));
        }
        verify_snapshot(&source.snapshot, expected, fs::metadata(&self.root)?.uid())
    }
}

fn materialize_checkout(
    snapshot: &Path,
    digest: &str,
    output: &Path,
    run_id: &str,
) -> Result<(SourcePreview, String), PhoneSourceError> {
    let preview = materialize_skills(snapshot, digest, output)?;
    let checkout = checkout_directory(output)?;
    let bytes = git(output, &["rev-parse", "--verify", "HEAD"])?;
    let head = std::str::from_utf8(&bytes)
        .ok()
        .and_then(|value| value.strip_suffix('\n'))
        .filter(|value| valid_commit(value))
        .ok_or(PhoneSourceError::Invalid("checkout HEAD is invalid"))?;
    let branch = format!("refs/heads/run/{run_id}");
    let absent = "0".repeat(head.len());
    git(output, &["update-ref", &branch, head, &absent])?;
    git(output, &["symbolic-ref", "HEAD", &branch])?;
    if git(output, &["symbolic-ref", "--quiet", "HEAD"])? != format!("{branch}\n").as_bytes()
        || !git(
            output,
            &["status", "--porcelain=v1", "--untracked-files=all", "-z"],
        )?
        .is_empty()
        || checkout_directory(output)? != checkout
    {
        return Err(PhoneSourceError::Invalid("created checkout is not clean"));
    }
    Ok((preview, head.into()))
}

fn checkout_directory(path: &Path) -> Result<Directory, PhoneSourceError> {
    if !fs::symlink_metadata(path)?.file_type().is_dir() {
        return Err(PhoneSourceError::Invalid("checkout output was replaced"));
    }
    let selected = directory(path)?;
    let metadata = fs::metadata(path)?;
    let parent = path
        .parent()
        .ok_or(PhoneSourceError::Invalid("checkout has no parent"))?;
    if Path::new(&selected.path) != path
        || metadata.uid() != fs::metadata(parent)?.uid()
        || metadata.mode() & 0o077 != 0
    {
        return Err(PhoneSourceError::Invalid(
            "checkout is not owner-controlled",
        ));
    }
    Ok(selected)
}

fn git(checkout: &Path, args: &[&str]) -> Result<Vec<u8>, PhoneSourceError> {
    let mut command = vec![
        "--no-pager".into(),
        "--no-replace-objects".into(),
        "--literal-pathspecs".into(),
        "-C".into(),
        checkout.as_os_str().into(),
    ];
    for option in [
        "core.fsmonitor=false",
        "core.hooksPath=/dev/null",
        "core.attributesFile=/dev/null",
        "core.excludesFile=/dev/null",
        "protocol.allow=never",
        "gc.auto=0",
        "maintenance.auto=false",
    ] {
        command.push(OsString::from("-c"));
        command.push(option.into());
    }
    command.extend(args.iter().map(OsString::from));
    run_trusted(Path::new("/usr/bin/git"), &command, "10s", 65_536)
}

fn source_preview(source: &SourceProof) -> SourcePreview {
    SourcePreview {
        snapshot_digest: source.snapshot_digest.clone(),
        base_digest: source.base_digest.clone(),
        base_commit: source.base_commit.clone(),
    }
}

fn checkout_hash(proof: &CheckoutProof) -> Result<String, serde_json::Error> {
    let bytes = serde_json::to_vec(&(
        &proof.schema,
        &proof.run_id,
        &proof.project_id,
        &proof.source_id,
        &proof.checkout_id,
        &proof.snapshot_digest,
        &proof.checkout,
        &proof.head,
        proof.reviewed,
    ))?;
    Ok(lower_hex(&Sha256::digest(bytes)))
}
