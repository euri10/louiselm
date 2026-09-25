//! Durable Run-scoped Provider request budget (`louiselm-qbr.5.1.3.2`).
//!
//! One record per upstream attempt, written before the attempt starts and never
//! removed: a spent unit is never refunded, and a restart recounts the records.
//! Every Session of one Run shares the directory named for that Run.
//!
//! Exhaustion or expiry also writes a Run-wide hold beside the records. The
//! hold stops every Session of the Run and withdraws Resume until an explicit
//! operator extension lifts it. Holds and extensions are numbered generations
//! and never removed: extension `n` lifts hold `n`, and a later exhaustion
//! writes hold `n + 1`.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
};

use serde::{Deserialize, Serialize};

use super::{
    BrokerError, lock,
    provider_extension::{Extension, ExtensionError, ExtensionRequest},
    read_record, sync_directory, write_new_record,
};
use crate::{Digest, provider_request::MAX_RUN_REQUESTS};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reservation {
    run_id: String,
    session_id: String,
    sequence: u32,
    max_run_requests: u32,
    reserved_at_ms: u64,
}

/// Why a Run's Provider requests stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HoldReason {
    /// Every granted unit is spent.
    Exhausted,
    /// The Provider permission's expiry passed.
    Expired,
}

/// Durable Run-wide stop, written once on first exhaustion or expiry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderHold {
    /// Held Run.
    pub run_id: String,
    /// First observed cause; a later cause never rewrites it.
    pub reason: HoldReason,
    /// First observation time, shared by every Session's Park and Attention.
    pub held_at_ms: u64,
}

/// Non-refundable per-Run attempt records.
pub(super) struct ProviderLedger {
    root: PathBuf,
    // One broker process owns this state; its Session workers share the ledger.
    writing: Mutex<()>,
}

impl ProviderLedger {
    pub(super) fn open(root: &Path) -> Result<Self, BrokerError> {
        for directory in ["runs", "holds", "extensions"] {
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

    /// Durably spends the Run's next unit and returns its sequence number.
    ///
    /// The record is durable before this returns, so the caller may start the
    /// upstream attempt; whatever happens next, the unit stays spent.
    pub(super) fn reserve(
        &self,
        run_id: &str,
        session_id: &str,
        max_run_requests: u32,
        now_ms: u64,
    ) -> Result<u32, BrokerError> {
        if !(1..=MAX_RUN_REQUESTS).contains(&max_run_requests) {
            return Err(BrokerError::InvalidGrant);
        }
        let _guard = lock(&self.writing);
        let directory = self.run_directory(run_id);
        if !directory.exists() {
            fs::create_dir(&directory).map_err(BrokerError::Storage)?;
            sync_directory(&self.root.join("runs"))?;
        }
        let total = total(max_run_requests, &self.extensions_locked(run_id)?)?;
        let spent = count(&directory)?;
        if spent > 0 {
            let first: Reservation =
                read_record(&directory.join("0.json"))?.ok_or(BrokerError::InvalidGrant)?;
            if first.run_id != run_id || first.max_run_requests != max_run_requests {
                return Err(BrokerError::InvalidGrant);
            }
        }
        if spent >= total {
            return Err(BrokerError::ProviderBudgetExhausted);
        }
        write_new_record(
            &directory.join(format!("{spent}.json")),
            &Reservation {
                run_id: run_id.into(),
                session_id: session_id.into(),
                sequence: spent,
                max_run_requests,
                reserved_at_ms: now_ms,
            },
        )?;
        Ok(spent)
    }

    /// Units already spent by the Run, including attempts with unknown outcomes.
    pub(super) fn spent(&self, run_id: &str) -> Result<u32, BrokerError> {
        let _guard = lock(&self.writing);
        let directory = self.run_directory(run_id);
        if directory.exists() {
            count(&directory)
        } else {
            Ok(0)
        }
    }

    /// Durably holds the Run, or returns the hold already recorded.
    ///
    /// Idempotent across Sessions and restarts: the first writer's reason and
    /// time win, so every Session derives the same Park and Attention identity.
    pub(super) fn hold(
        &self,
        run_id: &str,
        reason: HoldReason,
        now_ms: u64,
    ) -> Result<ProviderHold, BrokerError> {
        let _guard = lock(&self.writing);
        let directory = self.directory("holds", run_id);
        let path = directory.join(format!("{}.json", self.extensions_locked(run_id)?.len()));
        if let Some(existing) = read_hold(run_id, &path)? {
            // A previous call may have failed after creation but before fsync.
            fs::File::open(&path)
                .and_then(|file| file.sync_all())
                .map_err(BrokerError::Storage)?;
            sync_directory(&directory)?;
            return Ok(existing);
        }
        self.create(&directory)?;
        let hold = ProviderHold {
            run_id: run_id.into(),
            reason,
            held_at_ms: now_ms,
        };
        write_new_record(&path, &hold)?;
        Ok(hold)
    }

    /// The Run's current hold, when one stands unlifted.
    pub(super) fn held(&self, run_id: &str) -> Result<Option<ProviderHold>, BrokerError> {
        let _guard = lock(&self.writing);
        self.held_locked(run_id)
    }

    /// Durably appends an extension lifting the Run's current hold.
    ///
    /// A retry with the same `request_id` and fields returns the recorded
    /// extension, even after the hold it lifted is gone. `check` sees the
    /// current hold, the extensions so far and the units spent, and decides the
    /// operator policy under the same lock as the append.
    pub(super) fn extend(
        &self,
        run_id: &str,
        request: &ExtensionRequest,
        operator_uid: u32,
        now_ms: u64,
        check: impl FnOnce(&ProviderHold, &[Extension], u32) -> Result<(), ExtensionError>,
    ) -> Result<Extension, BrokerError> {
        let _guard = lock(&self.writing);
        let prior = self.extensions_locked(run_id)?;
        if let Some(recorded) = prior
            .iter()
            .find(|extension| extension.request.request_id == request.request_id)
        {
            return if recorded.request == *request && recorded.operator_uid == operator_uid {
                Ok(recorded.clone())
            } else {
                Err(ExtensionError::Conflict.into())
            };
        }
        let hold = self.held_locked(run_id)?.ok_or(ExtensionError::NotHeld)?;
        let run = self.run_directory(run_id);
        let spent = if run.exists() { count(&run)? } else { 0 };
        check(&hold, &prior, spent)?;
        let directory = self.directory("extensions", run_id);
        self.create(&directory)?;
        let extension = Extension {
            run_id: run_id.into(),
            request: request.clone(),
            operator_uid,
            approved_at_ms: now_ms,
            lifts: hold,
        };
        write_new_record(&directory.join(format!("{}.json", prior.len())), &extension)?;
        Ok(extension)
    }

    /// Every extension of the Run, oldest first.
    pub(super) fn extensions(&self, run_id: &str) -> Result<Vec<Extension>, BrokerError> {
        let _guard = lock(&self.writing);
        self.extensions_locked(run_id)
    }

    fn held_locked(&self, run_id: &str) -> Result<Option<ProviderHold>, BrokerError> {
        let generation = self.extensions_locked(run_id)?.len();
        read_hold(
            run_id,
            &self
                .directory("holds", run_id)
                .join(format!("{generation}.json")),
        )
    }

    /// Reads the contiguous extension records, each bound to the hold it lifted.
    fn extensions_locked(&self, run_id: &str) -> Result<Vec<Extension>, BrokerError> {
        let directory = self.directory("extensions", run_id);
        if !directory.exists() {
            return Ok(Vec::new());
        }
        let holds = self.directory("holds", run_id);
        (0..count(&directory)?)
            .map(|index| {
                let extension: Extension = read_record(&directory.join(format!("{index}.json")))?
                    .ok_or(BrokerError::InvalidGrant)?;
                let lifted = read_hold(run_id, &holds.join(format!("{index}.json")))?;
                if extension.run_id != run_id || lifted.as_ref() != Some(&extension.lifts) {
                    return Err(BrokerError::InvalidGrant);
                }
                Ok(extension)
            })
            .collect()
    }

    fn create(&self, directory: &Path) -> Result<(), BrokerError> {
        if !directory.exists() {
            fs::create_dir(directory).map_err(BrokerError::Storage)?;
            sync_directory(directory.parent().unwrap_or(&self.root))?;
        }
        Ok(())
    }

    fn directory(&self, kind: &str, run_id: &str) -> PathBuf {
        self.root
            .join(kind)
            .join(Digest::of(run_id.as_bytes()).hex())
    }

    fn run_directory(&self, run_id: &str) -> PathBuf {
        self.directory("runs", run_id)
    }
}

fn read_hold(run_id: &str, path: &Path) -> Result<Option<ProviderHold>, BrokerError> {
    let hold: Option<ProviderHold> = read_record(path)?;
    if hold.as_ref().is_some_and(|hold| hold.run_id != run_id) {
        return Err(BrokerError::InvalidGrant);
    }
    Ok(hold)
}

/// The Run's total: the grant plus every extension, never above the ceiling.
pub(super) fn total(max_run_requests: u32, extensions: &[Extension]) -> Result<u32, BrokerError> {
    extensions
        .iter()
        .try_fold(max_run_requests, |total, extension| {
            total.checked_add(extension.request.additional_requests)
        })
        .filter(|total| *total <= MAX_RUN_REQUESTS)
        .ok_or(BrokerError::InvalidGrant)
}

/// Counts attempt records, refusing anything but the contiguous `N.json` names
/// this ledger writes, so a stray or deleted entry cannot silently re-grant units.
fn count(directory: &Path) -> Result<u32, BrokerError> {
    let mut sequences = Vec::new();
    for entry in fs::read_dir(directory).map_err(BrokerError::Storage)? {
        let name = entry.map_err(BrokerError::Storage)?.file_name();
        let sequence = name
            .to_str()
            .and_then(|name| name.strip_suffix(".json"))
            .filter(|digits| digits == &"0" || !digits.starts_with('0'))
            .and_then(|digits| digits.parse::<u32>().ok())
            .filter(|sequence| *sequence < MAX_RUN_REQUESTS)
            .ok_or(BrokerError::InvalidGrant)?;
        sequences.push(sequence);
    }
    sequences.sort_unstable();
    if sequences
        .iter()
        .enumerate()
        .any(|(index, sequence)| u32::try_from(index).ok() != Some(*sequence))
    {
        return Err(BrokerError::InvalidGrant);
    }
    u32::try_from(sequences.len()).map_err(|_| BrokerError::InvalidGrant)
}

#[cfg(test)]
#[path = "provider_requests_tests.rs"]
mod tests;
