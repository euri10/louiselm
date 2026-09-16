//! Broker-owned request identity, durable outcomes and recoverable projection intent.

use super::attention::{
    AttentionCondition, AttentionReason, AttentionSubject, Outbox, ProjectionChange, RunLifecycle,
    RunState,
};
use super::{BrokerError, corrupt, lock, read_record, sync_directory, write_new_record};
use crate::{
    Digest,
    skill_request::{
        ApprovedSkillRequests, SkillRequest, SkillRequestOutcome, SkillRequestStatus, SkillSubject,
    },
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Binding {
    pub session_id: String,
    pub run_id: String,
    pub envelope_revision: u64,
    pub controller_uid: u32,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Record {
    pub binding: Binding,
    pub request: SkillRequest,
    pub operation_id: String,
    pub created_at_ms: u64,
}

impl Record {
    pub(super) fn subject(&self) -> AttentionSubject {
        match self.request.subject {
            SkillSubject::Session => AttentionSubject::Session(self.binding.session_id.clone()),
            SkillSubject::Run => AttentionSubject::Run(self.binding.run_id.clone()),
        }
    }

    fn condition(&self) -> AttentionCondition {
        AttentionCondition {
            subject: self.subject(),
            operation_id: self.operation_id.clone(),
            created_at_ms: self.created_at_ms,
            reason: AttentionReason::SkillApprovalPending,
        }
    }

    fn valid(&self) -> bool {
        self.request.valid()
            && self.created_at_ms > 0
            && self.binding.controller_uid != 0
            && super::is_record_identifier(&self.binding.session_id)
            && super::attention::canonical_uuid(&self.operation_id)
            && (self.request.subject != SkillSubject::Run
                || super::attention::canonical_uuid(&self.binding.run_id))
    }
}

pub(super) struct SkillRequests {
    root: PathBuf,
    writing: Mutex<()>,
}

impl SkillRequests {
    pub(super) fn open(root: &Path) -> Result<Self, BrokerError> {
        for directory in ["requests", "outcomes", "ended", "runs", "admissions"] {
            fs::create_dir_all(root.join(directory)).map_err(BrokerError::Storage)?;
        }
        sync_directory(root)?;
        if let Some(parent) = root.parent() {
            sync_directory(parent)?;
        }
        Ok(Self {
            root: root.into(),
            writing: Mutex::new(()),
        })
    }

    pub(super) fn accept(
        &self,
        binding: &Binding,
        request: &SkillRequest,
        permission: Option<&ApprovedSkillRequests>,
        now_ms: u64,
        outbox: &Outbox,
    ) -> Result<SkillRequestStatus, BrokerError> {
        if !permission.is_some_and(|approval| approval.permits(request, now_ms)) {
            return Err(BrokerError::InvalidGrant);
        }
        let _guard = lock(&self.writing);
        let path = self.request_path(&binding.session_id, &request.request_id);
        if let Some(record) = read_record::<Record>(&path)? {
            if !record.valid() || record.binding != *binding || record.request != *request {
                return Err(BrokerError::RequestMismatch);
            }
            sync_file(&path)?;
            self.project(&record, outbox)?;
            return self.result(&record);
        }
        let record = Record {
            binding: binding.clone(),
            request: request.clone(),
            operation_id: operation_uuid()?,
            created_at_ms: now_ms,
        };
        if !record.valid() || self.ended(&record.subject())? || self.records()?.len() >= 4096 {
            return Err(BrokerError::InvalidGrant);
        }
        if request.subject == SkillSubject::Run {
            let observed: RunLifecycle =
                read_record(&self.run_path(&binding.run_id))?.ok_or(BrokerError::InvalidGrant)?;
            if observed.run_id != binding.run_id
                || observed.revision == 0
                || observed.state == RunState::Disposed
            {
                return Err(BrokerError::InvalidGrant);
            }
        }
        write_new_record(&path, &record)?;
        self.project(&record, outbox)?;
        self.result(&record)
    }

    pub(super) fn finish(
        &self,
        operator_uid: u32,
        operation_id: &str,
        outcome: SkillRequestOutcome,
        outbox: &Outbox,
    ) -> Result<SkillRequestStatus, BrokerError> {
        let _guard = lock(&self.writing);
        let record = self.record(operation_id)?;
        if operator_uid != record.binding.controller_uid
            || !matches!(
                outcome,
                SkillRequestOutcome::Rejected | SkillRequestOutcome::Cancelled
            )
        {
            return Err(BrokerError::ControllerMismatch);
        }
        self.finish_record(&record, outcome)?;
        self.project(&record, outbox)?;
        self.result(&record)
    }

    pub(super) fn status(&self, operation_id: &str) -> Result<SkillRequestStatus, BrokerError> {
        let _guard = lock(&self.writing);
        self.result(&self.record(operation_id)?)
    }

    pub(super) fn inspect(
        &self,
        operator_uid: u32,
        operation_id: &str,
    ) -> Result<SkillRequestStatus, BrokerError> {
        let _guard = lock(&self.writing);
        let record = self.record(operation_id)?;
        if operator_uid != record.binding.controller_uid {
            return Err(BrokerError::ControllerMismatch);
        }
        self.result(&record)
    }

    pub(super) fn contains(
        &self,
        binding: &Binding,
        request: &SkillRequest,
    ) -> Result<bool, BrokerError> {
        let _guard = lock(&self.writing);
        match read_record::<Record>(&self.request_path(&binding.session_id, &request.request_id))? {
            Some(prior)
                if prior.valid() && prior.binding == *binding && prior.request == *request =>
            {
                Ok(true)
            }
            Some(_) => Err(BrokerError::RequestMismatch),
            None => Ok(false),
        }
    }

    pub(super) fn records(&self) -> Result<Vec<Record>, BrokerError> {
        let entries = fs::read_dir(self.root.join("requests"))
            .map_err(BrokerError::Storage)?
            .take(4097)
            .collect::<Result<Vec<_>, _>>()
            .map_err(BrokerError::Storage)?;
        if entries.len() > 4096 {
            return Err(corrupt("skill request bound exceeded"));
        }
        entries
            .into_iter()
            .map(|entry| {
                let record: Record =
                    read_record(&entry.path())?.ok_or(BrokerError::InvalidGrant)?;
                if !record.valid()
                    || entry.path()
                        != self.request_path(&record.binding.session_id, &record.request.request_id)
                {
                    return Err(corrupt("skill request binding changed"));
                }
                Ok(record)
            })
            .collect()
    }

    fn record(&self, operation_id: &str) -> Result<Record, BrokerError> {
        self.records()?
            .into_iter()
            .find(|record| record.operation_id == operation_id)
            .ok_or(BrokerError::InvalidGrant)
    }

    fn result(&self, record: &Record) -> Result<SkillRequestStatus, BrokerError> {
        let path = self.outcome_path(&record.operation_id);
        let outcome = match read_record(&path)? {
            Some(SkillRequestOutcome::Pending) => return Err(BrokerError::InvalidGrant),
            Some(outcome) => {
                sync_file(&path)?;
                outcome
            }
            None => SkillRequestOutcome::Pending,
        };
        Ok(SkillRequestStatus {
            request_id: record.request.request_id.clone(),
            operation_id: record.operation_id.clone(),
            outcome,
            packages: record.request.packages.clone(),
            agents: record.request.agents.clone(),
            admission: if outcome == SkillRequestOutcome::Approved {
                let path = self.root.join("admissions").join(&record.operation_id);
                let digest: String = read_record(&path)?.ok_or(BrokerError::InvalidGrant)?;
                if Digest::parse(&digest).is_err() {
                    return Err(BrokerError::InvalidGrant);
                }
                sync_file(&path)?;
                Some(digest)
            } else {
                None
            },
        })
    }

    pub(super) fn resolve(
        &self,
        record: &Record,
        source: &super::admission_source::AdmissionSource,
        outbox: &Outbox,
    ) -> Result<(), BrokerError> {
        let _guard = lock(&self.writing);
        if self.result(record)?.outcome != SkillRequestOutcome::Pending {
            return self.project(record, outbox);
        }
        if self.ended(&record.subject())? {
            self.finish_record(record, SkillRequestOutcome::Cancelled)?;
        } else if let Some(generation) = source.verify(&record.operation_id, &record.request)? {
            let path = self.root.join("admissions").join(&record.operation_id);
            match read_record::<String>(&path)? {
                Some(prior) if prior == generation => sync_file(&path)?,
                Some(_) => return Err(BrokerError::RequestMismatch),
                None => write_new_record(&path, &generation)?,
            }
            self.finish_record(record, SkillRequestOutcome::Approved)?;
        }
        self.project(record, outbox)
    }

    fn finish_record(
        &self,
        record: &Record,
        outcome: SkillRequestOutcome,
    ) -> Result<(), BrokerError> {
        let path = self.outcome_path(&record.operation_id);
        if let Some(prior) = read_record::<SkillRequestOutcome>(&path)? {
            if prior != outcome {
                return Err(BrokerError::RequestMismatch);
            }
            return sync_file(&path);
        }
        write_new_record(&path, &outcome)
    }

    fn project(&self, record: &Record, outbox: &Outbox) -> Result<(), BrokerError> {
        // Immutable intent precedes either projection; replay always preserves ordering.
        outbox.enqueue(
            &format!("skill-pending-{}", record.operation_id),
            ProjectionChange::Upsert(record.condition()),
        )?;
        if self.result(record)?.outcome != SkillRequestOutcome::Pending {
            outbox.enqueue(
                &format!("skill-finished-{}", record.operation_id),
                ProjectionChange::Clear(record.condition()),
            )?;
        }
        Ok(())
    }

    pub(super) fn end_subject(
        &self,
        subject: &AttentionSubject,
        outbox: &Outbox,
    ) -> Result<(), BrokerError> {
        let _guard = lock(&self.writing);
        self.end_locked(subject, outbox)
    }

    pub(super) fn terminal_receipt<T>(
        &self,
        subject: &AttentionSubject,
        outbox: &Outbox,
        append: impl FnOnce() -> Result<T, BrokerError>,
    ) -> Result<T, BrokerError> {
        // Receipt publication and request cancellation share approval's lock.
        // A verified terminal receipt can never become durable between the
        // approval end-check and its terminal outcome publication.
        let _guard = lock(&self.writing);
        let acknowledgement = append()?;
        self.end_locked(subject, outbox)?;
        Ok(acknowledgement)
    }

    fn end_locked(&self, subject: &AttentionSubject, outbox: &Outbox) -> Result<(), BrokerError> {
        let path = self.ended_path(subject);
        match read_record::<AttentionSubject>(&path)? {
            Some(prior) if prior == *subject => sync_file(&path)?,
            Some(_) => return Err(BrokerError::InvalidGrant),
            None => write_new_record(&path, subject)?,
        }
        for record in self
            .records()?
            .iter()
            .filter(|record| record.subject() == *subject)
        {
            if self.result(record)?.outcome == SkillRequestOutcome::Pending {
                self.finish_record(record, SkillRequestOutcome::Cancelled)?;
            }
            self.project(record, outbox)?;
        }
        Ok(())
    }

    fn ended(&self, subject: &AttentionSubject) -> Result<bool, BrokerError> {
        match read_record::<AttentionSubject>(&self.ended_path(subject))? {
            Some(prior) if prior == *subject => Ok(true),
            Some(_) => Err(BrokerError::InvalidGrant),
            None => Ok(false),
        }
    }

    pub(super) fn reconcile(&self, outbox: &Outbox) -> Result<(), BrokerError> {
        let _guard = lock(&self.writing);
        for record in self.records()? {
            if let AttentionSubject::Run(id) = record.subject() {
                let observed: Option<RunLifecycle> = read_record(&self.run_path(&id))?;
                if observed.is_some_and(|fact| {
                    fact.run_id == id && fact.revision > 0 && fact.state == RunState::Disposed
                }) {
                    self.end_locked(&AttentionSubject::Run(id), outbox)?;
                }
            }
            if self.ended(&record.subject())?
                && self.result(&record)?.outcome == SkillRequestOutcome::Pending
            {
                self.finish_record(&record, SkillRequestOutcome::Cancelled)?;
            }
            self.project(&record, outbox)?;
        }
        Ok(())
    }

    pub(super) fn observe_run(
        &self,
        observed: &RunLifecycle,
        outbox: &Outbox,
    ) -> Result<(), BrokerError> {
        if !super::attention::canonical_uuid(&observed.run_id) || observed.revision == 0 {
            return Err(BrokerError::InvalidGrant);
        }
        let _guard = lock(&self.writing);
        let path = self.run_path(&observed.run_id);
        if let Some(prior) = read_record::<RunLifecycle>(&path)?
            && (prior.run_id != observed.run_id
                || observed.revision < prior.revision
                || (observed.revision == prior.revision && *observed != prior)
                || (prior.state == RunState::Disposed && observed.state != RunState::Disposed))
        {
            return Err(BrokerError::RequestMismatch);
        }
        let mut temporary = tempfile::NamedTempFile::new_in(self.root.join("runs"))
            .map_err(BrokerError::Storage)?;
        serde_json::to_writer(temporary.as_file_mut(), observed)
            .map_err(|_| BrokerError::InvalidGrant)?;
        temporary
            .as_file()
            .sync_all()
            .map_err(BrokerError::Storage)?;
        temporary
            .persist(&path)
            .map_err(|error| BrokerError::Storage(error.error))?;
        sync_directory(&self.root.join("runs"))?;
        if observed.state == RunState::Disposed {
            self.end_locked(&AttentionSubject::Run(observed.run_id.clone()), outbox)?;
        }
        Ok(())
    }

    fn request_path(&self, session: &str, request: &str) -> PathBuf {
        self.root.join("requests").join(format!(
            "{}.json",
            Digest::of(format!("{session}\0{request}").as_bytes()).hex()
        ))
    }
    fn outcome_path(&self, operation: &str) -> PathBuf {
        self.root.join("outcomes").join(format!("{operation}.json"))
    }
    fn run_path(&self, run: &str) -> PathBuf {
        self.root.join("runs").join(format!("{run}.json"))
    }
    fn ended_path(&self, subject: &AttentionSubject) -> PathBuf {
        let identity = match subject {
            AttentionSubject::Session(id) => format!("session:{id}"),
            AttentionSubject::Run(id) => format!("run:{id}"),
        };
        self.root
            .join("ended")
            .join(format!("{}.json", Digest::of(identity.as_bytes()).hex()))
    }
}

fn sync_file(path: &Path) -> Result<(), BrokerError> {
    fs::File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(BrokerError::Storage)?;
    sync_directory(path.parent().ok_or(BrokerError::InvalidGrant)?)
}

fn operation_uuid() -> Result<String, BrokerError> {
    let mut bytes = [0_u8; 16];
    let mut offset = 0;
    while offset < bytes.len() {
        let count =
            rustix::rand::getrandom(&mut bytes[offset..], rustix::rand::GetRandomFlags::empty())
                .map_err(|error| BrokerError::Storage(error.into()))?;
        if count == 0 {
            return Err(BrokerError::InvalidGrant);
        }
        offset += count;
    }
    bytes[6] = (bytes[6] & 15) | 64;
    bytes[8] = (bytes[8] & 63) | 128;
    let hex = format!("{:032x}", u128::from_be_bytes(bytes));
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
}

#[cfg(test)]
#[path = "skill_request_tests.rs"]
mod tests;
