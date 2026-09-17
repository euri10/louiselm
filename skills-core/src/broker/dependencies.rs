//! Durable dependency candidates, atomic approval batches and at-most-once fetches.

use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
};

use super::{
    BrokerError, PendingAuthorization, lock, read_record, record_name, sync_directory,
    write_new_record,
};
use crate::{
    Digest,
    dependency_fetch::{
        Attendance, Candidate, Decision, DependencyPolicy, DependencyRequest, DependencySession,
        DependencyStatus, FetchPermit, StartingLockfile,
    },
};

const MAX_REQUESTS: usize = 4096;

#[cfg(test)]
#[path = "dependencies_tests.rs"]
mod tests;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Record {
    authorization_id: String,
    pub(super) request: DependencyRequest,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Approval {
    authorization_id: String,
    candidate_ids: Vec<String>,
}

pub(super) enum Admission {
    Status(DependencyStatus),
    Fetch {
        permit: FetchPermit,
        record: Box<Record>,
    },
}

pub(super) struct Dependencies {
    root: PathBuf,
    writing: Mutex<()>,
}

impl Dependencies {
    pub(super) fn open(root: &Path) -> Result<Self, BrokerError> {
        fs::create_dir_all(root).map_err(BrokerError::Storage)?;
        sync_directory(root)?;
        Ok(Self {
            root: root.into(),
            writing: Mutex::new(()),
        })
    }

    fn directory(&self, auth: &PendingAuthorization) -> Result<PathBuf, BrokerError> {
        let name = record_name(&auth.session_id)?;
        let directory = self.root.join(name);
        fs::create_dir_all(&directory).map_err(BrokerError::Storage)?;
        sync_directory(&self.root)?;
        Ok(directory)
    }

    fn path(
        &self,
        auth: &PendingAuthorization,
        request: &DependencyRequest,
        suffix: &str,
    ) -> Result<PathBuf, BrokerError> {
        Ok(self.directory(auth)?.join(format!(
            "{}-{suffix}.json",
            Digest::of(request.request_id.as_bytes()).hex()
        )))
    }

    fn records(&self, auth: &PendingAuthorization) -> Result<Vec<Record>, BrokerError> {
        let mut records = Vec::new();
        for entry in fs::read_dir(self.directory(auth)?).map_err(BrokerError::Storage)? {
            let entry = entry.map_err(BrokerError::Storage)?;
            if !entry
                .file_name()
                .to_string_lossy()
                .ends_with("-request.json")
            {
                continue;
            }
            if records.len() >= MAX_REQUESTS {
                return Err(BrokerError::InvalidGrant);
            }
            let record: Record = read_record(&entry.path())?.ok_or(BrokerError::InvalidGrant)?;
            record.request.validate()?;
            if record.authorization_id != auth.authorization_id {
                return Err(BrokerError::RequestMismatch);
            }
            records.push(record);
        }
        Ok(records)
    }

    fn approvals(&self, auth: &PendingAuthorization) -> Result<BTreeSet<String>, BrokerError> {
        let mut ids = BTreeSet::new();
        let mut batches = 0;
        for entry in fs::read_dir(self.directory(auth)?).map_err(BrokerError::Storage)? {
            let entry = entry.map_err(BrokerError::Storage)?;
            if !entry.file_name().to_string_lossy().starts_with("approved-") {
                continue;
            }
            batches += 1;
            if batches > MAX_REQUESTS {
                return Err(BrokerError::InvalidGrant);
            }
            let batch: Approval = read_record(&entry.path())?.ok_or(BrokerError::InvalidGrant)?;
            if batch.authorization_id != auth.authorization_id || batch.candidate_ids.len() > 32 {
                return Err(BrokerError::RequestMismatch);
            }
            ids.extend(batch.candidate_ids);
            if ids.len() > MAX_REQUESTS {
                return Err(BrokerError::InvalidGrant);
            }
        }
        Ok(ids)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "One locked at-most-once transaction binds the request, counts durable reservations, and records its intent before issuing a permit."
    )]
    pub(super) fn admit(
        &self,
        auth: &PendingAuthorization,
        starting: StartingLockfile,
        request: &DependencyRequest,
        now_ms: u64,
    ) -> Result<Admission, BrokerError> {
        let _guard = lock(&self.writing);
        let permission = auth
            .dependencies
            .as_ref()
            .ok_or(BrokerError::InvalidGrant)?;
        permission.validate(now_ms)?;
        request.validate()?;
        if starting.digest() != permission.lockfile_digest {
            return Err(BrokerError::RequestMismatch);
        }
        let path = self.path(auth, request, "request")?;
        let record = match read_record::<Record>(&path)? {
            Some(record)
                if record.authorization_id == auth.authorization_id
                    && record.request == *request =>
            {
                record
            }
            Some(_) => return Err(BrokerError::RequestMismatch),
            None => {
                if self.records(auth)?.len() >= MAX_REQUESTS {
                    return Err(BrokerError::InvalidGrant);
                }
                let record = Record {
                    authorization_id: auth.authorization_id.clone(),
                    request: request.clone(),
                };
                write_new_record(&path, &record)?;
                record
            }
        };
        if let Some(status) =
            read_record::<DependencyStatus>(&self.path(auth, request, "outcome")?)?
        {
            status.validate()?;
            // A previous publication is historical evidence, not proof that
            // mutable Session cache bytes remain intact for a fresh relay.
            return Ok(Admission::Status(match status {
                DependencyStatus::Complete { .. } => DependencyStatus::Unknown,
                status => status,
            }));
        }
        if self.path(auth, request, "intent")?.exists() {
            return Ok(Admission::Status(DependencyStatus::Unknown));
        }
        let records = self.records(auth)?;
        let approvals = self.approvals(auth)?;
        let mut preapproved = permission.preapproved.clone();
        let mut approved_ids: BTreeSet<_> = preapproved
            .iter()
            .map(Candidate::id)
            .collect::<Result<_, _>>()?;
        let mut count = 0_u32;
        let mut reserved = 0_u64;
        for prior in &records {
            let candidate_id = prior.request.candidate.id()?;
            if approvals.contains(&candidate_id) && approved_ids.insert(candidate_id) {
                preapproved.push(prior.request.candidate.clone());
            }
            if self.path(auth, &prior.request, "intent")?.exists() {
                count = count.checked_add(1).ok_or(BrokerError::InvalidGrant)?;
                reserved = reserved
                    .checked_add(prior.request.max_bytes)
                    .ok_or(BrokerError::InvalidGrant)?;
            }
        }
        // Classify candidates even when the attempt budget is exhausted; a
        // bounded temporary policy here cannot mint a permit without the checks below.
        let mut owner = DependencySession::new(
            DependencyPolicy {
                session_id: auth.session_id.clone(),
                run_id: auth.run_id.clone(),
                envelope_revision: auth.envelope_revision,
                attendance: auth.conformance.attendance,
                starting,
                preapproved,
                max_fetches: permission.max_fetches,
                max_bytes: permission.max_bytes,
                expires_at_ms: permission.expires_at_ms,
            },
            now_ms,
        )?;
        match owner.consider(&request.candidate, now_ms)? {
            Decision::Pending { candidate_id } => {
                let proposal = self.path(auth, request, "proposal")?;
                if !proposal.exists() {
                    write_new_record(&proposal, &record)?;
                }
                Ok(Admission::Status(DependencyStatus::Pending {
                    candidate_id,
                }))
            }
            Decision::Denied => Ok(Admission::Status(DependencyStatus::Denied)),
            Decision::Authorized => {
                if count >= permission.max_fetches
                    || reserved
                        .checked_add(request.max_bytes)
                        .is_none_or(|bytes| bytes > permission.max_bytes)
                {
                    return Ok(Admission::Status(DependencyStatus::Denied));
                }
                let permit = owner.begin_fetch(&request.candidate, request.max_bytes, now_ms)?;
                write_new_record(&self.path(auth, request, "intent")?, &record)?;
                Ok(Admission::Fetch {
                    permit,
                    record: Box::new(record),
                })
            }
        }
    }

    pub(super) fn finish(
        &self,
        auth: &PendingAuthorization,
        record: &Record,
        status: &DependencyStatus,
    ) -> Result<(), BrokerError> {
        let _guard = lock(&self.writing);
        status.validate()?;
        if record.authorization_id != auth.authorization_id
            || !self.path(auth, &record.request, "intent")?.exists()
        {
            return Err(BrokerError::RequestMismatch);
        }
        write_new_record(&self.path(auth, &record.request, "outcome")?, status)
    }

    pub(super) fn control(
        &self,
        auth: &PendingAuthorization,
        operator_uid: u32,
        candidate_ids: Option<&[String]>,
        now_ms: u64,
    ) -> Result<super::dependency_service::DependencyInspection, BrokerError> {
        let _guard = lock(&self.writing);
        if auth.controller_uid != operator_uid {
            return Err(BrokerError::ControllerMismatch);
        }
        let permission = auth
            .dependencies
            .as_ref()
            .ok_or(BrokerError::InvalidGrant)?;
        permission.validate(now_ms)?;
        let records = self.records(auth)?;
        let mut approved = self.approvals(auth)?;
        let mut proposals = std::collections::BTreeMap::new();
        for record in &records {
            let id = record.request.candidate.id()?;
            if self.path(auth, &record.request, "proposal")?.exists() {
                proposals.insert(id, record.request.candidate.clone());
            }
        }
        if let Some(ids) = candidate_ids {
            if auth.conformance.attendance != Attendance::Interactive
                || ids.is_empty()
                || ids.len() > 32
                || ids.iter().collect::<BTreeSet<_>>().len() != ids.len()
                || ids.iter().any(|id| !proposals.contains_key(id))
            {
                return Err(BrokerError::InvalidGrant);
            }
            let batch = Approval {
                authorization_id: auth.authorization_id.clone(),
                candidate_ids: ids.to_vec(),
            };
            let digest =
                Digest::of(&serde_json::to_vec(&batch).map_err(|_| BrokerError::InvalidGrant)?);
            let path = self
                .directory(auth)?
                .join(format!("approved-{}.json", digest.hex()));
            if !path.exists() {
                write_new_record(&path, &batch)?;
            }
            // A prior call may have failed after creation but before directory
            // sync. Retrying the exact batch must prove durability too.
            fs::File::open(&path)
                .and_then(|file| file.sync_all())
                .map_err(BrokerError::Storage)?;
            sync_directory(path.parent().ok_or(BrokerError::InvalidGrant)?)?;
            approved.extend(ids.iter().cloned());
        }
        proposals.retain(|id, _| !approved.contains(id));
        let has_more = proposals.len() > 32;
        let pending = proposals
            .into_iter()
            .take(32)
            .map(
                |(candidate_id, candidate)| super::dependency_service::PendingDependency {
                    candidate_id,
                    candidate,
                },
            )
            .collect();
        Ok(super::dependency_service::DependencyInspection {
            session_id: auth.session_id.clone(),
            envelope_revision: auth.envelope_revision,
            pending,
            has_more,
            approved: candidate_ids.unwrap_or_default().to_vec(),
            expires_at_ms: permission.expires_at_ms,
        })
    }
}
