//! Posture from authenticated launch history and retained supply producers.

use super::{BrokerError, BrokerService};
use crate::{
    launch_protocol::{
        BrokerConnection, EvidenceFreshness, FreshnessBasis, LaunchAuthorization, PostureStatus,
        SupervisorStatus,
    },
    launch_receipt::{ConformanceEvidence, ReceiptOutcome, SessionState},
    posture::{DimensionInput, DimensionName, EvidenceKind, EvidenceRef, FailureCode, Posture},
};

#[path = "supply_posture.rs"]
mod supply;

/// Private validated facts, never constructed from a status response.
/// Original signed admission bytes remain in `ReceiptStore` and are not rewritten.
pub(super) struct LaunchPostureEvidence {
    measurement: EvidenceRef,
    release: EvidenceRef,
    checked_at_ms: Option<u64>,
    launch_receipt_id: String,
    supply: Option<supply::RetainedSupply>,
    pub(super) conformance_admission: ConformanceEvidence,
    conformance_report: Option<EvidenceRef>,
    pub(super) current_conformance: Option<super::current_conformance::RetainedConformance>,
    conformance_authorization: crate::launch_protocol::ConformanceAuthorization,
}

impl BrokerService {
    /// Retains producer facts for the admitted Session without changing authority.
    ///
    /// Call on the owning broker worker after producer I/O completes.
    /// # Errors
    /// Refuses foreign launch evidence and stale or future observations.
    pub fn retain_supply_posture(
        &self,
        session: &mut super::BrokerSession,
        evidence: crate::supply_posture::SupplyEvidence,
        now_ms: u64,
    ) -> Result<(), BrokerError> {
        self.inspect_active(session)?;
        if evidence.request_digest != session.authorization().request_digest
            || evidence
                .receipt_id
                .as_ref()
                .is_some_and(|id| *id != session.posture_evidence.launch_receipt_id)
        {
            return Err(BrokerError::ReceiptUnauthorized);
        }
        let retained = &mut session.posture_evidence;
        if evidence.checked_at_ms > now_ms
            || retained
                .supply
                .as_ref()
                .is_some_and(|old| evidence.checked_at_ms <= old.checked_at_ms)
        {
            return Err(BrokerError::SupplyPosture("stale_or_future_observation"));
        }
        let previous = retained.supply.take();
        retained.supply = Some(supply::RetainedSupply::new(evidence, previous));
        Ok(())
    }

    /// Restores facts only at authenticated launch/reattachment, never during a read.
    pub(super) fn retain_launch_posture<F>(
        &self,
        authorization: &LaunchAuthorization,
        verify: &mut F,
    ) -> Result<LaunchPostureEvidence, BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        // The chain validates the exact subject, request, revision, trusted
        // release/signature, sequence, and actual Agent identity at Start.
        // Neither a bare measurement digest nor an unrelated signed record is proof.
        let chain = self.verified_history(authorization, verify)?;
        let launch = chain.first().ok_or(BrokerError::ReceiptUnauthorized)?;
        let start = chain.get(1).ok_or(BrokerError::ReceiptUnauthorized)?;
        let ReceiptOutcome::Launch { evidence, .. } = &launch.payload.outcome else {
            return Err(BrokerError::ReceiptUnauthorized);
        };
        if !matches!(start.payload.outcome, ReceiptOutcome::Start { .. }) {
            return Err(BrokerError::ReceiptUnauthorized);
        }
        let checked_at_ms = match self.start_receipt_stored_at(authorization) {
            Ok(observation) => observation,
            // Unreadable historical audit means missing posture evidence. Cold
            // Resume may still continue without commands; authority checks and
            // current receipt/audit durability remain mandatory at their owners.
            Err(BrokerError::Storage(_)) => None,
            Err(error) => return Err(error),
        };
        Ok(LaunchPostureEvidence {
            current_conformance: match self.receipts().current_conformance(authorization) {
                Ok(current) => current,
                Err(BrokerError::Storage(_)) => None,
                Err(error) => return Err(error),
            },
            conformance_authorization: authorization.conformance.clone(),
            conformance_admission: evidence.conformance.clone(),
            // verified_history also validates the exact retained report. The
            // reference preserves admission history, never current host proof.
            conformance_report: match &evidence.conformance {
                ConformanceEvidence::Certified { report_digest }
                | ConformanceEvidence::Waived {
                    report_digest: Some(report_digest),
                    ..
                } => Some(EvidenceRef::new(
                    EvidenceKind::ConformanceReport,
                    report_digest,
                )?),
                _ => None,
            },
            launch_receipt_id: launch.digest().to_string(),
            supply: None,
            measurement: EvidenceRef::new(
                EvidenceKind::RuntimeMeasurement,
                &evidence.runtime_measurement_digest,
            )?,
            release: EvidenceRef::new(EvidenceKind::ReleaseManifest, &launch.payload.release_id)?,
            checked_at_ms,
        })
    }
}

impl LaunchPostureEvidence {
    pub(super) fn permits_commands(&self, now_ms: u64) -> bool {
        self.conformance_admission == ConformanceEvidence::Unevaluated
            || self.current_conformance.as_ref().is_some_and(|current| {
                current.permits_commands(now_ms, &self.conformance_authorization)
            })
    }

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
            if dimension == DimensionName::Isolation
                && let Some(current) = &self.current_conformance
            {
                let (input, validity) = current.dimension(
                    supervisor,
                    quarantined,
                    now_ms,
                    &self.conformance_authorization,
                    &self.launch_receipt_id,
                )?;
                inputs.push(input);
                freshness[index] = validity;
                continue;
            }
            if dimension == DimensionName::Isolation
                && let Some(report) = &self.conformance_report
            {
                // Admission observations are retained; freshly applicable host
                // measurements/failure history still need the .12.4 producer.
                inputs.push(DimensionInput::failed(
                    dimension,
                    FailureCode::EvidenceMissing,
                    vec![report.clone()],
                ));
                continue;
            }
            if let Some(supply) = &self.supply
                && let Some((input, validity)) =
                    supply.dimension(dimension, supervisor, quarantined, now_ms)
            {
                inputs.push(input);
                freshness[index] = validity;
                continue;
            }
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
