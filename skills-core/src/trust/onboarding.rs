//! Atomic initial enrollment; provisional stores are never promoted.

use serde::Serialize;
use webauthn_rs::prelude::Uuid;

use super::{
    Role, TrustError, TrustStore,
    paper::PaperPhrase,
    passkey::Registration,
    persistence::LockedTrust,
    recovery::{self, RecoveryChange, RecoveryError, ReplacementProof},
    terminal,
};
use crate::{
    sshsig::{self, SkPolicy},
    store::Store,
};

/// Initial key-possession domain, never an ordinary release or Admission.
pub const SETUP_NAMESPACE: &str = "louiselm.skills.recovery-setup/1";

/// In-memory, random-bound candidate. Dropping it leaves no enrollment.
///
/// The caller owns trusted paper confirmation and display of [`Self::plan`].
/// Development stores may exercise software fixtures; production requires the
/// installed authority and protected store. Neither path grants Verified posture.
pub struct PendingSetup {
    trust: TrustStore,
}

#[derive(Serialize)]
struct Plan<'a> {
    schema: &'static str,
    enrollment: &'a TrustStore,
}

impl PendingSetup {
    /// Prepares distinct signing roles without opening or writing any store.
    ///
    /// # Errors
    /// Refuses empty domains or missing/equal signing keys.
    pub fn new(
        domain: &str,
        primary: &str,
        release: &str,
        policy: SkPolicy,
        at_ms: u64,
    ) -> Result<Self, RecoveryError> {
        let mut trust = TrustStore::initial(domain, primary, release, policy, at_ms)?;
        let ceremony = format!("setup/{}", Uuid::new_v4());
        for key in &mut trust.keys {
            key.enrolled_by.clone_from(&ceremony);
        }
        Ok(Self { trust })
    }

    /// Snapshot to which the separate registration challenge must bind.
    #[must_use]
    pub fn trust(&self) -> &TrustStore {
        &self.trust
    }

    fn confirmed(
        &self,
        paper: &PaperPhrase,
        registration: &Registration,
    ) -> Result<TrustStore, RecoveryError> {
        RecoveryChange::enroll_passkey(&self.trust, registration)?;
        if !registration.credential().backed_up()? {
            return Err(RecoveryError::InvalidChange(
                "setup requires a backed-up recovery passkey",
            ));
        }
        let mut trust = self.trust.clone();
        trust.paper_verifier = Some(paper.verifier(&trust.trust_domain));
        trust.passkey = Some(registration.credential().clone());
        Ok(trust)
    }

    /// Exact public plan both ordinary keys must sign in [`SETUP_NAMESPACE`].
    ///
    /// # Errors
    /// Refuses expired or differently bound registration and serialization failure.
    pub fn plan(
        &self,
        paper: &PaperPhrase,
        registration: &Registration,
    ) -> Result<Vec<u8>, RecoveryError> {
        let trust = self.confirmed(paper, registration)?;
        plan_bytes(&trust)
    }

    /// Publishes both methods and keys once, after checking both possession proofs.
    ///
    /// Consumes this candidate even on refusal. The caller supplies independently
    /// re-entered paper and must retain it until publication is acknowledged.
    ///
    /// # Errors
    /// Refuses untrusted production authority, existing/corrupt enrollment, stale
    /// registration, missing/duplicate/wrong proofs or publication failure. A
    /// directory-sync error may follow publication; inspect state before retrying.
    pub fn apply(
        self,
        store: &Store,
        paper: &PaperPhrase,
        registration: &Registration,
        proofs: &[ReplacementProof],
    ) -> Result<TrustStore, RecoveryError> {
        if store.provenance()?.trusted {
            terminal::require_production_root(store)?;
            recovery::require_hardware_policy(&self.trust)?;
        }
        let locked = LockedTrust::acquire(store)?;
        if locked.load()?.is_some() {
            return Err(TrustError::AlreadyBootstrapped(self.trust.trust_domain).into());
        }
        let trust = self.confirmed(paper, registration)?;
        let bytes = plan_bytes(&trust)?;
        if proofs.len() != 2 {
            return Err(RecoveryError::InvalidChange(
                "setup requires both signing-key proofs",
            ));
        }
        for role in [Role::Primary, Role::Release] {
            let mut matching = proofs.iter().filter(|proof| proof.role == role);
            let proof = matching.next().ok_or(RecoveryError::Unauthorized)?;
            if matching.next().is_some() {
                return Err(RecoveryError::Unauthorized);
            }
            let key = trust.key_for(role).ok_or(RecoveryError::Unauthorized)?;
            sshsig::verify(
                &proof.signature,
                SETUP_NAMESPACE,
                &bytes,
                &key.public_key,
                key.sk_policy,
            )?;
        }
        // Verification may invoke subprocesses; recheck the deadline before write.
        self.confirmed(paper, registration)?;
        locked.write(&trust)?;
        Ok(trust)
    }
}

fn plan_bytes(trust: &TrustStore) -> Result<Vec<u8>, RecoveryError> {
    serde_json::to_vec(&Plan {
        schema: SETUP_NAMESPACE,
        enrollment: trust,
    })
    .map_err(|_| RecoveryError::InvalidChange("setup plan serialization failed"))
}

/// Discards all authority after the operator confirms the displayed snapshot.
///
/// The caller owns explicit last-resort confirmation in a trusted local terminal.
/// This is not recovery: old history authorization is deliberately invalidated.
///
/// # Errors
/// Refuses uninstalled/unprotected authority, a changed or missing snapshot,
/// contention, malformed state or filesystem failure.
pub fn reset(store: &Store, expected: &crate::canonical::Digest) -> Result<(), RecoveryError> {
    terminal::require_production(store)?;
    let locked = LockedTrust::acquire(store)?;
    let trust = locked.load()?.ok_or(TrustError::NotBootstrapped)?;
    if &trust.digest() != expected {
        return Err(RecoveryError::InvalidChange(
            "trust changed after reset review",
        ));
    }
    locked.reset()?;
    Ok(())
}
