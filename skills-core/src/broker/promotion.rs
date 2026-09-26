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

/// Exact operator review for one use of tainted workspace output.
/// A digest of this preview grants nothing until the trusted operator commits it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromotionReview {
    /// Fixed closed review schema.
    pub schema: String,
    /// Exact request, including the verification record and selected bytes.
    pub request_digest: String,
    /// Normalized final-tree digest shown to the operator.
    pub output_digest: String,
    /// Canonical immutable Session output taint record.
    pub taint_digest: String,
    /// Exact permitted action; this review cannot authorize another operation.
    pub action: String,
    /// Pinned operator checkout identity.
    pub destination: DestinationIdentity,
}

impl PromotionReview {
    fn new(request: &PromotionRequest, taint_digest: &str) -> Result<Self, BrokerError> {
        request.validate()?;
        if !Digest::parse(taint_digest).is_ok_and(|digest| digest.to_string() == taint_digest) {
            return Err(BrokerError::InvalidGrant);
        }
        Ok(Self {
            schema: "louiselm.workspace.promotion-review/1".into(),
            request_digest: Digest::of(
                &serde_json::to_vec(request).map_err(|_| BrokerError::InvalidGrant)?,
            )
            .to_string(),
            output_digest: request.job.result_digest.clone(),
            taint_digest: taint_digest.into(),
            action: "workspace_promotion".into(),
            destination: request.destination,
        })
    }

    /// Canonical digest the operator must provide for this exact use.
    /// # Errors
    /// Refuses a review that cannot be serialized canonically.
    pub fn digest(&self) -> Result<String, BrokerError> {
        Ok(
            Digest::of(&serde_json::to_vec(self).map_err(|_| BrokerError::InvalidGrant)?)
                .to_string(),
        )
    }

    pub(crate) fn validate(&self, request: &PromotionRequest) -> Result<(), BrokerError> {
        if *self != Self::new(request, &self.taint_digest)? {
            return Err(BrokerError::RequestMismatch);
        }
        Ok(())
    }

    pub(crate) fn output_provenance(&self) -> Result<OutputProvenance, BrokerError> {
        let mut provenance = OutputProvenance::tainted(&self.taint_digest);
        provenance.clean_review_refs.push(self.digest()?);
        Ok(provenance)
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
    Prepared { review: Option<PromotionReview> },
    Granted { index: usize },
    Recorded { index: usize },
    Finished { status: PromotionStatus },
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub(crate) enum OperatorMessage {
    Commit {
        request_digest: String,
        review_digest: Option<String>,
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
        let mut output_provenance = self
            .workspace_output_provenance_for_session(&request.producer_session_id)
            .unwrap_or_else(|_| OutputProvenance::unknown());
        let review = read_record::<PromotionReview>(&directory.join("review.json"))?;
        if let Some(review) = &review {
            review.validate(&request)?;
            if output_provenance.taint_digest.as_deref() == Some(review.taint_digest.as_str()) {
                output_provenance.clean_review_refs.push(review.digest()?);
            }
        }
        let mut granted = 0;
        let mut completed = 0;
        // At most one effect per removed or resulting file, each with two records.
        let count = fs::read_dir(&directory)
            .map_err(BrokerError::Storage)?
            .take(crate::workspace::MAX_FILES * 4 + 4)
            .collect::<Result<Vec<_>, _>>()
            .map_err(BrokerError::Storage)?
            .len();
        if count > crate::workspace::MAX_FILES * 4 + 3 {
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
        if count
            != 1 + usize::from(review.is_some())
                + granted
                + completed
                + usize::from(result.is_some())
        {
            return Err(BrokerError::InvalidGrant);
        }
        if let Some(result) = result {
            if !result.complete || result.completed_steps != completed || completed != granted {
                return Err(BrokerError::InvalidGrant);
            }
            let expected = match &review {
                Some(review) => review.output_provenance()?,
                None => OutputProvenance::untainted(),
            };
            if result.output_provenance != expected {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[expect(
        clippy::unwrap_used,
        reason = "Test fixtures assert the complete review binding and may panic on malformed setup."
    )]
    fn tainted_review_binds_output_taint_action_and_destination() {
        let digest = Digest::of(b"fixture").to_string();
        let request = PromotionRequest {
            schema: "louiselm.workspace.promotion/1".into(),
            request_id: "one-use".into(),
            producer_session_id: "producer".into(),
            verifier_session_id: "verifier".into(),
            verification_digest: digest.clone(),
            job: JobPreview {
                schema: "louiselm.workspace.verification-preview/1".into(),
                state: "prepared".into(),
                job_digest: digest.clone(),
                snapshot_digest: digest.clone(),
                base_digest: digest.clone(),
                bundle_digest: digest.clone(),
                result_digest: digest.clone(),
                plan_digest: digest,
                output_provenance: OutputProvenance::unknown(),
                command_count: 1,
            },
            destination: DestinationIdentity {
                device: 1,
                inode: 2,
                uid: 1000,
            },
            expires_at_ms: 30_000,
        };
        let taint = Digest::of(b"taint").to_string();
        let review = PromotionReview::new(&request, &taint).unwrap();
        assert_eq!(review.output_digest, request.job.result_digest);
        assert_eq!(review.taint_digest, taint);
        assert_eq!(review.action, "workspace_promotion");
        assert_eq!(review.destination, request.destination);
        assert_ne!(
            review.digest().unwrap(),
            PromotionReview::new(&request, &Digest::of(b"other taint").to_string())
                .unwrap()
                .digest()
                .unwrap()
        );
        let mut changed = request;
        changed.destination.inode += 1;
        assert_ne!(
            review.digest().unwrap(),
            PromotionReview::new(&changed, &taint)
                .unwrap()
                .digest()
                .unwrap()
        );
    }
}
