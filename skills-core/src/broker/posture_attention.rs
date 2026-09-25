//! Durable per-Session dimension episodes and recoverable outbox intent.

use super::attention::{
    AttentionCondition, AttentionReason, AttentionSubject, Outbox, ProjectionChange,
};
use super::{BrokerError, lock, read_record, record_name, sync_directory};
use crate::{
    launch_protocol::LaunchAuthorization,
    posture::{DimensionName, DimensionState, FailureCode, Posture},
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Record {
    pub session_id: String,
    pub run_id: String,
    request_digest: String,
    observed_at_ms: u64,
    revision: u64,
    ended: bool,
    conditions: [Option<AttentionCondition>; 6],
    pending: Vec<ProjectionChange>,
}

pub(super) struct PostureAttention {
    root: PathBuf,
    // Lock order: episodes, then outbox. Never call a source or transport under this lock.
    writing: Mutex<()>,
}

impl PostureAttention {
    pub(super) fn open(root: &Path) -> Result<Self, BrokerError> {
        for directory in ["sessions", "ended-runs"] {
            fs::create_dir_all(root.join(directory)).map_err(BrokerError::Storage)?;
        }
        sync_directory(root)?;
        sync_directory(root.parent().ok_or(BrokerError::InvalidGrant)?)?;
        Ok(Self {
            root: root.into(),
            writing: Mutex::new(()),
        })
    }

    pub(super) fn observe(
        &self,
        auth: &LaunchAuthorization,
        posture: &Posture,
        waiting: bool,
        quarantined: bool,
        now: u64,
        outbox: &Outbox,
    ) -> Result<(), BrokerError> {
        if now == 0 || posture.session_id != auth.session_id || posture.run_id != auth.run_id {
            return Err(BrokerError::RequestMismatch);
        }
        let _guard = lock(&self.writing);
        if self.run_ended(&auth.run_id)? {
            return Ok(());
        }
        let prior = self.read(&auth.session_id)?;
        if prior.is_none() && !waiting && !quarantined {
            return Ok(());
        }
        if prior.is_none() && self.records_locked()?.len() >= 4096 {
            return Err(BrokerError::InvalidGrant);
        }
        let mut record = prior.unwrap_or_else(|| Record {
            session_id: auth.session_id.clone(),
            run_id: auth.run_id.clone(),
            request_digest: auth.request_digest.clone(),
            observed_at_ms: 0,
            revision: 0,
            ended: false,
            conditions: std::array::from_fn(|_| None),
            pending: vec![],
        });
        if record.request_digest != auth.request_digest || record.run_id != auth.run_id {
            return Err(BrokerError::RequestMismatch);
        }
        self.flush(&mut record, outbox)?;
        if record.ended {
            return Ok(());
        }
        if now < record.observed_at_ms {
            return Err(BrokerError::InvalidGrant);
        }
        record.observed_at_ms = now;
        let revision = record
            .revision
            .checked_add(1)
            .ok_or(BrokerError::InvalidGrant)?;
        for (index, (name, dimension)) in posture.dimensions.ordered().into_iter().enumerate() {
            // Slot zero carries the one Session-wide quarantine condition.
            // Clear prior per-dimension episodes through the same durable outbox.
            if quarantined && index != 0 {
                if let Some(condition) = record.conditions[index].take() {
                    record.pending.push(ProjectionChange::Clear(condition));
                }
                continue;
            }
            let previous = &record.conditions[index];
            let state = if quarantined {
                DimensionState::Failed
            } else {
                dimension.state
            };
            match state {
                DimensionState::Failed if waiting || quarantined || previous.is_some() => {
                    let reason = AttentionReason::SkillUnverified(if quarantined {
                        FailureCode::Quarantined
                    } else {
                        dimension.failure_code.ok_or(BrokerError::InvalidGrant)?
                    });
                    if previous.as_ref().is_some_and(|old| old.reason == reason) {
                        continue;
                    }
                    let mut condition = previous.clone().unwrap_or_else(|| AttentionCondition {
                        subject: AttentionSubject::Session(auth.session_id.clone()),
                        linked_run_id: super::attention::canonical_uuid(&auth.run_id)
                            .then(|| auth.run_id.clone()),
                        operation_id: super::attention::condition_id(
                            format!("{}:{revision}:{}", auth.request_digest, name.name())
                                .as_bytes(),
                        ),
                        created_at_ms: now,
                        reason: reason.clone(),
                    });
                    condition.reason = reason;
                    record
                        .pending
                        .push(ProjectionChange::Upsert(condition.clone()));
                    record.conditions[index] = Some(condition);
                }
                DimensionState::Verified | DimensionState::Waived => {
                    if let Some(condition) = record.conditions[index].take() {
                        record.pending.push(ProjectionChange::Clear(condition));
                    }
                }
                DimensionState::Failed => (),
            }
        }
        if !record.pending.is_empty() {
            record.revision = revision;
        }
        self.save(&record)?;
        self.flush(&mut record, outbox)
    }

    pub(super) fn end(&self, session: &str, outbox: &Outbox) -> Result<(), BrokerError> {
        let _guard = lock(&self.writing);
        self.end_locked(session, outbox)
    }

    fn end_locked(&self, session: &str, outbox: &Outbox) -> Result<(), BrokerError> {
        let Some(mut record) = self.read(session)? else {
            return Ok(());
        };
        self.flush(&mut record, outbox)?;
        if !record.ended {
            record.ended = true;
            record.revision = record
                .revision
                .checked_add(1)
                .ok_or(BrokerError::InvalidGrant)?;
            record.pending = record
                .conditions
                .iter_mut()
                .filter_map(Option::take)
                .map(ProjectionChange::Clear)
                .collect();
            self.save(&record)?;
        }
        self.flush(&mut record, outbox)
    }

    pub(super) fn reconcile(&self, outbox: &Outbox) -> Result<(), BrokerError> {
        let _guard = lock(&self.writing);
        for mut record in self.records_locked()? {
            if self.run_ended(&record.run_id)? {
                self.end_locked(&record.session_id, outbox)?;
            } else {
                self.flush(&mut record, outbox)?;
            }
        }
        Ok(())
    }

    pub(super) fn records(&self) -> Result<Vec<Record>, BrokerError> {
        let _guard = lock(&self.writing);
        self.records_locked()
    }

    pub(super) fn end_run(&self, run: &str, outbox: &Outbox) -> Result<(), BrokerError> {
        if !super::attention::canonical_uuid(run) {
            return Err(BrokerError::InvalidGrant);
        }
        let _guard = lock(&self.writing);
        if !self.run_ended(run)? {
            super::write_new_record(&self.root.join("ended-runs").join(record_name(run)?), &run)?;
        }
        for record in self
            .records_locked()?
            .into_iter()
            .filter(|record| record.run_id == run)
        {
            self.end_locked(&record.session_id, outbox)?;
        }
        Ok(())
    }

    fn run_ended(&self, run: &str) -> Result<bool, BrokerError> {
        match read_record::<String>(&self.root.join("ended-runs").join(record_name(run)?))? {
            Some(prior) if prior == run => Ok(true),
            Some(_) => Err(BrokerError::RequestMismatch),
            None => Ok(false),
        }
    }

    fn records_locked(&self) -> Result<Vec<Record>, BrokerError> {
        let entries = fs::read_dir(self.root.join("sessions"))
            .map_err(BrokerError::Storage)?
            .take(4097)
            .collect::<Result<Vec<_>, _>>()
            .map_err(BrokerError::Storage)?;
        if entries.len() > 4096 {
            return Err(BrokerError::InvalidGrant);
        }
        entries
            .into_iter()
            .map(|entry| {
                let record: Record =
                    read_record(&entry.path())?.ok_or(BrokerError::InvalidGrant)?;
                record.validate()?;
                if entry.path() != self.path(&record.session_id)? {
                    return Err(BrokerError::InvalidGrant);
                }
                Ok(record)
            })
            .collect()
    }

    fn read(&self, session: &str) -> Result<Option<Record>, BrokerError> {
        let value: Option<Record> = read_record(&self.path(session)?)?;
        if let Some(value) = &value {
            value.validate()?;
            if value.session_id != session {
                return Err(BrokerError::RequestMismatch);
            }
        }
        Ok(value)
    }

    fn flush(&self, record: &mut Record, outbox: &Outbox) -> Result<(), BrokerError> {
        if record.pending.is_empty() {
            return Ok(());
        }
        for (index, change) in record.pending.iter().enumerate() {
            outbox.enqueue(
                &format!(
                    "posture-{}-{}-{index}",
                    crate::Digest::of(record.session_id.as_bytes()).hex(),
                    record.revision
                ),
                change.clone(),
            )?;
        }
        record.pending.clear();
        self.save(record)
    }

    fn path(&self, session: &str) -> Result<PathBuf, BrokerError> {
        Ok(self.root.join("sessions").join(record_name(session)?))
    }

    fn save(&self, record: &Record) -> Result<(), BrokerError> {
        record.validate()?;
        let bytes = serde_json::to_vec(record).map_err(|_| BrokerError::InvalidGrant)?;
        if bytes.len() as u64 > super::MAX_RECORD_BYTES {
            return Err(BrokerError::InvalidGrant);
        }
        let mut file = tempfile::NamedTempFile::new_in(self.root.join("sessions"))
            .map_err(BrokerError::Storage)?;
        file.write_all(&bytes)
            .and_then(|()| file.as_file().sync_all())
            .map_err(BrokerError::Storage)?;
        file.persist(self.path(&record.session_id)?)
            .map_err(|error| BrokerError::Storage(error.error))?;
        sync_directory(&self.root.join("sessions"))
    }
}

impl Record {
    fn validate(&self) -> Result<(), BrokerError> {
        if !super::is_record_identifier(&self.session_id)
            || !super::is_record_identifier(&self.run_id)
            || crate::Digest::parse(&self.request_digest).is_err()
            || self.observed_at_ms == 0
            || self.pending.len() > 6
            || (!self.pending.is_empty() && self.revision == 0)
            || (self.ended && self.conditions.iter().any(Option::is_some))
        {
            return Err(BrokerError::InvalidGrant);
        }
        for (index, condition) in self.conditions.iter().enumerate() {
            if let Some(condition) = condition {
                self.condition(condition)?;
                let AttentionReason::SkillUnverified(code) = condition.reason else {
                    return Err(BrokerError::InvalidGrant);
                };
                if !DimensionName::ALL[index].accepts_failure(code) {
                    return Err(BrokerError::InvalidGrant);
                }
            }
        }
        for change in &self.pending {
            match change {
                ProjectionChange::Upsert(condition) => {
                    self.condition(condition)?;
                    if self.ended
                        || !self
                            .conditions
                            .iter()
                            .flatten()
                            .any(|current| current == condition)
                    {
                        return Err(BrokerError::InvalidGrant);
                    }
                }
                ProjectionChange::Clear(condition) => {
                    self.condition(condition)?;
                }
                ProjectionChange::ClearSubject(_) => return Err(BrokerError::InvalidGrant),
            }
        }
        Ok(())
    }

    fn condition(&self, condition: &AttentionCondition) -> Result<(), BrokerError> {
        condition.validate()?;
        if condition.created_at_ms > self.observed_at_ms {
            return Err(BrokerError::InvalidGrant);
        }
        if condition.subject != AttentionSubject::Session(self.session_id.clone())
            || condition.linked_run_id
                != super::attention::canonical_uuid(&self.run_id).then(|| self.run_id.clone())
            || !matches!(condition.reason, AttentionReason::SkillUnverified(_))
        {
            return Err(BrokerError::RequestMismatch);
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "posture_attention_tests.rs"]
mod tests;
