//! Local trust in endorsers: which signers may focus review, for what.
//!
//! An endorser entry is local trust, never portable and never authority. The
//! effective set is the release-bound defaults plus Primary-signed local
//! additions, minus removals. A removal always wins: an operator can always
//! distrust a default, and a removal can only cause more review. An addition
//! for a key that is also a default replaces that default, because it is an
//! explicit, signed local decision about exactly that key.
//!
//! [`EndorserSet::decide`] is recomputed at every use. It performs no I/O
//! besides the signature check, and persistence of the layers belongs to the
//! caller.

use std::collections::BTreeSet;

use thiserror::Error;

use super::{EndorsementError, EndorserRole, Record, valid_public_key, valid_text};
use crate::sshsig::SkPolicy;

/// The longest validity window any endorser entry may accept: 180 days.
pub const MAX_LIFETIME_MS: u64 = 180 * 24 * 60 * 60 * 1000;

/// Local trust in one endorser key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EndorserEntry {
    /// The endorser's `OpenSSH` public key, `<algorithm> <base64>`, no comment.
    pub public_key: String,
    /// Roles this key is trusted to sign as.
    pub roles: Vec<EndorserRole>,
    /// Local display label for human views. Never part of a portable record.
    pub label: String,
    /// What the key's signatures must prove about the authenticator.
    pub sk_policy: SkPolicy,
    /// Longest accepted `expires_at_ms - issued_at_ms`, at most [`MAX_LIFETIME_MS`].
    pub max_lifetime_ms: u64,
}

/// The effective endorser set after combining layers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EndorserSet {
    entries: Vec<EndorserEntry>,
}

/// Why a verified-format record may not focus review here.
#[derive(Debug)]
pub enum Distrust {
    /// No effective entry holds the signing key.
    UnknownSigner,
    /// The key is trusted, but not for the role it signed as.
    RoleNotAllowed {
        /// The role the record states.
        role: EndorserRole,
    },
    /// The record's validity window exceeds the entry's cap.
    LifetimeExceedsCap {
        /// The record's stated window.
        lifetime_ms: u64,
        /// The entry's cap.
        cap_ms: u64,
    },
    /// Proof, signature, hardware policy or validity refused the record.
    Refused(EndorsementError),
}

/// The local trust decision for one record at one time.
#[derive(Debug)]
pub enum TrustDecision {
    /// A trusted endorser made this statement and it is currently valid.
    Trusted {
        /// The entry's local display label.
        label: String,
    },
    /// The record must not focus review.
    Untrusted(Distrust),
}

/// Why endorser layers could not be combined.
#[derive(Debug, Error)]
pub enum EndorserError {
    /// An entry holds a value the format forbids.
    #[error("invalid endorser entry field {field}: {reason}")]
    Invalid {
        /// The offending field.
        field: &'static str,
        /// What is wrong with it.
        reason: &'static str,
    },
    /// The same key appears twice within one layer.
    #[error("endorser key listed twice in one layer: {key}")]
    Duplicate {
        /// The repeated key.
        key: String,
    },
}

impl EndorserSet {
    /// Combines release defaults, signed local additions and removals.
    ///
    /// # Errors
    /// Returns [`EndorserError::Invalid`] for a malformed entry in either
    /// layer and [`EndorserError::Duplicate`] for a key repeated within one
    /// layer. Removals of keys that are not present are accepted.
    pub fn effective(
        defaults: &[EndorserEntry],
        additions: &[EndorserEntry],
        removals: &[String],
    ) -> Result<Self, EndorserError> {
        check_layer(defaults)?;
        check_layer(additions)?;
        let removed: BTreeSet<&str> = removals.iter().map(String::as_str).collect();
        let replaced: BTreeSet<&str> = additions
            .iter()
            .map(|entry| entry.public_key.as_str())
            .collect();
        let entries = defaults
            .iter()
            .filter(|entry| !replaced.contains(entry.public_key.as_str()))
            .chain(additions)
            .filter(|entry| !removed.contains(entry.public_key.as_str()))
            .cloned()
            .collect();
        Ok(Self { entries })
    }

    /// The effective entries.
    #[must_use]
    pub fn entries(&self) -> &[EndorserEntry] {
        &self.entries
    }

    /// Decides whether `record` may focus review at `now_ms`.
    ///
    /// Local policy (signer, role, lifetime cap) is checked before the proof
    /// and signature, so the refusal names the first policy a record breaks.
    /// The signature is then verified under the entry's own hardware policy.
    #[must_use]
    pub fn decide(&self, record: &Record, now_ms: u64) -> TrustDecision {
        let signing = record.signing();
        let Some(entry) = self
            .entries
            .iter()
            .find(|entry| entry.public_key == signing.signer_key)
        else {
            return TrustDecision::Untrusted(Distrust::UnknownSigner);
        };
        if !entry.roles.contains(&signing.signer_role) {
            return TrustDecision::Untrusted(Distrust::RoleNotAllowed {
                role: signing.signer_role,
            });
        }
        let lifetime_ms = signing.expires_at_ms.saturating_sub(signing.issued_at_ms);
        if lifetime_ms > entry.max_lifetime_ms {
            return TrustDecision::Untrusted(Distrust::LifetimeExceedsCap {
                lifetime_ms,
                cap_ms: entry.max_lifetime_ms,
            });
        }
        match record.verify(now_ms, entry.sk_policy) {
            Ok(_) => TrustDecision::Trusted {
                label: entry.label.clone(),
            },
            Err(error) => TrustDecision::Untrusted(Distrust::Refused(error)),
        }
    }
}

fn check_layer(entries: &[EndorserEntry]) -> Result<(), EndorserError> {
    let mut seen = BTreeSet::new();
    for entry in entries {
        validate(entry)?;
        if !seen.insert(entry.public_key.as_str()) {
            return Err(EndorserError::Duplicate {
                key: entry.public_key.clone(),
            });
        }
    }
    Ok(())
}

fn validate(entry: &EndorserEntry) -> Result<(), EndorserError> {
    let invalid = |field, reason| Err(EndorserError::Invalid { field, reason });
    if !valid_public_key(&entry.public_key) {
        return invalid(
            "public_key",
            "must be '<algorithm> <base64>' with no comment",
        );
    }
    if entry.roles.is_empty() {
        return invalid("roles", "must trust at least one role");
    }
    if !valid_text(&entry.label) {
        return invalid("label", "must be one non-empty printable line");
    }
    if entry.max_lifetime_ms == 0 || entry.max_lifetime_ms > MAX_LIFETIME_MS {
        return invalid("max_lifetime_ms", "must be between 1 ms and 180 days");
    }
    Ok(())
}
