//! Durable, typed Attention state for the operator and paired devices.

use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, BufReader, BufWriter, Write},
    path::{Path, PathBuf},
};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

#[path = "attention_projection.rs"]
mod projection;
pub use projection::{BrokerProjection, ProjectionChange, ProjectionResult};

use crate::permissions::set_private_permissions;

const SCHEMA_VERSION: u8 = 1;
const MAX_SESSION_ID_BYTES: usize = 256;
const MAX_STAGE_BYTES: usize = 64;

/// The subject whose condition needs operator attention.
#[derive(Clone, Copy, Debug, Deserialize, Ord, PartialEq, Eq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionSubjectKind {
    /// A live ACP conversation.
    Session,
    /// A durable workflow Run.
    Run,
}

/// The closed v1 Attention taxonomy.
#[derive(Clone, Copy, Debug, Deserialize, Ord, PartialEq, Eq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionKind {
    /// An Agent turn completed and awaits inspection.
    TurnReady,
    /// An explicit ACP permission request awaits the operator.
    PermissionRequired,
    /// A Run is waiting for an operator decision after exhaustion.
    RunParked,
    /// A Session ended with an error while work remains to inspect.
    SessionFailed,
    /// A Skill candidate awaits the local admission ceremony.
    SkillApprovalPending,
    /// Trusted posture evidence does not verify Skill supply.
    SkillUnverified,
}

impl AttentionKind {
    /// Return the fixed, non-Agent-authored display label for this kind.
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::TurnReady => "Agent turn is ready",
            Self::PermissionRequired => "Permission is required",
            Self::RunParked => "Run is Parked",
            Self::SessionFailed => "Session failed",
            Self::SkillApprovalPending => "Skill approval is pending",
            Self::SkillUnverified => "Skill supply is unverified",
        }
    }
}

/// Closed, non-text detail carried by Skill-related Attention conditions.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionCode {
    /// A candidate requires the local Skill Admission ceremony.
    AdmissionRequired,
    /// The trusted release is absent or not root-owned.
    RootTrustFailed,
    /// Trusted bytes do not have a valid signature.
    SignatureInvalid,
    /// A signed Skill Generation is not remotely witnessed.
    WitnessMissing,
    /// Provider-native instruction sources are not completely controlled.
    NativeSupplyUncertain,
    /// Runtime bytes do not match their registered measurement.
    RuntimeDrift,
    /// Isolation evidence is absent, contradictory, or failed.
    IsolationFailed,
    /// The authenticated local control broker is unavailable.
    BrokerUnavailable,
    /// Durable audit persistence is unavailable.
    AuditPersistenceUnavailable,
    /// Provider disclosure is absent or incomplete.
    ProviderDisclosureMissing,
    /// Required trusted evidence is absent.
    EvidenceMissing,
    /// A failure has no recognized typed diagnosis.
    UnknownFailure,
}

/// Identity of one retry-safe Attention condition.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AttentionKey {
    /// Kind of subject carrying the condition.
    pub subject_kind: AttentionSubjectKind,
    /// Session identifier or canonical Run UUID.
    pub subject_id: String,
    /// Typed condition.
    pub kind: AttentionKind,
    /// Stable UUID for the source lifecycle operation.
    pub source_operation_id: String,
}

/// Validated operator-authored Attention data before persistence.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AttentionDraft {
    /// Kind of subject carrying the condition.
    pub subject_kind: AttentionSubjectKind,
    /// Session identifier or canonical Run UUID.
    pub subject_id: String,
    /// Typed condition.
    pub kind: AttentionKind,
    /// Stable UUID for the source lifecycle operation.
    pub source_operation_id: String,
    /// Unix epoch milliseconds when the condition became unresolved.
    pub created_at_ms: u64,
    /// Optional linked Run for a Session condition.
    pub linked_run_id: Option<String>,
    /// Optional bounded workflow stage name.
    pub stage: Option<String>,
    /// Optional closed detail code; required for Skill conditions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<AttentionCode>,
}

impl AttentionDraft {
    fn key(&self) -> AttentionKey {
        AttentionKey {
            subject_kind: self.subject_kind,
            subject_id: self.subject_id.clone(),
            kind: self.kind,
            source_operation_id: self.source_operation_id.clone(),
        }
    }
}

/// One unresolved, deterministic Attention card.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AttentionItem {
    /// Kind of subject carrying the condition.
    pub subject_kind: AttentionSubjectKind,
    /// Session identifier or canonical Run UUID.
    pub subject_id: String,
    /// Typed condition.
    pub kind: AttentionKind,
    /// Stable UUID for the source lifecycle operation.
    pub source_operation_id: String,
    /// Unix epoch milliseconds when the condition became unresolved.
    pub created_at_ms: u64,
    /// Whether delivery may proceed for this item.
    pub eligible: bool,
    /// Fixed display label derived from `kind`.
    pub reason: String,
    /// Optional linked Run for a Session condition.
    pub linked_run_id: Option<String>,
    /// Optional bounded workflow stage name.
    pub stage: Option<String>,
    /// Optional closed detail code; required for Skill conditions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<AttentionCode>,
}

impl AttentionItem {
    fn key(&self) -> AttentionKey {
        AttentionKey {
            subject_kind: self.subject_kind,
            subject_id: self.subject_id.clone(),
            kind: self.kind,
            source_operation_id: self.source_operation_id.clone(),
        }
    }
}

/// Authenticated snapshot exposed to local and paired-device readers.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AttentionSnapshot {
    /// Monotonic change generation, including changes after the inbox empties.
    pub generation: u64,
    /// Current unresolved items in deterministic order.
    pub items: Vec<AttentionItem>,
}

/// Concise machine-readable Attention health.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AttentionSummary {
    /// Monotonic change generation.
    pub generation: u64,
    /// Number of unresolved items.
    pub unresolved_count: usize,
    /// Unresolved count by fixed kind.
    pub kinds: BTreeMap<AttentionKind, usize>,
}

/// Durable Attention-store failure.
#[derive(Debug, Error)]
pub enum AttentionError {
    /// Input or persisted state violates the closed Attention contract.
    #[error("invalid Attention state: {0}")]
    Invalid(String),
    /// Attention filesystem operation failed.
    #[error("Attention storage failed: {0}")]
    Io(#[from] io::Error),
    /// Persisted Attention JSON is malformed.
    #[error("Attention data is malformed: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PersistedAttention {
    schema_version: u8,
    generation: u64,
    items: Vec<AttentionItem>,
    #[serde(default)]
    broker_projection: Option<projection::ProjectionCursor>,
}

/// Filesystem-backed owner of unresolved Attention state.
#[derive(Clone, Debug)]
pub struct AttentionStore {
    root: PathBuf,
}

impl AttentionStore {
    /// Open or initialize the private Attention directory.
    ///
    /// # Errors
    ///
    /// Returns filesystem or malformed-state errors.
    pub fn new(root: impl AsRef<Path>) -> Result<Self, AttentionError> {
        fs::create_dir_all(root.as_ref())?;
        set_private_permissions(root.as_ref(), true)?;
        let store = Self {
            root: root.as_ref().to_path_buf(),
        };
        store.with_lock(|store| {
            reject_symlink(&store.state_path(), "Attention state")?;
            if store.state_path().exists() {
                store.load()
            } else {
                let state = PersistedAttention {
                    schema_version: SCHEMA_VERSION,
                    generation: 0,
                    items: Vec::new(),
                    broker_projection: None,
                };
                store.persist(&state).map(|()| state)
            }
        })?;
        Ok(store)
    }

    /// Read the current unresolved snapshot.
    ///
    /// # Errors
    ///
    /// Returns filesystem or malformed-state errors.
    pub fn snapshot(&self) -> Result<AttentionSnapshot, AttentionError> {
        self.with_lock(|store| store.load().map(snapshot))
    }

    /// Summarize the current snapshot without exposing storage internals.
    ///
    /// # Errors
    ///
    /// Returns filesystem or malformed-state errors.
    pub fn summary(&self) -> Result<AttentionSummary, AttentionError> {
        let snapshot = self.snapshot()?;
        let mut kinds = BTreeMap::new();
        for item in &snapshot.items {
            *kinds.entry(item.kind).or_insert(0) += 1;
        }
        Ok(AttentionSummary {
            generation: snapshot.generation,
            unresolved_count: snapshot.items.len(),
            kinds,
        })
    }

    /// Add or update one condition; repeating identical data is a no-op.
    ///
    /// # Errors
    ///
    /// Rejects unbounded or contradictory fields before changing state.
    pub fn upsert(&self, draft: AttentionDraft) -> Result<AttentionSnapshot, AttentionError> {
        validate_draft(&draft)?;
        self.with_lock(|store| {
            let mut state = store.load()?;
            let item = AttentionItem {
                subject_kind: draft.subject_kind,
                subject_id: draft.subject_id,
                kind: draft.kind,
                source_operation_id: draft.source_operation_id,
                created_at_ms: draft.created_at_ms,
                eligible: false,
                reason: draft.kind.reason().to_owned(),
                linked_run_id: draft.linked_run_id,
                stage: draft.stage,
                code: draft.code,
            };
            let key = item.key();
            let changed = match state.items.iter_mut().find(|current| current.key() == key) {
                Some(current) if same_content(current, &item) => false,
                Some(current) => {
                    let eligible = current.eligible;
                    *current = item;
                    current.eligible = eligible;
                    true
                }
                None => {
                    state.items.push(item);
                    true
                }
            };
            if changed {
                advance_generation(&mut state)?;
                sort_items(&mut state.items);
                store.persist(&state)?;
            }
            Ok(snapshot(state))
        })
    }

    /// Set delivery eligibility for one condition.
    ///
    /// # Errors
    ///
    /// Rejects an invalid key and reports missing conditions explicitly.
    pub fn set_eligible(
        &self,
        key: &AttentionKey,
        eligible: bool,
    ) -> Result<AttentionSnapshot, AttentionError> {
        validate_key(key)?;
        self.with_lock(|store| {
            let mut state = store.load()?;
            let item = state
                .items
                .iter_mut()
                .find(|item| item.key() == *key)
                .ok_or_else(|| {
                    AttentionError::Invalid("Attention item was not found".to_owned())
                })?;
            if item.eligible != eligible {
                item.eligible = eligible;
                advance_generation(&mut state)?;
                store.persist(&state)?;
            }
            Ok(snapshot(state))
        })
    }

    /// Clear one condition; repeating a clear is a no-op.
    ///
    /// # Errors
    ///
    /// Rejects an invalid key and reports persistence failures.
    pub fn clear(&self, key: &AttentionKey) -> Result<AttentionSnapshot, AttentionError> {
        validate_key(key)?;
        self.with_lock(|store| {
            let mut state = store.load()?;
            let previous = state.items.len();
            state.items.retain(|item| item.key() != *key);
            if state.items.len() != previous {
                advance_generation(&mut state)?;
                store.persist(&state)?;
            }
            Ok(snapshot(state))
        })
    }

    /// Clear one condition kind for a Session; repeating a clear is a no-op.
    ///
    /// # Errors
    ///
    /// Rejects an invalid Session identifier and reports persistence failures.
    pub fn clear_session_kind(
        &self,
        session_id: &str,
        kind: AttentionKind,
    ) -> Result<AttentionSnapshot, AttentionError> {
        validate_subject_id(session_id)?;
        self.with_lock(|store| {
            let mut state = store.load()?;
            let previous = state.items.len();
            state.items.retain(|item| {
                item.subject_kind != AttentionSubjectKind::Session
                    || item.subject_id != session_id
                    || item.kind != kind
            });
            if state.items.len() != previous {
                advance_generation(&mut state)?;
                store.persist(&state)?;
            }
            Ok(snapshot(state))
        })
    }

    /// Clear every condition belonging to one Session; repeating a clear is a no-op.
    ///
    /// # Errors
    ///
    /// Rejects an invalid Session identifier and reports persistence failures.
    pub fn clear_session(&self, session_id: &str) -> Result<AttentionSnapshot, AttentionError> {
        validate_subject_id(session_id)?;
        self.with_lock(|store| {
            let mut state = store.load()?;
            let previous = state.items.len();
            state.items.retain(|item| {
                item.subject_kind != AttentionSubjectKind::Session || item.subject_id != session_id
            });
            if state.items.len() != previous {
                advance_generation(&mut state)?;
                store.persist(&state)?;
            }
            Ok(snapshot(state))
        })
    }

    fn state_path(&self) -> PathBuf {
        self.root.join("attention.json")
    }

    fn lock_path(&self) -> PathBuf {
        self.root.join("attention.lock")
    }

    fn with_lock<T>(
        &self,
        operation: impl FnOnce(&Self) -> Result<T, AttentionError>,
    ) -> Result<T, AttentionError> {
        reject_symlink(&self.lock_path(), "Attention lock")?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.lock_path())?;
        set_private_permissions(&self.lock_path(), false)?;
        lock.lock_exclusive()?;
        let result = operation(self);
        FileExt::unlock(&lock)?;
        result
    }

    fn load(&self) -> Result<PersistedAttention, AttentionError> {
        let state: PersistedAttention =
            serde_json::from_reader(BufReader::new(File::open(self.state_path())?))?;
        validate_state(&state)?;
        Ok(state)
    }

    fn persist(&self, state: &PersistedAttention) -> Result<(), AttentionError> {
        let temporary = self.root.join(format!(".attention-{}.tmp", Uuid::new_v4()));
        let result = (|| {
            let file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)?;
            set_private_permissions(&temporary, false)?;
            let mut writer = BufWriter::new(file);
            serde_json::to_writer_pretty(&mut writer, state)?;
            writer.write_all(b"\n")?;
            writer.flush()?;
            writer.get_ref().sync_all()?;
            fs::rename(&temporary, self.state_path())?;
            File::open(&self.root)?.sync_all()?;
            Ok(())
        })();
        if temporary.exists() {
            let _ = fs::remove_file(temporary);
        }
        result
    }
}

fn snapshot(state: PersistedAttention) -> AttentionSnapshot {
    AttentionSnapshot {
        generation: state.generation,
        items: state.items,
    }
}

fn validate_state(state: &PersistedAttention) -> Result<(), AttentionError> {
    if let Some(cursor) = &state.broker_projection {
        cursor.validate()?;
    }
    if state.schema_version != SCHEMA_VERSION {
        return Err(AttentionError::Invalid(
            "Attention schema version is unsupported".to_owned(),
        ));
    }
    for item in &state.items {
        validate_item(item)?;
    }
    let mut sorted = state.items.clone();
    sort_items(&mut sorted);
    if sorted
        .windows(2)
        .any(|items| items[0].key() == items[1].key())
    {
        return Err(AttentionError::Invalid(
            "Attention state contains duplicate keys".to_owned(),
        ));
    }
    if sorted != state.items {
        return Err(AttentionError::Invalid(
            "Attention items are not deterministically ordered".to_owned(),
        ));
    }
    Ok(())
}

fn reject_symlink(path: &Path, label: &str) -> Result<(), AttentionError> {
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(AttentionError::Invalid(format!("{label} is a symlink")));
    }
    Ok(())
}

fn validate_draft(draft: &AttentionDraft) -> Result<(), AttentionError> {
    validate_key(&draft.key())?;
    if draft.created_at_ms == 0 {
        return Err(AttentionError::Invalid(
            "created_at_ms must be positive".to_owned(),
        ));
    }
    validate_linked_run(draft.linked_run_id.as_deref())?;
    validate_stage(draft.stage.as_deref())?;
    validate_code(draft.kind, draft.code, draft.stage.as_deref())
}

fn validate_item(item: &AttentionItem) -> Result<(), AttentionError> {
    validate_key(&item.key())?;
    if item.created_at_ms == 0 {
        return Err(AttentionError::Invalid(
            "created_at_ms must be positive".to_owned(),
        ));
    }
    if item.reason != item.kind.reason() {
        return Err(AttentionError::Invalid(
            "Attention reason does not match kind".to_owned(),
        ));
    }
    validate_linked_run(item.linked_run_id.as_deref())?;
    validate_stage(item.stage.as_deref())?;
    validate_code(item.kind, item.code, item.stage.as_deref())
}

fn validate_code(
    kind: AttentionKind,
    code: Option<AttentionCode>,
    stage: Option<&str>,
) -> Result<(), AttentionError> {
    let valid = match kind {
        AttentionKind::SkillApprovalPending => code == Some(AttentionCode::AdmissionRequired),
        AttentionKind::SkillUnverified => {
            code.is_some_and(|value| value != AttentionCode::AdmissionRequired)
        }
        _ => code.is_none(),
    };
    if !valid {
        return Err(AttentionError::Invalid(
            "Attention code does not match kind".to_owned(),
        ));
    }
    if matches!(
        kind,
        AttentionKind::SkillApprovalPending | AttentionKind::SkillUnverified
    ) && stage.is_some()
    {
        return Err(AttentionError::Invalid(
            "Skill Attention conditions cannot carry stage text".to_owned(),
        ));
    }
    Ok(())
}

fn validate_key(key: &AttentionKey) -> Result<(), AttentionError> {
    validate_subject_id(&key.subject_id)?;
    if key.subject_kind == AttentionSubjectKind::Run {
        validate_uuid(&key.subject_id, "Run subject_id")?;
    }
    validate_uuid(&key.source_operation_id, "source_operation_id")
}

fn validate_subject_id(subject_id: &str) -> Result<(), AttentionError> {
    if subject_id.is_empty()
        || subject_id.len() > MAX_SESSION_ID_BYTES
        || subject_id
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
    {
        return Err(AttentionError::Invalid(
            "subject_id is empty, too long, or contains whitespace".to_owned(),
        ));
    }
    Ok(())
}

fn validate_linked_run(value: Option<&str>) -> Result<(), AttentionError> {
    if let Some(value) = value {
        validate_uuid(value, "linked_run_id")?;
    }
    Ok(())
}

fn validate_stage(value: Option<&str>) -> Result<(), AttentionError> {
    let Some(value) = value else { return Ok(()) };
    if value.is_empty()
        || value.len() > MAX_STAGE_BYTES
        || value
            .bytes()
            .any(|byte| !(byte.is_ascii_alphanumeric() || b"._/-".contains(&byte)))
    {
        return Err(AttentionError::Invalid(
            "stage is empty, too long, or contains unsupported characters".to_owned(),
        ));
    }
    Ok(())
}

fn validate_uuid(value: &str, field: &str) -> Result<(), AttentionError> {
    let parsed = Uuid::parse_str(value)
        .map_err(|_| AttentionError::Invalid(format!("{field} must be a UUID")))?;
    if parsed.to_string() != value {
        return Err(AttentionError::Invalid(format!(
            "{field} must use canonical UUID text"
        )));
    }
    Ok(())
}

fn sort_items(items: &mut [AttentionItem]) {
    items.sort_by(|left, right| {
        left.created_at_ms
            .cmp(&right.created_at_ms)
            .then_with(|| left.subject_kind.cmp(&right.subject_kind))
            .then_with(|| left.subject_id.cmp(&right.subject_id))
            .then_with(|| left.kind.cmp(&right.kind))
            .then_with(|| left.source_operation_id.cmp(&right.source_operation_id))
    });
}

fn same_content(left: &AttentionItem, right: &AttentionItem) -> bool {
    left.subject_kind == right.subject_kind
        && left.subject_id == right.subject_id
        && left.kind == right.kind
        && left.source_operation_id == right.source_operation_id
        && left.created_at_ms == right.created_at_ms
        && left.reason == right.reason
        && left.linked_run_id == right.linked_run_id
        && left.stage == right.stage
        && left.code == right.code
}

fn advance_generation(state: &mut PersistedAttention) -> Result<(), AttentionError> {
    state.generation = state
        .generation
        .checked_add(1)
        .ok_or_else(|| AttentionError::Invalid("Attention generation overflowed".to_owned()))?;
    Ok(())
}
