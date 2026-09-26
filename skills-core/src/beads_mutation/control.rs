//! Operator attestations clarify uncertainty without rewriting the original outcome.
use super::{
    BeadsEscalation, BeadsMutationOutcome, BeadsMutationStatus, canonical_uuid, identifier,
};
use crate::workspace::provenance::OutputProvenance;
use serde::{Deserialize, Serialize};

/// An operator's conclusion from independently inspecting canonical Beads evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BeadsReconciliation {
    /// The operator found evidence of the original effect.
    Applied,
    /// The operator established that the original effect did not occur.
    NotApplied,
}

/// Restricted decisions on existing records; none authorize or repeat a mutation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BeadsControlDecision {
    /// Dismiss a capability escalation without granting its requested authority.
    Dismiss,
    /// Attach an explicit operator attestation to an uncertain or failed attempt.
    Reconcile {
        /// Operator-confirmed conclusion, not a newly observed process result.
        outcome: BeadsReconciliation,
        /// Digest of the independently retained evidence; raw evidence is never stored here.
        evidence_digest: String,
    },
}

impl BeadsControlDecision {
    /// Validates bounded evidence identity without interpreting or trusting evidence bytes.
    #[must_use]
    pub fn valid(&self) -> bool {
        match self {
            Self::Dismiss => true,
            Self::Reconcile {
                evidence_digest, ..
            } => digest_valid(evidence_digest),
        }
    }
}

/// Immutable attestation kept alongside the original mutation outcome.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BeadsResolution {
    /// Operator-confirmed interpretation of the original attempt.
    pub outcome: BeadsReconciliation,
    /// Exact evidence fingerprint supplied by the authenticated operator.
    pub evidence_digest: String,
    /// Kernel-authenticated operator identity, never Session-supplied.
    pub operator_uid: u32,
    /// Broker timestamp of the first durable decision.
    pub decided_at_ms: u64,
}

/// Bounded operator detail for one operation; no raw mutation text or output.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BeadsInspectionDetail {
    /// A write attempt and its unchanged original result.
    Mutation {
        /// Exact canonical project binding.
        project_digest: String,
        /// Exact original request fingerprint for comparison with independently retained evidence.
        request_digest: String,
        /// Original broker observation; reconciliation never changes its outcome.
        status: BeadsMutationStatus,
        /// Current trust in Session-authored content, derived from broker taint evidence.
        output_provenance: OutputProvenance,
        /// Optional explicit operator attestation, never an automatic retry.
        resolution: Option<BeadsResolution>,
    },
    /// A missing-capability condition, independent of any write attempt.
    Escalation {
        /// The exact requested expansion.
        escalation: BeadsEscalation,
        /// Whether the operator dismissed this condition without granting it.
        dismissed: bool,
    },
}

/// Durable operation inspection, not live Session status or mutation authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BeadsInspection {
    /// Exact requested operation UUID.
    pub operation_id: String,
    /// Original authenticated Session.
    pub session_id: String,
    /// Original Run binding.
    pub run_id: String,
    /// Original capability-envelope revision.
    pub envelope_revision: u64,
    /// The original result and any separately attributed operator decision.
    pub detail: BeadsInspectionDetail,
}

impl BeadsInspection {
    /// Rejects contradictory or malformed serialized inspection output.
    #[must_use]
    pub fn valid(&self) -> bool {
        canonical_uuid(&self.operation_id)
            && identifier(&self.session_id)
            && identifier(&self.run_id)
            && match &self.detail {
                BeadsInspectionDetail::Mutation {
                    project_digest,
                    request_digest,
                    status,
                    output_provenance,
                    resolution,
                } => {
                    digest_valid(project_digest)
                        && digest_valid(request_digest)
                        && status.valid()
                        && output_provenance.validate().is_ok()
                        && status.operation_id == self.operation_id
                        && resolution.as_ref().is_none_or(|value| {
                            status.outcome != BeadsMutationOutcome::Completed
                                && digest_valid(&value.evidence_digest)
                                && value.operator_uid != 0
                                && value.decided_at_ms > 0
                        })
                }
                BeadsInspectionDetail::Escalation { escalation, .. } => {
                    escalation.valid() && escalation.operation_id == self.operation_id
                }
            }
    }
}

fn digest_valid(value: &str) -> bool {
    crate::Digest::parse(value).is_ok_and(|digest| digest.to_string() == value)
}
