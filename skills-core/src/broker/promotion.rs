//! Exact operator promotion, with durable per-effect admission and observations.

mod service;

use super::{BrokerError, BrokerService, read_record, record_name};
use crate::{
    Digest,
    workspace::{
        promotion::{ApplicationResult, DestinationIdentity},
        provenance::OutputProvenance,
        verification::JobPreview,
    },
};
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};

/// Exact operator selection; deserializing it never grants permission to write.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromotionRequest {
    /// Fixed closed request schema.
    pub schema: String,
    /// Single-use operation identity, retained across retries and restarts.
    pub request_id: String,
    /// Original Session whose workspace bytes are being promoted.
    pub producer_session_id: String,
    /// Distinct verifier whose canonical evidence is selected.
    pub verifier_session_id: String,
    /// Digest of the canonical complete verification record.
    pub verification_digest: String,
    /// Exact job/snapshot/base/bundle/result/plan selection shown for approval.
    pub job: JobPreview,
    /// Actual pinned operator directory; root never receives its pathname.
    pub destination: DestinationIdentity,
    /// Exclusive absolute expiry, unchanged by retries.
    pub expires_at_ms: u64,
}

impl PromotionRequest {
    pub(crate) fn validate(&self) -> Result<(), BrokerError> {
        record_name(&self.request_id)?;
        record_name(&self.producer_session_id)?;
        record_name(&self.verifier_session_id)?;
        if self.producer_session_id == self.verifier_session_id {
            return Err(BrokerError::InvalidGrant);
        }
        Digest::parse(&self.verification_digest).map_err(|_| BrokerError::InvalidGrant)?;
        for value in [
            &self.job.job_digest,
            &self.job.snapshot_digest,
            &self.job.base_digest,
            &self.job.bundle_digest,
            &self.job.result_digest,
            &self.job.plan_digest,
        ] {
            Digest::parse(value).map_err(|_| BrokerError::InvalidGrant)?;
        }
        if self.job.schema != "louiselm.workspace.verification-preview/1"
            || self.job.state != "prepared"
            || !(1..=32).contains(&self.job.command_count)
            || self.job.output_provenance.code
                != crate::workspace::provenance::OutputProvenanceCode::Unknown
            || self.job.output_provenance.validate().is_err()
        {
            return Err(BrokerError::InvalidGrant);
        }
        if self.schema != "louiselm.workspace.promotion/1"
            || self.destination.uid == 0
            || self.expires_at_ms == 0
        {
            return Err(BrokerError::InvalidGrant);
        }
        Ok(())
    }
}

/// Durable observations, without interpreting an uncertain effect as rolled back.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", deny_unknown_fields)]
pub enum PromotionStatus {
    /// No mutation authority has been spent.
    NotRequested,
    /// No complete durable result; a granted step may have changed the checkout.
    Unknown {
        /// Effects for which the broker durably spent authority.
        granted_steps: usize,
        /// Effects acknowledged as synchronized by the authenticated applicator.
        completed_steps: usize,
        /// Current provenance of the original producer's workspace output.
        output_provenance: OutputProvenance,
    },
    /// Exact synchronized outcome from the authenticated operator applicator.
    Completed {
        /// Historical effect evidence; subsequent quarantine does not undo writes.
        result: ApplicationResult,
        /// Historical writes remain complete even when later quarantine taints them.
        output_provenance: OutputProvenance,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub(crate) enum Reply {
    Prepared,
    Granted { index: usize },
    Recorded { index: usize },
    Finished { status: PromotionStatus },
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub(crate) enum OperatorMessage {
    Commit {
        request_digest: String,
    },
    Step {
        event: crate::workspace::promotion::StepEvent,
    },
    Complete {
        result: ApplicationResult,
    },
}

impl BrokerService {
    pub(super) fn promotion_path(&self, id: &str) -> Result<PathBuf, BrokerError> {
        record_name(id)?;
        Ok(self.authorizations().root.join("promotions").join(id))
    }

    /// Inspects actual durable effects without reauthorizing or replaying them.
    /// Run on the broker worker. Completed is historical, not current eligibility.
    /// # Errors
    /// Refuses malformed identities, corrupt journals and unavailable persistence.
    pub fn promotion_status(&self, id: &str) -> Result<PromotionStatus, BrokerError> {
        let directory = self.promotion_path(id)?;
        let Some(request) = read_record::<PromotionRequest>(&directory.join("request.json"))?
        else {
            if directory.try_exists().map_err(BrokerError::Storage)? {
                return Err(BrokerError::InvalidGrant);
            }
            return Ok(PromotionStatus::NotRequested);
        };
        request.validate()?;
        if request.request_id != id {
            return Err(BrokerError::RequestMismatch);
        }
        // Inspection must preserve the effect count even if the producer's
        // provenance evidence has since become unavailable.
        let output_provenance = self
            .workspace_output_provenance_for_session(&request.producer_session_id)
            .unwrap_or_else(|_| OutputProvenance::unknown());
        let mut granted = 0;
        let mut completed = 0;
        // At most one effect per removed or resulting file, each with two records.
        let count = fs::read_dir(&directory)
            .map_err(BrokerError::Storage)?
            .take(crate::workspace::MAX_FILES * 4 + 3)
            .collect::<Result<Vec<_>, _>>()
            .map_err(BrokerError::Storage)?
            .len();
        if count > crate::workspace::MAX_FILES * 4 + 2 {
            return Err(BrokerError::InvalidGrant);
        }
        for index in 0..crate::workspace::MAX_FILES * 2 {
            let Some(stored) = read_record::<usize>(&directory.join(format!("{index}.grant")))?
            else {
                break;
            };
            if stored != index {
                return Err(BrokerError::InvalidGrant);
            }
            granted += 1;
            if let Some(done) = read_record::<usize>(&directory.join(format!("{index}.done")))? {
                if done != index || completed != index {
                    return Err(BrokerError::InvalidGrant);
                }
                completed += 1;
            }
        }
        let result = read_record::<ApplicationResult>(&directory.join("result.json"))?;
        if count != 1 + granted + completed + usize::from(result.is_some()) {
            return Err(BrokerError::InvalidGrant);
        }
        if let Some(result) = result {
            if !result.complete || result.completed_steps != completed || completed != granted {
                return Err(BrokerError::InvalidGrant);
            }
            Ok(PromotionStatus::Completed {
                result,
                output_provenance,
            })
        } else {
            Ok(PromotionStatus::Unknown {
                granted_steps: granted,
                completed_steps: completed,
                output_provenance,
            })
        }
    }
}
