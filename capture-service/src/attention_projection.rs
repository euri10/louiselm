//! Ordered projection from the one canonical Control broker.

use super::{
    AttentionDraft, AttentionError, AttentionItem, AttentionKey, AttentionStore,
    AttentionSubjectKind, advance_generation, sort_items, validate_draft, validate_key,
    validate_subject_id, validate_uuid,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// One normalized broker-owned condition transition.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProjectionChange {
    /// Create/update a condition and make it eligible for independent delivery.
    Upsert {
        /// Bounded typed condition; no Agent-authored summary.
        attention: AttentionDraft,
    },
    /// Resolve exactly one condition.
    Clear {
        /// Original condition identity, retained across retries.
        key: AttentionKey,
    },
    /// Resolve all conditions when a subject ends.
    ClearSubject {
        /// Kind of terminal subject.
        subject_kind: AttentionSubjectKind,
        /// Exact terminal subject identity.
        subject_id: String,
    },
}

/// One ordered entry from the broker's durable outbox.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrokerProjection {
    /// Monotonic sequence starting at one; never reset on broker restart.
    pub sequence: u64,
    /// Normalized state transition.
    pub change: ProjectionChange,
}

/// Delivery acknowledgement, conveying no authorization or launcher authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectionResult {
    /// Exact offered sequence, including on an ignored stale retry.
    pub sequence: u64,
    /// SHA-256 of the sorted-key canonical JSON projection.
    pub digest: String,
    /// Whether this entry advanced the receiver's durable cursor.
    pub applied: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProjectionCursor {
    sequence: u64,
    digest: String,
}

impl ProjectionCursor {
    pub(super) fn validate(&self) -> Result<(), AttentionError> {
        if self.sequence == 0
            || self.digest.len() != 64
            || !self
                .digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(AttentionError::Invalid(
                "broker projection cursor is invalid".into(),
            ));
        }
        Ok(())
    }
}

impl AttentionStore {
    /// Applies an authenticated broker projection atomically with its ordered cursor.
    ///
    /// The caller must authenticate the configured broker before calling this method.
    /// Stale deliveries never recreate resolved items. A gap or conflicting current
    /// sequence refuses the whole mutation. No receipt or policy state is changed.
    ///
    /// # Errors
    /// Rejects invalid input, gaps/conflicts, exhausted counters or persistence failure.
    pub fn project(
        &self,
        projection: &BrokerProjection,
    ) -> Result<ProjectionResult, AttentionError> {
        validate(projection)?;
        // Values use sorted object keys in both crates, independent of struct field order.
        let canonical = serde_json::to_vec(&serde_json::to_value(projection)?)?;
        let digest = format!("{:x}", Sha256::digest(canonical));
        self.with_lock(|store| {
            let mut state = store.load()?;
            let previous = state
                .broker_projection
                .as_ref()
                .map_or(0, |cursor| cursor.sequence);
            let result = ProjectionResult {
                sequence: projection.sequence,
                digest: digest.clone(),
                applied: false,
            };
            if projection.sequence <= previous {
                if projection.sequence == previous
                    && state
                        .broker_projection
                        .as_ref()
                        .is_some_and(|cursor| cursor.digest != digest)
                {
                    return Err(AttentionError::Invalid(
                        "broker projection sequence conflicts".into(),
                    ));
                }
                // A previous rename may have succeeded before its directory fsync
                // failed. Re-establish durability before acknowledging a retry.
                store.persist(&state)?;
                return Ok(result);
            }
            if previous.checked_add(1) != Some(projection.sequence) {
                return Err(AttentionError::Invalid(
                    "broker projection sequence has a gap".into(),
                ));
            }
            apply(&mut state.items, &projection.change);
            sort_items(&mut state.items);
            advance_generation(&mut state)?;
            state.broker_projection = Some(ProjectionCursor {
                sequence: projection.sequence,
                digest,
            });
            store.persist(&state)?;
            Ok(ProjectionResult {
                applied: true,
                ..result
            })
        })
    }
}

fn validate(projection: &BrokerProjection) -> Result<(), AttentionError> {
    if projection.sequence == 0 {
        return Err(AttentionError::Invalid(
            "broker projection sequence must be positive".into(),
        ));
    }
    match &projection.change {
        ProjectionChange::Upsert { attention } => validate_draft(attention),
        ProjectionChange::Clear { key } => validate_key(key),
        ProjectionChange::ClearSubject {
            subject_kind,
            subject_id,
        } => {
            validate_subject_id(subject_id)?;
            if *subject_kind == AttentionSubjectKind::Run {
                validate_uuid(subject_id, "Run subject_id")?;
            }
            Ok(())
        }
    }
}

fn apply(items: &mut Vec<AttentionItem>, change: &ProjectionChange) {
    match change {
        ProjectionChange::Upsert { attention } => {
            let item = AttentionItem {
                subject_kind: attention.subject_kind,
                subject_id: attention.subject_id.clone(),
                kind: attention.kind,
                source_operation_id: attention.source_operation_id.clone(),
                created_at_ms: attention.created_at_ms,
                eligible: true,
                reason: attention.kind.reason().into(),
                linked_run_id: attention.linked_run_id.clone(),
                stage: attention.stage.clone(),
                code: attention.code,
            };
            if let Some(previous) = items
                .iter_mut()
                .find(|previous| previous.key() == item.key())
            {
                *previous = item;
            } else {
                items.push(item);
            }
        }
        ProjectionChange::Clear { key } => items.retain(|item| item.key() != *key),
        ProjectionChange::ClearSubject {
            subject_kind,
            subject_id,
        } => items
            .retain(|item| item.subject_kind != *subject_kind || item.subject_id != *subject_id),
    }
}
