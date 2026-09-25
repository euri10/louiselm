//! Pure conformance admission policy.
//!
//! Deciding is separate from reading protected storage: the caller supplies an
//! authenticated `CertificateStatus` and freshly measured `HostSnapshot`, and
//! this module answers only whether that evidence admits a Verified launch.
//! Current passing evidence is eligibility, never a substitute for the other
//! Verified dimensions.
//!
//! The decision vocabulary is platform-independent so receipts can record it;
//! only the evaluation reads installed-host evidence.

use serde::{Deserialize, Serialize};

#[cfg(target_os = "linux")]
use super::installed::{CertificateStatus, HostSnapshot};

/// Whether an operator is present to authorize an explicit waiver.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Attendance {
    /// An operator can answer a waiver prompt scoped to this Session.
    Interactive,
    /// No operator is present; an unattended Run never waives.
    #[default]
    Unattended,
}

/// Activation policy read only from the protected installed launcher configuration.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Enforcement {
    /// Ordinary installation remains explicitly unevaluated until cutover.
    #[default]
    PreCutover,
    /// Every new launch must pass conformance admission or an exact authorized waiver.
    Enforced,
}

/// The exact condition that stops current evidence from admitting a launch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Condition {
    /// No retained certificate matches the freshly measured host inputs.
    Missing,
    /// Retained evidence does not match current measurements or did not pass.
    Stale,
    /// An attempt is running or was interrupted without completion proof.
    Incomplete,
    /// An observed containment failure is retained for this host.
    ContainmentFailure,
    /// Required production Sender guard enforcement is absent or no longer proved.
    GuardUnavailable,
}

impl Condition {
    /// Whether an explicit interactive waiver may cover this condition.
    #[must_use]
    pub const fn is_waivable(self) -> bool {
        matches!(self, Self::Missing | Self::Stale | Self::Incomplete)
    }
}

/// An operator's explicit approval of one condition for one Session.
///
/// A waiver degrades only the condition it names and cannot outlive its
/// Session; it is never a statement that the evidence exists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Waiver {
    /// Session the operator approved, which alone may present this waiver.
    pub session_id: String,
    /// The exact condition approved, never a blanket approval.
    pub condition: Condition,
}

/// What the caller is asking admission to allow.
#[derive(Clone, Copy, Debug)]
pub struct Request<'a> {
    /// Session this launch would start.
    pub session_id: &'a str,
    /// Whether an operator is present to authorize a waiver.
    pub attendance: Attendance,
    /// An operator waiver already authorized for this Session, if any.
    pub waiver: Option<&'a Waiver>,
}

/// Admission decision for one launch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Admission {
    /// Evidence is current and passing; the digest binds it to the receipt.
    Admitted {
        /// Digest of the exact observation report admission relied on.
        report_digest: String,
    },
    /// An operator waived this exact condition; isolation stays unverified.
    Waived {
        /// The condition the operator approved for this Session.
        condition: Condition,
        /// Digest of the evidence the waiver rode past, where any exists.
        report_digest: Option<String>,
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
///
/// A waiver applies only when its own Session presents it, an operator is
/// present, and it names this exact condition; it degrades that one condition
/// and leaves isolation unverified for presentation to report.
#[cfg(target_os = "linux")]
#[must_use]
pub fn evaluate(
    status: &CertificateStatus,
    measured: &HostSnapshot,
    request: &Request<'_>,
) -> Admission {
    if status
        .history
        .failures
        .iter()
        .any(|failure| failure.check == super::SENDER_GUARD_CHECK)
    {
        return Admission::Refused(Condition::GuardUnavailable);
    }
    if !status.history.failures.is_empty() {
        return Admission::Refused(Condition::ContainmentFailure);
    }
    // Guard support cannot borrow old observations, another host's proof, or
    // a waiver for missing evidence. An otherwise incomplete report can still
    // carry this exact completed probe, preserving unrelated waiver policy.
    if status.pending
        || !status.certificate.as_ref().is_some_and(|certificate| {
            certificate.canonical_bytes().is_ok()
                && &certificate.host == measured
                && certificate.observations.cleanup == super::Cleanup::Confirmed
                && certificate.observations.checks.iter().any(|check| {
                    check.name == super::SENDER_GUARD_CHECK
                        && check.control == super::Outcome::Allowed
                        && matches!(check.confined, super::Outcome::Denied(_))
                })
        })
    {
        return Admission::Refused(Condition::GuardUnavailable);
    }
    let retained = status
        .certificate
        .as_ref()
        .and_then(|certificate| certificate.observations.digest().ok())
        .map(|digest| digest.to_string());
    let condition = if status.pending {
        Condition::Incomplete
    } else {
        match &status.certificate {
            None => Condition::Missing,
            Some(certificate) => match &retained {
                Some(digest) if certificate.is_current(measured) => {
                    return Admission::Admitted {
                        report_digest: digest.clone(),
                    };
                }
                // Unreadable retained evidence is stale, never an empty history.
                Some(_) | None => Condition::Stale,
            },
        }
    };
    if request.attendance == Attendance::Interactive
        && request.waiver.is_some_and(|waiver| {
            waiver.session_id == request.session_id && waiver.condition == condition
        })
    {
        return Admission::Waived {
            condition,
            report_digest: retained,
        };
    }
    match request.attendance {
        Attendance::Interactive => Admission::Waivable(condition),
        Attendance::Unattended => Admission::Refused(condition),
    }
}

#[cfg(all(test, target_os = "linux"))]
#[path = "admission_tests.rs"]
mod tests;
