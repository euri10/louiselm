#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Decision-table fixtures assert admission refusals."
)]

use super::*;
use crate::{
    Digest,
    conformance::{
        Check, Cleanup, Failure, FailureHistory, Outcome, REPORT_SCHEMA, REQUIRED_CHECKS, Report,
        Scope,
        installed::{Certificate, CertificateStatus, HostSnapshot, PROFILE},
    },
};

fn host() -> HostSnapshot {
    let digest = Digest::of(b"fixture").to_string();
    HostSnapshot {
        profile: PROFILE.into(),
        machine_digest: digest.clone(),
        boot_id: "00000000-0000-0000-0000-000000000001".into(),
        release_digest: digest.clone(),
        inputs: ["launcher", "backend", "policy", "kernel", "loader"]
            .into_iter()
            .map(|key| (key.into(), digest.clone()))
            .collect(),
    }
}

fn passing_report() -> Report {
    Report {
        schema: REPORT_SCHEMA.into(),
        scope: Scope::InstalledHost,
        checks: REQUIRED_CHECKS
            .iter()
            .map(|name| Check {
                name: (*name).into(),
                control: Outcome::Allowed,
                confined: Outcome::Denied("kernel denial".into()),
            })
            .collect(),
        completed: true,
        cleanup: Cleanup::Confirmed,
    }
}

fn certified(host: HostSnapshot) -> Certificate {
    Certificate::new(host, passing_report()).unwrap()
}

fn status(certificate: Option<Certificate>) -> CertificateStatus {
    CertificateStatus {
        certificate,
        history: FailureHistory::default(),
        pending: false,
    }
}

#[test]
fn current_passing_evidence_admits_and_binds_the_exact_report_digest() {
    let measured = host();
    let status = status(Some(certified(measured.clone())));
    let expected = passing_report().digest().unwrap().to_string();

    match evaluate(&status, &measured, Attendance::Unattended) {
        Admission::Admitted { report_digest } => assert_eq!(report_digest, expected),
        other => panic!("current passing evidence must admit, observed {other:?}"),
    }
}

#[test]
fn a_changed_measured_input_makes_retained_evidence_stale() {
    let certified_host = host();
    let status = status(Some(certified(certified_host.clone())));
    let mut measured = certified_host;
    measured
        .inputs
        .insert("launcher".into(), Digest::of(b"upgraded").to_string());

    assert_eq!(
        evaluate(&status, &measured, Attendance::Unattended),
        Admission::Refused(Condition::Stale),
        "a version label cannot stand in for unchanged bytes"
    );
}

#[test]
fn a_reboot_makes_retained_evidence_stale() {
    let certified_host = host();
    let status = status(Some(certified(certified_host.clone())));
    let mut measured = certified_host;
    measured.boot_id = "00000000-0000-0000-0000-000000000002".into();

    assert_eq!(
        evaluate(&status, &measured, Attendance::Unattended),
        Admission::Refused(Condition::Stale)
    );
}

#[test]
fn absent_evidence_is_waivable_only_when_an_operator_is_present() {
    let measured = host();
    let status = status(None);

    assert_eq!(
        evaluate(&status, &measured, Attendance::Interactive),
        Admission::Waivable(Condition::Missing)
    );
    assert_eq!(
        evaluate(&status, &measured, Attendance::Unattended),
        Admission::Refused(Condition::Missing),
        "unattended Runs never waive"
    );
}

#[test]
fn an_interrupted_attempt_is_incomplete_and_never_a_pass() {
    let measured = host();
    let mut status = status(Some(certified(measured.clone())));
    status.pending = true;

    assert_eq!(
        evaluate(&status, &measured, Attendance::Interactive),
        Admission::Waivable(Condition::Incomplete),
        "an unfinished attempt cannot ride on an older certificate"
    );
    assert_eq!(
        evaluate(&status, &measured, Attendance::Unattended),
        Admission::Refused(Condition::Incomplete)
    );
}

#[test]
fn a_retained_containment_failure_is_non_waivable() {
    let measured = host();
    let mut status = status(Some(certified(measured.clone())));
    status.history.failures.push(Failure {
        check: REQUIRED_CHECKS[0].into(),
        report_digest: Digest::of(b"failing").to_string(),
        scope: Scope::InstalledHost,
    });

    for attendance in [Attendance::Interactive, Attendance::Unattended] {
        assert_eq!(
            evaluate(&status, &measured, attendance),
            Admission::Refused(Condition::ContainmentFailure),
            "observed containment failure outranks current passing evidence"
        );
    }
}

#[test]
fn unconfirmed_cleanup_is_retained_as_a_non_waivable_failure() {
    let measured = host();
    let mut status = status(Some(certified(measured.clone())));
    status.history.failures.push(Failure {
        check: "cleanup".into(),
        report_digest: Digest::of(b"unproven").to_string(),
        scope: Scope::InstalledHost,
    });

    assert_eq!(
        evaluate(&status, &measured, Attendance::Interactive),
        Admission::Refused(Condition::ContainmentFailure),
        "cleanup that proves containment is not best-effort"
    );
}
