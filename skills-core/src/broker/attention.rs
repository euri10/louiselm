//! Durable, ordered Attention projection; delivery never grants authority.

use super::{
    BrokerError, corrupt, is_record_identifier, lock, read_record, sync_directory, write_new_record,
};

#[path = "attention_transport.rs"]
mod transport;
use crate::{Digest, posture::FailureCode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
};
pub use transport::{AttentionEndpoint, RunLifecycle, RunState};

/// Trusted subject of one condition.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "id",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum AttentionSubject {
    /// One broker-bound Session.
    Session(String),
    /// One admitted Run, named by its canonical UUID.
    Run(String),
}

impl AttentionSubject {
    fn parts(&self) -> (&str, &str) {
        match self {
            Self::Session(id) => ("session", id),
            Self::Run(id) => ("run", id),
        }
    }

    fn validate(&self) -> Result<(), BrokerError> {
        let (kind, id) = self.parts();
        if id.is_empty()
            || id.len() > 256
            || id.chars().any(|c| c.is_whitespace() || c.is_control())
            || (kind == "run" && !canonical_uuid(id))
        {
            return Err(BrokerError::InvalidGrant);
        }
        Ok(())
    }
}

/// Closed condition reasons, rendered by capture-service rather than the Agent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "code",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum AttentionReason {
    /// A required broker effect lacks explicit operator-granted capability.
    PermissionRequired,
    /// A Run awaits an operator decision.
    RunParked,
    /// A Session ended abnormally or recovery failed.
    SessionFailed,
    /// Exact packaged bytes await an authorized admission ceremony.
    SkillApprovalPending,
    /// One trusted posture dimension failed.
    SkillUnverified(FailureCode),
}

impl AttentionReason {
    fn fields(&self) -> (&str, Option<&str>) {
        match self {
            Self::PermissionRequired => ("permission_required", None),
            Self::RunParked => ("run_parked", None),
            Self::SessionFailed => ("session_failed", None),
            Self::SkillApprovalPending => ("skill_approval_pending", Some("admission_required")),
            Self::SkillUnverified(code) => ("skill_unverified", Some(code.name())),
        }
    }
}

/// Bounded condition data produced by trusted lifecycle/skill policy.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttentionCondition {
    /// Authenticated owning Session or Run.
    pub subject: AttentionSubject,
    /// Stable canonical UUID for this condition's lifetime.
    pub operation_id: String,
    /// First unresolved time in epoch milliseconds.
    pub created_at_ms: u64,
    /// Fixed typed reason; contains no user-authored message.
    pub reason: AttentionReason,
}

impl AttentionCondition {
    fn key(&self) -> Value {
        let (subject_kind, subject_id) = self.subject.parts();
        json!({"subject_kind": subject_kind, "subject_id": subject_id,
            "kind": self.reason.fields().0, "source_operation_id": self.operation_id})
    }

    fn validate(&self) -> Result<(), BrokerError> {
        self.subject.validate()?;
        if !canonical_uuid(&self.operation_id) || self.created_at_ms == 0 {
            return Err(BrokerError::InvalidGrant);
        }
        Ok(())
    }
}

/// One normalized transition; a later failure uses a fresh condition UUID.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ProjectionChange {
    /// Make a condition unresolved.
    Upsert(AttentionCondition),
    /// Resolve its exact original key.
    Clear(AttentionCondition),
    /// Subject termination clears its remaining conditions.
    ClearSubject(AttentionSubject),
}

impl ProjectionChange {
    fn validate(&self) -> Result<(), BrokerError> {
        match self {
            Self::Upsert(condition) | Self::Clear(condition) => condition.validate(),
            Self::ClearSubject(subject) => subject.validate(),
        }
    }

    fn wire(&self) -> Value {
        match self {
            Self::Upsert(condition) => {
                let mut attention = condition.key();
                attention["created_at_ms"] = json!(condition.created_at_ms);
                attention["linked_run_id"] = Value::Null;
                attention["stage"] = Value::Null;
                if let Some(code) = condition.reason.fields().1 {
                    attention["code"] = json!(code);
                }
                json!({"type": "upsert", "attention": attention})
            }
            Self::Clear(condition) => json!({"type": "clear", "key": condition.key()}),
            Self::ClearSubject(subject) => {
                let (kind, id) = subject.parts();
                json!({"type": "clear_subject", "subject_kind": kind, "subject_id": id})
            }
        }
    }
}

/// Exact durable outbox entry, including enqueue idempotency identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Projection {
    /// Monotonic sequence, starting at one.
    pub sequence: u64,
    /// Broker-owned source mutation identity; not an Agent-selected payload.
    pub mutation_id: String,
    /// Typed projection, never a canonical authorization or receipt.
    pub change: ProjectionChange,
}

impl Projection {
    /// Canonical projection object accepted by capture-service.
    #[must_use]
    pub fn wire(&self) -> Value {
        json!({"sequence": self.sequence, "change": self.change.wire()})
    }

    /// SHA-256 of sorted-key canonical projection JSON, without a prefix.
    #[must_use]
    pub fn digest(&self) -> String {
        Digest::of(self.wire().to_string().as_bytes())
            .hex()
            .to_owned()
    }
}

/// Explicitly owned broker outbox with crash-durable append and ACK markers.
/// Records are retained, bounded at 4096 entries; exhaustion refuses new entries.
/// All methods perform blocking disk I/O on the broker worker.
pub struct Outbox {
    root: PathBuf,
    writing: Mutex<()>,
}

impl Outbox {
    /// Publishes the oldest pending entry directly to capture-service.
    /// This blocks the broker's delivery worker while the asynchronous transport
    /// completes. Failure retains the entry and changes no authorization state.
    ///
    /// # Errors
    /// Returns transport/authentication failure or unavailable outbox persistence.
    pub fn deliver_next(&self, endpoint: &AttentionEndpoint) -> Result<bool, BrokerError> {
        let Some(entry) = self.next()? else {
            return Ok(false);
        };
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let worker = endpoint.publish(
            entry.clone(),
            Box::new(move |result| {
                let _delivered = sender.send(result);
            }),
        )?;
        worker.join().map_err(|_| {
            BrokerError::Attention(std::io::Error::other("projection worker failed"))
        })?;
        receiver.recv().map_err(|_| {
            BrokerError::Attention(std::io::Error::other("projection result unavailable"))
        })??;
        self.acknowledge(entry.sequence, &entry.digest())?;
        Ok(true)
    }
    /// Opens the dedicated broker-owned outbox directory.
    /// # Errors
    /// Returns unavailable durable storage.
    pub fn open(root: &Path) -> Result<Self, BrokerError> {
        for child in ["entries", "acks"] {
            fs::create_dir_all(root.join(child)).map_err(BrokerError::Storage)?;
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

    /// Durably enqueues once; retrying an identity requires exactly the same change.
    /// # Errors
    /// Refuses invalid input, conflicting retry, exhausted bounds or unavailable storage.
    pub fn enqueue(
        &self,
        mutation_id: &str,
        change: ProjectionChange,
    ) -> Result<Projection, BrokerError> {
        if !is_record_identifier(mutation_id) {
            return Err(BrokerError::InvalidGrant);
        }
        change.validate()?;
        let _guard = lock(&self.writing);
        let entries = self.entries()?;
        if let Some(existing) = entries
            .iter()
            .find(|entry| entry.mutation_id == mutation_id)
        {
            if existing.change != change {
                return Err(BrokerError::InvalidGrant);
            }
            self.sync_entry(existing.sequence)?;
            return Ok(existing.clone());
        }
        if entries.len() >= 4096 {
            return Err(corrupt("Attention outbox exceeds its bound"));
        }
        let sequence = u64::try_from(entries.len()).map_err(|_| BrokerError::InvalidGrant)? + 1;
        let entry = Projection {
            sequence,
            mutation_id: mutation_id.into(),
            change,
        };
        write_new_record(&self.path("entries", sequence), &entry)?;
        Ok(entry)
    }

    /// Returns the oldest unacknowledged projection, retaining it on delivery failure.
    /// # Errors
    /// Returns corrupt or unavailable outbox state.
    pub fn next(&self) -> Result<Option<Projection>, BrokerError> {
        let _guard = lock(&self.writing);
        self.pending()
    }

    /// Records only the exact oldest pending acknowledgement.
    /// # Errors
    /// Rejects out-of-order or substituted ACKs and unavailable durable storage.
    pub fn acknowledge(&self, sequence: u64, digest: &str) -> Result<(), BrokerError> {
        let _guard = lock(&self.writing);
        let pending = self.pending()?.ok_or(BrokerError::InvalidGrant)?;
        if pending.sequence != sequence || pending.digest() != digest {
            return Err(BrokerError::InvalidGrant);
        }
        write_new_record(&self.path("acks", sequence), &digest)
    }

    fn pending(&self) -> Result<Option<Projection>, BrokerError> {
        for entry in self.entries()? {
            let ack: Option<String> = read_record(&self.path("acks", entry.sequence))?;
            match ack {
                None => return Ok(Some(entry)),
                Some(digest) if digest == entry.digest() => {}
                Some(_) => return Err(corrupt("Attention acknowledgement changed")),
            }
        }
        Ok(None)
    }

    fn entries(&self) -> Result<Vec<Projection>, BrokerError> {
        let count = fs::read_dir(self.root.join("entries"))
            .map_err(BrokerError::Storage)?
            .take(4097)
            .collect::<Result<Vec<_>, _>>()
            .map_err(BrokerError::Storage)?
            .len();
        if count > 4096 {
            return Err(corrupt("Attention outbox exceeds its bound"));
        }
        (1..=u64::try_from(count).map_err(|_| BrokerError::InvalidGrant)?)
            .map(|sequence| {
                let entry: Projection = read_record(&self.path("entries", sequence))?
                    .ok_or_else(|| corrupt("Attention outbox has a gap"))?;
                entry.change.validate()?;
                if entry.sequence != sequence || !is_record_identifier(&entry.mutation_id) {
                    return Err(corrupt("Attention outbox entry changed"));
                }
                Ok(entry)
            })
            .collect()
    }

    fn sync_entry(&self, sequence: u64) -> Result<(), BrokerError> {
        fs::File::open(self.path("entries", sequence))
            .and_then(|file| file.sync_all())
            .map_err(BrokerError::Storage)?;
        sync_directory(&self.root.join("entries"))
    }

    fn path(&self, directory: &str, sequence: u64) -> PathBuf {
        self.root
            .join(directory)
            .join(format!("{sequence:020}.json"))
    }
}

pub(crate) fn canonical_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if [8, 13, 18, 23].contains(&index) {
                byte == b'-'
            } else {
                byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
            }
        })
}
