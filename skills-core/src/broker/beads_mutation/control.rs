//! Authenticated inspection and immutable operator decisions; never a tracker invocation.
use super::escalation::EscalationRecord;
use super::{
    BeadsMutationOutcome, BeadsMutations, Binding, BrokerError, DecisionRecord, Record, fs, lock,
    read_record, sync_directory, write_new_record,
};
use crate::{
    beads_mutation::{
        BeadsControlDecision, BeadsInspection, BeadsInspectionDetail, BeadsResolution,
    },
    broker::attention::{Outbox, ProjectionChange},
};

impl BeadsMutations {
    pub(in crate::broker) fn control(
        &self,
        operator_uid: u32,
        operation_id: &str,
        decision: Option<&BeadsControlDecision>,
        now_ms: u64,
        outbox: &Outbox,
    ) -> Result<BeadsInspection, BrokerError> {
        if !super::super::attention::canonical_uuid(operation_id)
            || decision.is_some_and(|value| !value.valid())
        {
            return Err(BrokerError::InvalidGrant);
        }
        let _guard = lock(&self.writing);
        // ponytail: bounded scans reuse the existing request ledger; no second intent index.
        for entry in fs::read_dir(self.root.join("requests"))
            .map_err(BrokerError::Storage)?
            .take(4096)
        {
            let entry = entry.map_err(BrokerError::Storage)?;
            let record: Record = read_record(&entry.path())?.ok_or(BrokerError::InvalidGrant)?;
            if !record.valid() {
                return Err(BrokerError::InvalidGrant);
            }
            if record.operation_id != operation_id {
                continue;
            }
            if record.binding.controller_uid != operator_uid {
                return Err(BrokerError::ControllerMismatch);
            }
            let outcome = read_record(&self.outcome_path(operation_id))?
                .unwrap_or(BeadsMutationOutcome::Unknown);
            if decision.is_some_and(|value| {
                !matches!(value, BeadsControlDecision::Reconcile { .. })
                    || outcome == BeadsMutationOutcome::Completed
            }) {
                return Err(BrokerError::InvalidGrant);
            }
            let resolution =
                match self.record_decision(operator_uid, operation_id, decision, now_ms)? {
                    Some(DecisionRecord {
                        decision:
                            BeadsControlDecision::Reconcile {
                                outcome,
                                evidence_digest,
                            },
                        operator_uid,
                        decided_at_ms,
                    }) => Some(BeadsResolution {
                        outcome,
                        evidence_digest,
                        operator_uid,
                        decided_at_ms,
                    }),
                    Some(_) => return Err(BrokerError::InvalidGrant),
                    None => None,
                };
            return inspected(
                &record.binding,
                operation_id,
                BeadsInspectionDetail::Mutation {
                    project_digest: record.project_digest.clone(),
                    request_digest: record.request_digest.clone(),
                    status: record.status(outcome),
                    resolution,
                },
            );
        }
        for entry in fs::read_dir(self.root.join("escalations"))
            .map_err(BrokerError::Storage)?
            .take(4096)
        {
            let entry = entry.map_err(BrokerError::Storage)?;
            let record: EscalationRecord =
                read_record(&entry.path())?.ok_or(BrokerError::InvalidGrant)?;
            if !record.valid() {
                return Err(BrokerError::InvalidGrant);
            }
            if record.escalation.operation_id != operation_id {
                continue;
            }
            if record.binding.controller_uid != operator_uid {
                return Err(BrokerError::ControllerMismatch);
            }
            if decision.is_some_and(|value| *value != BeadsControlDecision::Dismiss) {
                return Err(BrokerError::InvalidGrant);
            }
            let dismissed = self.dismissed(operator_uid, operation_id, decision, now_ms)?;
            if dismissed {
                outbox.enqueue(
                    &format!("beads-dismiss-{operation_id}"),
                    ProjectionChange::Clear(record.condition()),
                )?;
            }
            return inspected(
                &record.binding,
                operation_id,
                BeadsInspectionDetail::Escalation {
                    escalation: record.escalation,
                    dismissed,
                },
            );
        }
        Err(BrokerError::UnknownAuthorization)
    }

    fn dismissed(
        &self,
        operator_uid: u32,
        operation_id: &str,
        decision: Option<&BeadsControlDecision>,
        now_ms: u64,
    ) -> Result<bool, BrokerError> {
        match self.record_decision(operator_uid, operation_id, decision, now_ms)? {
            Some(DecisionRecord {
                decision: BeadsControlDecision::Dismiss,
                ..
            }) => Ok(true),
            Some(_) => Err(BrokerError::InvalidGrant),
            None => Ok(false),
        }
    }

    fn record_decision(
        &self,
        operator_uid: u32,
        operation_id: &str,
        decision: Option<&BeadsControlDecision>,
        now_ms: u64,
    ) -> Result<Option<DecisionRecord>, BrokerError> {
        let prior = self.decision(operation_id)?;
        if prior.as_ref().is_some_and(|record| {
            record.operator_uid != operator_uid
                || decision.is_some_and(|value| *value != record.decision)
        }) {
            return Err(BrokerError::RequestMismatch);
        }
        let Some(decision) = decision else {
            return Ok(prior);
        };
        let path = self.decision_path(operation_id);
        if prior.is_none() {
            if operator_uid == 0 || now_ms == 0 {
                return Err(BrokerError::InvalidGrant);
            }
            write_new_record(
                &path,
                &DecisionRecord {
                    decision: decision.clone(),
                    operator_uid,
                    decided_at_ms: now_ms,
                },
            )?;
        }
        // A lost reply or failed directory fsync is retried with identical evidence.
        fs::File::open(path)
            .and_then(|file| file.sync_all())
            .map_err(BrokerError::Storage)?;
        sync_directory(&self.root.join("decisions"))?;
        self.decision(operation_id)
    }
}

fn inspected(
    binding: &Binding,
    operation_id: &str,
    detail: BeadsInspectionDetail,
) -> Result<BeadsInspection, BrokerError> {
    let result = BeadsInspection {
        operation_id: operation_id.into(),
        session_id: binding.session_id.clone(),
        run_id: binding.run_id.clone(),
        envelope_revision: binding.envelope_revision,
        detail,
    };
    if !result.valid() {
        return Err(BrokerError::InvalidGrant);
    }
    Ok(result)
}
