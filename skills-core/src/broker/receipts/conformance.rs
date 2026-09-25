//! Exact observations bound by an authenticated launch's admission digest.

use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
};

use super::ReceiptStore;
use crate::{
    Digest,
    broker::{BrokerError, corrupt, sync_directory},
    conformance::{MAX_REPORT_BYTES, Report, ReportResult, Scope, admission::Condition},
    launch_protocol::LaunchAuthorization,
    launch_receipt::{ConformanceEvidence, ReceiptOutcome, SignedReceipt},
};

impl ReceiptStore {
    /// Read the exact observations bound by this Session's signed admission.
    ///
    /// Revalidates the chain and report; no report means the admission bound none.
    /// Historical observations do not establish current host conformance. This
    /// blocking inspection discloses producer observations to its trusted caller;
    /// public status must expose only bounded references, never these raw bytes.
    ///
    /// # Errors
    /// Refuses foreign/invalid authorization, unverified history, missing bound
    /// observations, changed bytes or storage failure.
    pub fn conformance_report<F>(
        &self,
        authorization: &LaunchAuthorization,
        mut verify: F,
    ) -> Result<Option<Vec<u8>>, BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        let chain = self.verified_chain(authorization, &mut verify)?;
        let launch = chain.first().ok_or(BrokerError::ReceiptUnauthorized)?;
        self.read_conformance_report(launch)
    }

    pub(super) fn retain_conformance_report(
        &self,
        receipt: &SignedReceipt,
        bytes: Option<&[u8]>,
    ) -> Result<(), BrokerError> {
        validate(receipt, bytes)?;
        let Some(bytes) = bytes else { return Ok(()) };
        let directory = self.root.join("conformance");
        fs::create_dir_all(&directory).map_err(BrokerError::Storage)?;
        sync_directory(&self.root)?;
        // Publish complete private bytes before publishing the receipt. An
        // interrupted append may leave an orphan report, never a false ACK.
        let mut pending =
            tempfile::NamedTempFile::new_in(&directory).map_err(BrokerError::Storage)?;
        pending.write_all(bytes).map_err(BrokerError::Storage)?;
        pending.as_file().sync_all().map_err(BrokerError::Storage)?;
        match pending.persist_noclobber(self.conformance_path(receipt)?) {
            Ok(_) => (),
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                // Retry after interruption may reuse only the exact same report.
                // A corrupt or different orphan is preserved for inspection.
                if self.read_conformance_report(receipt)?.as_deref() != Some(bytes) {
                    return Err(corrupt("retained conformance report conflicts"));
                }
                self.sync_conformance_report(receipt)?;
            }
            Err(error) => return Err(BrokerError::Storage(error.error)),
        }
        sync_directory(&directory)
    }

    pub(in crate::broker) fn read_conformance_report(
        &self,
        receipt: &SignedReceipt,
    ) -> Result<Option<Vec<u8>>, BrokerError> {
        let Some(_) = digest(receipt) else {
            validate(receipt, None)?;
            return Ok(None);
        };
        let mut bytes = Vec::new();
        self.open_conformance_report(receipt)?
            .take(MAX_REPORT_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(BrokerError::Storage)?;
        validate(receipt, Some(&bytes))
            .map_err(|_| corrupt("stored conformance report contradicts admission"))?;
        Ok(Some(bytes))
    }

    pub(super) fn sync_conformance_report(
        &self,
        receipt: &SignedReceipt,
    ) -> Result<(), BrokerError> {
        if digest(receipt).is_some() {
            self.open_conformance_report(receipt)?
                .sync_all()
                .map_err(BrokerError::Storage)?;
            sync_directory(&self.root.join("conformance"))?;
        }
        Ok(())
    }

    fn conformance_path(&self, receipt: &SignedReceipt) -> Result<PathBuf, BrokerError> {
        // Reuse the chain's identifier validation; never turn a peer string into
        // a path component without it.
        self.session_directory(&receipt.payload.session_id)?;
        Ok(self
            .root
            .join("conformance")
            .join(format!("{}.json", receipt.payload.session_id)))
    }

    fn open_conformance_report(&self, receipt: &SignedReceipt) -> Result<File, BrokerError> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(
                (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK)
                    .bits()
                    .cast_signed(),
            )
            .open(self.conformance_path(receipt)?)
            .map_err(BrokerError::Storage)?;
        if !file.metadata().map_err(BrokerError::Storage)?.is_file() {
            return Err(corrupt("stored conformance report is not a regular file"));
        }
        Ok(file)
    }
}

fn digest(receipt: &SignedReceipt) -> Option<&str> {
    match &receipt.payload.outcome {
        ReceiptOutcome::Launch { evidence, .. } => match &evidence.conformance {
            ConformanceEvidence::Certified { report_digest }
            | ConformanceEvidence::Waived {
                report_digest: Some(report_digest),
                ..
            } => Some(report_digest),
            _ => None,
        },
        _ => None,
    }
}

fn validate(receipt: &SignedReceipt, bytes: Option<&[u8]>) -> Result<(), BrokerError> {
    let admission = match &receipt.payload.outcome {
        ReceiptOutcome::Launch { evidence, .. } => &evidence.conformance,
        _ => &ConformanceEvidence::Unevaluated,
    };
    validate_report(admission, bytes)
}

pub(in crate::broker) fn validate_report(
    admission: &ConformanceEvidence,
    bytes: Option<&[u8]>,
) -> Result<(), BrokerError> {
    let refusal = BrokerError::ConformanceReport;
    if matches!(
        admission,
        ConformanceEvidence::Waived {
            condition: Condition::GuardUnavailable,
            ..
        }
    ) {
        return Err(refusal("guard_unavailable_cannot_be_waived"));
    }
    if matches!(
        admission,
        ConformanceEvidence::Waived {
            condition: Condition::ContainmentFailure,
            ..
        }
    ) {
        return Err(refusal("containment_failure_cannot_be_waived"));
    }
    let digest = match admission {
        ConformanceEvidence::Certified { report_digest }
        | ConformanceEvidence::Waived {
            report_digest: Some(report_digest),
            ..
        } => Some(report_digest.as_str()),
        _ => None,
    };
    match (digest, bytes) {
        (None, None) => Ok(()),
        (None, Some(_)) => Err(refusal("unexpected_report")),
        (Some(_), None) => Err(refusal("missing_report")),
        (Some(expected), Some(bytes)) => {
            if bytes.len() > MAX_REPORT_BYTES {
                return Err(refusal("oversized_report"));
            }
            if Digest::of(bytes).to_string() != expected {
                return Err(refusal("report_digest_mismatch"));
            }
            let report = Report::parse_canonical(bytes).map_err(|_| refusal("invalid_report"))?;
            if report.scope != Scope::InstalledHost {
                return Err(refusal("report_is_not_installed_host_evidence"));
            }
            let result = report.result().map_err(|_| refusal("invalid_report"))?;
            if matches!(result, ReportResult::Failed(_))
                || (matches!(admission, ConformanceEvidence::Certified { .. })
                    && result != ReportResult::Passed)
                || matches!(
                    admission,
                    ConformanceEvidence::Waived {
                        condition: Condition::Missing,
                        ..
                    }
                )
            {
                return Err(refusal("report_contradicts_admission"));
            }
            Ok(())
        }
    }
}
