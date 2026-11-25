//! Payload-free, durable evidence for phone actions.

use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    sync::Mutex,
};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::{MAX_INTEGER, fixed, valid_string};
use crate::{hex::lower_hex, permissions::set_private_permissions};

// ponytail: one bounded snapshot; partition records if 16 MiB becomes limiting.
// Exhaustion refuses new writes; it never evicts unresolved or finished records.
const MAX_STORE_BYTES: u64 = 16 * 1024 * 1024;

/// Signed action identity and the native owner's locally established scope.
///
/// Callers must obtain `project` and `agent` from the exact native owner, never
/// from a phone claim, and validate the request and current authority first.
/// All identifiers and hashes use the phone-control wire's lowercase hex form.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ActionIdentity {
    /// Original, unique action identifier.
    pub operation_id: String,
    /// Receiver-key identity.
    pub receiver_identity: String,
    /// Exact controller incarnation.
    pub controller_id: String,
    /// Exact subject incarnation.
    pub subject_id: String,
    /// Versioned action number; reads 1 and 2 cannot be reserved here.
    pub action: u8,
    /// Digest of the exact request payload, not the payload itself.
    pub payload_hash: String,
    /// Grant used by the original action.
    pub grant_id: String,
    /// Original grant revision.
    pub grant_revision: u64,
    /// Original working-period identifier.
    pub period_id: String,
    /// Original expected native-owner revision.
    pub expected_revision: u64,
    /// Canonical absolute project path established by the owner.
    pub project: String,
    /// Configured Agent name established by the owner.
    pub agent: String,
    /// Computer timestamp when this identity was first reserved.
    pub recorded_at_ms: u64,
}

/// The native owner's last proven outcome; acceptance is not completion.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ActionOutcome {
    /// Delivery or admission is not proven.
    Unknown,
    /// The owner reports acceptance, with an opaque evidence digest.
    Accepted {
        /// Digest identifying the owner's evidence.
        evidence: String,
    },
    /// The owner reports rejection, with an opaque evidence digest.
    Rejected {
        /// Digest identifying the owner's evidence.
        evidence: String,
    },
    /// The owner proved no effect and durably fenced the original attempt.
    FencedNoEffect {
        /// Digest identifying that owner's proof and durable fence.
        evidence: String,
    },
}

/// A retained action identity and the last native-owner report.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ActionEvidence {
    /// Immutable request bindings and locally established scope.
    pub identity: ActionIdentity,
    /// The last proven outcome, never inferred from a missing response.
    pub outcome: ActionOutcome,
    /// Computer timestamp of the retained report, or of the reservation.
    pub updated_at_ms: u64,
}

/// Whether a request acquired a new durable reservation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActionReservation {
    /// The identity and replay guard are durable; a caller may continue admission.
    New,
    /// This identity was already reserved and must not be forwarded again.
    Existing,
}

/// A closed evidence-store refusal.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ActionEvidenceError {
    /// A field or owner report is malformed.
    #[error("malformed action evidence")]
    Malformed,
    /// An existing identity or outcome conflicts with the submitted one.
    #[error("conflicting action evidence")]
    Conflict,
    /// An uncertain action for this exact Session must be reconciled first.
    #[error("an action for this Session is unresolved")]
    Unresolved,
    /// Evidence is absent, damaged, locked, or cannot be durably stored.
    #[error("action evidence unavailable")]
    Unavailable,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    v: u8,
    actions: BTreeMap<String, ActionEvidence>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Seal {
    v: u8,
    bytes: u64,
    sha256: String,
}

struct StoreLock(File);

impl Drop for StoreLock {
    fn drop(&mut self) {
        if let Err(error) = FileExt::unlock(&self.0) {
            eprintln!("action evidence lock release failed: {error}");
        }
    }
}

/// Exclusive, synchronous filesystem owner of durable action evidence.
///
/// Run its methods off asynchronous executor threads. Keep one instance for
/// the service lifetime; its advisory lock is explicitly released on drop.
pub struct ActionEvidenceStore {
    directory: PathBuf,
    _lock: StoreLock,
    failed: Mutex<bool>,
}

impl ActionEvidenceStore {
    /// Open or initialize the private evidence store at `directory`.
    ///
    /// A retained lock file distinguishes a new store from a missing store.
    /// A partial, corrupt, or already-owned store is unavailable, not empty.
    ///
    /// # Errors
    ///
    /// Returns [`ActionEvidenceError::Unavailable`] if storage cannot be
    /// initialized, locked, verified, or durably synchronized.
    pub fn open(directory: impl AsRef<Path>) -> Result<Self, ActionEvidenceError> {
        Self::open_inner(directory.as_ref(), true)
    }

    pub(super) fn open_existing(directory: &Path) -> Result<Self, ActionEvidenceError> {
        Self::open_inner(directory, false)
    }

    fn open_inner(directory: &Path, initialize: bool) -> Result<Self, ActionEvidenceError> {
        let parent = directory.parent().ok_or(ActionEvidenceError::Unavailable)?;
        if initialize {
            fs::create_dir_all(parent).map_err(unavailable)?;
            set_private_permissions(parent, true).map_err(unavailable)?;
        }
        let lock_path = directory.with_extension("lock");
        let (file, created) = if initialize {
            match OpenOptions::new()
                .create_new(true)
                .read(true)
                .write(true)
                .open(&lock_path)
            {
                Ok(file) => (file, true),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (
                    OpenOptions::new()
                        .read(true)
                        .write(true)
                        .open(&lock_path)
                        .map_err(unavailable)?,
                    false,
                ),
                Err(error) => return Err(unavailable(error)),
            }
        } else {
            (
                OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&lock_path)
                    .map_err(unavailable)?,
                false,
            )
        };
        if initialize {
            set_private_permissions(&lock_path, false).map_err(unavailable)?;
        }
        file.try_lock_exclusive().map_err(unavailable)?;
        let lock = StoreLock(file);
        let exists = directory.try_exists().map_err(unavailable)?;
        if created == exists {
            return Err(ActionEvidenceError::Unavailable);
        }
        if created {
            fs::create_dir(directory).map_err(unavailable)?;
            set_private_permissions(directory, true).map_err(unavailable)?;
            File::open(parent)
                .and_then(|file| file.sync_all())
                .map_err(unavailable)?;
            persist(
                directory,
                &Snapshot {
                    v: 1,
                    actions: BTreeMap::new(),
                },
            )?;
        } else {
            load(directory)?;
        }
        Ok(Self {
            directory: directory.to_owned(),
            _lock: lock,
            failed: Mutex::new(false),
        })
    }

    /// Reserve an authenticated action before any forwarding or effect.
    ///
    /// Existing reservations must never be forwarded again. A changed binding
    /// for the same operation is a conflict, even under a different grant.
    /// Callers remain responsible for current authority and native-owner guards.
    ///
    /// # Errors
    ///
    /// Returns a malformed, conflicting, or unavailable refusal.
    pub fn reserve(
        &self,
        identity: &ActionIdentity,
    ) -> Result<ActionReservation, ActionEvidenceError> {
        self.reserve_inner(identity, None)
    }

    /// Reserve a Session prompt only after every unknown action is accounted for.
    ///
    /// The caller must authenticate the request and resolve the subject's scope
    /// through the native owner. `independent` contains only operation IDs the
    /// native owner has proved independent of this exact prompt; a different
    /// controller or subject ID alone does not establish independence. This
    /// guard never expires or removes evidence.
    ///
    /// # Errors
    ///
    /// Returns an unresolved, malformed, conflicting, or unavailable refusal.
    pub fn reserve_session_prompt(
        &self,
        identity: &ActionIdentity,
        independent: &[String],
    ) -> Result<ActionReservation, ActionEvidenceError> {
        if identity.action != 3 {
            return Err(ActionEvidenceError::Malformed);
        }
        for operation in independent {
            fixed::<16>(operation).map_err(|_| ActionEvidenceError::Malformed)?;
        }
        self.reserve_inner(identity, Some(independent))
    }

    fn reserve_inner(
        &self,
        identity: &ActionIdentity,
        independent: Option<&[String]>,
    ) -> Result<ActionReservation, ActionEvidenceError> {
        validate_identity(identity)?;
        self.access(|snapshot| {
            if let Some(existing) = snapshot.actions.get(&identity.operation_id) {
                return if same_identity(&existing.identity, identity) {
                    Ok((ActionReservation::Existing, false))
                } else {
                    Err(ActionEvidenceError::Conflict)
                };
            }
            if independent.is_some_and(|proved| {
                snapshot.actions.values().any(|record| {
                    record.identity.receiver_identity == identity.receiver_identity
                        && record.outcome == ActionOutcome::Unknown
                        && !proved.contains(&record.identity.operation_id)
                })
            }) {
                return Err(ActionEvidenceError::Unresolved);
            }
            let project = fs::canonicalize(&identity.project).map_err(unavailable)?;
            if project != Path::new(&identity.project) || !project.is_dir() {
                return Err(ActionEvidenceError::Malformed);
            }
            snapshot.actions.insert(
                identity.operation_id.clone(),
                ActionEvidence {
                    identity: identity.clone(),
                    outcome: ActionOutcome::Unknown,
                    updated_at_ms: identity.recorded_at_ms,
                },
            );
            Ok((ActionReservation::New, true))
        })
    }

    /// Read evidence for one operation, without interpreting absent data.
    ///
    /// This method does not authorize a phone; its caller must recheck current
    /// pairing, grant, period, and the record's project and Agent scope.
    ///
    /// # Errors
    ///
    /// Returns malformed for an invalid ID and unavailable for missing or
    /// damaged evidence, an unavailable store, or a failed read.
    pub fn lookup(&self, operation_id: &str) -> Result<ActionEvidence, ActionEvidenceError> {
        fixed::<16>(operation_id).map_err(|_| ActionEvidenceError::Malformed)?;
        self.access(|snapshot| {
            snapshot
                .actions
                .get(operation_id)
                .cloned()
                .map(|evidence| (evidence, false))
                .ok_or(ActionEvidenceError::Unavailable)
        })
    }

    pub(super) fn is_reserved(&self, operation_id: &str) -> Result<bool, ActionEvidenceError> {
        fixed::<16>(operation_id).map_err(|_| ActionEvidenceError::Malformed)?;
        self.access(|snapshot| Ok((snapshot.actions.contains_key(operation_id), false)))
    }

    pub(super) fn page(
        &self,
        after: Option<&str>,
        limit: usize,
        receiver: &str,
        projects: &[String],
        agents: &[String],
    ) -> Result<(Vec<ActionEvidence>, Option<String>), ActionEvidenceError> {
        self.access(|snapshot| {
            let mut records: Vec<_> = snapshot
                .actions
                .values()
                .filter(|record| {
                    let identity = &record.identity;
                    after.is_none_or(|cursor| identity.operation_id.as_str() > cursor)
                        && identity.receiver_identity == receiver
                        && agents.contains(&identity.agent)
                        && projects
                            .iter()
                            .any(|project| Path::new(&identity.project).starts_with(project))
                })
                .take(limit + 1)
                .cloned()
                .collect();
            let next = if records.len() > limit {
                records.pop();
                records
                    .last()
                    .map(|record| record.identity.operation_id.clone())
            } else {
                None
            };
            Ok(((records, next), false))
        })
    }

    /// Durably archive the exact native owner's report for a reserved action.
    ///
    /// The caller must authenticate and serialize the native owner. In
    /// particular, it may report a fence only after that owner has proved no
    /// effect and durably prevented late execution. This store cannot prove
    /// either property. Conflicting terminal reports are never overwritten.
    ///
    /// # Errors
    ///
    /// Returns a malformed, conflicting, or unavailable refusal.
    pub fn report(
        &self,
        identity: &ActionIdentity,
        outcome: &ActionOutcome,
        at_ms: u64,
    ) -> Result<(), ActionEvidenceError> {
        validate_identity(identity)?;
        validate_outcome(outcome)?;
        if at_ms > MAX_INTEGER {
            return Err(ActionEvidenceError::Malformed);
        }
        self.access(|snapshot| {
            let record = snapshot
                .actions
                .get_mut(&identity.operation_id)
                .ok_or(ActionEvidenceError::Unavailable)?;
            if !same_identity(&record.identity, identity) {
                return Err(ActionEvidenceError::Conflict);
            }
            if &record.outcome == outcome {
                return Ok(((), false));
            }
            if record.outcome != ActionOutcome::Unknown {
                return Err(ActionEvidenceError::Conflict);
            }
            if at_ms < record.updated_at_ms {
                return Err(ActionEvidenceError::Malformed);
            }
            record.outcome = outcome.clone();
            record.updated_at_ms = at_ms;
            Ok(((), true))
        })
    }

    fn access<T>(
        &self,
        operation: impl FnOnce(&mut Snapshot) -> Result<(T, bool), ActionEvidenceError>,
    ) -> Result<T, ActionEvidenceError> {
        let mut failed = self
            .failed
            .lock()
            .map_err(|_| ActionEvidenceError::Unavailable)?;
        if *failed {
            return Err(ActionEvidenceError::Unavailable);
        }
        let mut snapshot = match load(&self.directory) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                *failed = true;
                return Err(error);
            }
        };
        let (result, changed) = operation(&mut snapshot)?;
        if changed {
            let bytes = snapshot_bytes(&snapshot)?;
            if let Err(error) = persist_bytes(&self.directory, &bytes) {
                *failed = true;
                return Err(error);
            }
        }
        Ok(result)
    }
}

pub(super) fn same_identity(left: &ActionIdentity, right: &ActionIdentity) -> bool {
    let mut right = right.clone();
    right.recorded_at_ms = left.recorded_at_ms;
    left == &right
}

pub(super) fn validate_identity(value: &ActionIdentity) -> Result<(), ActionEvidenceError> {
    for id in [
        &value.operation_id,
        &value.controller_id,
        &value.subject_id,
        &value.grant_id,
        &value.period_id,
    ] {
        fixed::<16>(id).map_err(|_| ActionEvidenceError::Malformed)?;
    }
    for digest in [&value.receiver_identity, &value.payload_hash] {
        fixed::<32>(digest).map_err(|_| ActionEvidenceError::Malformed)?;
    }
    let path = Path::new(&value.project);
    if value.action < 3
        || value.grant_revision == 0
        || value.grant_revision > MAX_INTEGER
        || value.expected_revision > MAX_INTEGER
        || value.recorded_at_ms > MAX_INTEGER
        || !valid_string(&value.project)
        || !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::CurDir | Component::ParentDir))
        || value.agent.is_empty()
        || !valid_string(&value.agent)
    {
        return Err(ActionEvidenceError::Malformed);
    }
    Ok(())
}

fn validate_outcome(outcome: &ActionOutcome) -> Result<(), ActionEvidenceError> {
    match outcome {
        ActionOutcome::Unknown => Ok(()),
        ActionOutcome::Accepted { evidence }
        | ActionOutcome::Rejected { evidence }
        | ActionOutcome::FencedNoEffect { evidence } => fixed::<32>(evidence)
            .map(|_| ())
            .map_err(|_| ActionEvidenceError::Malformed),
    }
}

fn load(directory: &Path) -> Result<Snapshot, ActionEvidenceError> {
    let bytes = read_bounded(&directory.join("actions.json"), MAX_STORE_BYTES)?;
    let seal_bytes = read_bounded(&directory.join("seal.json"), 512)?;
    let seal: Seal =
        serde_json::from_slice(&seal_bytes).map_err(|_| ActionEvidenceError::Unavailable)?;
    if seal.v != 1
        || seal.bytes != bytes.len() as u64
        || seal.sha256 != lower_hex(&Sha256::digest(&bytes))
    {
        return Err(ActionEvidenceError::Unavailable);
    }
    let snapshot: Snapshot =
        serde_json::from_slice(&bytes).map_err(|_| ActionEvidenceError::Unavailable)?;
    if snapshot.v != 1
        || snapshot.actions.iter().any(|(id, record)| {
            id != &record.identity.operation_id
                || validate_identity(&record.identity).is_err()
                || validate_outcome(&record.outcome).is_err()
                || record.updated_at_ms > MAX_INTEGER
                || record.updated_at_ms < record.identity.recorded_at_ms
        })
    {
        return Err(ActionEvidenceError::Unavailable);
    }
    Ok(snapshot)
}

fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>, ActionEvidenceError> {
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(unavailable)?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(unavailable)?;
    if bytes.len() as u64 > limit {
        return Err(ActionEvidenceError::Unavailable);
    }
    Ok(bytes)
}

fn persist(directory: &Path, snapshot: &Snapshot) -> Result<(), ActionEvidenceError> {
    persist_bytes(directory, &snapshot_bytes(snapshot)?)
}

fn snapshot_bytes(snapshot: &Snapshot) -> Result<Vec<u8>, ActionEvidenceError> {
    let bytes = serde_json::to_vec(snapshot).map_err(|_| ActionEvidenceError::Unavailable)?;
    if bytes.len() as u64 > MAX_STORE_BYTES {
        return Err(ActionEvidenceError::Unavailable);
    }
    Ok(bytes)
}

fn persist_bytes(directory: &Path, bytes: &[u8]) -> Result<(), ActionEvidenceError> {
    let seal = serde_json::to_vec(&Seal {
        v: 1,
        bytes: bytes.len() as u64,
        sha256: lower_hex(&Sha256::digest(bytes)),
    })
    .map_err(|_| ActionEvidenceError::Unavailable)?;
    replace(directory, "actions.json", bytes)?;
    replace(directory, "seal.json", &seal)
}

fn replace(directory: &Path, name: &str, bytes: &[u8]) -> Result<(), ActionEvidenceError> {
    let temporary = directory.join(format!(".action-{}", uuid::Uuid::new_v4()));
    let result = (|| -> io::Result<()> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        set_private_permissions(&temporary, false)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, directory.join(name))?;
        File::open(directory)?.sync_all()
    })();
    if temporary.exists() {
        // Temporary bytes are never consulted as evidence after a failure.
        let _ = fs::remove_file(temporary);
    }
    result.map_err(unavailable)
}

fn unavailable(_: io::Error) -> ActionEvidenceError {
    ActionEvidenceError::Unavailable
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test fixtures and assertions")]
mod tests {
    use super::{
        ActionEvidence, ActionEvidenceError, ActionEvidenceStore, ActionIdentity, ActionOutcome,
        ActionReservation,
    };

    #[test]
    #[allow(
        clippy::used_underscore_binding,
        reason = "the lock owner must be cloned to test inherited descriptors"
    )]
    fn lock_releases_even_when_its_descriptor_was_cloned() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("actions");
        let store = ActionEvidenceStore::open(&path).unwrap();
        let inherited = store._lock.0.try_clone().unwrap();
        drop(store);
        let reopened = ActionEvidenceStore::open(&path).unwrap();
        drop(reopened);
        drop(inherited);
    }

    #[test]
    fn full_store_refuses_new_records_without_disabling_existing_history() {
        let root = tempfile::tempdir().unwrap();
        let store = ActionEvidenceStore::open(root.path().join("actions")).unwrap();
        let original = ActionIdentity {
            operation_id: "01".repeat(16),
            receiver_identity: "02".repeat(32),
            controller_id: "03".repeat(16),
            subject_id: "04".repeat(16),
            action: 3,
            payload_hash: "05".repeat(32),
            grant_id: "06".repeat(16),
            grant_revision: 1,
            period_id: "07".repeat(16),
            expected_revision: 1,
            project: std::fs::canonicalize(root.path())
                .unwrap()
                .to_str()
                .unwrap()
                .into(),
            agent: "codex".into(),
            recorded_at_ms: 1,
        };
        store.reserve(&original).unwrap();
        let result = store.access(|snapshot| {
            for number in 0..2_100 {
                let mut identity = original.clone();
                identity.operation_id = format!("{number:032x}");
                identity.agent = "x".repeat(8192);
                snapshot.actions.insert(
                    identity.operation_id.clone(),
                    ActionEvidence {
                        identity,
                        outcome: ActionOutcome::Unknown,
                        updated_at_ms: 1,
                    },
                );
            }
            Ok(((), true))
        });
        assert_eq!(result, Err(ActionEvidenceError::Unavailable));
        assert_eq!(
            store.lookup(&original.operation_id).unwrap().identity,
            original
        );
    }

    #[test]
    fn unresolved_action_blocks_until_the_owner_proves_independence_or_fences_it() {
        let root = tempfile::tempdir().unwrap();
        let store = ActionEvidenceStore::open(root.path().join("actions")).unwrap();
        let first = ActionIdentity {
            operation_id: "01".repeat(16),
            receiver_identity: "02".repeat(32),
            controller_id: "03".repeat(16),
            subject_id: "04".repeat(16),
            action: 3,
            payload_hash: "05".repeat(32),
            grant_id: "06".repeat(16),
            grant_revision: 1,
            period_id: "07".repeat(16),
            expected_revision: 1,
            project: std::fs::canonicalize(root.path())
                .unwrap()
                .to_str()
                .unwrap()
                .into(),
            agent: "codex".into(),
            recorded_at_ms: 1,
        };
        assert_eq!(
            store.reserve_session_prompt(&first, &[]),
            Ok(ActionReservation::New)
        );
        let mut next = first.clone();
        next.operation_id = "08".repeat(16);
        assert_eq!(
            store.reserve_session_prompt(&next, &[]),
            Err(ActionEvidenceError::Unresolved)
        );
        let mut independent = next.clone();
        independent.operation_id = "0b".repeat(16);
        independent.subject_id = "09".repeat(16);
        assert_eq!(
            store.reserve_session_prompt(&independent, &[]),
            Err(ActionEvidenceError::Unresolved)
        );
        assert_eq!(
            store.reserve_session_prompt(&independent, std::slice::from_ref(&first.operation_id)),
            Ok(ActionReservation::New)
        );
        store
            .report(
                &first,
                &ActionOutcome::FencedNoEffect {
                    evidence: "0a".repeat(32),
                },
                2,
            )
            .unwrap();
        assert_eq!(
            store.reserve_session_prompt(&next, std::slice::from_ref(&independent.operation_id)),
            Ok(ActionReservation::New)
        );
    }
}
