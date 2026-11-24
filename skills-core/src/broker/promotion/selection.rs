//! Durable operator review context, without a review or execution lifetime.

use super::{BrokerError, PromotionRequest};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Operator-owned checkout and rule selection retained with exact saved work.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedSelection {
    /// Closed review-context schema.
    pub schema: String,
    /// Originating Run, never reactivated by review.
    pub run_id: String,
    /// Selected work item.
    pub bead_id: String,
    /// Historical producer.
    pub producer_session_id: String,
    /// Historical independent verifier.
    pub verifier_session_id: String,
    /// Durable result and logical application identity.
    pub request_id: String,
    /// Operator-owned destination; its device/inode is separately pinned.
    pub checkout: PathBuf,
    /// Operator-owned local effect-journal parent.
    pub journal_parent: PathBuf,
    /// Exact reviewed Run-branch baseline.
    pub expected_head: String,
    /// Current operator-selected verification rules, read but never executed by review.
    pub plan: PathBuf,
}

impl SavedSelection {
    pub(super) fn validate(&self, request: &PromotionRequest) -> Result<(), BrokerError> {
        for id in [&self.run_id, &self.bead_id] {
            super::super::record_name(id)?;
        }
        if self.schema != "louiselm.run-promotion-selection/1"
            || self.request_id != request.request_id
            || self.producer_session_id != request.producer_session_id
            || self.verifier_session_id != request.verifier_session_id
            || !valid_commit(&self.expected_head)
            || [&self.checkout, &self.journal_parent, &self.plan]
                .iter()
                .any(|path| !path.is_absolute() || path.as_os_str().len() > 4096)
        {
            return Err(BrokerError::RequestMismatch);
        }
        Ok(())
    }
}

pub(super) fn valid_commit(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
