//! Durable administrative history, never current supervisor or permission state.
use super::{Binding, MAX_BYTES, Proof, ProofError};
use crate::launch_receipt::SessionState;
use serde::{Deserialize, Serialize};

/// A separate recovery fact is not a supervisor-issued Disposal receipt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum InspectionOutcome {
    /// No accepted recovery or terminal receipt; this says nothing about liveness.
    Unrecovered {},
    /// The original chain ends in an authenticated supervisor Disposal.
    Disposed {},
    /// Root established cleanup after another boot and the broker accepted it.
    RecoveredAfterReboot {
        /// Exact independently verified proof, separate from the original chain.
        proof: Box<Proof>,
    },
}

/// Authenticated durable evidence obtainable without a surviving supervisor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Inspection {
    /// Closed administrative inspection schema.
    pub schema: String,
    /// Original consumption and existing receipt head, never renewed authority.
    pub binding: Binding,
    /// Last historical receipt state; absent when no receipt was ever accepted.
    pub receipt_state: Option<SessionState>,
    /// Explicit distinction between normal Disposal and separate reboot recovery.
    pub outcome: InspectionOutcome,
}

impl Inspection {
    /// Validate scope and consistency, without claiming to authenticate signatures.
    /// # Errors
    /// Refuses malformed, oversized or contradictory history/proof observations.
    pub fn validate(&self) -> Result<(), ProofError> {
        if self.schema != "louiselm.consumed-recovery.inspection/1"
            || serde_json::to_vec(self)
                .map_err(|_| ProofError::Malformed)?
                .len()
                > MAX_BYTES
        {
            return Err(ProofError::Malformed);
        }
        self.binding.validate()?;
        if self.binding.receipt_head.is_some() != self.receipt_state.is_some() {
            return Err(ProofError::InvalidBinding);
        }
        let terminal = self.receipt_state == Some(SessionState::Terminal);
        match &self.outcome {
            InspectionOutcome::Disposed {} if terminal => Ok(()),
            InspectionOutcome::Unrecovered {} if !terminal => Ok(()),
            InspectionOutcome::RecoveredAfterReboot { proof } if !terminal => {
                proof.validate()?;
                if proof.payload.binding != self.binding {
                    return Err(ProofError::InvalidBinding);
                }
                Ok(())
            }
            _ => Err(ProofError::InvalidBinding),
        }
    }
}
