//! Required-action denial is durable and deduplicated by exact missing scope.
use super::{
    BeadsMutationRequest, BeadsMutations, Binding, BrokerError, DecisionRecord, Deserialize,
    Digest, Serialize, fs, lock, operation_uuid, read_record, sync_directory, write_new_record,
};
use crate::{
    beads_mutation::{BeadsCapability, BeadsEscalation},
    broker::attention::{
        AttentionCondition, AttentionReason, AttentionSubject, Outbox, ProjectionChange,
    },
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EscalationRecord {
    pub(super) binding: Binding,
    pub(super) escalation: BeadsEscalation,
    pub(super) created_at_ms: u64,
}

impl EscalationRecord {
    pub(super) fn valid(&self) -> bool {
        self.binding.valid() && self.escalation.valid() && self.created_at_ms > 0
    }

    pub(super) fn condition(&self) -> AttentionCondition {
        AttentionCondition {
            subject: AttentionSubject::Session(self.binding.session_id.clone()),
            operation_id: self.escalation.operation_id.clone(),
            created_at_ms: self.created_at_ms,
            reason: AttentionReason::PermissionRequired,
        }
    }
}

impl BeadsMutations {
    pub(in crate::broker) fn escalate(
        &self,
        binding: &Binding,
        request: &BeadsMutationRequest,
        project_digest: String,
        now_ms: u64,
        outbox: &Outbox,
    ) -> Result<BeadsEscalation, BrokerError> {
        let _guard = lock(&self.writing);
        if !binding.valid() || !request.valid() || !request.required {
            return Err(BrokerError::InvalidGrant);
        }
        let capability = BeadsCapability::for_mutation(project_digest, &request.kind);
        let digest = Digest::of(
            &serde_json::to_vec(&(binding, &capability)).map_err(|_| BrokerError::InvalidGrant)?,
        );
        let directory = self.root.join("escalations");
        let path = directory.join(format!("{}.json", digest.hex()));
        let record = if let Some(record) = read_record::<EscalationRecord>(&path)? {
            if !record.valid()
                || record.binding != *binding
                || record.escalation.capability != capability
            {
                return Err(BrokerError::RequestMismatch);
            }
            record
        } else {
            // Bound both the shared store and a single Session's unsolicited requests.
            let mut total = 0;
            let mut owned = 0;
            for entry in fs::read_dir(&directory).map_err(BrokerError::Storage)? {
                let entry = entry.map_err(BrokerError::Storage)?;
                let previous: EscalationRecord =
                    read_record(&entry.path())?.ok_or(BrokerError::InvalidGrant)?;
                if !previous.valid() {
                    return Err(BrokerError::InvalidGrant);
                }
                total += 1;
                if previous.binding.session_id == binding.session_id {
                    owned += 1;
                }
                if total >= 4096 || owned >= 64 {
                    return Err(BrokerError::InvalidGrant);
                }
            }
            let record = EscalationRecord {
                binding: binding.clone(),
                escalation: BeadsEscalation {
                    operation_id: operation_uuid()?,
                    capability,
                },
                created_at_ms: now_ms,
            };
            if !record.valid() {
                return Err(BrokerError::InvalidGrant);
            }
            write_new_record(&path, &record)?;
            record
        };
        fs::File::open(&path)
            .and_then(|file| file.sync_all())
            .map_err(BrokerError::Storage)?;
        sync_directory(&directory)?;
        let operation = &record.escalation.operation_id;
        match self.decision(operation)? {
            Some(DecisionRecord {
                decision: crate::beads_mutation::BeadsControlDecision::Dismiss,
                operator_uid,
                ..
            }) if operator_uid == binding.controller_uid => {
                outbox.enqueue(
                    &format!("beads-dismiss-{operation}"),
                    ProjectionChange::Clear(record.condition()),
                )?;
            }
            Some(_) => return Err(BrokerError::InvalidGrant),
            None => {
                outbox.enqueue(
                    &format!("beads-capability-{operation}"),
                    ProjectionChange::Upsert(record.condition()),
                )?;
            }
        }
        Ok(record.escalation)
    }
}
