//! Private storage for explicitly published prepared-Run snapshots.
//!
//! Storage is not authority: callers must establish and validate the binding
//! through the trusted owner before publication and check current phone scope,
//! revocation and registration state before delivering protected content.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::permissions::set_private_permissions;

const SCHEMA: &str = "louiselm.prepared-runs/1";
const MAX_SELECTION: usize = 65_536;
const MAX_STORE: usize = 8 * 1024 * 1024;
const MAX_REGISTRATIONS: usize = 50;
const MAX_REVISIONS: usize = 64;

/// Owner-established identities for one exact prepared Run revision.
///
/// These fields are references to verified owner evidence, not evidence or
/// authority by themselves. The caller must verify the complete original
/// project, source and checkout chain and contributing Agent set.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedRunBinding {
    /// Exact canonical Run UUID.
    pub run_id: String,
    /// Private original-project identity, encoded as 16 lowercase hex bytes.
    pub project_id: String,
    /// Private identity of the owner-bound frozen source.
    pub source_id: String,
    /// Private identity of the owner-bound reviewed checkout.
    pub checkout_id: String,
    /// Complete configured-Agent set for this prepared revision.
    pub agents: Vec<String>,
}

/// One immutable registered snapshot and its owner-established binding.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedRunRevision {
    /// Monotonic revision within the registration.
    pub revision: u64,
    /// Owner-established references for this exact revision.
    pub binding: PreparedRunBinding,
    /// Exact private selection bytes, represented as UTF-8.
    pub selection: String,
}

/// Locally retained revisions, including an unpublished registration.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedRunRegistration {
    /// Stable opaque registration identity.
    pub id: String,
    /// Whether the latest revision is available for authorized discovery.
    pub published: bool,
    /// Complete retained revisions in ascending order.
    pub revisions: Vec<PreparedRunRevision>,
}

/// Non-sensitive registration metadata for a bounded local listing.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedRunSummary {
    /// Stable opaque registration identity.
    pub id: String,
    /// Exact Run UUID for the latest revision.
    pub run_id: String,
    /// Latest published revision.
    pub revision: u64,
}

/// A private registration was invalid, unavailable, or could not be persisted.
#[derive(Debug, Error)]
pub enum PreparedRunError {
    /// The input, stored record, or expected revision was invalid.
    #[error("prepared Run registration refused: {0}")]
    Invalid(&'static str),
    /// Private storage could not be accessed or updated.
    #[error("prepared Run storage unavailable")]
    Io(#[from] io::Error),
    /// A stored registration could not be decoded or encoded.
    #[error("prepared Run storage is invalid")]
    Json(#[from] serde_json::Error),
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct State {
    schema: String,
    registrations: Vec<PreparedRunRegistration>,
}

/// Filesystem-backed, process-safe private prepared-Run registrations.
///
/// All operations block on local filesystem I/O and serialize with other store
/// operations. This store does not authenticate its callers or authorize
/// delivery; only a trusted desktop owner may publish or unpublish.
#[derive(Clone, Debug)]
pub struct PreparedRunStore {
    root: PathBuf,
}

impl PreparedRunStore {
    /// Create a private store at a new path, or open an existing intact store.
    /// The parent directory must already exist and be owner-controlled.
    ///
    /// # Errors
    /// Refuses non-directory paths, missing/damaged existing state and I/O errors.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, PreparedRunError> {
        let root = root.as_ref();
        let created = match fs::create_dir(root) {
            Ok(()) => true,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => false,
            Err(error) => return Err(error.into()),
        };
        if !fs::symlink_metadata(root)?.file_type().is_dir() {
            return Err(PreparedRunError::Invalid("store root is not a directory"));
        }
        set_private_permissions(root, true)?;
        let store = Self { root: root.into() };
        store.locked(|| {
            if created {
                store.write(&State {
                    schema: SCHEMA.into(),
                    registrations: Vec::new(),
                })?;
            }
            store.load()?;
            Ok(())
        })?;
        Ok(store)
    }

    /// Publish a private exact snapshot, or replace an existing revision.
    ///
    /// `previous` identifies the same registration and its expected current
    /// revision. The trusted caller must validate the selection and its binding
    /// before calling; this operation creates no Run or execution authority.
    ///
    /// # Errors
    /// Refuses invalid input, changed Run/project identity, stale revisions,
    /// exhausted storage bounds, damaged state, and persistence failures.
    pub fn publish(
        &self,
        previous: Option<(&str, u64)>,
        binding: &PreparedRunBinding,
        selection: &str,
    ) -> Result<PreparedRunSummary, PreparedRunError> {
        validate_binding(binding)?;
        validate_selection(selection)?;
        self.locked(|| {
            let mut state = self.load()?;
            let summary = if let Some((id, expected)) = previous {
                let entry = state
                    .registrations
                    .iter_mut()
                    .find(|entry| entry.id == id)
                    .ok_or(PreparedRunError::Invalid("registration is unavailable"))?;
                let current = entry
                    .revisions
                    .last()
                    .ok_or(PreparedRunError::Invalid("registration has no revision"))?;
                if current.revision != expected
                    || current.binding.run_id != binding.run_id
                    || current.binding.project_id != binding.project_id
                    || entry.revisions.len() >= MAX_REVISIONS
                {
                    return Err(PreparedRunError::Invalid(
                        "stale or conflicting registration",
                    ));
                }
                let revision = expected
                    .checked_add(1)
                    .ok_or(PreparedRunError::Invalid("revision overflow"))?;
                entry.revisions.push(PreparedRunRevision {
                    revision,
                    binding: binding.clone(),
                    selection: selection.to_owned(),
                });
                entry.published = true;
                PreparedRunSummary {
                    id: entry.id.clone(),
                    run_id: binding.run_id.clone(),
                    revision,
                }
            } else {
                if state.registrations.len() >= MAX_REGISTRATIONS {
                    return Err(PreparedRunError::Invalid("registration limit reached"));
                }
                if state
                    .registrations
                    .iter()
                    .any(|entry| entry.revisions[0].binding.run_id == binding.run_id)
                {
                    return Err(PreparedRunError::Invalid("Run is already registered"));
                }
                let id = Uuid::new_v4().simple().to_string();
                if state.registrations.iter().any(|entry| entry.id == id) {
                    return Err(PreparedRunError::Invalid("registration identity collision"));
                }
                state.registrations.push(PreparedRunRegistration {
                    id: id.clone(),
                    published: true,
                    revisions: vec![PreparedRunRevision {
                        revision: 1,
                        binding: binding.clone(),
                        selection: selection.to_owned(),
                    }],
                });
                state
                    .registrations
                    .sort_by(|left, right| left.id.cmp(&right.id));
                PreparedRunSummary {
                    id,
                    run_id: binding.run_id.clone(),
                    revision: 1,
                }
            };
            self.write(&state)?;
            Ok(summary)
        })
    }

    /// Remove a current revision from discovery while retaining all snapshots.
    ///
    /// # Errors
    /// Refuses missing, unpublished or stale registrations and storage errors.
    pub fn unpublish(&self, id: &str, expected: u64) -> Result<(), PreparedRunError> {
        self.locked(|| {
            let mut state = self.load()?;
            let entry = state
                .registrations
                .iter_mut()
                .find(|entry| entry.id == id)
                .ok_or(PreparedRunError::Invalid("registration is unavailable"))?;
            if !entry.published
                || entry.revisions.last().map(|revision| revision.revision) != Some(expected)
            {
                return Err(PreparedRunError::Invalid(
                    "stale or unpublished registration",
                ));
            }
            entry.published = false;
            self.write(&state)
        })
    }

    /// List published metadata without exposing instructions or local paths.
    /// This is an owner-local listing and does not apply phone scope.
    ///
    /// # Errors
    /// Returns invalid-state and storage errors.
    pub fn list(&self) -> Result<Vec<PreparedRunSummary>, PreparedRunError> {
        self.locked(|| {
            let state = self.load()?;
            state
                .registrations
                .into_iter()
                .filter(|entry| entry.published)
                .map(|entry| {
                    let current = entry
                        .revisions
                        .last()
                        .ok_or(PreparedRunError::Invalid("registration has no revision"))?;
                    Ok(PreparedRunSummary {
                        id: entry.id,
                        run_id: current.binding.run_id.clone(),
                        revision: current.revision,
                    })
                })
                .collect()
        })
    }

    /// Read the exact currently published revision, without granting phone access.
    /// Callers must recheck publication and current authority before delivery.
    ///
    /// # Errors
    /// Refuses missing, unpublished or stale registrations and storage errors.
    pub fn read(&self, id: &str, expected: u64) -> Result<PreparedRunRevision, PreparedRunError> {
        self.locked(|| {
            let state = self.load()?;
            let entry = state
                .registrations
                .into_iter()
                .find(|entry| entry.id == id && entry.published)
                .ok_or(PreparedRunError::Invalid("registration is unavailable"))?;
            entry
                .revisions
                .into_iter()
                .last()
                .filter(|revision| revision.revision == expected)
                .ok_or(PreparedRunError::Invalid("registration revision is stale"))
        })
    }

    /// Inspect all locally retained revisions, including unpublished snapshots.
    /// This owner-only operation does not authorize protected delivery.
    ///
    /// # Errors
    /// Returns an error for missing registrations, damaged state, or storage I/O.
    pub fn inspect(&self, id: &str) -> Result<PreparedRunRegistration, PreparedRunError> {
        self.locked(|| {
            self.load()?
                .registrations
                .into_iter()
                .find(|entry| entry.id == id)
                .ok_or(PreparedRunError::Invalid("registration is unavailable"))
        })
    }

    fn locked<T>(
        &self,
        action: impl FnOnce() -> Result<T, PreparedRunError>,
    ) -> Result<T, PreparedRunError> {
        let path = self.root.join(".lock");
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)?;
        set_private_permissions(&path, false)?;
        lock.lock_exclusive()?;
        let result = action();
        FileExt::unlock(&lock)?;
        result
    }

    fn load(&self) -> Result<State, PreparedRunError> {
        let mut bytes = Vec::new();
        File::open(self.root.join("registrations.json"))?
            .take((MAX_STORE + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_STORE {
            return Err(PreparedRunError::Invalid("store size limit exceeded"));
        }
        let state: State = serde_json::from_slice(&bytes)?;
        validate_state(&state)?;
        Ok(state)
    }

    fn write(&self, state: &State) -> Result<(), PreparedRunError> {
        validate_state(state)?;
        let bytes = serde_json::to_vec(state)?;
        if bytes.len() > MAX_STORE {
            return Err(PreparedRunError::Invalid("store size limit exceeded"));
        }
        let temporary = self.root.join(format!(".prepared-{}", Uuid::new_v4()));
        let result = (|| {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)?;
            set_private_permissions(&temporary, false)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            fs::rename(&temporary, self.root.join("registrations.json"))?;
            File::open(&self.root)?.sync_all()?;
            Ok(())
        })();
        if temporary.exists() {
            // An unpublished temporary file is never read as authority.
            let _ = fs::remove_file(temporary);
        }
        result
    }
}

fn valid_id(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        && value.bytes().any(|byte| byte != b'0')
}

fn validate_binding(binding: &PreparedRunBinding) -> Result<(), PreparedRunError> {
    if Uuid::parse_str(&binding.run_id).map(|id| id.to_string()) != Ok(binding.run_id.clone())
        || !valid_id(&binding.project_id)
        || !valid_id(&binding.source_id)
        || !valid_id(&binding.checkout_id)
        || binding.agents.is_empty()
        || binding.agents.len() > 50
        || binding
            .agents
            .iter()
            .any(|agent| agent.is_empty() || agent.len() > 512 || agent.contains('\0'))
        || binding
            .agents
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != binding.agents.len()
    {
        return Err(PreparedRunError::Invalid("invalid owner binding"));
    }
    Ok(())
}

fn validate_selection(selection: &str) -> Result<(), PreparedRunError> {
    if selection.is_empty() || selection.len() > MAX_SELECTION || selection.contains('\0') {
        return Err(PreparedRunError::Invalid(
            "invalid selection size or encoding",
        ));
    }
    if !serde_json::from_str::<serde_json::Value>(selection)?.is_object() {
        return Err(PreparedRunError::Invalid("selection is not an object"));
    }
    Ok(())
}

fn validate_state(state: &State) -> Result<(), PreparedRunError> {
    if state.schema != SCHEMA || state.registrations.len() > MAX_REGISTRATIONS {
        return Err(PreparedRunError::Invalid("invalid store header"));
    }
    let mut previous = "";
    let mut run_ids = std::collections::BTreeSet::new();
    for entry in &state.registrations {
        if !valid_id(&entry.id)
            || entry.id.as_str() <= previous
            || entry.revisions.is_empty()
            || entry.revisions.len() > MAX_REVISIONS
        {
            return Err(PreparedRunError::Invalid("invalid registration"));
        }
        previous = &entry.id;
        let first = &entry.revisions[0].binding;
        if !run_ids.insert(&first.run_id) {
            return Err(PreparedRunError::Invalid("duplicate registered Run"));
        }
        for (index, revision) in entry.revisions.iter().enumerate() {
            validate_binding(&revision.binding)?;
            validate_selection(&revision.selection)?;
            if revision.revision != (index + 1) as u64
                || revision.binding.run_id != first.run_id
                || revision.binding.project_id != first.project_id
            {
                return Err(PreparedRunError::Invalid("invalid registration history"));
            }
        }
    }
    Ok(())
}
