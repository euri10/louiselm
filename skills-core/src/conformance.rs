//! Bounded observations shared by hostile probes and host certification.
//!
//! These pure records do not authenticate their producer or establish currency.
//! The installed authority must measure, protect and revalidate host evidence
//! before using it for admission. A disposable-guest report is never that proof.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::Digest;

pub mod admission;
#[cfg(target_os = "linux")]
pub mod installed;

/// Canonical observation report schema.
pub const REPORT_SCHEMA: &str = "louiselm.conformance.observations/1";
/// Maximum encoded observation report size.
pub const MAX_REPORT_BYTES: usize = 128 * 1024;

/// Required hostile matrix: 45 named attacks and whole-tree lifecycle.
///
/// Other acceptance gates (registry, startup and relay composition) still run
/// independently. Completing this inventory alone never certifies their paths.
pub const REQUIRED_CHECKS: [&str; 46] = [
    "operator-home",
    "symlink-operator-home",
    "operator-checkout",
    "symlink-operator-checkout",
    "operator-credentials",
    "symlink-operator-credentials",
    "native-provider-config",
    "symlink-native-provider-config",
    "native-mcp-config",
    "symlink-native-mcp-config",
    "runtime-write-provider-config.json",
    "runtime-write-mcp.json",
    "runtime-write-adapter.js",
    "ambient-environment",
    "inherited-file-fd",
    "inherited-socket-fd",
    "proc-operator",
    "ptrace-operator",
    "signal-operator",
    "proc-launcher",
    "ptrace-launcher",
    "signal-launcher",
    "proc-broker",
    "ptrace-broker",
    "signal-broker",
    "proc-second-session",
    "ptrace-second-session",
    "signal-second-session",
    "second-session-file",
    "first-session-file",
    "proc-first-session",
    "ptrace-first-session",
    "signal-first-session",
    "pathname-unix",
    "dbus-systemd",
    "docker",
    "abstract-unix",
    "ipv4",
    "ipv6",
    "ipv4-loopback",
    "ipv6-loopback",
    "own-channel-first",
    "foreign-channel-first",
    "own-channel-second",
    "foreign-channel-second",
    "lifecycle",
];

/// Environment in which the trusted producer made observations.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// Disposable component fixtures; no installed-host claim.
    DisposableGuest,
    /// Installed host; authority and measurements require separate validation.
    InstalledHost,
}

/// An operation's actual result, never inferred from a mechanism flag.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Outcome {
    /// The sentinel operation succeeded.
    Allowed,
    /// The operation met a recognized denial, with its bounded observation.
    Denied(String),
    /// The observation was unavailable or unexpected, including timeout.
    Error(String),
}

impl Outcome {
    fn valid(&self) -> bool {
        match self {
            Self::Allowed => true,
            Self::Denied(detail) | Self::Error(detail) => {
                !detail.is_empty() && detail.len() <= 1024
            }
        }
    }
}

/// One response from the common inside/outside probe implementation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    /// Fixed probe name.
    pub name: String,
    /// Actual result.
    pub outcome: Outcome,
}

/// One positive control paired with one confined observation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Check {
    /// Name in the required inventory.
    pub name: String,
    /// Sentinel operation without the tested restriction.
    pub control: Outcome,
    /// The same operation under the tested restriction.
    pub confined: Outcome,
}

/// Construct an exact group of paired observations without accepting omissions.
///
/// # Errors
/// Rejects unknown, duplicate, missing, unexpected or unbounded observations.
pub fn pair(
    names: &[&str],
    outside: &[Observation],
    inside: &[Observation],
) -> Result<Vec<Check>, ReportError> {
    if names.is_empty()
        || names.len() > REQUIRED_CHECKS.len()
        || outside.len() != names.len()
        || inside.len() != names.len()
        || names.iter().collect::<BTreeSet<_>>().len() != names.len()
    {
        return Err(ReportError::InvalidObservations);
    }
    let find = |rows: &[Observation], name: &str| {
        let mut matching = rows.iter().filter(|row| row.name == name);
        let row = matching.next().ok_or(ReportError::InvalidObservations)?;
        if matching.next().is_some() || !row.outcome.valid() {
            return Err(ReportError::InvalidObservations);
        }
        Ok(row.outcome.clone())
    };
    names
        .iter()
        .map(|name| {
            if !REQUIRED_CHECKS.contains(name) {
                return Err(ReportError::InvalidObservations);
            }
            Ok(Check {
                name: (*name).into(),
                control: find(outside, name)?,
                confined: find(inside, name)?,
            })
        })
        .collect()
}

/// Actual cleanup status of this test's owned resources.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cleanup {
    /// No cleanup proof yet; never eligible for a pass.
    Pending,
    /// All owned process trees and reusable identities were safely disposed.
    Confirmed,
    /// An attempted cleanup could not establish containment/disposal.
    Unconfirmed,
}

/// Exact observations; a caller cannot declare its own passing verdict.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    /// Must be [`REPORT_SCHEMA`].
    pub schema: String,
    /// Distinguishes component fixtures from installed-host observations.
    pub scope: Scope,
    /// At most one pair for each required check.
    pub checks: Vec<Check>,
    /// False after interruption or any unfinished required work.
    pub completed: bool,
    /// Cleanup evidence, independently required for success.
    pub cleanup: Cleanup,
}

/// Derived outcome; observed failure takes precedence over incomplete work.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReportResult {
    /// Complete inventory, successful controls, observed denials and cleanup.
    Passed,
    /// No observed failure, but required evidence is absent or inconclusive.
    Incomplete,
    /// Non-waivable observed failures, identified by sorted fixed names.
    Failed(Vec<String>),
}

impl Report {
    /// Validate structure and derive the verdict from actual observations.
    ///
    /// # Errors
    /// Rejects wrong schemas and contradictory, unexpected or oversized rows.
    pub fn result(&self) -> Result<ReportResult, ReportError> {
        if self.schema != REPORT_SCHEMA || self.checks.len() > REQUIRED_CHECKS.len() {
            return Err(ReportError::InvalidObservations);
        }
        let mut names = BTreeSet::new();
        let mut failures = BTreeSet::new();
        let mut complete = self.completed && self.cleanup == Cleanup::Confirmed;
        for check in &self.checks {
            if !REQUIRED_CHECKS.contains(&check.name.as_str())
                || !names.insert(check.name.as_str())
                || !check.control.valid()
                || !check.confined.valid()
            {
                return Err(ReportError::InvalidObservations);
            }
            if check.confined == Outcome::Allowed {
                failures.insert(check.name.clone());
            }
            complete &=
                check.control == Outcome::Allowed && matches!(check.confined, Outcome::Denied(_));
        }
        if self.cleanup == Cleanup::Unconfirmed {
            failures.insert("cleanup".into());
        }
        Ok(if !failures.is_empty() {
            ReportResult::Failed(failures.into_iter().collect())
        } else if complete && names.len() == REQUIRED_CHECKS.len() {
            ReportResult::Passed
        } else {
            ReportResult::Incomplete
        })
    }

    /// Encode validated observations in their recorded order.
    ///
    /// # Errors
    /// Returns invalid evidence or encoding/size errors.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ReportError> {
        self.result()?;
        let bytes = serde_json::to_vec(self).map_err(|_| ReportError::InvalidEncoding)?;
        if bytes.len() > MAX_REPORT_BYTES {
            return Err(ReportError::InvalidEncoding);
        }
        Ok(bytes)
    }

    /// Read bounded, canonical, closed-schema evidence without normalization.
    ///
    /// # Errors
    /// Returns an error for malformed, noncanonical or oversized input.
    pub fn parse_canonical(bytes: &[u8]) -> Result<Self, ReportError> {
        if bytes.len() > MAX_REPORT_BYTES {
            return Err(ReportError::InvalidEncoding);
        }
        let report: Self =
            serde_json::from_slice(bytes).map_err(|_| ReportError::InvalidEncoding)?;
        if report.canonical_bytes()? != bytes {
            return Err(ReportError::InvalidEncoding);
        }
        Ok(report)
    }

    /// Hash the exact canonical observation report.
    ///
    /// # Errors
    /// Returns an error when the report cannot be canonically encoded.
    pub fn digest(&self) -> Result<Digest, ReportError> {
        Ok(Digest::of(&self.canonical_bytes()?))
    }
}

/// Invalid report data, distinct from a valid report of a failed probe.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum ReportError {
    /// The fixed inventory or bounded exactly-one observation contract failed.
    #[error("invalid conformance observations")]
    InvalidObservations,
    /// Encoded evidence was oversized, malformed or noncanonical.
    #[error("invalid conformance encoding")]
    InvalidEncoding,
}

/// Retained evidence for a failure, independent of certificate currency.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Failure {
    /// Failed check, or `cleanup` for unresolved disposal.
    pub check: String,
    /// Exact failing observation report.
    pub report_digest: String,
    /// A fixture pass cannot clear an installed-host failure.
    pub scope: Scope,
}

/// Pure failure-history reduction; persistence/authentication belong to authority.
///
/// Callers must supply authenticated reports applicable to the same host and
/// tested boundaries. Never replace unreadable history with [`Self::default`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FailureHistory {
    /// Retained failures; expiry, reboot and release changes do not remove them.
    pub failures: Vec<Failure>,
}

impl FailureHistory {
    /// Record failures or clear covered failures after a complete passing run.
    ///
    /// Unconfirmed cleanup needs proof about the original resources: disposing
    /// a fresh fixture cannot clear it. This function never modifies its input.
    ///
    /// # Errors
    /// Rejects malformed history/reports; the caller must retain existing state.
    pub fn record(&self, report: &Report) -> Result<Self, ReportError> {
        if self.failures.len() > 2 * (REQUIRED_CHECKS.len() + 1)
            || self.failures.iter().any(|failure| {
                (failure.check != "cleanup" && !REQUIRED_CHECKS.contains(&failure.check.as_str()))
                    || Digest::parse(&failure.report_digest).is_err()
            })
            || self.failures.iter().enumerate().any(|(index, failure)| {
                self.failures[..index]
                    .iter()
                    .any(|other| other.scope == failure.scope && other.check == failure.check)
            })
        {
            return Err(ReportError::InvalidObservations);
        }
        let digest = report.digest()?.to_string();
        let mut history = self.clone();
        match report.result()? {
            ReportResult::Passed => history
                .failures
                .retain(|failure| failure.scope != report.scope || failure.check == "cleanup"),
            ReportResult::Incomplete => (),
            ReportResult::Failed(failures) => {
                for check in failures {
                    if !history
                        .failures
                        .iter()
                        .any(|old| old.check == check && old.scope == report.scope)
                    {
                        history.failures.push(Failure {
                            check,
                            report_digest: digest.clone(),
                            scope: report.scope,
                        });
                    }
                }
            }
        }
        Ok(history)
    }
}
