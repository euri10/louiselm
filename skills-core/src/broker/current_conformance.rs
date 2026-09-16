//! Retained supervisor observations with source-owned expiry and immutable admission.

use super::{
    BrokerError, BrokerService, BrokerSession, corrupt, lock, read_record, sync_directory,
};
use crate::{
    launch_protocol::{
        BrokerConnection, CONFORMANCE_FRESHNESS_MS, ConformanceCheck, ConformanceFailure,
        ConformanceUpdate, EvidenceFreshness, FreshnessBasis, LaunchAuthorization,
        SupervisorStatus,
    },
    launch_receipt::{ConformanceEvidence, SessionState},
    posture::{DimensionInput, DimensionName, EvidenceKind, EvidenceRef, FailureCode},
};
use serde::{Deserialize, Serialize};
use std::{fs, io::Write};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RetainedConformance {
    update: ConformanceUpdate,
    last_verified: Option<(String, u64)>,
}

impl super::ReceiptStore {
    pub(super) fn current_conformance(
        &self,
        authorization: &LaunchAuthorization,
    ) -> Result<Option<RetainedConformance>, BrokerError> {
        let path = self.current_conformance_path(&authorization.session_id)?;
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() && metadata.len() <= 8192 => {}
            Ok(_) => return Err(corrupt("invalid current conformance file")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(BrokerError::Storage(error)),
        }
        let record: Option<RetainedConformance> = read_record(&path)?;
        if let Some(record) = &record {
            record.update.validate_for(authorization)?;
            if let Some((digest, at)) = &record.last_verified {
                EvidenceRef::new(EvidenceKind::ConformanceReport, digest)?;
                if record
                    .update
                    .last_success_at_ms
                    .is_none_or(|success| *at > success)
                {
                    return Err(corrupt("future conformance history"));
                }
            }
            if let ConformanceCheck::Current {
                evidence: ConformanceEvidence::Certified { report_digest },
            } = &record.update.check
                && record.last_verified.as_ref()
                    != Some(&(report_digest.clone(), record.update.observed_at_ms))
            {
                return Err(corrupt("contradictory current conformance history"));
            }
        }
        Ok(record)
    }

    pub(super) fn retain_current_conformance(
        &self,
        authorization: &LaunchAuthorization,
        update: &ConformanceUpdate,
    ) -> Result<RetainedConformance, BrokerError> {
        update.validate_for(authorization)?;
        let _guard = lock(&self.appending);
        let previous = self.current_conformance(authorization)?;
        if let Some(previous) = &previous {
            if &previous.update == update {
                return Ok(previous.clone());
            }
            if update.sequence <= previous.update.sequence
                || update.observed_at_ms < previous.update.observed_at_ms
                || update.last_success_at_ms < previous.update.last_success_at_ms
                || matches!(
                    previous.update.check,
                    ConformanceCheck::Invalid {
                        failure: ConformanceFailure::Condition(
                            crate::conformance::admission::Condition::ContainmentFailure
                        )
                    }
                ) && !matches!(
                    update.check,
                    ConformanceCheck::Invalid {
                        failure: ConformanceFailure::Condition(
                            crate::conformance::admission::Condition::ContainmentFailure
                        )
                    }
                )
            {
                return Err(corrupt("stale or contradictory conformance update"));
            }
        }
        let last_verified = match &update.check {
            ConformanceCheck::Current {
                evidence: ConformanceEvidence::Certified { report_digest },
            } => Some((report_digest.clone(), update.observed_at_ms)),
            _ => previous.and_then(|previous| previous.last_verified),
        };
        let retained = RetainedConformance {
            update: update.clone(),
            last_verified,
        };
        let path = self.current_conformance_path(&authorization.session_id)?;
        let parent = path.parent().ok_or(BrokerError::InvalidGrant)?;
        fs::create_dir_all(parent).map_err(BrokerError::Storage)?;
        let mut temporary =
            tempfile::NamedTempFile::new_in(parent).map_err(BrokerError::Storage)?;
        let bytes = serde_json::to_vec(&retained).map_err(|_| BrokerError::InvalidGrant)?;
        temporary
            .write_all(&bytes)
            .and_then(|()| temporary.as_file().sync_all())
            .map_err(BrokerError::Storage)?;
        temporary
            .persist(&path)
            .map_err(|error| BrokerError::Storage(error.error))?;
        sync_directory(parent)?;
        sync_directory(&self.root)?;
        Ok(retained)
    }

    fn current_conformance_path(
        &self,
        session_id: &str,
    ) -> Result<std::path::PathBuf, BrokerError> {
        Ok(self
            .root
            .join("current-conformance")
            .join(super::record_name(session_id)?))
    }
}

impl BrokerService {
    pub(super) fn retain_conformance_update(
        &self,
        session: &mut BrokerSession,
        update: &ConformanceUpdate,
        now_ms: u64,
    ) -> Result<(), BrokerError> {
        if update.observed_at_ms > now_ms
            || self.receipts().state(&update.session_id)? == Some(SessionState::Terminal)
        {
            return Err(BrokerError::InvalidGrant);
        }
        let retained = self
            .receipts()
            .retain_current_conformance(session.authorization(), update)?;
        session.posture_evidence.current_conformance = Some(retained);
        Ok(())
    }
}

impl RetainedConformance {
    pub(super) fn permits_commands(
        &self,
        now_ms: u64,
        authorization: &crate::launch_protocol::ConformanceAuthorization,
    ) -> bool {
        !self.update.suspended
            && self.update.observed_at_ms <= now_ms
            && now_ms
                < self
                    .update
                    .observed_at_ms
                    .saturating_add(CONFORMANCE_FRESHNESS_MS)
            && match &self.update.check {
                ConformanceCheck::Current {
                    evidence: ConformanceEvidence::Certified { .. },
                } => true,
                ConformanceCheck::Current {
                    evidence: ConformanceEvidence::Waived { condition, .. },
                } => authorization.waiver.as_ref().is_some_and(|waiver| {
                    waiver.condition == *condition && now_ms < waiver.expires_at_ms
                }),
                _ => false,
            }
    }

    pub(super) fn dimension(
        &self,
        supervisor: &SupervisorStatus,
        quarantined: bool,
        now_ms: u64,
        authorization: &crate::launch_protocol::ConformanceAuthorization,
        launch_receipt: &str,
    ) -> Result<(DimensionInput, EvidenceFreshness), BrokerError> {
        let dimension = DimensionName::Isolation;
        let mut references = self
            .last_verified
            .as_ref()
            .map(|(digest, _)| EvidenceRef::new(EvidenceKind::ConformanceReport, digest))
            .transpose()?
            .into_iter()
            .collect::<Vec<_>>();
        references.push(EvidenceRef::new(
            EvidenceKind::IsolationReceipt,
            launch_receipt,
        )?);
        let current = !quarantined
            && matches!(
                supervisor.state,
                SessionState::Running | SessionState::Parked
            )
            && supervisor.broker_connection == BrokerConnection::Connected
            && self.update.observed_at_ms <= now_ms
            && now_ms
                < self
                    .update
                    .observed_at_ms
                    .saturating_add(CONFORMANCE_FRESHNESS_MS);
        let input = match &self.update.check {
            ConformanceCheck::Current {
                evidence: ConformanceEvidence::Certified { .. },
            } if current => DimensionInput::verified(dimension, references),
            ConformanceCheck::Current {
                evidence: ConformanceEvidence::Waived { condition, .. },
            } if current => {
                if let Some(waiver) = &authorization.waiver
                    && waiver.condition == *condition
                    && waiver.expires_at_ms > now_ms
                {
                    DimensionInput::waived(
                        dimension,
                        FailureCode::IsolationFailed,
                        references,
                        EvidenceRef::new(EvidenceKind::WaiverReceipt, &waiver.receipt_digest)?,
                    )
                } else {
                    DimensionInput::failed(dimension, FailureCode::EvidenceInvalidated, references)
                }
            }
            ConformanceCheck::Invalid {
                failure:
                    ConformanceFailure::Condition(
                        crate::conformance::admission::Condition::ContainmentFailure,
                    ),
            } => DimensionInput::failed(dimension, FailureCode::IsolationFailed, references),
            _ => DimensionInput::failed(dimension, FailureCode::EvidenceInvalidated, references),
        };
        Ok((
            input,
            EvidenceFreshness {
                basis: if self.last_verified.is_none() {
                    FreshnessBasis::Missing
                } else if current
                    && matches!(
                        self.update.check,
                        ConformanceCheck::Current {
                            evidence: ConformanceEvidence::Certified { .. }
                        }
                    )
                {
                    FreshnessBasis::Check
                } else {
                    FreshnessBasis::Invalidated
                },
                last_verified_at_ms: self.last_verified.as_ref().map(|(_, at)| *at),
            },
        ))
    }
}
