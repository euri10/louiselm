//! Policy for explicitly recovering a consumed launch without altering receipt history.
pub use crate::launch_recovery::Binding;
use crate::{
    broker::{BrokerError, BrokerService, InstalledBroker},
    launch_receipt::{ReceiptHead, SessionState},
    launch_recovery::{Acknowledgement, Inspection, InspectionOutcome, Proof},
};
use serde::{Deserialize, Serialize};

/// Exact consumption plus any independently authenticated recovery outcome.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    /// Immutable original consumption, never renewed authority.
    pub binding: Binding,
    /// Separate proof; no Launcher receipt or receipt head is invented.
    pub recovered: Option<Proof>,
}

impl BrokerService {
    /// Inspect authenticated durable history, without a live supervisor or cleanup.
    /// Existing history quarantine remains authoritative; no reservation is released.
    /// # Errors
    /// Refuses unknown consumption, invalid/revoked history or inconsistent proof.
    pub fn inspect_consumed_recovery<F>(
        &self,
        id: &str,
        mut verify: F,
    ) -> Result<Inspection, BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        self.authorizations().with_consumption(id, |held, digest| {
            self.check_history(&held.session_id)?;
            let chain = self
                .receipts()
                .verified_chain(&held.launch_authorization(), &mut verify)?;
            let proof = self
                .receipts()
                .verified_recovery(held, digest, &mut verify)?;
            let binding = Binding {
                authorization: held.launch_authorization(),
                consumption_digest: digest.into(),
                receipt_head: chain.last().map(|receipt| ReceiptHead {
                    sequence: receipt.payload.sequence,
                    digest: receipt.digest().to_string(),
                }),
            };
            let receipt_state = chain.last().map(|receipt| receipt.payload.resulting_state);
            let outcome = if let Some(proof) = proof {
                // A concurrent append/recovery between the reads must not combine
                // different heads into a success-looking inspection.
                if proof.payload.binding != binding {
                    return Err(BrokerError::RequestMismatch);
                }
                InspectionOutcome::RecoveredAfterReboot {
                    proof: Box::new(proof),
                }
            } else if receipt_state == Some(SessionState::Terminal) {
                InspectionOutcome::Disposed {}
            } else {
                InspectionOutcome::Unrecovered {}
            };
            let inspection = Inspection {
                schema: "louiselm.consumed-recovery.inspection/1".into(),
                binding,
                receipt_state,
                outcome,
            };
            inspection.validate()?;
            Ok(inspection)
        })
    }

    /// Inspect one exact consumption under the assignment lock, without mechanics.
    /// # Errors
    /// Refuses unknown/changed consumption, quarantine or invalid/terminal history.
    pub fn consumed_recovery_snapshot<F>(
        &self,
        id: &str,
        mut verify: F,
    ) -> Result<Snapshot, BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        self.authorizations().with_consumption(id, |held, digest| {
            self.check_history(&held.session_id)?;
            let binding = self
                .receipts()
                .recovery_binding(held, digest, &mut verify)?;
            let recovered = self
                .receipts()
                .verified_recovery(held, digest, &mut verify)?;
            if recovered.is_none() {
                self.authorizations()
                    .require_recovery_reservation(held, &mut |prior| {
                        self.identity_disposed(prior, &mut verify)
                    })?;
            }
            Ok(Snapshot { binding, recovered })
        })
    }

    /// Durably admit the exact separate signed proof. It cannot replace receipts.
    /// Idempotent retry reestablishes durability without repeating mechanics.
    /// # Errors
    /// Refuses malformed/forged/foreign proof, changed consumption/head, invalid
    /// or terminal history, quarantine, conflicting completion or unavailable storage.
    pub fn accept_consumed_recovery<F>(
        &self,
        proof: &Proof,
        mut verify: F,
    ) -> Result<Acknowledgement, BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        proof.validate()?;
        let id = &proof.payload.binding.authorization.authorization_id;
        self.authorizations().with_consumption(id, |held, digest| {
            if proof.payload.binding.authorization != held.launch_authorization()
                || proof.payload.binding.consumption_digest != digest
            {
                return Err(BrokerError::RequestMismatch);
            }
            self.check_history(&held.session_id)?;
            if self
                .receipts()
                .verified_recovery(held, digest, &mut verify)?
                .is_none()
            {
                self.authorizations()
                    .require_recovery_reservation(held, &mut |prior| {
                        self.identity_disposed(prior, &mut verify)
                    })?;
            }
            self.receipts()
                .store_recovery(held, digest, proof, &mut verify)
        })
    }
}

impl InstalledBroker {
    /// Inspect original history and any separate recovery using installed authority.
    /// # Errors
    /// Refuses unknown subjects, revoked/malformed evidence or unavailable storage.
    pub fn inspect_consumed_recovery(&self, id: &str) -> Result<Inspection, BrokerError> {
        self.service
            .inspect_consumed_recovery(id, |key, payload, signature| {
                self.verifier.verify(key, payload, signature).is_ok()
            })
    }

    /// Inspect a root-authenticated candidate with installed historical authority.
    /// # Errors
    /// Refuses unknown/changed/quarantined or conflicting state.
    pub fn consumed_recovery_snapshot(&self, id: &str) -> Result<Snapshot, BrokerError> {
        self.service
            .consumed_recovery_snapshot(id, |key, payload, signature| {
                self.verifier.verify(key, payload, signature).is_ok()
            })
    }

    /// Admit exact recovery bytes through their separate machine-signature verifier.
    /// # Errors
    /// Refuses invalid authority, conflicting scope/history or unavailable durability.
    pub fn accept_consumed_recovery(&self, proof: &Proof) -> Result<Acknowledgement, BrokerError> {
        self.service
            .accept_consumed_recovery(proof, |key, payload, signature| {
                self.verifier.verify(key, payload, signature).is_ok()
            })
    }
}
