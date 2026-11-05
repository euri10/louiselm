//! Bounded, non-authoritative evidence for an Endorsement withdrawal.

use serde::{Deserialize, Serialize};

use super::{AttentionError, AttentionKind};

/// Whether Admission relied on the withdrawn Endorsement for this package.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WithdrawalUrgency {
    /// The withdrawn record was part of the Admission evidence.
    ReliedOn,
    /// The record names the package, but Admission did not rely on it.
    SubjectOnly,
}

/// Closed evidence displayed with a supply-scoped withdrawal condition.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AttentionWithdrawal {
    /// Affected package's canonical `sha256:` digest.
    pub package_digest: String,
    /// Signed withdrawal record's canonical `sha256:` digest.
    pub withdrawal_digest: String,
    /// Local trust label or bounded key fingerprint, never portable review prose.
    pub endorser: String,
    /// Strongest qualifying use of this package and withdrawal pair.
    pub urgency: WithdrawalUrgency,
}

pub(super) fn validate(
    kind: AttentionKind,
    withdrawal: Option<&AttentionWithdrawal>,
) -> Result<(), AttentionError> {
    let Some(withdrawal) = withdrawal else {
        return if kind == AttentionKind::EndorsementWithdrawn {
            Err(AttentionError::Invalid(
                "withdrawal evidence is required".into(),
            ))
        } else {
            Ok(())
        };
    };
    if kind != AttentionKind::EndorsementWithdrawn {
        return Err(AttentionError::Invalid(
            "withdrawal evidence does not match kind".into(),
        ));
    }
    for digest in [&withdrawal.package_digest, &withdrawal.withdrawal_digest] {
        if !digest.strip_prefix("sha256:").is_some_and(|value| {
            value.len() == 64
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        }) {
            return Err(AttentionError::Invalid(
                "withdrawal digests must be canonical sha256 text".into(),
            ));
        }
    }
    if withdrawal.endorser.trim().is_empty()
        || withdrawal.endorser.len() > 256
        || withdrawal.endorser.chars().any(char::is_control)
    {
        return Err(AttentionError::Invalid(
            "withdrawal endorser must be a nonempty printable line of at most 256 bytes".into(),
        ));
    }
    Ok(())
}
