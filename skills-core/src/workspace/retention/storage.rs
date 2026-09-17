//! One stable directory lock serializes broker pinning and privileged deletion.

use super::{
    DEFAULT_RETENTION_MS, EvidenceReferences, MAX_RECORD_BYTES, PrimaryEvidence, RetentionRecord,
    SCHEMA, invalid,
};
use crate::{
    launch::LaunchRequest,
    workspace::{WorkspaceError, filesystem},
};
use std::{
    fs::{self, File},
    io::Write,
    os::unix::fs::{DirBuilderExt, MetadataExt},
    path::{Path, PathBuf},
};

pub(crate) struct Store {
    root: File,
    path: PathBuf,
    owner: (u32, u32),
}

impl Store {
    pub(crate) fn create(path: &Path) -> Result<(), WorkspaceError> {
        match fs::DirBuilder::new().mode(0o700).create(path) {
            Ok(()) => File::open(path.parent().ok_or_else(invalid)?)?.sync_all()?,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }

    pub(crate) fn lock(path: &Path, owner: (u32, u32)) -> Result<Self, WorkspaceError> {
        let root = filesystem::open_directory(path)?;
        let meta = root.metadata()?;
        if (meta.uid(), meta.gid()) != owner || meta.mode() & 0o7777 != 0o700 {
            return Err(invalid());
        }
        // Nonblocking refusal bounds an operator request even during a large cleanup.
        root.try_lock()
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        Ok(Self {
            root,
            path: path.into(),
            owner,
        })
    }

    pub(crate) fn register(
        &mut self,
        launch: &LaunchRequest,
        now_ms: u64,
    ) -> Result<RetentionRecord, WorkspaceError> {
        launch.validate().map_err(|_| invalid())?;
        let name = name(&launch.session_id)?;
        if filesystem::read_source(&self.root, &name, MAX_RECORD_BYTES)?.is_some() {
            let prior = self.read(&launch.session_id)?;
            if prior.launch != *launch {
                return Err(invalid());
            }
            return Ok(prior);
        }
        let record = RetentionRecord {
            schema: SCHEMA.into(),
            launch: launch.clone(),
            recorded_at_ms: now_ms,
            expires_at_ms: now_ms
                .checked_add(DEFAULT_RETENTION_MS)
                .ok_or_else(invalid)?,
            pinned: false,
            primary_evidence: PrimaryEvidence::NotChecked,
            evidence: EvidenceReferences::default(),
        };
        self.write(&record)?;
        Ok(record)
    }

    pub(crate) fn read(&self, id: &str) -> Result<RetentionRecord, WorkspaceError> {
        let name = name(id)?;
        let meta = fs::symlink_metadata(self.path.join(&name))?;
        if (meta.uid(), meta.gid()) != self.owner || meta.mode() & 0o7777 != 0o600 {
            return Err(invalid());
        }
        let source =
            filesystem::read_source(&self.root, &name, MAX_RECORD_BYTES)?.ok_or_else(invalid)?;
        let record: RetentionRecord = serde_json::from_slice(&source.bytes)?;
        record.validate()?;
        if record.launch.session_id != id || serde_json::to_vec(&record)? != source.bytes {
            return Err(invalid());
        }
        Ok(record)
    }

    pub(crate) fn write(&mut self, record: &RetentionRecord) -> Result<(), WorkspaceError> {
        record.validate()?;
        let bytes = serde_json::to_vec(record)?;
        if bytes.len() > MAX_RECORD_BYTES {
            return Err(invalid());
        }
        let mut temporary = tempfile::NamedTempFile::new_in(&self.path)?;
        temporary.write_all(&bytes)?;
        // Root cleanup keeps records readable/writable by the installed broker.
        let meta = temporary.as_file().metadata()?;
        if (meta.uid(), meta.gid()) != self.owner {
            rustix::fs::fchown(
                temporary.as_file(),
                Some(rustix::process::Uid::from_raw(self.owner.0)),
                Some(rustix::process::Gid::from_raw(self.owner.1)),
            )
            .map_err(std::io::Error::from)?;
        }
        temporary.as_file().sync_all()?;
        temporary
            .persist(self.path.join(name(&record.launch.session_id)?))
            .map_err(|error| error.error)?;
        self.root.sync_all()?;
        Ok(())
    }

    pub(crate) fn pin(
        &mut self,
        id: &str,
        pinned: bool,
    ) -> Result<RetentionRecord, WorkspaceError> {
        let mut record = self.read(id)?;
        if record.primary_evidence != PrimaryEvidence::NotChecked {
            return Err(invalid());
        }
        record.pinned = pinned;
        self.write(&record)?;
        Ok(record)
    }
}

fn name(id: &str) -> Result<String, WorkspaceError> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        return Err(invalid());
    }
    Ok(format!("{id}.json"))
}
