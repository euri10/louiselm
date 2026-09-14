//! Pure conformance admission policy.
//!
//! Deciding is separate from reading protected storage: the caller supplies an
//! authenticated [`CertificateStatus`] and freshly measured [`HostSnapshot`],
//! and this module answers only whether that evidence admits a Verified launch.
//! Current passing evidence is eligibility, never a substitute for the other
//! Verified dimensions.

use super::installed::{CertificateStatus, HostSnapshot};

/// Whether an operator is present to authorize an explicit waiver.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Attendance {
    /// An operator can answer a waiver prompt scoped to this Session.
    Interactive,
    /// No operator is present; an unattended Run never waives.
    Unattended,
}

/// The exact condition that stops current evidence from admitting a launch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Condition {
    /// No retained certificate matches the freshly measured host inputs.
    Missing,
    /// Retained evidence does not match current measurements or did not pass.
    Stale,
    /// An attempt is running or was interrupted without completion proof.
    Incomplete,
    /// An observed containment failure is retained for this host.
    ContainmentFailure,
}

/// Admission decision for one launch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Admission {
    /// Evidence is current and passing; the digest binds it to the receipt.
    Admitted {
        /// Digest of the exact observation report admission relied on.
        report_digest: String,
    },
    /// Refusable by default, but an operator may waive this exact condition.
    Waivable(Condition),
    /// No waiver applies.
    Refused(Condition),
}

/// Decide whether retained host evidence admits a Verified launch.
///
/// A retained containment failure outranks every other input, including a
/// current passing certificate: only recertification covering the failed
/// boundary clears it, and no waiver reaches it. Unresolved cleanup is carried
/// in that same history, because cleanup proving containment is not
/// best-effort. An unfinished attempt is incomplete rather than a pass, so it
/// cannot fall back to an older certificate.
#[must_use]
pub fn evaluate(
    status: &CertificateStatus,
    measured: &HostSnapshot,
    attendance: Attendance,
) -> Admission {
    if !status.history.failures.is_empty() {
        return Admission::Refused(Condition::ContainmentFailure);
    }
    let condition = if status.pending {
        Condition::Incomplete
    } else {
        match &status.certificate {
            None => Condition::Missing,
            Some(certificate) => match certificate.observations.digest() {
                Ok(digest) if certificate.is_current(measured) => {
                    return Admission::Admitted {
                        report_digest: digest.to_string(),
                    };
                }
                // Unreadable retained evidence is stale, never an empty history.
                Ok(_) | Err(_) => Condition::Stale,
            },
        }
    };
    match attendance {
        Attendance::Interactive => Admission::Waivable(condition),
        Attendance::Unattended => Admission::Refused(condition),
    }
}

#[cfg(test)]
#[path = "admission_tests.rs"]
mod tests;
