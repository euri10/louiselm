//! Explicit installed read-only Admission evidence authority, never caller-selected.

use super::BrokerError;
use crate::{Digest, Policy, Store, admission, quarantine, skill_request::SkillRequest};
use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

/// How far the operator's skill quarantine reaches one pinned Generation.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum QuarantineReach {
    /// Nothing excluded touches this Generation.
    Unaffected {
        /// Digest of the quarantine bytes checked, when one exists.
        quarantine: Option<Digest>,
    },
    /// An excluded package is a member, or everything is excluded.
    Affected {
        /// Digest of the exact quarantine bytes that reached this Generation.
        quarantine: Digest,
    },
}

/// Protected source selected by the administrator, not an Agent or CLI request.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AdmissionSource {
    /// Existing trusted store containing packages, public trust and signed records.
    pub store: PathBuf,
    /// Enrolled trust domain expected in every accepted signature.
    pub trust_domain: String,
    /// Installed operator who owns and writes the store.
    pub operator_uid: u32,
    /// Dedicated reader identity; must differ from the operator and root.
    pub broker_uid: u32,
}

impl AdmissionSource {
    /// Reads fixed root-owned configuration. Absence disables linked Admission.
    /// # Errors
    /// Refuses malformed or writable configuration; does not create a store.
    pub fn installed() -> io::Result<Option<Self>> {
        let bytes = match super::attention_config::read_protected(Path::new(
            "/etc/louiselm-broker-admission.json",
        )) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let source: Self = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        if !source.store.is_absolute()
            || source.trust_domain.is_empty()
            || source.trust_domain.len() > 256
            || source.operator_uid == 0
            || source.broker_uid == 0
            || source.operator_uid == source.broker_uid
            || source
                .store
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
        {
            return Err(io::Error::other("invalid Admission source configuration"));
        }
        Ok(Some(source))
    }

    pub(super) fn verify(
        &self,
        operation: &str,
        request: &SkillRequest,
    ) -> Result<Option<String>, BrokerError> {
        let store = self.trusted_store()?;
        self.check(&self.store.join("packages"), true)?;
        for package in &request.packages {
            let digest = crate::Digest::parse(package).map_err(|_| BrokerError::InvalidGrant)?;
            let mut remaining = 65536;
            self.check_tree(
                &self.store.join("packages").join(digest.directory_name()),
                &mut remaining,
            )?;
        }
        admission::verify_linked(
            &store,
            &Policy::embedded(),
            &self.trust_domain,
            operation,
            &request.packages,
            &request.agents,
        )
        .map_err(BrokerError::AdmissionEvidence)
    }

    /// Whether the operator's skill quarantine reaches a Session pinned to
    /// `generation`.
    ///
    /// Reads only operator- or root-owned evidence, like Admission. A missing
    /// quarantine reaches nothing. When `known_clear` names the exact quarantine
    /// bytes already found not to reach this Generation, the Generation is not
    /// re-verified.
    ///
    /// # Errors
    /// Refuses unsafe ownership or modes, an unreadable, oversized, malformed or
    /// unknown-schema quarantine, and an unverifiable Generation. Callers must
    /// not read an error as "unaffected".
    pub(super) fn skill_quarantine(
        &self,
        generation: &str,
        known_clear: Option<&Digest>,
    ) -> Result<QuarantineReach, BrokerError> {
        for ancestor in self.store.ancestors() {
            self.check(ancestor, true)?;
        }
        let path = self.store.join("quarantine.json");
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(QuarantineReach::Unaffected { quarantine: None });
            }
            Err(error) => return Err(BrokerError::Storage(error)),
            Ok(_) => self.check(&path, false)?,
        }
        let bytes = super::read_bounded(&path)?.ok_or(BrokerError::InvalidGrant)?;
        let digest = Digest::of(&bytes);
        if known_clear == Some(&digest) {
            return Ok(QuarantineReach::Unaffected {
                quarantine: Some(digest),
            });
        }
        let quarantine: quarantine::Quarantine =
            serde_json::from_slice(&bytes).map_err(|_| BrokerError::InvalidGrant)?;
        if quarantine.schema != quarantine::QUARANTINE_SCHEMA {
            return Err(BrokerError::InvalidGrant);
        }
        if quarantine
            .excluded_generations
            .iter()
            .any(|excluded| excluded == generation)
        {
            return Ok(QuarantineReach::Affected { quarantine: digest });
        }
        if quarantine.excluded.is_empty() {
            return Ok(QuarantineReach::Unaffected {
                quarantine: Some(digest),
            });
        }
        let store = self.trusted_store()?;
        let parsed_generation = Digest::parse(generation).map_err(|_| BrokerError::InvalidGrant)?;
        let members =
            admission::linked_generation_members(&store, &self.trust_domain, &parsed_generation)
                .map_err(BrokerError::AdmissionEvidence)?;
        Ok(
            if quarantine::partition(Some(&quarantine), generation, &members)
                .1
                .is_empty()
            {
                QuarantineReach::Unaffected {
                    quarantine: Some(digest),
                }
            } else {
                QuarantineReach::Affected { quarantine: digest }
            },
        )
    }

    /// Checks the trusted evidence tree and opens the store read-only.
    fn trusted_store(&self) -> Result<Store, BrokerError> {
        // Trusted writers may replace evidence atomically; no untrusted identity,
        // including the broker, may write any ancestor or evidence inode.
        for ancestor in self.store.ancestors() {
            self.check(ancestor, true)?;
        }
        for directory in ["trust", "generations"] {
            let mut remaining = 65536;
            self.check_tree(&self.store.join(directory), &mut remaining)?;
        }
        self.check(&self.store.join("provenance.json"), false)?;
        let store = Store::open_existing(&self.store).map_err(admission::AdmissionError::from)?;
        let provenance = store
            .existing_provenance()
            .map_err(admission::AdmissionError::from)?;
        if !provenance.trusted || provenance.created_by_release.is_none() {
            return Err(BrokerError::InvalidGrant);
        }
        Ok(store)
    }

    fn check(&self, path: &Path, directory: bool) -> Result<(), BrokerError> {
        let metadata = fs::symlink_metadata(path).map_err(BrokerError::Storage)?;
        if metadata.is_dir() != directory
            || (!directory && (!metadata.is_file() || metadata.nlink() != 1))
            || ![0, self.operator_uid].contains(&metadata.uid())
            || metadata.uid() == self.broker_uid
            || metadata.mode() & 0o022 != 0
        {
            return Err(BrokerError::InvalidGrant);
        }
        Ok(())
    }

    fn check_tree(&self, path: &Path, remaining: &mut usize) -> Result<(), BrokerError> {
        *remaining = remaining.checked_sub(1).ok_or(BrokerError::InvalidGrant)?;
        let metadata = fs::symlink_metadata(path).map_err(BrokerError::Storage)?;
        self.check(path, metadata.is_dir())?;
        if metadata.is_dir() {
            for entry in fs::read_dir(path).map_err(BrokerError::Storage)? {
                self.check_tree(&entry.map_err(BrokerError::Storage)?.path(), remaining)?;
            }
        }
        Ok(())
    }
}
