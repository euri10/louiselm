//! The operator's record of what the broker decided.
//!
//! Every entry is normalized: bounded identifiers, a slot number, and a stable
//! typed decision. Prompts, environments, commands, request bytes, receipt
//! payloads, and signature material never reach this file, so an operator can
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
            .map_err(BrokerError::Storage);
        drop(appending);
        result
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
