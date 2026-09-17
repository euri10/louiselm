//! Explicit operator access to retained conformance evidence, never live attestation.

use serde::{Deserialize, Serialize};

use super::{
    BrokerError, BrokerService, corrupt, is_record_identifier, read_record,
    receipts::validate_report, record_name,
};
use crate::{
    conformance::{MAX_REPORT_BYTES, admission::Condition},
    launch_protocol::{
        ConformanceCheck, ConformanceUpdate, conformance::validate_admission_history,
    },
    launch_receipt::{ConformanceEvidence, ReceiptOutcome},
};

const SCHEMA: &str = "louiselm.operator-conformance-evidence/1";
// JSON string escaping can expand each report byte sixfold. Keep ordinary
// protocol requests at their existing bound; only this explicit response grows.
pub(super) const MAX_INSPECTION_BYTES: usize =
    6 * MAX_REPORT_BYTES + crate::launch_protocol::MAX_PROTOCOL_MESSAGE_BYTES;

/// Last retained supervisor observation, with authority and process identities removed.
/// A historical successful check may have expired; this record grants nothing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConformanceObservation {
    /// Source sequence, not a counter incremented by inspection.
    pub sequence: u64,
    /// Original source observation time.
    pub observed_at_ms: u64,
    /// Original last successful check time, if one exists.
    pub last_success_at_ms: Option<u64>,
    /// Whether the source withheld authority pending explicit recovery.
    pub suspended: bool,
    /// Exact sanitized condition or evidence observed by the source.
    pub check: ConformanceCheck,
}

/// The actual historical waiver decision, with operator and authorization identifiers removed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConformanceWaiverSummary {
    /// Only this condition was waived for the inspected Session.
    pub condition: Condition,
    /// Original exclusive expiry; inspection never renews it.
    pub expires_at_ms: u64,
    /// Digest of the durable operator waiver receipt.
    pub receipt_digest: String,
}

impl From<ConformanceUpdate> for ConformanceObservation {
    fn from(update: ConformanceUpdate) -> Self {
        Self {
            sequence: update.sequence,
            observed_at_ms: update.observed_at_ms,
            last_success_at_ms: update.last_success_at_ms,
            suspended: update.suspended,
            check: update.check,
        }
    }
}

/// Bounded historical evidence returned only to the authenticated operator.
/// Consult ordinary Session status for current posture, mechanical state and
/// allowed actions. Reading this record runs no probes and authorizes no recovery.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConformanceInspection {
    /// Exact schema identifier.
    pub schema: String,
    /// Session named by the operator.
    pub session_id: String,
    /// Original owning Run.
    pub run_id: String,
    /// Immutable decision in the signed sequence-zero receipt.
    pub admission: ConformanceEvidence,
    /// Actual approval used by a waived admission, even after its expiry.
    pub waiver: Option<ConformanceWaiverSummary>,
    /// Exact UTF-8 observation bytes as a JSON string, or explicit absence.
    pub report: Option<String>,
    /// Last retained check and suspension cause; absence never means success.
    pub last_check: Option<ConformanceObservation>,
}

impl ConformanceInspection {
    fn validate(&self) -> Result<(), BrokerError> {
        if self.schema != SCHEMA
            || !is_record_identifier(&self.session_id)
            || !is_record_identifier(&self.run_id)
        {
            return Err(corrupt("invalid conformance inspection"));
        }
        validate_admission_history(&self.admission)?;
        validate_report(&self.admission, self.report.as_deref().map(str::as_bytes))?;
        match (&self.admission, &self.waiver) {
            (ConformanceEvidence::Waived { condition, .. }, Some(waiver))
                if *condition == waiver.condition
                    && waiver.expires_at_ms > 0
                    && crate::Digest::parse(&waiver.receipt_digest)
                        .is_ok_and(|digest| digest.to_string() == waiver.receipt_digest) => {}
            (ConformanceEvidence::Waived { .. }, _) | (_, Some(_)) => {
                return Err(corrupt("contradictory conformance waiver"));
            }
            _ => {}
        }
        if let Some(last) = &self.last_check {
            if last.sequence == 0
                || last
                    .last_success_at_ms
                    .is_some_and(|at| at > last.observed_at_ms)
                || matches!(last.check, ConformanceCheck::Invalid { .. }) && !last.suspended
            {
                return Err(corrupt("invalid conformance observation"));
            }
            if let ConformanceCheck::Current { evidence } = &last.check {
                validate_admission_history(evidence)?;
                if *evidence == ConformanceEvidence::Unevaluated
                    || last.last_success_at_ms != Some(last.observed_at_ms)
                {
                    return Err(corrupt("invalid successful conformance observation"));
                }
            }
        }
        Ok(())
    }

    /// Serialize validated canonical inspection bytes without changing report bytes.
    /// # Errors
    /// Refuses contradictory, malformed or oversized evidence.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, BrokerError> {
        self.validate()?;
        let bytes =
            serde_json::to_vec(self).map_err(|_| corrupt("invalid conformance inspection"))?;
        if bytes.len() > MAX_INSPECTION_BYTES {
            return Err(corrupt("oversized conformance inspection"));
        }
        Ok(bytes)
    }

    /// Parse exact canonical historical evidence; authentication belongs to transport.
    /// # Errors
    /// Refuses unknown fields, altered reports, invalid observations or excessive input.
    pub fn parse_canonical(bytes: &[u8]) -> Result<Self, BrokerError> {
        if bytes.len() > MAX_INSPECTION_BYTES {
            return Err(corrupt("oversized conformance inspection"));
        }
        let record: Self = serde_json::from_slice(bytes)
            .map_err(|_| corrupt("malformed conformance inspection"))?;
        if record.canonical_bytes()? != bytes {
            return Err(corrupt("noncanonical conformance inspection"));
        }
        Ok(record)
    }
}

impl BrokerService {
    /// Read one Session's retained evidence for a trusted operator caller.
    /// Installed storage revalidates the signed chain and exact report. This
    /// blocking read requires no live supervisor, changes no authority and runs
    /// no probes. `None` means unknown Session, not missing evidence.
    /// # Errors
    /// Refuses quarantined, unverified, corrupt or unavailable evidence.
    pub fn inspect_conformance(
        &self,
        session_id: &str,
    ) -> Result<Option<ConformanceInspection>, BrokerError> {
        let Some(pending) = self.authorizations().consumed_for_session(session_id)? else {
            return Ok(None);
        };
        // Inspection cannot quarantine, restore or otherwise mutate authority.
        // Enforce existing refusals without the lifecycle path's side effects.
        self.receipts().check_authority()?;
        let failure = self
            .authorizations()
            .root
            .join("history-failures")
            .join(record_name(session_id)?);
        if self.receipts().key_revocation(session_id)?.is_some()
            || read_record::<super::attention::AttentionCondition>(&failure)?.is_some()
        {
            return Err(BrokerError::ReceiptUnauthorized);
        }
        let authorization = pending.launch_authorization();
        let chain = self.receipts().inspection_chain(&authorization)?;
        let launch = chain.first().ok_or(BrokerError::ReceiptUnauthorized)?;
        let ReceiptOutcome::Launch { evidence, .. } = &launch.payload.outcome else {
            return Err(BrokerError::ReceiptUnauthorized);
        };
        let report = self
            .receipts()
            .read_conformance_report(launch)?
            .map(String::from_utf8)
            .transpose()
            .map_err(|_| corrupt("invalid report encoding"))?;
        let record =
            ConformanceInspection {
                schema: SCHEMA.into(),
                session_id: authorization.session_id.clone(),
                run_id: authorization.run_id.clone(),
                admission: evidence.conformance.clone(),
                waiver: if matches!(evidence.conformance, ConformanceEvidence::Waived { .. }) {
                    authorization.conformance.waiver.as_ref().map(|waiver| {
                        ConformanceWaiverSummary {
                            condition: waiver.condition,
                            expires_at_ms: waiver.expires_at_ms,
                            receipt_digest: waiver.receipt_digest.clone(),
                        }
                    })
                } else {
                    None
                },
                report,
                last_check: self
                    .receipts()
                    .current_conformance(&authorization)?
                    .map(|record| record.update.into()),
            };
        record.validate()?;
        Ok(Some(record))
    }
}
