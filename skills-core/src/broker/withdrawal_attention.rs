//! Current-supply withdrawal episodes and crash-recoverable projection intent.

use super::{
    BrokerError, BrokerService,
    admission_source::AdmissionSource,
    attention::{
        AttentionCondition, AttentionReason, AttentionSubject, Outbox, ProjectionChange,
        WithdrawalDetails, WithdrawalUrgency, condition_id,
    },
    corrupt, lock, sync_directory,
};
use crate::endorsement::alerts::WithdrawalAlert;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};

const MAX_CONDITIONS: usize = 4096;
// Aggregate state includes active conditions and both sides of a pending diff.
// Keep its bound separate from the broker's 64-KiB single-operation records.
const MAX_STATE_BYTES: u64 = 16 * 1024 * 1024;
const STAGING_PREFIX: &str = ".withdrawal-";

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    operator_uid: u32,
    revision: u64,
    local_layer_observed: bool,
    conditions: Vec<AttentionCondition>,
    pending: Vec<ProjectionChange>,
}

pub(super) struct WithdrawalAttention {
    root: PathBuf,
    // One serialized source observation and state transition, then outbox.
    // Never acquire this lock from outbox delivery or other request stores.
    writing: Mutex<()>,
}

impl BrokerService {
    /// Reconciles protected current supply without requiring a live Session.
    /// Runs on the broker's blocking evidence worker. Failed or unsettled reads
    /// retain existing conditions; only verified absence can resolve them.
    /// # Errors
    /// Returns source verification, bounded-state or durable projection failures.
    pub fn reconcile_supply_withdrawals(
        &self,
        source: &AdmissionSource,
        now_ms: u64,
    ) -> Result<(), BrokerError> {
        let _guard = lock(&self.withdrawal_attention.writing);
        let previous = self.withdrawal_attention.load(source.operator_uid)?;
        let expected: Vec<String> = previous
            .conditions
            .iter()
            .filter_map(condition_key)
            .map(|(_, digest)| digest.to_owned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let (alerts, local_layer_present) =
            source.withdrawal_alerts(&expected, previous.local_layer_observed)?;
        self.withdrawal_attention.observe(
            source.operator_uid,
            &alerts,
            now_ms,
            &self.attention,
            local_layer_present,
        )
    }
}

impl WithdrawalAttention {
    pub(super) fn open(root: &Path) -> Result<Self, BrokerError> {
        fs::create_dir_all(root).map_err(BrokerError::Storage)?;
        sync_directory(root)?;
        sync_directory(root.parent().ok_or(BrokerError::InvalidGrant)?)?;
        Ok(Self {
            root: root.into(),
            writing: Mutex::new(()),
        })
    }

    pub(super) fn replay(&self, outbox: &Outbox) -> Result<(), BrokerError> {
        let _guard = lock(&self.writing);
        let entries = fs::read_dir(&self.root)
            .map_err(BrokerError::Storage)?
            .take(4097)
            .collect::<Result<Vec<_>, _>>()
            .map_err(BrokerError::Storage)?;
        if entries.len() > 4096 {
            return Err(BrokerError::InvalidGrant);
        }
        for entry in entries {
            // A kill before rename leaves only uncommitted staging bytes.
            if entry
                .file_name()
                .as_encoded_bytes()
                .starts_with(STAGING_PREFIX.as_bytes())
            {
                continue;
            }
            let mut record = read(&entry.path())?.ok_or(BrokerError::InvalidGrant)?;
            record.validate()?;
            if entry.path() != self.path(record.operator_uid) {
                return Err(BrokerError::InvalidGrant);
            }
            self.flush(&mut record, outbox)?;
        }
        Ok(())
    }

    // Caller serializes the complete read/observe cycle, not only this write.
    fn observe(
        &self,
        operator_uid: u32,
        alerts: &[WithdrawalAlert],
        now_ms: u64,
        outbox: &Outbox,
        local_layer_present: bool,
    ) -> Result<(), BrokerError> {
        if operator_uid == 0 || now_ms == 0 || alerts.len() > MAX_CONDITIONS {
            return Err(BrokerError::InvalidGrant);
        }
        let mut record = self.load(operator_uid)?;
        self.flush(&mut record, outbox)?;
        let newly_observed_layer = local_layer_present && !record.local_layer_observed;
        record.local_layer_observed |= local_layer_present;
        let revision = record
            .revision
            .checked_add(1)
            .ok_or(BrokerError::InvalidGrant)?;
        let mut desired: BTreeMap<(&str, &str), AttentionCondition> = BTreeMap::new();
        for alert in alerts {
            let details = WithdrawalDetails {
                package_digest: alert.member.clone(),
                withdrawal_digest: alert.withdrawal.clone(),
                endorser: alert.endorser.clone(),
                urgency: alert.urgency,
            };
            let key = (alert.member.as_str(), alert.withdrawal.as_str());
            if let Some(existing) = desired.get_mut(&key) {
                let AttentionReason::EndorsementWithdrawn(previous) = &mut existing.reason else {
                    return Err(BrokerError::InvalidGrant);
                };
                if previous.endorser != details.endorser {
                    return Err(BrokerError::RequestMismatch);
                }
                if details.urgency == WithdrawalUrgency::ReliedOn {
                    previous.urgency = details.urgency;
                }
                continue;
            }
            let previous = record
                .conditions
                .iter()
                .find(|condition| condition_key(condition) == Some(key));
            let condition = AttentionCondition {
                subject: AttentionSubject::Supply(operator_uid.to_string()),
                linked_run_id: None,
                operation_id: previous.map_or_else(
                    || {
                        condition_id(
                            format!(
                                "withdrawal:{operator_uid}:{revision}:{}:{}",
                                alert.member, alert.withdrawal
                            )
                            .as_bytes(),
                        )
                    },
                    |old| old.operation_id.clone(),
                ),
                created_at_ms: previous.map_or(now_ms, |old| old.created_at_ms),
                reason: AttentionReason::EndorsementWithdrawn(details),
            };
            condition.validate()?;
            desired.insert(key, condition);
        }
        for previous in &record.conditions {
            if !desired.contains_key(&condition_key(previous).ok_or(BrokerError::InvalidGrant)?) {
                record
                    .pending
                    .push(ProjectionChange::Clear(previous.clone()));
            }
        }
        for current in desired.values() {
            if !record.conditions.contains(current) {
                record
                    .pending
                    .push(ProjectionChange::Upsert(current.clone()));
            }
        }
        if record.pending.is_empty() {
            if newly_observed_layer {
                self.save(&record)?;
            }
            return Ok(());
        }
        record.revision = revision;
        record.conditions = desired.into_values().collect();
        self.save(&record)?;
        self.flush(&mut record, outbox)
    }

    fn flush(&self, record: &mut Record, outbox: &Outbox) -> Result<(), BrokerError> {
        if record.pending.is_empty() {
            return Ok(());
        }
        for (index, change) in record.pending.iter().enumerate() {
            outbox.enqueue(
                &format!(
                    "withdrawal-{}-{}-{index}",
                    record.operator_uid, record.revision
                ),
                change.clone(),
            )?;
        }
        record.pending.clear();
        self.save(record)
    }

    fn path(&self, operator_uid: u32) -> PathBuf {
        self.root.join(format!("{operator_uid}.json"))
    }

    fn load(&self, operator_uid: u32) -> Result<Record, BrokerError> {
        let record = read(&self.path(operator_uid))?.unwrap_or(Record {
            operator_uid,
            ..Record::default()
        });
        record.validate()?;
        if record.operator_uid != operator_uid {
            return Err(BrokerError::ControllerMismatch);
        }
        Ok(record)
    }

    fn save(&self, record: &Record) -> Result<(), BrokerError> {
        record.validate()?;
        let bytes = serde_json::to_vec(record).map_err(|_| BrokerError::InvalidGrant)?;
        if bytes.len() as u64 > MAX_STATE_BYTES {
            return Err(BrokerError::InvalidGrant);
        }
        let mut file = tempfile::Builder::new()
            .prefix(STAGING_PREFIX)
            .tempfile_in(&self.root)
            .map_err(BrokerError::Storage)?;
        file.write_all(&bytes)
            .and_then(|()| file.as_file().sync_all())
            .map_err(BrokerError::Storage)?;
        file.persist(self.path(record.operator_uid))
            .map_err(|error| BrokerError::Storage(error.error))?;
        sync_directory(&self.root)
    }
}

fn read(path: &Path) -> Result<Option<Record>, BrokerError> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(BrokerError::Storage(error)),
    };
    let metadata = file.metadata().map_err(BrokerError::Storage)?;
    if !metadata.is_file() || metadata.len() > MAX_STATE_BYTES {
        return Err(corrupt("withdrawal state exceeds its bound"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_STATE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(BrokerError::Storage)?;
    if bytes.len() as u64 > MAX_STATE_BYTES {
        return Err(corrupt("withdrawal state exceeds its bound"));
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| corrupt("withdrawal state is malformed"))
}

fn condition_key(condition: &AttentionCondition) -> Option<(&str, &str)> {
    match &condition.reason {
        AttentionReason::EndorsementWithdrawn(details) => {
            Some((&details.package_digest, &details.withdrawal_digest))
        }
        _ => None,
    }
}

impl Record {
    fn validate(&self) -> Result<(), BrokerError> {
        if self.operator_uid == 0
            || self.conditions.len() > MAX_CONDITIONS
            || self.pending.len() > MAX_CONDITIONS * 2
            || (self.revision == 0 && (!self.conditions.is_empty() || !self.pending.is_empty()))
        {
            return Err(BrokerError::InvalidGrant);
        }
        let mut seen = BTreeSet::new();
        let mut operations = BTreeSet::new();
        for condition in &self.conditions {
            self.condition(condition)?;
            if !seen.insert(condition_key(condition)) || !operations.insert(&condition.operation_id)
            {
                return Err(BrokerError::InvalidGrant);
            }
        }
        for change in &self.pending {
            match change {
                ProjectionChange::Upsert(condition) if self.conditions.contains(condition) => {
                    self.condition(condition)?;
                }
                ProjectionChange::Clear(condition)
                    if !self
                        .conditions
                        .iter()
                        .any(|current| current.operation_id == condition.operation_id) =>
                {
                    self.condition(condition)?;
                }
                _ => return Err(BrokerError::InvalidGrant),
            }
        }
        Ok(())
    }

    fn condition(&self, condition: &AttentionCondition) -> Result<(), BrokerError> {
        condition.validate()?;
        if condition.subject != AttentionSubject::Supply(self.operator_uid.to_string())
            || condition_key(condition).is_none()
        {
            return Err(BrokerError::InvalidGrant);
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "withdrawal_attention_tests.rs"]
mod tests;
