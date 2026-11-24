//! Saved decisions, not disposable Sessions or Run lifetime, own pending Attention.

use super::{BrokerError, BrokerService, SavedResult};
use crate::{
    Digest,
    broker::{
        attention::{
            AttentionCondition, AttentionReason, AttentionSubject, ProjectionChange, condition_id,
        },
        is_record_identifier, now_ms, read_record, write_new_record,
    },
};
use std::fs;

impl BrokerService {
    // Caller holds the promotion guard: a late pending event must never follow its clear.
    pub(super) fn queue_saved_promotion(&self, saved: &SavedResult) -> Result<(), BrokerError> {
        let result_id = &saved.request.request_id;
        let path = self.saved_path(result_id)?.join("attention.json");
        let binding = serde_json::to_vec(&("saved-promotion", &saved.selection.run_id, result_id))
            .map_err(|_| BrokerError::InvalidGrant)?;
        let operation_id = condition_id(&binding);
        let condition = if let Some(condition) = read_record::<AttentionCondition>(&path)? {
            condition
        } else {
            let condition = AttentionCondition {
                linked_run_id: None,
                subject: AttentionSubject::Run(saved.selection.run_id.clone()),
                operation_id: operation_id.clone(),
                created_at_ms: now_ms()?,
                reason: AttentionReason::PromotionPending(result_id.clone()),
            };
            condition.validate()?;
            write_new_record(&path, &condition)?;
            condition
        };
        if condition.operation_id != operation_id
            || condition.subject != AttentionSubject::Run(saved.selection.run_id.clone())
            || condition.reason != AttentionReason::PromotionPending(result_id.clone())
            || condition.linked_run_id.is_some()
        {
            return Err(BrokerError::RequestMismatch);
        }
        condition.validate()?;
        let resolved = saved.discarded || saved.accepted.is_some();
        let mutation = format!(
            "promotion-{}-{}",
            Digest::of(&binding).hex(),
            if resolved { "resolved" } else { "pending" }
        );
        self.attention.enqueue(
            &mutation,
            if resolved {
                ProjectionChange::Clear(condition)
            } else {
                ProjectionChange::Upsert(condition)
            },
        )?;
        Ok(())
    }

    /// Reconstructs undelivered saved-decision projections after any broker interruption.
    /// Runs on the delivery worker; no Run/Session, execution or application is revived.
    /// # Errors
    /// Refuses corrupt published results, invalid ownership, exhausted bounds and storage failure.
    pub fn reconcile_saved_promotions(&self) -> Result<(), BrokerError> {
        let _guard = self.lifecycle.promotion_guard();
        let parent = self.authorizations().root.join("saved-results");
        let entries = match fs::read_dir(parent) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(BrokerError::Storage(error)),
        };
        for (index, entry) in entries.take(4097).enumerate() {
            if index == 4096 {
                return Err(BrokerError::InvalidGrant);
            }
            let entry = entry.map_err(BrokerError::Storage)?;
            let name = entry.file_name();
            let id = name.to_str().ok_or(BrokerError::InvalidGrant)?;
            if id.starts_with(".workspace-") {
                continue;
            }
            if !is_record_identifier(id) {
                return Err(BrokerError::InvalidGrant);
            }
            if !entry
                .path()
                .join("published.json")
                .try_exists()
                .map_err(BrokerError::Storage)?
            {
                continue;
            }
            super::saved::private_directory(&entry.path())?;
            let mut record: SavedResult =
                read_record(&entry.path().join("saved.json"))?.ok_or(BrokerError::InvalidGrant)?;
            let published: String = read_record(&entry.path().join("published.json"))?
                .ok_or(BrokerError::InvalidGrant)?;
            if record.schema != "louiselm.workspace.saved-result/1"
                || record.request.request_id != id
                || published
                    != Digest::of(
                        &serde_json::to_vec(&record).map_err(|_| BrokerError::InvalidGrant)?,
                    )
                    .to_string()
                || record.discarded
                || record.accepted.is_some()
            {
                return Err(BrokerError::RequestMismatch);
            }
            // Attention reads only durable decision metadata, never source payloads or
            // verification eligibility. Corrupt/missing bytes do not resolve a decision.
            record.discarded = match read_record::<bool>(&entry.path().join("discarded.json"))? {
                None => false,
                Some(true) => true,
                Some(false) => return Err(BrokerError::InvalidGrant),
            };
            record.accepted = read_record(&entry.path().join("accepted.json"))?;
            if let Some(accepted) = &record.accepted {
                let request: super::PromotionRequest =
                    read_record(&self.promotion_path(id)?.join("request.json"))?
                        .ok_or(BrokerError::RequestMismatch)?;
                if record.discarded
                    || !super::selection::valid_commit(&accepted.commit)
                    || Digest::parse(&accepted.approval_digest).is_err()
                    || accepted.request_digest
                        != Digest::of(
                            &serde_json::to_vec(&request).map_err(|_| BrokerError::InvalidGrant)?,
                        )
                        .to_string()
                    || !matches!(self.promotion_status(id)?, super::PromotionStatus::Completed { result, .. } if result.complete)
                {
                    return Err(BrokerError::RequestMismatch);
                }
            }
            self.queue_saved_promotion(&record)?;
        }
        Ok(())
    }
}
