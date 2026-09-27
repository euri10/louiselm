//! Durable operator approval that bounds every Session in one Run.

use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::Mutex,
};

use serde::{Deserialize, Serialize};

use super::{
    ApprovedCommands, BrokerError, GrantRequest, lock, read_record, record_name, sync_directory,
    write_new_record,
};
use crate::{
    Digest,
    beads_mutation::{ApprovedBeadsMutations, BeadsRole},
    launch_protocol::VerificationRequest,
    provider_request::ApprovedProviderRequests,
};

/// Exact broker-owned authority approved by the operator before child launches.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunEnvelope {
    /// Versioned closed schema.
    pub schema: String,
    /// One durable Run identity.
    pub run_id: String,
    /// Exact registered envelope named in every child launch.
    pub envelope_id: String,
    /// Monotonic policy revision; child grants must name the current one.
    pub envelope_revision: u64,
    /// Installed operator UID that approved this Run.
    pub controller_uid: u32,
    /// Exact Beads project, issue IDs and effects available to children.
    pub bead_scope: ApprovedBeadsMutations,
    /// Upper bound on Provider access and the single shared Run request total.
    pub provider_requests: ApprovedProviderRequests,
    /// One exact privileged command scope, when the Run needs it.
    pub commands: Option<ApprovedCommands>,
    /// Digest of the fixed verification plan selected at Run start.
    pub verification_plan_digest: String,
    /// Maximum number of distinct Session launches across this Run.
    pub max_sessions: u32,
    /// Exclusive expiry of the whole Run approval.
    pub expires_at_ms: u64,
}

impl RunEnvelope {
    /// Canonical bytes used for the receipt digest.
    /// # Panics
    /// Derived JSON-native fields have no fallible serializer.
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "Derived JSON-native fields have no fallible serializer."
    )]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("Run envelope is serializable")
    }

    fn validate(&self, now_ms: u64) -> Result<(), BrokerError> {
        if self.schema != "louiselm.broker.run-envelope/1"
            || record_name(&self.run_id).is_err()
            || record_name(&self.envelope_id).is_err()
            || self.envelope_revision == 0
            || self.controller_uid == 0
            || !(1..=32).contains(&self.max_sessions)
            || now_ms >= self.expires_at_ms
            || !self.bead_scope.valid(now_ms)
            || self.bead_scope.expires_at_ms > self.expires_at_ms
            || !self.provider_requests.valid(now_ms)
            || self.provider_requests.expires_at_ms > self.expires_at_ms
            || self.commands.as_ref().is_some_and(|commands| {
                commands.expires_at_ms > self.expires_at_ms
                    || commands.policy(&self.envelope_id, now_ms).is_err()
            })
            || !Digest::parse(&self.verification_plan_digest)
                .is_ok_and(|digest| digest.to_string() == self.verification_plan_digest)
        {
            return Err(BrokerError::InvalidGrant);
        }
        Ok(())
    }

    fn permits_child(&self, grant: &GrantRequest, now_ms: u64) -> Result<(), BrokerError> {
        self.validate(now_ms)?;
        let request = &grant.request;
        if request.run_id != self.run_id
            || request.envelope_id != self.envelope_id
            || request.envelope_revision != self.envelope_revision
            || grant.controller_uid != self.controller_uid
            || grant.expires_at_ms > self.expires_at_ms
            || grant.dependencies.is_some()
            || grant.skill_requests.is_some()
        {
            return Err(BrokerError::InvalidGrant);
        }
        match (&self.commands, &grant.commands) {
            (_, None) => {}
            (Some(parent), Some(child))
                if child.command_digest == parent.command_digest
                    && child.timeout_ms <= parent.timeout_ms
                    && child.expires_at_ms <= parent.expires_at_ms
                    && (!child.allow_delegation || parent.allow_delegation)
                    && match (parent.uses, child.uses) {
                        (None, _) => true,
                        (Some(limit), Some(uses)) => uses <= limit,
                        (Some(_), None) => false,
                    } => {}
            _ => return Err(BrokerError::InvalidGrant),
        }
        if grant.beads_mutations.as_ref().is_some_and(|child| {
            !child.valid(now_ms)
                || child.project_digest != self.bead_scope.project_digest
                || child.role == BeadsRole::Coordinator
                    && self.bead_scope.role != BeadsRole::Coordinator
                || child.max_mutations > self.bead_scope.max_mutations
                || child.expires_at_ms > self.bead_scope.expires_at_ms
                || !child
                    .issue_ids
                    .iter()
                    .all(|id| self.bead_scope.issue_ids.contains(id))
                || !child
                    .effects
                    .iter()
                    .all(|effect| self.bead_scope.effects.contains(effect))
        }) {
            return Err(BrokerError::InvalidGrant);
        }
        if let Some(child) = &grant.provider_requests {
            let parent = &self.provider_requests;
            if !child.valid(now_ms)
                || child.disclosure_profile != parent.disclosure_profile
                || child.provider != parent.provider
                || child.upstream != parent.upstream
                || child.max_run_requests != parent.max_run_requests
                || child.max_effort > parent.max_effort
                || child.expires_at_ms > parent.expires_at_ms
                || !child
                    .addresses
                    .iter()
                    .all(|address| parent.addresses.contains(address))
                || !child
                    .models
                    .iter()
                    .all(|model| parent.models.contains(model))
            {
                return Err(BrokerError::InvalidGrant);
            }
        }
        Ok(())
    }
}

/// Canonical receipt for an exact durable Run approval.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunAuthorization {
    /// Versioned response schema.
    pub schema: String,
    /// Approved Run identity.
    pub run_id: String,
    /// Current revision.
    pub envelope_revision: u64,
    /// Digest of the complete exact approved envelope.
    pub envelope_digest: String,
}

/// Exact child authorization returned to the trusted controller.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildAuthorization {
    /// Versioned response schema.
    pub schema: String,
    /// Approved Run identity.
    pub run_id: String,
    /// Approved Session identity.
    pub session_id: String,
    /// Current operator-approved Run policy digest.
    pub envelope_digest: String,
    /// Exact distinct Session launch request digest.
    pub request_digest: String,
    /// Single-use launch authorization identity.
    pub authorization_id: String,
    /// Current Run policy revision.
    pub envelope_revision: u64,
}

/// Private append-only Run approvals; revisions never rewrite prior authority.
#[derive(Debug)]
pub struct RunEnvelopeStore {
    root: PathBuf,
    serial: Mutex<()>,
}

impl RunEnvelopeStore {
    /// Opens the broker-private approval directory.
    /// # Errors
    /// Refuses unavailable storage.
    pub fn open(root: &Path) -> Result<Self, BrokerError> {
        fs::create_dir_all(root).map_err(BrokerError::Storage)?;
        Ok(Self {
            root: root.to_owned(),
            serial: Mutex::new(()),
        })
    }

    /// Stores one operator-approved revision before returning its exact digest.
    /// # Errors
    /// Refuses invalid, duplicate, skipped or stale approvals and storage failure.
    pub fn approve(
        &self,
        envelope: &RunEnvelope,
        now_ms: u64,
    ) -> Result<RunAuthorization, BrokerError> {
        self.approve_with(envelope, now_ms, || Ok(()))
    }

    /// Approves only while the caller's Run-lifecycle guard holds this store lock.
    /// # Errors
    /// Refuses invalid or stale policy and propagates the lifecycle guard failure.
    pub fn approve_with(
        &self,
        envelope: &RunEnvelope,
        now_ms: u64,
        guard: impl FnOnce() -> Result<(), BrokerError>,
    ) -> Result<RunAuthorization, BrokerError> {
        envelope.validate(now_ms)?;
        let _serial = lock(&self.serial);
        guard()?;
        let current = self.current(&envelope.run_id)?;
        let expected = current.as_ref().map_or(Ok(1), |prior| {
            prior
                .envelope_revision
                .checked_add(1)
                .ok_or(BrokerError::InvalidGrant)
        })?;
        if envelope.envelope_revision != expected {
            return Err(BrokerError::InvalidGrant);
        }
        let directory = self.root.join(&envelope.run_id);
        fs::create_dir_all(&directory).map_err(BrokerError::Storage)?;
        sync_directory(&self.root)?;
        write_new_record(
            &directory.join(format!("{}.json", envelope.envelope_revision)),
            envelope,
        )?;
        Ok(RunAuthorization {
            schema: "louiselm.broker.run-authorization/1".into(),
            run_id: envelope.run_id.clone(),
            envelope_revision: envelope.envelope_revision,
            envelope_digest: Digest::of(&envelope.canonical_bytes()).to_string(),
        })
    }

    /// Checks one child against the current durable approval.
    /// # Errors
    /// Refuses absent, stale, expired or widened authority and corrupt storage.
    pub fn check_child(
        &self,
        grant: &GrantRequest,
        now_ms: u64,
    ) -> Result<RunEnvelope, BrokerError> {
        self.with_child(grant, now_ms, |current| Ok(current.clone()))
    }

    /// Holds the revision lock while a bounded child authorization is made durable.
    /// # Errors
    /// Refuses absent or widened authority, or propagates the authorization failure.
    pub fn with_child<T>(
        &self,
        grant: &GrantRequest,
        now_ms: u64,
        authorize: impl FnOnce(&RunEnvelope) -> Result<T, BrokerError>,
    ) -> Result<T, BrokerError> {
        let _serial = lock(&self.serial);
        let current = self
            .current(&grant.request.run_id)?
            .ok_or(BrokerError::InvalidGrant)?;
        current.permits_child(grant, now_ms)?;
        authorize(&current)
    }

    /// Checks a separate verification request against this Run's fixed plan.
    /// # Errors
    /// Refuses a changed plan, stale child identity or expired Run.
    pub fn check_verification(
        &self,
        request: &VerificationRequest,
        plan_digest: &str,
        now_ms: u64,
    ) -> Result<(), BrokerError> {
        let _serial = lock(&self.serial);
        let current = self
            .current(&request.launch.run_id)?
            .ok_or(BrokerError::InvalidGrant)?;
        current.validate(now_ms)?;
        if request.launch.envelope_id != current.envelope_id
            || request.launch.envelope_revision != current.envelope_revision
            || request.expires_at_ms > current.expires_at_ms
            || plan_digest != current.verification_plan_digest
        {
            return Err(BrokerError::InvalidGrant);
        }
        Ok(())
    }

    fn current(&self, run_id: &str) -> Result<Option<RunEnvelope>, BrokerError> {
        record_name(run_id)?;
        let entries = match fs::read_dir(self.root.join(run_id)) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(BrokerError::Storage(error)),
        };
        let mut latest: Option<RunEnvelope> = None;
        let mut revisions = std::collections::BTreeSet::new();
        for entry in entries {
            let entry = entry.map_err(BrokerError::Storage)?;
            let record: RunEnvelope =
                read_record(&entry.path())?.ok_or(BrokerError::InvalidGrant)?;
            record.validate(0)?;
            if record.run_id != run_id
                || entry.file_name()
                    != std::ffi::OsStr::new(&format!("{}.json", record.envelope_revision))
                || !revisions.insert(record.envelope_revision)
            {
                return Err(BrokerError::InvalidGrant);
            }
            if latest
                .as_ref()
                .is_none_or(|prior| record.envelope_revision > prior.envelope_revision)
            {
                latest = Some(record);
            }
        }
        if latest.as_ref().is_some_and(|record| {
            usize::try_from(record.envelope_revision).ok() != Some(revisions.len())
        }) {
            return Err(BrokerError::InvalidGrant);
        }
        Ok(latest)
    }
}
