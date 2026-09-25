//! Session-owned validated supply facts and their source-specific lifetime.

use crate::{
    launch_protocol::{BrokerConnection, EvidenceFreshness, FreshnessBasis, SupervisorStatus},
    launch_receipt::SessionState,
    posture::{DimensionInput, DimensionName, EvidenceRef, FailureCode},
    supply_posture::SupplyEvidence,
};

const DIMENSIONS: [DimensionName; 3] = [
    DimensionName::ManagedSupply,
    DimensionName::NativeSupply,
    DimensionName::ProviderDisclosure,
];

pub(super) struct RetainedSupply {
    pub(super) checked_at_ms: u64,
    dimensions: [Result<EvidenceRef, FailureCode>; 3],
    last_success: [Option<(EvidenceRef, u64)>; 3],
}

impl RetainedSupply {
    pub(super) fn new(evidence: SupplyEvidence, previous: Option<Self>) -> Self {
        let mut last_success = previous.map_or([None, None, None], |old| old.last_success);
        for (index, result) in evidence.dimensions.iter().enumerate() {
            if let Ok(reference) = result {
                last_success[index] = Some((reference.clone(), evidence.checked_at_ms));
            }
        }
        Self {
            checked_at_ms: evidence.checked_at_ms,
            dimensions: evidence.dimensions,
            last_success,
        }
    }

    pub(super) fn dimension(
        &self,
        dimension: DimensionName,
        supervisor: &SupervisorStatus,
        quarantined: bool,
        now_ms: u64,
    ) -> Option<(DimensionInput, EvidenceFreshness)> {
        let index = DIMENSIONS.iter().position(|name| *name == dimension)?;
        // Frozen supply and disclosure describe the immutable admitted inputs.
        // Native controls additionally depend on the original live confinement.
        let current = !quarantined
            && self.checked_at_ms <= now_ms
            && (dimension != DimensionName::NativeSupply
                || (matches!(
                    supervisor.state,
                    SessionState::Running | SessionState::Parked
                ) && supervisor.broker_connection == BrokerConnection::Connected));
        let result = match &self.dimensions[index] {
            Ok(reference) if current => Ok(reference),
            Ok(_) if quarantined => Err(FailureCode::Quarantined),
            Ok(_) => Err(FailureCode::EvidenceInvalidated),
            Err(code) => Err(*code),
        };
        let evidence = self.last_success[index].as_ref();
        let input = match result {
            Ok(reference) => DimensionInput::verified(dimension, vec![reference.clone()]),
            Err(code) => DimensionInput::failed(
                dimension,
                code,
                evidence
                    .map(|(reference, _)| reference.clone())
                    .into_iter()
                    .collect(),
            ),
        };
        Some((
            input,
            EvidenceFreshness {
                basis: if evidence.is_none() {
                    FreshnessBasis::Missing
                } else if result.is_ok() {
                    FreshnessBasis::Launch
                } else {
                    FreshnessBasis::Invalidated
                },
                last_verified_at_ms: evidence.map(|(_, at)| *at),
            },
        ))
    }
}
