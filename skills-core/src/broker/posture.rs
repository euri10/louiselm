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
    pub(super) conformance_authorization: crate::launch_protocol::ConformanceAuthorization,
    pub(super) waiver_revision: u64,
}

impl BrokerService {
    pub(super) fn reconcile_posture_attention<F>(
        &self,
        endpoint: Option<&super::attention::AttentionEndpoint>,
        mut verify: F,
    ) -> Result<(), BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        self.posture_attention.reconcile(&self.attention)?;
        let mut runs = std::collections::BTreeSet::new();
        let mut failure = None;
        for record in self.posture_attention.records()? {
            let result = (|| {
                let auth = self
                    .authorizations()
                    .consumed_for_session(&record.session_id)?
                    .ok_or(BrokerError::UnknownAuthorization)?;
                let history = self.verified_history(&auth.launch_authorization(), &mut verify)?;
                if history.last().is_some_and(|receipt| {
                    receipt.payload.resulting_state == SessionState::Terminal
                }) {
                    self.posture_attention
                        .end(&record.session_id, &self.attention)?;
                }
                if let Some(endpoint) = endpoint
                    && super::attention::canonical_uuid(&record.run_id)
                    && runs.insert(record.run_id.clone())
                {
                    self.refresh_run(endpoint, &record.run_id)?;
                }
                Ok(())
            })();
            if let Err(error) = result {
                failure.get_or_insert(error);
            }
        }
        failure.map_or(Ok(()), Err)
    }

    /// Reconcile waiting Session conditions on its serialized source worker.
    /// Reads authenticated mechanics and evaluates retained proof, not display JSON.
    /// Call independently of status reads so expiry and waiver changes reach Attention.
    /// This performs blocking source/storage I/O and never delivers over the network.
    /// # Errors
    /// Refuses untrusted history, foreign mechanics or unavailable durable projection.
    pub fn project_posture_attention<F>(
        &self,
        session: &mut super::BrokerSession,
        now_ms: u64,
        verify: F,
    ) -> Result<(), BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        let started = std::time::Instant::now();
        let status = self.supervisor_status(session, verify)?;
        if status.state == SessionState::Terminal {
            return self
                .posture_attention
                .end(&status.session_id, &self.attention);
        }
        let now =
            now_ms.saturating_add(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX));
        let quarantined = self.lifecycle.is_quarantined(&status.session_id)?;
        let (posture, _) = session
            .posture_evidence
            .evaluate(&status, quarantined, now)?;
        let waiting = status.state == SessionState::Parked
            || session
                .posture_evidence
                .current_conformance
                .as_ref()
                .is_some_and(|current| current.update.suspended);
        self.posture_attention.observe(
            session.authorization(),
            &posture,
            waiting,
            quarantined,
            now,
            &self.attention,
        )
    }

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
        let (waiver_revision, waiver) = self.waivers.decision(authorization)?;
        let mut current_authorization = authorization.clone();
        current_authorization.conformance.waiver = waiver;
        Ok(LaunchPostureEvidence {
            waiver_revision,
            current_conformance: match self
                .receipts()
                .current_conformance(&current_authorization, waiver_revision)
            {
                Ok(current) => current,
                Err(BrokerError::Storage(_)) => None,
                Err(error) => return Err(error),
            },
            conformance_authorization: current_authorization.conformance,
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
        let (posture, freshness) = self.evaluate(supervisor, quarantined, now_ms)?;
        Ok(PostureStatus::from_posture(&posture, freshness))
    }

    pub(super) fn evaluate(
        &self,
        supervisor: &SupervisorStatus,
        quarantined: bool,
        now_ms: u64,
    ) -> Result<(Posture, [EvidenceFreshness; 6]), BrokerError> {
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
                DimensionInput::failed(
                    dimension,
                    if quarantined {
                        FailureCode::Quarantined
                    } else {
                        FailureCode::EvidenceInvalidated
                    },
                    evidence,
                )
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
        Ok((posture, freshness))
    }
}
