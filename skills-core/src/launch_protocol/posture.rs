//! Display-only posture records. Parsing these bytes cannot create evidence.

use serde::{Deserialize, Serialize};

use super::{ErrorCode, PostureSummary, ProtocolError};
use crate::{
    dossier::NextAction,
    posture::{
        self, DimensionName, DimensionState, EMBEDDED_INSTRUCTIONS_NOTICE, EvidenceKind,
        FailureCode, PROVIDER_DISCLOSURE_NOTICE, Posture, PostureState, Requirement,
    },
};

/// What the retained evidence's last check can establish.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FreshnessBasis {
    /// No successful trusted check is retained.
    Missing,
    /// The broker validated the launch proof, not a new probe on this read.
    Launch,
    /// A previously checked proof no longer establishes the current dimension.
    Invalidated,
}

/// Source-specific freshness, without implying periodic runtime measurement.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceFreshness {
    /// What the retained last check establishes now.
    pub basis: FreshnessBasis,
    /// Broker-clock time of the successful proof validation, never the read time.
    /// `None` means no recorded success; it is not an invented zero timestamp.
    pub last_verified_at_ms: Option<u64>,
}

/// A display-only reference; it cannot be passed as trusted evidence.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatusEvidence {
    /// Trusted source category being described.
    pub kind: EvidenceKind,
    /// Bounded opaque reference, never a path or raw payload.
    pub id: String,
}

/// One dimension's bounded, non-authoritative explanation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DimensionStatus {
    /// Canonical dimension name.
    pub dimension: DimensionName,
    /// Current result of the broker's evaluation.
    pub state: DimensionState,
    /// Requirement this dimension answers.
    pub requirement: Requirement,
    /// Bounded sorted references to retained evidence.
    pub evidence: Vec<StatusEvidence>,
    /// Typed reason when the requirement is not satisfied.
    pub failure_code: Option<FailureCode>,
    /// Fixed action and explanation derived from the failure code.
    pub next_action: NextAction,
    /// Original validation time and its limited meaning.
    pub freshness: EvidenceFreshness,
}

/// All six dimensions as a display record, never an admission input.
/// Parsing status does not make the trusted evaluation deserializable:
/// ```compile_fail
/// let _: louiselm_skills::posture::Posture = serde_json::from_str("{}").unwrap();
/// ```
/// Nor can a peer deserialize the evaluator's trusted inputs:
/// ```compile_fail
/// let _: louiselm_skills::posture::DimensionInput = serde_json::from_str("{}").unwrap();
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PostureStatus {
    /// Aggregate result consistent with the six dimensions.
    pub state: PostureSummary,
    /// Fixed cloud plaintext disclosure.
    pub provider_disclosure_notice: String,
    /// Fixed distinction between admitted supply and executable instructions.
    pub embedded_instructions_notice: String,
    /// Exactly six dimensions in the canonical order.
    pub dimensions: [DimensionStatus; 6],
}

impl PostureStatus {
    /// Projects a trusted evaluation into a separate, non-authoritative record.
    /// Freshness entries follow [`DimensionName::ALL`]. No source is probed.
    #[must_use]
    pub fn from_posture(posture: &Posture, freshness: [EvidenceFreshness; 6]) -> Self {
        let dimensions = posture.dimensions.ordered();
        Self {
            state: match posture.state {
                PostureState::FullyVerified => PostureSummary::FullyVerified,
                PostureState::Waived => PostureSummary::Waived,
                PostureState::Unverified => PostureSummary::Unverified,
            },
            provider_disclosure_notice: posture.provider_disclosure_notice.clone(),
            embedded_instructions_notice: posture.embedded_instructions_notice.clone(),
            dimensions: std::array::from_fn(|index| {
                let (name, dimension) = dimensions[index];
                DimensionStatus {
                    dimension: name,
                    state: dimension.state,
                    requirement: dimension.requirement,
                    evidence: dimension
                        .evidence
                        .iter()
                        .map(|reference| StatusEvidence {
                            kind: reference.kind,
                            id: reference.id.clone(),
                        })
                        .collect(),
                    failure_code: dimension.failure_code,
                    next_action: dimension.next_action.clone(),
                    freshness: freshness[index],
                }
            }),
        }
    }

    /// Validates bounded display fields without constructing trusted evidence.
    ///
    /// # Errors
    /// Refuses missing/reordered dimensions, contradictory summaries, evidence,
    /// freshness or actions, and modified fixed disclosure statements.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        let invalid = || ProtocolError::new(ErrorCode::InvalidRequest, None, None);
        if self.provider_disclosure_notice != PROVIDER_DISCLOSURE_NOTICE
            || self.embedded_instructions_notice != EMBEDDED_INSTRUCTIONS_NOTICE
        {
            return Err(invalid());
        }
        for (dimension, expected) in self.dimensions.iter().zip(DimensionName::ALL) {
            if dimension.dimension != expected {
                return Err(invalid());
            }
            dimension.validate()?;
        }
        let any_failed = self
            .dimensions
            .iter()
            .any(|d| d.state == DimensionState::Failed);
        let any_waived = self
            .dimensions
            .iter()
            .any(|d| d.state == DimensionState::Waived);
        let expected = if any_failed {
            PostureSummary::Unverified
        } else if any_waived {
            PostureSummary::Waived
        } else {
            PostureSummary::FullyVerified
        };
        let initializing = self.state == PostureSummary::Pending
            && self.dimensions.iter().all(|d| {
                d.state == DimensionState::Failed
                    && d.failure_code == Some(FailureCode::EvidenceMissing)
                    && d.freshness.basis == FreshnessBasis::Missing
                    && d.evidence.is_empty()
            });
        if self.state != expected && !initializing {
            return Err(invalid());
        }
        Ok(())
    }
}

impl DimensionStatus {
    fn validate(&self) -> Result<(), ProtocolError> {
        let invalid = || ProtocolError::new(ErrorCode::InvalidRequest, None, None);
        if self.requirement != self.dimension.requirement()
            || (self.state == DimensionState::Verified) != self.failure_code.is_none()
            || self
                .failure_code
                .is_some_and(|code| !self.dimension.accepts_failure(code))
            || self.next_action != posture::next_action(self.failure_code)
            || self.evidence.len() > 16
            || self.evidence.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err(invalid());
        }
        let mut primary = false;
        let mut waiver = false;
        for reference in &self.evidence {
            posture::validate_identifier("status evidence", &reference.id)
                .map_err(|_| invalid())?;
            primary |= self.dimension.accepts_evidence(reference.kind);
            waiver |= reference.kind == EvidenceKind::WaiverReceipt;
            if !self.dimension.accepts_evidence(reference.kind)
                && !matches!(
                    reference.kind,
                    EvidenceKind::WaiverReceipt | EvidenceKind::AuditReceipt
                )
            {
                return Err(invalid());
            }
        }
        if (self.state == DimensionState::Verified && !primary)
            || (self.state == DimensionState::Waived) != waiver
        {
            return Err(invalid());
        }
        match self.freshness.basis {
            FreshnessBasis::Missing => {
                if self.freshness.last_verified_at_ms.is_some()
                    || self.state == DimensionState::Verified
                {
                    return Err(invalid());
                }
            }
            FreshnessBasis::Launch | FreshnessBasis::Invalidated => {
                if self.freshness.last_verified_at_ms.is_none()
                    || !primary
                    || (self.freshness.basis == FreshnessBasis::Invalidated
                        && self.state == DimensionState::Verified)
                {
                    return Err(invalid());
                }
            }
        }
        Ok(())
    }
}
