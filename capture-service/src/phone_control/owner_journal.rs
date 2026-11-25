//! Durable native-owner admission and no-effect fences.
//!
//! `inspect` reads the outcome for an exact identity without creating a store
//! or reserving an absent operation. `begin` only reserves: it is not Session
//! admission. A report of `fenced_no_effect` requires the living owner to have
//! independently proved no effect and excluded any outstanding native gate.

use std::path::Path;

use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::hex::lower_hex;
use crate::phone_control::evidence::{phase_evidence, same_identity, validate_identity};
use crate::phone_control::{
    ActionEvidence, ActionEvidenceError, ActionEvidenceStore, ActionIdentity, ActionOutcome,
    ActionReservation, MAX_FRAME, MAX_INTEGER,
};

#[cfg(test)]
#[path = "owner_journal_tests.rs"]
mod tests;

#[derive(Debug, Eq, PartialEq)]
pub(super) enum Begin {
    /// The owner may attempt this exact operation once.
    New,
    /// The operation was already reserved; it must not execute again.
    Existing(ActionOutcome),
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum Fence {
    /// The owner durably excluded the operation before it began.
    Fenced(String),
    /// Admission already began, so this journal cannot prove no effect.
    Uncertain,
}

/// Separate native-owner evidence, never the capture-side archive.
pub(super) struct OwnerJournal {
    store: ActionEvidenceStore,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Command {
    Begin {
        v: u8,
        identity: ActionIdentity,
    },
    Inspect {
        v: u8,
        identity: ActionIdentity,
    },
    InspectPhases {
        v: u8,
        identity: ActionIdentity,
    },
    Fence {
        v: u8,
        identity: ActionIdentity,
        at_ms: u64,
    },
    Report {
        v: u8,
        identity: ActionIdentity,
        outcome: ActionOutcome,
        at_ms: u64,
    },
}

pub(crate) fn handle(path: &Path, input: &[u8]) -> Value {
    let result = (|| {
        let line = input.strip_suffix(b"\n").unwrap_or(input);
        if input.len() > MAX_FRAME || line.contains(&b'\n') || line.contains(&b'\r') {
            return Err(ActionEvidenceError::Malformed);
        }
        let command: Command =
            serde_json::from_slice(line).map_err(|_| ActionEvidenceError::Malformed)?;
        let version = match &command {
            Command::Begin { v, .. }
            | Command::Inspect { v, .. }
            | Command::InspectPhases { v, .. }
            | Command::Fence { v, .. }
            | Command::Report { v, .. } => *v,
        };
        if version != 1 {
            return Err(ActionEvidenceError::Malformed);
        }
        let journal = match &command {
            Command::Inspect { identity, .. } | Command::InspectPhases { identity, .. } => {
                validate_identity(identity)?;
                OwnerJournal::open_existing(path)?
            }
            _ => OwnerJournal::open(path)?,
        };
        match command {
            Command::Begin { identity, .. } => match journal.begin(&identity)? {
                Begin::New => Ok(json!({"v":1,"type":"begun"})),
                Begin::Existing(outcome) => Ok(json!({"v":1,"type":"existing","outcome":outcome})),
            },
            Command::Inspect { identity, .. } => {
                let record = journal.inspect(&identity)?;
                Ok(json!({"v":1,"type":"existing","outcome":record.outcome}))
            }
            Command::InspectPhases { identity, .. } => {
                let record = journal.inspect(&identity)?;
                let (accepted, dispatched, completed) = phase_evidence(&record);
                Ok(
                    json!({"v":1,"type":"action_phases","outcome":record.outcome,
                    "accepted_evidence":accepted,"dispatched_evidence":dispatched,"completed_evidence":completed}),
                )
            }
            Command::Fence {
                identity, at_ms, ..
            } => match journal.fence(&identity, at_ms)? {
                Fence::Fenced(evidence) => Ok(json!({"v":1,"type":"fenced","evidence":evidence})),
                Fence::Uncertain => Ok(json!({"v":1,"type":"uncertain"})),
            },
            Command::Report {
                identity,
                outcome,
                at_ms,
                ..
            } => {
                journal.report(&identity, &outcome, at_ms)?;
                Ok(json!({"v":1,"type":"recorded"}))
            }
        }
    })();
    match result {
        Ok(response) => response,
        Err(error) => json!({"v":1,"type":"error","code":match error {
            ActionEvidenceError::Malformed => "malformed",
            ActionEvidenceError::Conflict => "conflict",
            ActionEvidenceError::Unavailable | ActionEvidenceError::Unresolved => "unavailable",
        }}),
    }
}

impl OwnerJournal {
    #[cfg(test)]
    pub(super) fn open(path: impl AsRef<Path>) -> Result<Self, ActionEvidenceError> {
        Ok(Self {
            store: ActionEvidenceStore::open(path)?,
        })
    }

    #[cfg(not(test))]
    pub(super) fn open(path: impl AsRef<Path>) -> Result<Self, ActionEvidenceError> {
        let path = path.as_ref();
        let state = path
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .ok_or(ActionEvidenceError::Unavailable)?;
        let tls = state.join("louiselm/capture/tls");
        let receiver = crate::TlsIdentity::existing_public_key_sha256(&tls)
            .map_err(|_| ActionEvidenceError::Unavailable)?;
        Ok(Self {
            store: ActionEvidenceStore::open_bound(
                path,
                tls.join("phone-action-owner.json"),
                &receiver,
                super::ActionEvidenceKind::Owner,
            )?,
        })
    }

    #[cfg(test)]
    fn open_existing(path: &Path) -> Result<Self, ActionEvidenceError> {
        Ok(Self {
            store: ActionEvidenceStore::open_existing(path)?,
        })
    }

    #[cfg(not(test))]
    fn open_existing(path: &Path) -> Result<Self, ActionEvidenceError> {
        Self::open(path)
    }

    fn inspect(&self, identity: &ActionIdentity) -> Result<ActionEvidence, ActionEvidenceError> {
        validate_identity(identity)?;
        let record = self.store.lookup(&identity.operation_id)?;
        if !same_identity(&record.identity, identity) {
            return Err(ActionEvidenceError::Conflict);
        }
        Ok(record)
    }

    pub(super) fn begin(&self, identity: &ActionIdentity) -> Result<Begin, ActionEvidenceError> {
        match self.store.reserve(identity)? {
            ActionReservation::New => Ok(Begin::New),
            ActionReservation::Existing => Ok(Begin::Existing(
                self.store.lookup(&identity.operation_id)?.outcome,
            )),
        }
    }

    pub(super) fn report(
        &self,
        identity: &ActionIdentity,
        outcome: &ActionOutcome,
        at_ms: u64,
    ) -> Result<(), ActionEvidenceError> {
        self.store.report(identity, outcome, at_ms)
    }

    pub(super) fn fence(
        &self,
        identity: &ActionIdentity,
        at_ms: u64,
    ) -> Result<Fence, ActionEvidenceError> {
        if at_ms < identity.recorded_at_ms || at_ms > MAX_INTEGER {
            return Err(ActionEvidenceError::Malformed);
        }
        match self.store.reserve(identity)? {
            ActionReservation::Existing => {
                match self.store.lookup(&identity.operation_id)?.outcome {
                    ActionOutcome::FencedNoEffect { evidence } => Ok(Fence::Fenced(evidence)),
                    _ => Ok(Fence::Uncertain),
                }
            }
            ActionReservation::New => {
                // Reservation wins against every late begin before this report is written.
                let mut binding = identity.clone();
                binding.recorded_at_ms = 0;
                let bytes =
                    serde_json::to_vec(&binding).map_err(|_| ActionEvidenceError::Unavailable)?;
                let mut digest = Sha256::new();
                digest.update(b"louiselm-phone-owner-fence-v1\0");
                digest.update(bytes);
                let evidence = lower_hex(&digest.finalize());
                self.store.report(
                    identity,
                    &ActionOutcome::FencedNoEffect {
                        evidence: evidence.clone(),
                    },
                    at_ms,
                )?;
                Ok(Fence::Fenced(evidence))
            }
        }
    }
}
