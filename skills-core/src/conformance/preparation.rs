//! Bounded root-owned pre-admission observations. No process tree or launch grant.

use super::{ReportError, admission::Condition};
#[cfg(target_os = "linux")]
use super::{
    admission::{self, Admission, Attendance, Enforcement},
    installed::{CertificateStore, CertificationError, measure},
};
use crate::Digest;
#[cfg(target_os = "linux")]
use crate::{
    launch::LaunchRequest,
    launcher_install::{LauncherConfig, LauncherPaths},
};
use serde::{Deserialize, Serialize};
#[cfg(target_os = "linux")]
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    time::Instant,
};

#[cfg(target_os = "linux")]
const MAX_BYTES: u64 = 4096;
/// Maximum age of a preparation at planning, approval and initial admission.
pub const MAX_AGE_MS: u64 = 300_000;

/// Minimal trusted observation bound to one exact prospective launch.
/// Deserialization alone establishes no authority; use protected storage.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preparation {
    /// Closed schema.
    pub schema: String,
    /// Canonical launch digest, including Run, revision and authorization identity.
    pub request_digest: String,
    /// Prospective Session.
    pub session_id: String,
    /// Authenticated installed operator.
    pub operator_uid: u32,
    /// Exact protected launcher configuration.
    pub policy_digest: String,
    /// Host boot which produced the observation.
    pub boot_id: String,
    /// Observation completion time, never renewed by reads.
    pub observed_at_ms: u64,
    /// Exact missing evidence condition; never an exception for broken controls.
    pub condition: Condition,
}

impl Preparation {
    /// Check structure and freshness without authenticating the producer.
    /// # Errors
    /// Rejects malformed, future, expired or non-waivable observations.
    pub fn validate(&self, now_ms: u64) -> Result<(), ReportError> {
        if self.schema != "louiselm.conformance-preparation/1"
            || crate::launch_protocol::validate_identifier(&self.session_id).is_err()
            || Digest::parse(&self.request_digest).is_err()
            || Digest::parse(&self.policy_digest).is_err()
            || !uuid(&self.boot_id)
            || self.operator_uid == 0
            || now_ms < self.observed_at_ms
            || now_ms - self.observed_at_ms >= MAX_AGE_MS
            || !matches!(
                self.condition,
                Condition::Missing | Condition::Stale | Condition::Incomplete
            )
        {
            return Err(ReportError::InvalidObservations);
        }
        Ok(())
    }

    /// Recheck the installed policy and boot; this does not replace fresh host measurement.
    /// # Errors
    /// Rejects changed policy/boot, stale observations and unavailable host identity.
    #[cfg(target_os = "linux")]
    pub fn validate_current(
        &self,
        config: &LauncherConfig,
        now_ms: u64,
    ) -> Result<(), CertificationError> {
        self.validate(now_ms)
            .map_err(|_| CertificationError::Invalid)?;
        let policy = serde_json::to_vec(config).map_err(|_| CertificationError::Invalid)?;
        if config.conformance != Enforcement::Enforced
            || self.operator_uid != config.operator_uid
            || self.policy_digest != Digest::of(&policy).to_string()
            || self.boot_id != fs::read_to_string("/proc/sys/kernel/random/boot_id")?.trim()
        {
            return Err(CertificationError::Invalid);
        }
        Ok(())
    }

    /// Read the fixed root-owned observation for this Session. Performs bounded filesystem I/O.
    /// # Errors
    /// Refuses unsafe ancestry, links, foreign owners, malformed bytes and stale policy/boot.
    #[cfg(target_os = "linux")]
    pub fn read(
        paths: &LauncherPaths,
        config: &LauncherConfig,
        session_id: &str,
        now_ms: u64,
    ) -> Result<Self, CertificationError> {
        crate::launch_protocol::validate_identifier(session_id)
            .map_err(|_| CertificationError::Invalid)?;
        let root = paths.state_root.join("pre-admission");
        super::installed::storage::trusted_directory(&root, 0)?;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(
                i32::try_from((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits())
                    .map_err(|_| CertificationError::Invalid)?,
            )
            .open(root.join(format!("{session_id}.json")))?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.uid() != 0
            || metadata.mode() & 0o022 != 0
            || metadata.nlink() != 1
            || metadata.len() > MAX_BYTES
        {
            return Err(CertificationError::Invalid);
        }
        let mut bytes = Vec::new();
        file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(CertificationError::Invalid);
        }
        let value: Self =
            serde_json::from_slice(&bytes).map_err(|_| CertificationError::Invalid)?;
        if value.session_id != session_id {
            return Err(CertificationError::Invalid);
        }
        value.validate_current(config, now_ms)?;
        Ok(value)
    }
}

/// Measure and retain a prospective launch's exact waivable condition as root.
/// Never consumes authorization, acquires a Session identity or starts an Agent.
/// # Errors
/// Refuses untrusted policy, unreadable history, passing/broken controls or expired inspection.
#[cfg(target_os = "linux")]
pub fn prepare(
    paths: &LauncherPaths,
    config: &LauncherConfig,
    request: &LaunchRequest,
    now_ms: u64,
    deadline: Instant,
) -> Result<Preparation, CertificationError> {
    if !rustix::process::geteuid().is_root() || config.conformance != Enforcement::Enforced {
        return Err(CertificationError::Invalid);
    }
    request
        .validate()
        .map_err(|_| CertificationError::Invalid)?;
    let started = Instant::now();
    let host = measure(paths, config, deadline)?;
    let status = CertificateStore::inspect(&paths.state_root.join("conformance"), &host)?;
    let Admission::Waivable(condition) = admission::evaluate(
        &status,
        &host,
        &admission::Request {
            session_id: &request.session_id,
            attendance: Attendance::Interactive,
            waiver: None,
        },
    ) else {
        return Err(CertificationError::Invalid);
    };
    if Instant::now() >= deadline {
        return Err(CertificationError::Invalid);
    }
    let value = Preparation {
        schema: "louiselm.conformance-preparation/1".into(),
        request_digest: request.digest().to_string(),
        session_id: request.session_id.clone(),
        operator_uid: config.operator_uid,
        policy_digest: Digest::of(
            &serde_json::to_vec(config).map_err(|_| CertificationError::Invalid)?,
        )
        .to_string(),
        boot_id: host.boot_id,
        condition,
        observed_at_ms: now_ms
            .saturating_add(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)),
    };
    let root = paths.state_root.join("pre-admission");
    super::installed::storage::trusted_directory(&paths.state_root, 0)?;
    match fs::DirBuilder::new().mode(0o711).create(&root) {
        Ok(()) => (),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(error) => return Err(error.into()),
    }
    super::installed::storage::trusted_directory(&root, 0)?;
    let mut file = tempfile::NamedTempFile::new_in(&root)?;
    file.write_all(&serde_json::to_vec(&value).map_err(|_| CertificationError::Invalid)?)?;
    file.as_file()
        .set_permissions(fs::Permissions::from_mode(0o444))?;
    file.as_file().sync_all()?;
    file.persist(root.join(format!("{}.json", request.session_id)))
        .map_err(|error| error.error)?;
    File::open(&root)?.sync_all()?;
    File::open(&paths.state_root)?.sync_all()?;
    Ok(value)
}

pub(super) fn uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if [8, 13, 18, 23].contains(&index) {
                byte == b'-'
            } else {
                byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
            }
        })
}
