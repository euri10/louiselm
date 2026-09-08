//! Independent recovery methods authorize exact, state-bound trust changes.
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

use super::{
    paper::PaperPhrase,
    passkey::{self, Registration},
    terminal,
};

/// Hardware authorization domain for recovery-method or signing-key changes.
pub const RECOVERY_NAMESPACE: &str = "louiselm.skills.recovery-change/1";
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
pub struct RecoveryChange {
    /// Domain-separated change schema.
    pub schema: String,
    /// Exact trust domain.
    pub trust_domain: String,
    /// Next trust-change sequence.
    pub sequence: u64,
    /// Digest of the complete prior trust snapshot, including approval history.
    pub predecessor: String,
    /// Exact replacement keys, or empty when changing recovery methods alone.
    pub replacements: Vec<ReplacementKey>,
    /// Public verifier of the new, confirmed paper phrase, or None to preserve it.
    pub next_verifier: Option<String>,
    /// New public credential, or None to leave passkey recovery unchanged.
    pub next_passkey: Option<passkey::EnrolledPasskey>,
}

impl RecoveryChange {
    /// Builds a plan; no authority changes until [`apply`] succeeds.
    ///
    /// # Errors
    /// Refuses exhausted counters, reused phrases, or invalid/duplicate key changes.
    pub fn new(
        trust: &TrustStore,
        mut replacements: Vec<ReplacementKey>,
        next: Option<&PaperPhrase>,
    ) -> Result<Self, RecoveryError> {
        for replacement in &mut replacements {
            replacement.public_key = normalize(&replacement.public_key);
        }
        replacements.sort_by_key(|replacement| replacement.role.name());
        let change = Self {
            schema: RECOVERY_NAMESPACE.to_owned(),
            trust_domain: trust.trust_domain.clone(),
            sequence: trust.next_sequence()?,
            predecessor: trust.digest().to_string(),
            replacements,
            next_verifier: next.map(|phrase| phrase.verifier(&trust.trust_domain)),
            next_passkey: None,
        };
        validate(trust, &change)?;
        Ok(change)
    }

    /// Builds a passkey enrollment/replacement plan without changing paper recovery.
    ///
    /// # Errors
    /// Refuses stale/expired registration, reused credentials or exhausted state.
    pub fn enroll_passkey(
        trust: &TrustStore,
        registration: &Registration,
    ) -> Result<Self, RecoveryError> {
        let change = Self {
            schema: RECOVERY_NAMESPACE.to_owned(),
            trust_domain: trust.trust_domain.clone(),
            sequence: trust.next_sequence()?,
            predecessor: trust.digest().to_string(),
            replacements: Vec::new(),
            next_verifier: None,
            next_passkey: Some(registration.credential().clone()),
        };
        registration.check(&change)?;
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
        serde_json::to_vec(self).expect("JSON-native recovery change")
    }
}

/// Authority for this exact recovery change. Secret phrases are never serialized.
pub enum RecoveryAuthorization<'a> {
    /// Current ordinary signing key authorizes enrollment or replacement.
    SigningKey {
        /// Primary or release role; recovery-role confusion is refused.
        role: Role,
        /// Signature over the canonical change in [`RECOVERY_NAMESPACE`].
        signature: &'a str,
    },
    /// Current paper phrase, consumed/replaced if the change succeeds.
    Phrase(&'a PaperPhrase),
    /// Verified exact-change passkey assertion, consumed by apply.
    Passkey(passkey::Approval),
}

/// Required possession/confirmation evidence for newly enrolled recovery methods.
#[derive(Clone, Copy, Default)]
pub struct RecoveryConfirmation<'a> {
    /// Independently re-entered new paper phrase, required only when changing it.
    pub paper: Option<&'a PaperPhrase>,
    /// Fresh verified candidate, required only when changing the passkey.
    pub registration: Option<&'a Registration>,
}

/// A new key's signature over the entire reviewed change.
#[derive(Clone, Debug)]
pub struct ReplacementProof {
    /// Exactly one proof is required for each replaced role.
    pub role: Role,
    /// Signature in [`POSSESSION_NAMESPACE`], not an ordinary approval.
    pub signature: String,
}

/// A refused recovery ceremony. Secret input never appears in diagnostics.
#[derive(Debug, Error)]
pub enum RecoveryError {
    /// The local ceremony exhausted its shared lifetime before approval.
    #[error("recovery ceremony expired; inspect recovery status before starting a new ceremony")]
    Expired,
    /// A `WebAuthn` proof, registration or pending-state binding was refused.
    #[error("passkey recovery refused; expired, invalid or already used ceremony")]
    Passkey,
    /// A pre-submission browser failure, with only allowlisted diagnostic fields.
    #[error(
        "passkey {operation} failed ({code}); no change applied; inspect recovery status before retrying"
    )]
    Browser {
        /// Operation named by the trusted ceremony, not browser-supplied text.
        operation: &'static str,
        /// Allowlisted browser error name; never an arbitrary native message.
        code: &'static str,
    },
    /// Phrase length, words, or checksum is invalid.
    #[error("invalid paper phrase; expected 24 checksummed English words")]
    InvalidPhrase,
    /// The submitted phrase is not the current recovery authority.
    #[error("recovery authorization refused")]
    Unauthorized,
    /// The written replacement was not confirmed exactly.
    #[error("new paper phrase confirmation did not match; trust unchanged")]
    Confirmation,
    /// The operator cancelled or the trusted terminal closed.
    #[error("recovery cancelled; no change applied")]
    Cancelled,
    /// A closed-schema or state-bound condition was violated.
    #[error("recovery change refused: {0}")]
    InvalidChange(&'static str),
    /// No installed, protected production authority is available.
    #[error("recovery needs the trusted installed tool and protected production store")]
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
    #[error("recovery I/O failed: {0}")]
    Io(#[from] io::Error),
}

/// Applies a fully confirmed recovery change under the shared trust/approval lock.
///
/// The caller owns trusted display/input and must show the exact public plan.
/// Changed paper requires independent re-entry; changed passkeys require fresh
/// registration possession. Paper authorization consumes/replaces the paper;
/// other methods preserve it unless explicitly replacing it. No method signs
/// ordinary approvals. All signatures cover the same public plan.
///
/// # Errors
/// Refuses stale/malformed plans, wrong or reused phrases, failed confirmation,
/// role confusion, missing/invalid possession proofs, or untrusted production
/// authority. Propagates persistence errors; after a directory-sync failure the
/// new state may be visible, so retain both written phrases until inspecting it.
pub fn apply(
    store: &Store,
    change: &RecoveryChange,
    authorization: RecoveryAuthorization<'_>,
    proofs: &[ReplacementProof],
    confirmation: RecoveryConfirmation<'_>,
    at_ms: u64,
) -> Result<TrustStore, RecoveryError> {
    let production = store.provenance()?.trusted;
    let deadline = match &authorization {
        RecoveryAuthorization::Passkey(approval) => Some(approval.deadline()),
        _ => None,
    }
    .into_iter()
    .chain(confirmation.registration.map(Registration::deadline))
    .min();
    if production {
        terminal::require_production(store)?;
    }
    let locked = LockedTrust::acquire(store)?;
    let mut trust = locked.load()?.ok_or(TrustError::NotBootstrapped)?;
    validate(&trust, change)?;
    if production {
        require_hardware_policy(&trust)?;
    }
    if confirmation
        .paper
        .map(|phrase| phrase.verifier(&trust.trust_domain))
        != change.next_verifier
    {
        return Err(RecoveryError::Confirmation);
    }
    match (change.next_passkey.as_ref(), confirmation.registration) {
        (Some(_), Some(registration)) => registration.check(change)?,
        (None, None) => (),
        _ => return Err(RecoveryError::Passkey),
    }
    let updated_passkey = authorize(&trust, change, authorization)?;
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
            enrolled_by: "recovery-change".to_owned(),
            attestation: None,
        });
    }
    if let Some(next) = &change.next_verifier
        && let Some(previous) = trust.paper_verifier.replace(next.clone())
    {
        trust.retired_paper_verifiers.insert(previous);
    }
    if let Some(next) = &change.next_passkey {
        if let Some(previous) = trust.passkey.replace(next.clone()) {
            trust.retired_passkeys.insert(previous.fingerprint()?);
        }
    } else if let Some(updated) = updated_passkey {
        trust.passkey = Some(updated);
    }
    trust.keys.sort_by_key(|key| key.role.name());
    trust.sequence = change.sequence;
    if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
        return Err(RecoveryError::Passkey);
    }
    locked.write(&trust)?;
    Ok(trust)
}

pub(crate) fn validate(trust: &TrustStore, change: &RecoveryChange) -> Result<(), RecoveryError> {
    if change.schema != RECOVERY_NAMESPACE
        || change.trust_domain != trust.trust_domain
        || change.sequence != trust.next_sequence()?
        || change.predecessor != trust.digest().to_string()
    {
        return Err(RecoveryError::InvalidChange(
            "plan is malformed or stale; review a new plan",
        ));
    }
    if change.replacements.is_empty()
        && change.next_verifier.is_none()
        && change.next_passkey.is_none()
    {
        return Err(RecoveryError::InvalidChange("empty recovery change"));
    }
    if change
        .next_verifier
        .as_ref()
        .is_some_and(|next| Digest::parse(next).is_err())
    {
        return Err(RecoveryError::InvalidChange("invalid replacement verifier"));
    }
    if change.next_verifier.as_ref().is_some_and(|next| {
        trust.paper_verifier.as_ref() == Some(next) || trust.retired_paper_verifiers.contains(next)
    }) {
        return Err(RecoveryError::InvalidChange(
            "a paper phrase must never be reused",
        ));
    }
    if let Some(next) = &change.next_passkey {
        next.validate()?;
        let id = next.fingerprint()?;
        if trust.retired_passkeys.contains(&id)
            || trust
                .passkey
                .as_ref()
                .map(passkey::EnrolledPasskey::fingerprint)
                .transpose()?
                .as_ref()
                == Some(&id)
        {
            return Err(RecoveryError::Passkey);
        }
    }
    let mut roles = BTreeSet::new();
    let mut keys = BTreeSet::new();
    for replacement in &change.replacements {
        if !roles.insert(replacement.role.name())
            || !keys.insert(&replacement.public_key)
            || replacement.public_key.is_empty()
            || replacement.public_key != normalize(&replacement.public_key)
            || trust
                .keys
                .iter()
                .chain(&trust.retired)
                .any(|key| key.public_key == replacement.public_key)
        {
            return Err(RecoveryError::InvalidChange(
                "replacement keys must be new, distinct ordinary-role keys",
            ));
        }
    }
    Ok(())
}

fn authorize(
    trust: &TrustStore,
    change: &RecoveryChange,
    authorization: RecoveryAuthorization<'_>,
) -> Result<Option<passkey::EnrolledPasskey>, RecoveryError> {
    match authorization {
        RecoveryAuthorization::Passkey(approval) => {
            return approval.authorize(trust, change).map(Some);
        }
        RecoveryAuthorization::Phrase(phrase) => {
            if change.next_verifier.is_none()
                || trust.paper_verifier.as_ref() != Some(&phrase.verifier(&trust.trust_domain))
            {
                return Err(RecoveryError::Unauthorized);
            }
        }
        RecoveryAuthorization::SigningKey { role, signature } => {
            let key = trust.key_for(role).ok_or(RecoveryError::Unauthorized)?;
            sshsig::verify(
                signature,
                RECOVERY_NAMESPACE,
                &change.canonical_bytes(),
                &key.public_key,
                key.sk_policy,
            )?;
        }
    }
    Ok(None)
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
    change: &RecoveryChange,
    proofs: &[ReplacementProof],
) -> Result<(), RecoveryError> {
    if proofs.len() != change.replacements.len() {
        return Err(RecoveryError::InvalidChange(
            "one possession proof is required per replacement",
        ));
    }
    for replacement in &change.replacements {
        let matching = proofs
            .iter()
            .filter(|proof| proof.role == replacement.role)
            .collect::<Vec<_>>();
        let [proof] = matching.as_slice() else {
            return Err(RecoveryError::InvalidChange(
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

pub(crate) fn require_hardware_policy(trust: &TrustStore) -> Result<(), RecoveryError> {
    let primary = trust.admission_key()?;
    let release = trust
        .key_for(Role::Release)
        .ok_or(RecoveryError::UntrustedAuthority)?;
    if primary.sk_policy != SkPolicy::require_presence_and_verification()
        || release.sk_policy != SkPolicy::require_presence_and_verification()
        || release.public_key == primary.public_key
    {
        return Err(RecoveryError::UntrustedAuthority);
    }
    Ok(())
}
