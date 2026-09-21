//! Shared hostile-observation oracle; fixture reports confer no host authority.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Tests assert explicit conformance outcomes."
)]

use louiselm_skills::conformance::{
    Check, Cleanup, FailureHistory, MAX_REPORT_BYTES, Observation, Outcome, REPORT_SCHEMA,
    REQUIRED_CHECKS, Report, ReportResult, Scope, pair,
};

#[test]
fn installed_certificate_rejects_guest_scope_and_changed_host_inputs() {
    use louiselm_skills::conformance::installed::{Certificate, HostSnapshot};
    let digest = louiselm_skills::Digest::of(b"fixture").to_string();
    let host = HostSnapshot {
        profile: "debian13-x86_64-glibc/1".into(),
        machine_digest: digest.clone(),
        boot_id: "00000000-0000-0000-0000-000000000001".into(),
        release_digest: digest.clone(),
        inputs: ["launcher", "backend", "policy", "kernel", "loader"]
            .into_iter()
            .map(|name| (name.into(), digest.clone()))
            .collect(),
    };
    let mut observations = passing();
    assert!(Certificate::new(host.clone(), observations.clone()).is_err());
    observations.scope = Scope::InstalledHost;
    let certificate = Certificate::new(host.clone(), observations).unwrap();
    let bytes = certificate.canonical_bytes().unwrap();
    assert_eq!(Certificate::parse_canonical(&bytes).unwrap(), certificate);
    assert!(certificate.is_current(&host));
    let mut rebooted = host;
    rebooted.boot_id = "00000000-0000-0000-0000-000000000002".into();
    assert!(!certificate.is_current(&rebooted));
}

fn passing() -> Report {
    Report {
        schema: REPORT_SCHEMA.into(),
        scope: Scope::DisposableGuest,
        checks: REQUIRED_CHECKS
            .iter()
            .map(|name| Check {
                name: (*name).into(),
                control: Outcome::Allowed,
                confined: Outcome::Denied("observed denial".into()),
            })
            .collect(),
        completed: true,
        cleanup: Cleanup::Confirmed,
    }
}

/// Consumes the actual opt-in VM artifact through the existing report oracle.
#[test]
#[ignore = "requires ownership_probe.py output in LOUISELM_TEST_KERNEL_GUARD_REPORT"]
fn kernel_guard_component_report_cannot_certify_a_host() {
    use louiselm_skills::conformance::installed::{Certificate, HostSnapshot};
    use std::io::Read;

    let path = std::env::var_os("LOUISELM_TEST_KERNEL_GUARD_REPORT").unwrap();
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .unwrap()
        .take(u64::try_from(MAX_REPORT_BYTES).unwrap() + 1)
        .read_to_end(&mut bytes)
        .unwrap();
    assert!(bytes.len() <= MAX_REPORT_BYTES);
    let artifact: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(artifact["verdict"], "OWNERSHIP_COMPONENT_PASS_NOT_VERIFIED");
    let report: Report = serde_json::from_value(artifact["observations"].clone()).unwrap();
    assert_eq!(report.scope, Scope::DisposableGuest);
    assert_eq!(report.result().unwrap(), ReportResult::Incomplete);
    assert_eq!(report.cleanup, Cleanup::Confirmed);
    assert_eq!(
        Report::parse_canonical(&report.canonical_bytes().unwrap()).unwrap(),
        report
    );
    let digest = louiselm_skills::Digest::of(b"fixture").to_string();
    let host = HostSnapshot {
        profile: "debian13-x86_64-glibc/1".into(),
        machine_digest: digest.clone(),
        boot_id: "00000000-0000-0000-0000-000000000001".into(),
        release_digest: digest.clone(),
        inputs: ["launcher", "backend", "policy", "kernel", "loader"]
            .into_iter()
            .map(|name| (name.into(), digest.clone()))
            .collect(),
    };
    assert!(Certificate::new(host, report).is_err());
}

#[test]
fn complete_exact_observations_pass_and_round_trip_without_granting_host_authority() {
    let report = passing();
    assert_eq!(report.result().unwrap(), ReportResult::Passed);
    let bytes = report.canonical_bytes().unwrap();
    assert_eq!(Report::parse_canonical(&bytes).unwrap(), report);
    assert_eq!(report.scope, Scope::DisposableGuest);
    assert_ne!(report.scope, Scope::InstalledHost);
}

#[test]
fn absence_failed_controls_cancellation_and_unexpected_errors_are_incomplete() {
    for variant in 0..6 {
        let mut report = passing();
        match variant {
            0 => report.checks.clear(),
            1 => {
                report.checks.pop();
            }
            2 => report.checks[0].control = Outcome::Denied("EPERM".into()),
            3 => report.checks[0].confined = Outcome::Error("timeout".into()),
            4 => report.completed = false,
            5 => report.cleanup = Cleanup::Pending,
            _ => unreachable!(),
        }
        assert_eq!(
            report.result().unwrap(),
            ReportResult::Incomplete,
            "{variant}"
        );
    }
}

#[test]
fn duplicate_contradictory_unknown_and_unbounded_observations_are_rejected() {
    for variant in 0..5 {
        let mut report = passing();
        match variant {
            0 => report.checks.push(report.checks[0].clone()),
            1 => {
                let mut duplicate = report.checks[0].clone();
                duplicate.confined = Outcome::Allowed;
                report.checks.push(duplicate);
            }
            2 => report.checks[0].name = "caller-selected-check".into(),
            3 => report.checks[0].confined = Outcome::Denied(String::new()),
            4 => report.checks[0].confined = Outcome::Error("x".repeat(1025)),
            _ => unreachable!(),
        }
        assert!(report.result().is_err(), "{variant}");
    }
    let report = passing();
    let mut value = serde_json::to_value(&report).unwrap();
    value["pass"] = true.into();
    assert!(Report::parse_canonical(&serde_json::to_vec(&value).unwrap()).is_err());
    let mut bytes = report.canonical_bytes().unwrap();
    bytes.push(b'\n');
    assert!(Report::parse_canonical(&bytes).is_err());
    assert!(Report::parse_canonical(&vec![b' '; MAX_REPORT_BYTES + 1]).is_err());

    let mut escaped = passing();
    for check in &mut escaped.checks {
        check.control = Outcome::Error("\0".repeat(1024));
        check.confined = Outcome::Error("\0".repeat(1024));
    }
    assert_eq!(escaped.result().unwrap(), ReportResult::Incomplete);
    assert!(escaped.canonical_bytes().is_err());
}

#[test]
fn observed_failure_survives_incomplete_and_cancelled_recertification() {
    let mut report = passing();
    report.checks[0].confined = Outcome::Allowed;
    report.completed = false;
    let failed_name = report.checks[0].name.clone();
    assert_eq!(
        report.result().unwrap(),
        ReportResult::Failed(vec![failed_name.clone()])
    );
    let history = FailureHistory::default().record(&report).unwrap();
    assert_eq!(history.failures[0].check, failed_name);
    assert_eq!(
        history.failures[0].report_digest,
        report.digest().unwrap().to_string()
    );

    let mut incomplete = passing();
    incomplete.completed = false;
    assert_eq!(history.record(&incomplete).unwrap(), history);
    incomplete.completed = true;
    incomplete.checks.pop();
    assert_eq!(history.record(&incomplete).unwrap(), history);
    let restored: FailureHistory =
        serde_json::from_slice(&serde_json::to_vec(&history).unwrap()).unwrap();
    assert_eq!(restored, history);
    assert!(history.record(&passing()).unwrap().failures.is_empty());

    let mut oversized = passing();
    for check in &mut oversized.checks {
        check.confined = Outcome::Denied("\0".repeat(1024));
    }
    assert!(oversized.canonical_bytes().is_err());
    assert!(history.record(&oversized).is_err());
}

#[test]
fn cleanup_failure_is_durable_even_without_completed_probes() {
    let mut report = passing();
    report.checks.clear();
    report.completed = false;
    report.cleanup = Cleanup::Unconfirmed;
    assert_eq!(
        report.result().unwrap(),
        ReportResult::Failed(vec!["cleanup".into()])
    );
    let history = FailureHistory::default().record(&report).unwrap();
    let mut later = passing();
    later.cleanup = Cleanup::Pending;
    assert_eq!(history.record(&later).unwrap(), history);
    // A fresh fixture's cleanup says nothing about the earlier unresolved tree.
    assert_eq!(history.record(&passing()).unwrap(), history);
}

#[test]
fn a_guest_pass_cannot_clear_an_installed_failure_and_invalid_history_is_not_empty() {
    let mut failed = passing();
    failed.scope = Scope::InstalledHost;
    failed.checks[0].confined = Outcome::Allowed;
    let history = FailureHistory::default().record(&failed).unwrap();
    assert_eq!(history.record(&passing()).unwrap(), history);
    let mut recertified = passing();
    recertified.scope = Scope::InstalledHost;
    assert!(history.record(&recertified).unwrap().failures.is_empty());

    let mut malformed = history.clone();
    malformed.failures[0].report_digest = "unreadable".into();
    assert!(malformed.record(&recertified).is_err());
    malformed = history.clone();
    malformed.failures.push(history.failures[0].clone());
    assert!(malformed.record(&recertified).is_err());
    malformed = history;
    malformed.failures[0].check = "unknown-boundary".into();
    assert!(malformed.record(&recertified).is_err());
}

#[test]
fn paired_observations_reject_extra_rows_duplicate_names_and_unknown_inventory() {
    let names = ["operator-home", "operator-checkout"];
    let outside: Vec<_> = names
        .iter()
        .map(|name| Observation {
            name: (*name).into(),
            outcome: Outcome::Allowed,
        })
        .collect();
    let inside: Vec<_> = names
        .iter()
        .rev()
        .map(|name| Observation {
            name: (*name).into(),
            outcome: Outcome::Denied("ENOENT".into()),
        })
        .collect();
    let paired = pair(&names, &outside, &inside).unwrap();
    assert_eq!(paired[0].name, names[0]);
    assert_eq!(paired[1].name, names[1]);
    assert!(pair(&[names[0], names[0]], &outside, &inside).is_err());
    assert!(pair(&names, &outside[..1], &inside).is_err());
    let mut extra = inside.clone();
    extra.push(inside[0].clone());
    assert!(pair(&names, &outside, &extra).is_err());
    let mut unknown = inside.clone();
    unknown[0].name = "unrequested".into();
    assert!(pair(&names, &outside, &unknown).is_err());
    assert!(pair(&["unrequested"], &unknown[..1], &unknown[..1]).is_err());
    let duplicate = vec![inside[0].clone(), inside[0].clone()];
    assert!(pair(&names, &outside, &duplicate).is_err());
}
