//! Single-use paper authorization for exact, state-bound trust changes.
//! Development stores support software test keys; production requires the
//! installed authority and hardware policy. Neither path grants Verified posture.

use std::{collections::BTreeSet, io};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{EnrolledKey, Role, TrustError, TrustStore, normalize, persistence::LockedTrust};
use crate::{
    canonical::Digest,
    sshsig::{self, SkPolicy},
    store::{Store, StoreError},
};

mod phrase;
pub use phrase::PaperPhrase;
pub(crate) mod terminal;

/// Hardware authorization domain for paper enrollment or replacement.
pub const PAPER_NAMESPACE: &str = "louiselm.skills.paper-change/1";
/// Replacement keys prove possession in a separate, non-approval namespace.
pub const POSSESSION_NAMESPACE: &str = "louiselm.skills.recovery-possession/1";

/// A replacement ordinary signing credential, never a recovery signer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplacementKey {
    /// Ordinary signing role being replaced.
    pub role: Role,
    /// OpenSSH public key; assertion policy is inherited from the bound state.
    pub public_key: String,
}

/// Public bytes reviewed by the operator and signed by replacement keys.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaperChange {
    /// Domain-separated change schema.
    pub schema: String,
    /// Exact trust domain.
    pub trust_domain: String,
    /// Next trust-change sequence.
    pub sequence: u64,
    /// Digest of the complete prior trust snapshot, including approval history.
    pub predecessor: String,
    /// Exact replacement keys, or empty for paper enrollment/replacement alone.
    pub replacements: Vec<ReplacementKey>,
    /// Public verifier of the new, confirmed paper phrase.
    pub next_verifier: String,
}

impl PaperChange {
    /// Builds a plan; no authority changes until [`apply`] succeeds.
    ///
    /// # Errors
    /// Refuses exhausted counters, reused phrases, or invalid/duplicate key changes.
    pub fn new(
        trust: &TrustStore,
        mut replacements: Vec<ReplacementKey>,
        next: &PaperPhrase,
    ) -> Result<Self, PaperError> {
        for replacement in &mut replacements {
            replacement.public_key = normalize(&replacement.public_key);
        }
        replacements.sort_by_key(|replacement| replacement.role.name());
        let change = Self {
            schema: PAPER_NAMESPACE.to_owned(),
            trust_domain: trust.trust_domain.clone(),
            sequence: trust.next_sequence()?,
            predecessor: trust.digest().to_string(),
            replacements,
            next_verifier: next.verifier(&trust.trust_domain),
        };
        validate(trust, &change)?;
        Ok(change)
    }

    /// Returns exact public authorization bytes, containing no phrase.
    ///
    /// # Panics
    /// Only if a future schema introduces a fallible custom serializer.
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "Derived closed schema contains only JSON-native values."
    )]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("JSON-native paper change")
    }
}

/// Authority for this exact paper change. A phrase is borrowed, never serialized.
#[derive(Clone, Copy)]
pub enum PaperAuthorization<'a> {
    /// Current ordinary signing key authorizes enrollment or replacement.
    SigningKey {
        /// Primary or release role; recovery-role confusion is refused.
        role: Role,
        /// Signature over the canonical change in [`PAPER_NAMESPACE`].
        signature: &'a str,
    },
    /// Current paper phrase, consumed/replaced if the change succeeds.
    Phrase(&'a PaperPhrase),
}

/// A new key's signature over the entire reviewed change.
#[derive(Clone, Debug)]
pub struct ReplacementProof {
    /// Exactly one proof is required for each replaced role.
    pub role: Role,
    /// Signature in [`POSSESSION_NAMESPACE`], not an ordinary approval.
    pub signature: String,
}

/// A refused paper ceremony. Secret input never appears in diagnostics.
#[derive(Debug, Error)]
pub enum PaperError {
    /// Phrase length, words, or checksum is invalid.
    #[error("invalid paper phrase; expected 24 checksummed English words")]
    InvalidPhrase,
    /// The submitted phrase is not the current recovery authority.
    #[error("paper recovery authorization refused")]
    Unauthorized,
    /// The written replacement was not confirmed exactly.
    #[error("new paper phrase confirmation did not match; trust unchanged")]
    Confirmation,
    /// The operator cancelled or the trusted terminal closed.
    #[error("paper recovery cancelled; no change applied")]
    Cancelled,
    /// A closed-schema or state-bound condition was violated.
    #[error("paper change refused: {0}")]
    InvalidChange(&'static str),
    /// No installed, protected production authority is available.
    #[error("paper recovery needs the trusted installed tool and protected production store")]
    UntrustedAuthority,
    /// Trust state could not be read or durably changed.
    #[error(transparent)]
    Trust(#[from] TrustError),
    /// Store provenance is unusable.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// A public signature was refused.
    #[error(transparent)]
    Signature(#[from] sshsig::SignatureError),
    /// Randomness or local terminal I/O failed.
    #[error("paper recovery I/O failed: {0}")]
    Io(#[from] io::Error),
}

/// Applies a fully confirmed paper change under the shared trust/approval lock.
///
/// The caller owns trusted display/input: show the exact plan, generate and
/// display a new phrase, then pass the independently re-entered confirmation.
/// All signatures cover the same public plan. Hardware enrollment and phrase
/// authorization both retire the prior phrase; neither signs ordinary approvals.
///
/// # Errors
/// Refuses stale/malformed plans, wrong or reused phrases, failed confirmation,
/// role confusion, missing/invalid possession proofs, or untrusted production
/// authority. Propagates persistence errors; after a directory-sync failure the
/// new state may be visible, so retain both written phrases until inspecting it.
pub fn apply(
    store: &Store,
    change: &PaperChange,
    authorization: PaperAuthorization<'_>,
    proofs: &[ReplacementProof],
    confirmation: &PaperPhrase,
    at_ms: u64,
) -> Result<TrustStore, PaperError> {
    let production = store.provenance()?.trusted;
    if production {
        terminal::require_production(store)?;
    }
    let locked = LockedTrust::acquire(store)?;
    let mut trust = locked.load()?.ok_or(TrustError::NotBootstrapped)?;
    validate(&trust, change)?;
    if production {
        require_hardware_policy(&trust)?;
    }
    if confirmation.verifier(&trust.trust_domain) != change.next_verifier {
        return Err(PaperError::Confirmation);
    }
    authorize(&trust, change, authorization)?;
    verify_possession(&trust, change, proofs)?;
    for replacement in &change.replacements {
        let policy = replacement_policy(&trust, replacement.role);
        trust.retired.extend(
            trust
                .keys
                .iter()
                .filter(|key| key.role == replacement.role)
                .cloned(),
        );
        trust.keys.retain(|key| key.role != replacement.role);
        trust.keys.push(EnrolledKey {
            role: replacement.role,
            public_key: replacement.public_key.clone(),
            sk_policy: policy,
            enrolled_at_ms: at_ms,
            enrolled_by: "paper-change".to_owned(),
            attestation: None,
        });
    }
    if let Some(previous) = trust.paper_verifier.replace(change.next_verifier.clone()) {
        trust.retired_paper_verifiers.insert(previous);
    }
    trust.keys.sort_by_key(|key| key.role.name());
    trust.sequence = change.sequence;
    locked.write(&trust)?;
    Ok(trust)
}

fn validate(trust: &TrustStore, change: &PaperChange) -> Result<(), PaperError> {
    if change.schema != PAPER_NAMESPACE
        || change.trust_domain != trust.trust_domain
        || change.sequence != trust.next_sequence()?
        || change.predecessor != trust.digest().to_string()
    {
        return Err(PaperError::InvalidChange(
            "plan is malformed or stale; review a new plan",
        ));
    }
    if Digest::parse(&change.next_verifier).is_err() {
        return Err(PaperError::InvalidChange("invalid replacement verifier"));
    }
    if trust.paper_verifier.as_ref() == Some(&change.next_verifier)
        || trust
            .retired_paper_verifiers
            .contains(&change.next_verifier)
    {
        return Err(PaperError::InvalidChange(
            "a paper phrase must never be reused",
        ));
    }
    let mut roles = BTreeSet::new();
    let mut keys = BTreeSet::new();
    for replacement in &change.replacements {
        if replacement.role == Role::Recovery
            || !roles.insert(replacement.role.name())
            || !keys.insert(&replacement.public_key)
            || replacement.public_key.is_empty()
            || replacement.public_key != normalize(&replacement.public_key)
            || trust
                .keys
                .iter()
                .chain(&trust.retired)
                .any(|key| key.public_key == replacement.public_key)
        {
            return Err(PaperError::InvalidChange(
                "replacement keys must be new, distinct ordinary-role keys",
            ));
        }
    }
    Ok(())
}

fn authorize(
    trust: &TrustStore,
    change: &PaperChange,
    authorization: PaperAuthorization<'_>,
) -> Result<(), PaperError> {
    match authorization {
        PaperAuthorization::Phrase(phrase) => {
            if trust.paper_verifier.as_ref() != Some(&phrase.verifier(&trust.trust_domain)) {
                return Err(PaperError::Unauthorized);
            }
        }
        PaperAuthorization::SigningKey { role, signature } => {
            if role == Role::Recovery {
                return Err(PaperError::Unauthorized);
            }
            let key = trust.key_for(role).ok_or(PaperError::Unauthorized)?;
            sshsig::verify(
                signature,
                PAPER_NAMESPACE,
                &change.canonical_bytes(),
                &key.public_key,
                key.sk_policy,
            )?;
        }
    }
    Ok(())
}

fn replacement_policy(trust: &TrustStore, role: Role) -> SkPolicy {
    trust
        .key_for(role)
        .map_or_else(SkPolicy::require_presence_and_verification, |key| {
            key.sk_policy
        })
}

fn verify_possession(
    trust: &TrustStore,
    change: &PaperChange,
    proofs: &[ReplacementProof],
) -> Result<(), PaperError> {
    if proofs.len() != change.replacements.len() {
        return Err(PaperError::InvalidChange(
            "one possession proof is required per replacement",
        ));
    }
    for replacement in &change.replacements {
        let matching = proofs
            .iter()
            .filter(|proof| proof.role == replacement.role)
            .collect::<Vec<_>>();
        let [proof] = matching.as_slice() else {
            return Err(PaperError::InvalidChange(
                "duplicate or missing possession proof",
            ));
        };
        sshsig::verify(
            &proof.signature,
            POSSESSION_NAMESPACE,
            &change.canonical_bytes(),
            &replacement.public_key,
            replacement_policy(trust, replacement.role),
        )?;
    }
    Ok(())
}

pub(crate) fn require_hardware_policy(trust: &TrustStore) -> Result<(), PaperError> {
    if trust.admission_key()?.sk_policy != SkPolicy::require_presence_and_verification()
        || trust
            .key_for(Role::Release)
            .is_some_and(|key| key.sk_policy != SkPolicy::require_presence_and_verification())
    {
        return Err(PaperError::UntrustedAuthority);
    }
    Ok(())
}
