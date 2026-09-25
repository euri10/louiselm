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

fn incomplete(host: HostSnapshot) -> Certificate {
    let mut report = passing_report();
    report.checks.remove(0);
    Certificate::new(host, report).unwrap()
}

const SESSION: &str = "session-fixture";

fn unwaived(attendance: Attendance) -> Request<'static> {
    Request {
        session_id: SESSION,
        attendance,
        waiver: None,
    }
}

fn status(certificate: Option<Certificate>) -> CertificateStatus {
    CertificateStatus {
        certificate,
        history: FailureHistory::default(),
        pending: false,
    }
}

#[test]
fn missing_sender_guard_proof_is_never_waivable() {
    let measured = host();
    let waiver = Waiver {
        session_id: SESSION.into(),
        condition: Condition::GuardUnavailable,
    };
    for attendance in [Attendance::Interactive, Attendance::Unattended] {
        assert_eq!(
            evaluate(&status(None), &measured, &waived(attendance, &waiver)),
            Admission::Refused(Condition::GuardUnavailable)
        );
        let mut report = passing_report();
        report
            .checks
            .retain(|check| check.name != crate::conformance::SENDER_GUARD_CHECK);
        assert_eq!(
            evaluate(
                &status(Some(Certificate::new(measured.clone(), report).unwrap())),
                &measured,
                &waived(attendance, &waiver)
            ),
            Admission::Refused(Condition::GuardUnavailable)
        );
    }
}

#[test]
fn failed_guard_observations_and_changed_measured_bytes_refuse_every_waiver() {
    let measured = host();
    for condition in [
        Condition::Missing,
        Condition::Stale,
        Condition::Incomplete,
        Condition::GuardUnavailable,
    ] {
        let waiver = Waiver {
            session_id: SESSION.into(),
            condition,
        };
        for attendance in [Attendance::Interactive, Attendance::Unattended] {
            for outcome in [
                Outcome::Allowed,
                Outcome::Error("production load failed".into()),
            ] {
                let mut report = passing_report();
                report
                    .checks
                    .iter_mut()
                    .find(|check| check.name == crate::conformance::SENDER_GUARD_CHECK)
                    .unwrap()
                    .confined = outcome;
                assert_eq!(
                    evaluate(
                        &status(Some(Certificate::new(measured.clone(), report).unwrap())),
                        &measured,
                        &waived(attendance, &waiver)
                    ),
                    Admission::Refused(Condition::GuardUnavailable)
                );
            }
            for input in [
                "launcher",
                "loader",
                "library/libbpf.so.1",
                "/sys/kernel/btf/vmlinux",
                "/sys/kernel/security/lsm",
            ] {
                let retained = status(Some(certified(measured.clone())));
                let mut changed = measured.clone();
                changed
                    .inputs
                    .insert(input.into(), Digest::of(b"changed").to_string());
                assert_eq!(
                    evaluate(&retained, &changed, &waived(attendance, &waiver)),
                    Admission::Refused(Condition::GuardUnavailable)
                );
            }
        }
    }
}

#[test]
fn current_passing_evidence_admits_and_binds_the_exact_report_digest() {
    let measured = host();
    let status = status(Some(certified(measured.clone())));
    let expected = passing_report().digest().unwrap().to_string();

    match evaluate(&status, &measured, &unwaived(Attendance::Unattended)) {
        Admission::Admitted { report_digest } => assert_eq!(report_digest, expected),
        other => panic!("current passing evidence must admit, observed {other:?}"),
    }
}

#[test]
fn a_changed_measured_input_invalidates_guard_support() {
    let certified_host = host();
    let status = status(Some(certified(certified_host.clone())));
    let mut measured = certified_host;
    measured
        .inputs
        .insert("launcher".into(), Digest::of(b"upgraded").to_string());

    assert_eq!(
        evaluate(&status, &measured, &unwaived(Attendance::Unattended)),
        Admission::Refused(Condition::GuardUnavailable),
        "a version label cannot stand in for unchanged bytes"
    );
}

#[test]
fn a_reboot_requires_current_guard_support() {
    let certified_host = host();
    let status = status(Some(certified(certified_host.clone())));
    let mut measured = certified_host;
    measured.boot_id = "00000000-0000-0000-0000-000000000002".into();

    assert_eq!(
        evaluate(&status, &measured, &unwaived(Attendance::Unattended)),
        Admission::Refused(Condition::GuardUnavailable)
    );
}

#[test]
fn absent_evidence_cannot_prove_required_guard_support() {
    let measured = host();
    let status = status(None);

    assert_eq!(
        evaluate(&status, &measured, &unwaived(Attendance::Interactive)),
        Admission::Refused(Condition::GuardUnavailable)
    );
    assert_eq!(
        evaluate(&status, &measured, &unwaived(Attendance::Unattended)),
        Admission::Refused(Condition::GuardUnavailable),
        "unattended Runs never waive"
    );
}

#[test]
fn an_interrupted_attempt_is_incomplete_and_never_a_pass() {
    let measured = host();
    let mut status = status(Some(certified(measured.clone())));
    status.pending = true;

    assert_eq!(
        evaluate(&status, &measured, &unwaived(Attendance::Interactive)),
        Admission::Refused(Condition::GuardUnavailable),
        "an unfinished attempt cannot ride on an older certificate"
    );
    assert_eq!(
        evaluate(&status, &measured, &unwaived(Attendance::Unattended)),
        Admission::Refused(Condition::GuardUnavailable)
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
            evaluate(&status, &measured, &unwaived(attendance)),
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
        evaluate(&status, &measured, &unwaived(Attendance::Interactive)),
        Admission::Refused(Condition::ContainmentFailure),
        "cleanup that proves containment is not best-effort"
    );
}

fn waived(attendance: Attendance, waiver: &Waiver) -> Request<'_> {
    Request {
        session_id: SESSION,
        attendance,
        waiver: Some(waiver),
    }
}

#[test]
fn an_explicit_waiver_admits_the_exact_condition_it_names() {
    let measured = host();
    let certificate = incomplete(measured.clone());
    let digest = certificate.observations.digest().unwrap().to_string();
    let status = status(Some(certificate));
    let waiver = Waiver {
        session_id: SESSION.into(),
        condition: Condition::Stale,
    };

    assert_eq!(
        evaluate(
            &status,
            &measured,
            &waived(Attendance::Interactive, &waiver)
        ),
        Admission::Waived {
            condition: Condition::Stale,
            report_digest: Some(digest),
        },
        "the waived launch records the actual evidence status"
    );
}

#[test]
fn a_waiver_binds_the_stale_report_it_rode_past() {
    let certified_host = host();
    let certificate = incomplete(certified_host.clone());
    let digest = certificate.observations.digest().unwrap().to_string();
    let status = status(Some(certificate));
    let measured = certified_host;
    let waiver = Waiver {
        session_id: SESSION.into(),
        condition: Condition::Stale,
    };

    assert_eq!(
        evaluate(
            &status,
            &measured,
            &waived(Attendance::Interactive, &waiver)
        ),
        Admission::Waived {
            condition: Condition::Stale,
            report_digest: Some(digest),
        },
        "an auditor must reach the exact evidence the waiver bypassed"
    );
}

#[test]
fn a_waiver_never_covers_a_condition_it_did_not_name() {
    let measured = host();
    let status = status(Some(incomplete(measured.clone())));
    let waiver = Waiver {
        session_id: SESSION.into(),
        condition: Condition::Missing,
    };

    assert_eq!(
        evaluate(
            &status,
            &measured,
            &waived(Attendance::Interactive, &waiver)
        ),
        Admission::Waivable(Condition::Stale),
        "approving absent evidence does not approve an incomplete report"
    );
}

#[test]
fn a_waiver_is_scoped_to_the_session_that_authorized_it() {
    let measured = host();
    let status = status(Some(incomplete(measured.clone())));
    let waiver = Waiver {
        session_id: "another-session".into(),
        condition: Condition::Stale,
    };

    assert_eq!(
        evaluate(
            &status,
            &measured,
            &waived(Attendance::Interactive, &waiver)
        ),
        Admission::Waivable(Condition::Stale),
        "a waiver cannot be replayed into a different Session"
    );
}

#[test]
fn an_unattended_run_cannot_present_a_waiver() {
    let measured = host();
    let status = status(Some(incomplete(measured.clone())));
    let waiver = Waiver {
        session_id: SESSION.into(),
        condition: Condition::Stale,
    };

    assert_eq!(
        evaluate(&status, &measured, &waived(Attendance::Unattended, &waiver)),
        Admission::Refused(Condition::Stale),
        "unattended Runs never waive, even holding an operator's waiver"
    );
}

#[test]
fn no_waiver_reaches_an_observed_containment_failure() {
    let measured = host();
    let mut status = status(Some(certified(measured.clone())));
    status.history.failures.push(Failure {
        check: REQUIRED_CHECKS[0].into(),
        report_digest: Digest::of(b"failing").to_string(),
        scope: Scope::InstalledHost,
    });
    let waiver = Waiver {
        session_id: SESSION.into(),
        condition: Condition::ContainmentFailure,
    };

    assert_eq!(
        evaluate(
            &status,
            &measured,
            &waived(Attendance::Interactive, &waiver)
        ),
        Admission::Refused(Condition::ContainmentFailure),
        "a waiver naming containment failure is still refused"
    );
}
