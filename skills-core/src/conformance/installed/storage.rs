//! One atomic state file retains failure history independently of report currency.

use super::{Certificate, CertificationError, HostSnapshot, MAX_CERTIFICATE_BYTES};
use crate::{
    Digest,
    conformance::{FailureHistory, Report, Scope},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

const STATE_SCHEMA: &str = "louiselm.conformance.store/1";
const MAX_STATE_BYTES: usize = 512 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    schema: String,
    machine: Option<String>,
    history: FailureHistory,
    reports: BTreeMap<String, String>,
    pending: bool,
}

/// Protected current evidence plus independent persistent failures.
#[derive(Debug)]
pub struct CertificateStatus {
    /// Latest retained certificate matching these exact host inputs.
    /// Its observations may be incomplete or failed; admission decides whether
    /// they pass. Filtering here would misclassify existing evidence as missing.
    pub certificate: Option<Certificate>,
    /// Known failures, including those preceding reboot or a release update.
    pub history: FailureHistory,
    /// An attempt is running or interrupted, with no completion/cleanup proof.
    pub pending: bool,
}

/// Exclusive root-owned certification transaction and retained exact reports.
/// Dropping an unfinished attempt leaves a durable pending marker.
pub struct CertificateStore {
    root: PathBuf,
    owner: u32,
    lock: File,
    state: State,
}

impl CertificateStore {
    /// Open or initialize a private store under trusted ancestors, as root.
    /// Blocks no waiting writer: concurrent or interrupted work is refused.
    ///
    /// # Errors
    /// Rejects untrusted paths, missing/corrupt existing state and lock contention.
    pub fn open(root: &Path) -> Result<Self, CertificationError> {
        if !rustix::process::geteuid().is_root() {
            return Err(CertificationError::Invalid);
        }
        Self::open_owned(root, 0)
    }

    fn open_owned(root: &Path, owner: u32) -> Result<Self, CertificationError> {
        let parent = root.parent().ok_or(CertificationError::Invalid)?;
        trusted_directory(parent, owner)?;
        let created = match fs::DirBuilder::new().mode(0o700).create(root) {
            Ok(()) => true,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
            Err(error) => return Err(error.into()),
        };
        trusted_directory(root, owner)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits().cast_signed())
            .open(root.join("lock"))?;
        secure_file(&lock, owner)?;
        lock.try_lock().map_err(|_| CertificationError::Pending)?;
        if created {
            write_atomic(
                root,
                "state.json",
                &serde_json::to_vec(&State {
                    schema: STATE_SCHEMA.into(),
                    machine: None,
                    history: FailureHistory::default(),
                    reports: BTreeMap::new(),
                    pending: false,
                })
                .map_err(|_| CertificationError::Invalid)?,
            )?;
            File::open(parent)?.sync_all()?;
        }
        let state = read_state(root, owner)?;
        if state.pending {
            return Err(CertificationError::Pending);
        }
        Ok(Self {
            root: root.into(),
            owner,
            lock,
            state,
        })
    }

    /// Read protected evidence without creating files or modifying authority.
    /// This reports pending attempts distinctly; callers must not infer success
    /// from a certificate while failures or unresolved cleanup remain.
    ///
    /// # Errors
    /// Rejects absent, unreadable, malformed or untrusted state/report bytes.
    pub fn inspect(
        root: &Path,
        host: &HostSnapshot,
    ) -> Result<CertificateStatus, CertificationError> {
        Self::inspect_owned(root, host, 0)
    }

    fn inspect_owned(
        root: &Path,
        host: &HostSnapshot,
        owner: u32,
    ) -> Result<CertificateStatus, CertificationError> {
        host.validate()?;
        trusted_directory(root, owner)?;
        let state = read_state(root, owner)?;
        if state
            .machine
            .as_ref()
            .is_some_and(|machine| machine != &host.machine_digest)
        {
            return Err(CertificationError::Invalid);
        }
        let mut certificate = None;
        // Index exact host inputs so inspection reads one certificate, not the
        // entire historical report collection. Failures are validated separately.
        if let Some(digest) = state.reports.get(&host_key(host)?) {
            let bytes = read_protected(
                &root.join(report_name(digest)?),
                owner,
                MAX_CERTIFICATE_BYTES,
            )?;
            if Digest::of(&bytes).to_string() != *digest {
                return Err(CertificationError::Invalid);
            }
            let value = Certificate::parse_canonical(&bytes)?;
            if &value.host != host {
                return Err(CertificationError::Invalid);
            }
            certificate = Some(value);
        }
        Ok(CertificateStatus {
            certificate,
            history: state.history,
            pending: state.pending,
        })
    }

    pub(super) fn begin(&mut self, host: &HostSnapshot) -> Result<(), CertificationError> {
        host.validate()?;
        if self.state.pending || self.state.reports.len() >= 4096 {
            return Err(CertificationError::Pending);
        }
        if self
            .state
            .machine
            .as_ref()
            .is_some_and(|machine| machine != &host.machine_digest)
        {
            return Err(CertificationError::Invalid);
        }
        self.state.machine = Some(host.machine_digest.clone());
        self.state.pending = true;
        self.save()
    }

    pub(super) fn observe(&mut self, report: &Report) -> Result<(), CertificationError> {
        if !self.state.pending || report.scope != Scope::InstalledHost || report.completed {
            return Err(CertificationError::Invalid);
        }
        self.state.history = self.state.history.record(report)?;
        // Retain exact partial observations before acknowledging them. A later
        // cancellation cannot erase an already observed boundary failure.
        let bytes = report.canonical_bytes()?;
        let name = format!("observations-{}.json", Digest::of(&bytes).hex());
        write_atomic(&self.root, &name, &bytes)?;
        self.save()
    }

    pub(super) fn remember_resources(
        &self,
        directory: &Path,
        identities: &[crate::launcher_install::Identity],
    ) -> Result<(), CertificationError> {
        if !self.state.pending || identities.len() > 3 {
            return Err(CertificationError::Invalid);
        }
        let bytes = serde_json::to_vec(&serde_json::json!({
            "schema": "louiselm.conformance.resources/1", "directory": directory,
            "worker_pid": std::process::id(), "identities": identities,
            "session_prefix": "cert-",
        }))
        .map_err(|_| CertificationError::Invalid)?;
        write_atomic(&self.root, "attempt.json", &bytes)
    }

    pub(super) fn finish(&mut self, certificate: &Certificate) -> Result<(), CertificationError> {
        if !self.state.pending
            || self.state.machine.as_ref() != Some(&certificate.host.machine_digest)
        {
            return Err(CertificationError::Invalid);
        }
        let bytes = certificate.canonical_bytes()?;
        let observation_bytes = certificate.observations.canonical_bytes()?;
        write_atomic(
            &self.root,
            &format!("observations-{}.json", Digest::of(&observation_bytes).hex()),
            &observation_bytes,
        )?;
        let digest = Digest::of(&bytes).to_string();
        write_atomic(&self.root, &report_name(&digest)?, &bytes)?;
        self.state.history = self.state.history.record(&certificate.observations)?;
        self.state
            .reports
            .insert(host_key(&certificate.host)?, digest);
        self.state.pending = false;
        self.save()
    }

    fn save(&self) -> Result<(), CertificationError> {
        // The directory is root-private; still reject changed ownership/modes.
        trusted_directory(&self.root, self.owner)?;
        let bytes = serde_json::to_vec(&self.state).map_err(|_| CertificationError::Invalid)?;
        if bytes.len() > MAX_STATE_BYTES {
            return Err(CertificationError::Invalid);
        }
        write_atomic(&self.root, "state.json", &bytes)
    }
}

impl Drop for CertificateStore {
    fn drop(&mut self) {
        // A concurrent fork may retain this open-file description until exec.
        // Unlock after all publication has settled, even with such duplicates.
        // Failure only retains exclusion until the last descriptor closes:
        // it cannot clear pending/failure evidence or release an identity lease.
        let _ = self.lock.unlock();
    }
}

fn read_state(root: &Path, owner: u32) -> Result<State, CertificationError> {
    let bytes = read_protected(&root.join("state.json"), owner, MAX_STATE_BYTES)?;
    let state: State = serde_json::from_slice(&bytes).map_err(|_| CertificationError::Invalid)?;
    if state.schema != STATE_SCHEMA
        || state.reports.len() > 4096
        || state
            .machine
            .as_ref()
            .is_some_and(|value| Digest::parse(value).is_err())
        || state
            .reports
            .iter()
            .any(|(key, value)| Digest::parse(key).is_err() || Digest::parse(value).is_err())
        || serde_json::to_vec(&state).map_err(|_| CertificationError::Invalid)? != bytes
    {
        return Err(CertificationError::Invalid);
    }
    let incomplete = Report {
        schema: crate::conformance::REPORT_SCHEMA.into(),
        scope: Scope::InstalledHost,
        checks: Vec::new(),
        completed: false,
        cleanup: crate::conformance::Cleanup::Pending,
    };
    state.history.record(&incomplete)?;
    if state
        .history
        .failures
        .iter()
        .any(|failure| failure.scope != Scope::InstalledHost)
    {
        return Err(CertificationError::Invalid);
    }
    for failure in &state.history.failures {
        let digest =
            Digest::parse(&failure.report_digest).map_err(|_| CertificationError::Invalid)?;
        let bytes = read_protected(
            &root.join(format!("observations-{}.json", digest.hex())),
            owner,
            crate::conformance::MAX_REPORT_BYTES,
        )?;
        if Digest::of(&bytes) != digest {
            return Err(CertificationError::Invalid);
        }
        let report = Report::parse_canonical(&bytes)?;
        if report.scope != Scope::InstalledHost
            || !matches!(report.result()?, crate::conformance::ReportResult::Failed(checks) if checks.contains(&failure.check))
        {
            return Err(CertificationError::Invalid);
        }
    }
    Ok(state)
}

fn report_name(digest: &str) -> Result<String, CertificationError> {
    Ok(format!(
        "certificate-{}.json",
        Digest::parse(digest)
            .map_err(|_| CertificationError::Invalid)?
            .hex()
    ))
}

fn host_key(host: &HostSnapshot) -> Result<String, CertificationError> {
    Ok(Digest::of(&serde_json::to_vec(host).map_err(|_| CertificationError::Invalid)?).to_string())
}

pub(super) fn trusted_directory(path: &Path, owner: u32) -> Result<(), CertificationError> {
    if !path.is_absolute() {
        return Err(CertificationError::Invalid);
    }
    for ancestor in path.ancestors() {
        let metadata = fs::symlink_metadata(ancestor)?;
        if !metadata.is_dir()
            || (metadata.uid() != 0 && metadata.uid() != owner)
            || (metadata.mode() & 0o022 != 0
                && !(metadata.uid() == 0 && metadata.mode() & 0o1000 != 0 && ancestor != path))
        {
            return Err(CertificationError::Invalid);
        }
    }
    Ok(())
}

fn secure_file(file: &File, owner: u32) -> Result<(), CertificationError> {
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != owner
        || metadata.mode() & 0o077 != 0
        || metadata.nlink() != 1
    {
        return Err(CertificationError::Invalid);
    }
    Ok(())
}

fn read_protected(path: &Path, owner: u32, maximum: usize) -> Result<Vec<u8>, CertificationError> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(
            (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK)
                .bits()
                .cast_signed(),
        )
        .open(path)?;
    secure_file(&file, owner)?;
    let mut bytes = Vec::new();
    file.take(maximum as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > maximum {
        return Err(CertificationError::Invalid);
    }
    Ok(bytes)
}

fn write_atomic(root: &Path, name: &str, bytes: &[u8]) -> Result<(), CertificationError> {
    let mut temporary = tempfile::NamedTempFile::new_in(root)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(root.join(name))
        .map_err(|error| error.error)?;
    File::open(root)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
#[path = "storage_tests.rs"]
mod tests;
