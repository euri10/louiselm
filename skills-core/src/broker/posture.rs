//! Runtime posture from the broker's authenticated, durable launch history.

use super::{BrokerError, BrokerService};
use crate::{
    launch_protocol::{
        BrokerConnection, EvidenceFreshness, FreshnessBasis, LaunchAuthorization, PostureStatus,
        SupervisorStatus,
    },
    launch_receipt::{ReceiptOutcome, SessionState},
    posture::{DimensionInput, DimensionName, EvidenceKind, EvidenceRef, FailureCode, Posture},
};

/// Private validated facts, never constructed from a status response.
/// Original signed admission bytes remain in `ReceiptStore` and are not rewritten.
pub(super) struct RuntimePostureEvidence {
    measurement: EvidenceRef,
    release: EvidenceRef,
    checked_at_ms: Option<u64>,
}

impl BrokerService {
    /// Restores facts only at authenticated launch/reattachment, never during a read.
    pub(super) fn retain_launch_posture<F>(
        &self,
        authorization: &LaunchAuthorization,
        verify: &mut F,
    ) -> Result<RuntimePostureEvidence, BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        // The chain validates the exact subject, request, revision, trusted
        // release/signature, sequence, and actual Agent identity at Start.
        // Neither a bare measurement digest nor an unrelated signed record is proof.
        let chain = self.receipts().verified_chain(authorization, verify)?;
        let launch = chain.first().ok_or(BrokerError::ReceiptUnauthorized)?;
        let start = chain.get(1).ok_or(BrokerError::ReceiptUnauthorized)?;
        let ReceiptOutcome::Launch { evidence, .. } = &launch.payload.outcome else {
            return Err(BrokerError::ReceiptUnauthorized);
        };
        if !matches!(start.payload.outcome, ReceiptOutcome::Start { .. }) {
            return Err(BrokerError::ReceiptUnauthorized);
        }
        let checked_at_ms = self.start_receipt_stored_at(authorization)?;
        Ok(RuntimePostureEvidence {
            measurement: EvidenceRef::new(
                EvidenceKind::RuntimeMeasurement,
                &evidence.runtime_measurement_digest,
            )?,
            release: EvidenceRef::new(EvidenceKind::ReleaseManifest, &launch.payload.release_id)?,
            checked_at_ms,
        })
    }
}

impl RuntimePostureEvidence {
    /// Pure projection of retained proof and already-read mechanical facts.
    pub(super) fn status(
        &self,
        supervisor: &SupervisorStatus,
        quarantined: bool,
        now_ms: u64,
    ) -> Result<PostureStatus, BrokerError> {
        let missing = EvidenceFreshness {
            basis: FreshnessBasis::Missing,
            last_verified_at_ms: None,
        };
        let mut freshness = [missing; 6];
        let mut inputs = Vec::with_capacity(6);
        for (index, dimension) in DimensionName::ALL.into_iter().enumerate() {
            if dimension != DimensionName::Runtime || self.checked_at_ms.is_none() {
                inputs.push(DimensionInput::failed(
                    dimension,
                    FailureCode::EvidenceMissing,
                    vec![],
                ));
                continue;
            }
            // This is the launch proof's limited lifetime basis, not a new
            // executable measurement or conformance probe. Other dimensions
            // need their own producers, even when launch succeeded.
            let current = matches!(
                supervisor.state,
                SessionState::Running | SessionState::Parked
            ) && supervisor.broker_connection == BrokerConnection::Connected
                && !quarantined
                && self.checked_at_ms.is_some_and(|checked| checked <= now_ms);
            let evidence = vec![self.measurement.clone(), self.release.clone()];
            inputs.push(if current {
                DimensionInput::verified(dimension, evidence)
            } else {
                DimensionInput::failed(dimension, FailureCode::EvidenceInvalidated, evidence)
            });
            freshness[index] = EvidenceFreshness {
                basis: if current {
                    FreshnessBasis::Launch
                } else {
                    FreshnessBasis::Invalidated
                },
                last_verified_at_ms: self.checked_at_ms,
            };
        }
        let posture = Posture::evaluate(&supervisor.session_id, &supervisor.run_id, inputs)?;
        Ok(PostureStatus::from_posture(&posture, freshness))
    }
}
