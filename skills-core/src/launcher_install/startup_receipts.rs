//! Protected exact startup evidence, persisted before transport can observe it.

use super::{
    LauncherError, LauncherPaths, LauncherSigner, history, read_required_json,
    require_root_metadata, write_json_atomic,
};
use crate::launch_receipt::{ReceiptOutcome, SignedReceipt};
use serde::{Deserialize, Serialize};
use std::{fs, os::unix::fs::DirBuilderExt, path::PathBuf, time::Instant};

/// One exact envelope and the optional observations its signature binds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartupReceipt {
    /// Original canonical signed bytes; replay never reserializes them.
    pub receipt_bytes: Vec<u8>,
    /// Exact supplemental conformance bytes, if the Launch receipt binds them.
    pub report_bytes: Option<Vec<u8>>,
}

fn directory(paths: &LauncherPaths) -> PathBuf {
    paths.state_root.join("startup-receipts")
}

impl LauncherSigner {
    /// Saves an exact signed startup prefix without replacing earlier bytes.
    /// # Errors
    /// Refuses unsafe storage, foreign bindings, conflicting prefixes and
    /// persistence failure. Nothing may be sent before this succeeds.
    pub(crate) fn save_startup_receipts(
        &self,
        incoming: &[StartupReceipt],
        deadline: Instant,
    ) -> Result<(), LauncherError> {
        let first = incoming
            .first()
            .ok_or_else(|| LauncherError::Invalid("empty startup evidence".into()))?;
        let first = SignedReceipt::parse_canonical(&first.receipt_bytes)
            .map_err(|error| LauncherError::Malformed(error.to_string()))?;
        let _lock = history::signing_lock(&self.paths, deadline)?;
        self.require_key_authority(&first.payload.signing_key_id)?;
        let binding = history::load(&self.paths, &first.payload.session_id)?;
        let directory = directory(&self.paths);
        match fs::DirBuilder::new().mode(0o700).create(&directory) {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(error) => return Err(super::io_error("startup evidence directory", error)),
        }
        require_root_metadata(&directory, 0o700, true, "startup evidence directory")?;
        fs::File::open(&self.paths.state_root)
            .and_then(|file| file.sync_all())
            .map_err(|error| super::io_error("startup evidence directory durability", error))?;
        let path = directory.join(format!("{}.json", first.payload.session_id));
        let mut records = incoming.to_vec();
        if path
            .try_exists()
            .map_err(|e| super::io_error("startup evidence", e))?
        {
            require_root_metadata(&path, 0o600, false, "startup evidence")?;
            let old: Vec<StartupReceipt> = read_required_json(&path)?;
            if incoming.len() == 1
                && first.payload.sequence == old.len() as u64
                && first.payload.outcome.is_failed_launch()
            {
                records = old;
                records.extend_from_slice(incoming);
            } else if !records.starts_with(&old) {
                return Err(LauncherError::Invalid(
                    "startup evidence prefix conflict".into(),
                ));
            }
        }
        let receipts = validate(&records)?;
        for receipt in &receipts {
            binding.check(&receipt.payload)?;
        }
        write_json_atomic(&path, &records, 0o600)
    }
}

fn validate(records: &[StartupReceipt]) -> Result<Vec<SignedReceipt>, LauncherError> {
    if records.is_empty() || records.len() > 3 {
        return Err(LauncherError::Invalid(
            "invalid startup evidence length".into(),
        ));
    }
    let receipts = records
        .iter()
        .map(|record| {
            let receipt = SignedReceipt::parse_canonical(&record.receipt_bytes)
                .map_err(|e| LauncherError::Malformed(e.to_string()))?;
            if record
                .report_bytes
                .as_ref()
                .is_some_and(|bytes| bytes.len() > crate::conformance::MAX_REPORT_BYTES)
            {
                return Err(LauncherError::Invalid(
                    "startup report exceeds bound".into(),
                ));
            }
            Ok(receipt)
        })
        .collect::<Result<Vec<_>, LauncherError>>()?;
    let first = &receipts[0].payload;
    crate::launch_receipt::verify_chain(
        &receipts,
        &crate::launch_receipt::ChainAnchor {
            session_id: first.session_id.clone(),
            run_id: first.run_id.clone(),
            release_id: first.release_id.clone(),
            signing_key_id: first.signing_key_id.clone(),
        },
        |_, _, _| true,
    )
    .map_err(|e| LauncherError::Malformed(e.to_string()))?;
    if receipts.iter().any(|receipt| {
        !receipt.payload.outcome.is_failed_launch()
            && !matches!(
                receipt.payload.outcome,
                ReceiptOutcome::Launch { .. } | ReceiptOutcome::Start { .. }
            )
    }) {
        return Err(LauncherError::Invalid("not startup evidence".into()));
    }
    Ok(receipts)
}
