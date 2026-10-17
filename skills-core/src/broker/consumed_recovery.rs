//! Policy for explicitly recovering a consumed launch without altering receipt history.
pub use crate::launch_recovery::Binding;
use crate::{
    broker::{BrokerError, BrokerService, InstalledBroker},
    launch_recovery::{Acknowledgement, Proof},
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
    /// Inspect one exact consumption under the assignment lock, without mechanics.
    /// # Errors
    /// Refuses unknown/changed consumption, quarantine or ordinary receipt history.
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
            if !self
                .receipts()
                .verified_chain(&held.launch_authorization(), &mut verify)?
                .is_empty()
            {
                return Err(BrokerError::InvalidGrant);
            }
            let recovered = self
                .receipts()
                .verified_recovery(held, digest, &mut verify)?;
            if recovered.is_none() {
                self.authorizations()
                    .require_recovery_reservation(held, &mut |prior| {
                        self.identity_disposed(prior, &mut verify)
                    })?;
            }
            Ok(Snapshot {
                binding: Binding {
                    authorization: held.launch_authorization(),
                    consumption_digest: digest.into(),
                },
                recovered,
            })
        })
    }

    /// Durably admit the exact separate signed proof. It cannot replace receipts.
    /// Idempotent retry reestablishes durability without repeating mechanics.
    /// # Errors
    /// Refuses malformed/forged/foreign proof, changed consumption, ordinary
    /// history, quarantine, conflicting completion or unavailable storage.
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
            if proof.payload.binding
                != (Binding {
                    authorization: held.launch_authorization(),
                    consumption_digest: digest.into(),
                })
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
