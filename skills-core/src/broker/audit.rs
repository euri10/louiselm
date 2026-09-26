//! The operator's record of what the broker decided.
//!
//! Every entry is normalized: bounded identifiers, a slot number, and a stable
//! typed decision. Prompts, environments, commands, request bytes, receipt
//! payloads, Provider credentials, and signature material never reach this file, so an operator can
//! read it and a hostile Session cannot write prose into it.

use std::{
    fs::OpenOptions,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};

use serde::{Deserialize, Serialize};

use crate::{
    broker::{BrokerError, corrupt, is_record_identifier, lock, sync_directory},
    launch_protocol::ErrorCode,
};

/// File holding the append-only operator record.
const DECISIONS_FILE: &str = "decisions.jsonl";

/// Longest operator record this build reads back, in entries.
const MAX_ENTRIES: usize = 4096;

/// One stable broker decision, named without prose.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AuditDecision {
    /// Operator authorized marker replacement. Stored in the machine-scoped
    /// identity-adoptions directory before replacement; not proof of completion.
    StateIdentityAdoption {
        /// Authenticated operator that explicitly confirmed adoption.
        operator_uid: u32,
        /// Broker UID recorded by the previous marker.
        previous_uid: u32,
        /// Broker GID recorded by the previous marker.
        previous_gid: u32,
        /// Installed broker UID authorized to adopt state.
        new_uid: u32,
        /// Installed broker GID authorized to adopt state.
        new_gid: u32,
    },
    /// A pending authorization was atomically spent by the launcher.
    AuthorizationConsumed,
    /// The launcher's request did not consume an authorization.
    AuthorizationRefused {
        /// Stable code the supervisor also received.
        error: ErrorCode,
    },
    /// One exact signed receipt became durable.
    ReceiptStored {
        /// Chain position that became durable.
        sequence: u64,
    },
    /// One offered receipt was refused whole and never stored.
    ReceiptRefused {
        /// Stable code naming why the chain refused it.
        error: ErrorCode,
    },
    /// An exact tool process received a reserved portion of the Agent budget.
    ToolGranted {
        /// Broker-assigned grant sequence.
        grant: u64,
        /// Authenticated tool's host process ID.
        pid: u32,
        /// Envelope revision whose operator permission permitted delegation.
        revision: u64,
        /// Optional reserved count, never refunded; omission records uncapped authority.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        uses: Option<u32>,
    },
    /// Broker stopped one grant; supervisor cleanup remains pending.
    ToolRevocationRequested {
        /// Exact grant being cancelled.
        grant: u64,
    },
    /// Supervisor proved the named grant's enforcement and termination.
    ToolRevoked {
        /// Exact grant whose cancellation completed.
        grant: u64,
    },
    /// A capability request was denied; untrusted request fields are omitted.
    CapabilityDenied,
    /// Durable intent to commit, before the final lifetime/deadline recheck.
    /// This alone never proves the external effect occurred.
    EffectCommitIntent {
        /// Tool grant, or `None` for an Agent-originated action.
        grant: Option<u64>,
        /// Principal-local request sequence.
        sequence: u64,
    },
    /// The committed effect returned its actual result, even after revocation.
    EffectFinished {
        /// Tool grant, or `None` for an Agent-originated action.
        grant: Option<u64>,
        /// Principal-local request sequence.
        sequence: u64,
        /// Whether the adapter completed successfully, not whether a command exited zero.
        succeeded: bool,
    },
    /// The Agent channel and every delegated grant became unusable.
    CapabilitiesRevoked,
    /// Immutable Session output taint was recorded; the record holds its digest.
    SessionOutputTainted,
    /// Broker stopped approvals; supervisor enforcement is still pending.
    CapabilitiesRevocationRequested,
    /// A spent effect has no known actual outcome and must not be retried.
    EffectOutcomeUnknown {
        /// Session-wide effect sequence.
        sequence: u64,
    },
}

/// One normalized entry in the operator record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditEntry {
    /// Broker clock reading for the decision.
    pub at_ms: u64,
    /// Session the decision concerns.
    pub session_id: String,
    /// Run that owns the Session.
    pub run_id: String,
    /// Authorization the decision concerns.
    pub authorization_id: String,
    /// Installed identity slot bound to the Session.
    pub identity_slot: u32,
    /// What the broker decided.
    pub decision: AuditDecision,
}

/// Append-only durable operator record.
#[derive(Debug)]
pub struct AuditLog {
    path: PathBuf,
    appending: Mutex<()>,
}

impl AuditLog {
    /// Opens or creates the operator record under `root`.
    ///
    /// # Errors
    /// Returns [`BrokerError::Storage`] when the record cannot be created.
    pub fn open(root: &Path) -> Result<Self, BrokerError> {
        std::fs::create_dir_all(root).map_err(BrokerError::Storage)?;
        sync_directory(root)?;
        Ok(Self {
            path: root.join(DECISIONS_FILE),
            appending: Mutex::new(()),
        })
    }

    /// Durably appends one normalized entry.
    ///
    /// # Errors
    /// Returns [`BrokerError::InvalidGrant`] for identifiers that are not
    /// normalized and [`BrokerError::Storage`] when the entry cannot be made
    /// durable.
    pub fn record(&self, entry: &AuditEntry) -> Result<(), BrokerError> {
        if !is_record_identifier(&entry.session_id)
            || !is_record_identifier(&entry.run_id)
            || !is_record_identifier(&entry.authorization_id)
        {
            return Err(BrokerError::InvalidGrant);
        }
        let mut line =
            serde_json::to_vec(entry).map_err(|_| corrupt("audit entry is unwritable"))?;
        line.push(b'\n');

        let appending = lock(&self.appending);
        let mut file = OpenOptions::new()
            .append(true)
            .create(true)
            .open(&self.path)
            .map_err(BrokerError::Storage)?;
        let result = file
            .write_all(&line)
            .and_then(|()| file.sync_all())
            .map_err(BrokerError::Storage)
            .and_then(|()| {
                // The first append creates the filename. File fsync alone does
                // not establish durability of that directory entry (vib4).
                let parent = self.path.parent().ok_or(BrokerError::InvalidGrant)?;
                sync_directory(parent)
            });
        drop(appending);
        result
    }

    /// Finds a decision without allocating the bounded operator view.
    /// Authority and proof lookups must still work after that view's entry limit.
    /// Reads one entry at a time and stops at the first matching observation.
    pub(in crate::broker) fn find(
        &self,
        mut predicate: impl FnMut(&AuditEntry) -> bool,
    ) -> Result<Option<AuditEntry>, BrokerError> {
        let file = match std::fs::File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(BrokerError::Storage(error)),
        };
        for line in BufReader::new(file).lines() {
            let line = line.map_err(BrokerError::Storage)?;
            if line.is_empty() {
                continue;
            }
            let entry: AuditEntry =
                serde_json::from_str(&line).map_err(|_| corrupt("audit entry is malformed"))?;
            if predicate(&entry) {
                return Ok(Some(entry));
            }
        }
        Ok(None)
    }

    /// Reads the operator record in the order the broker decided.
    ///
    /// # Errors
    /// Returns [`BrokerError::Storage`] when the record cannot be read or has
    /// grown past the bound this build reads back.
    pub fn entries(&self) -> Result<Vec<AuditEntry>, BrokerError> {
        let file = match std::fs::File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(BrokerError::Storage(error)),
        };
        let mut entries = Vec::new();
        for line in BufReader::new(file).lines() {
            let line = line.map_err(BrokerError::Storage)?;
            if line.is_empty() {
                continue;
            }
            entries.push(
                serde_json::from_str(&line).map_err(|_| corrupt("audit entry is malformed"))?,
            );
            if entries.len() > MAX_ENTRIES {
                return Err(corrupt("operator record exceeds its bound"));
            }
        }
        Ok(entries)
    }
}
