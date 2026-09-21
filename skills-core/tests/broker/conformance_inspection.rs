//! Real authenticated operator reads consume retained receipt-bound evidence.

use super::*;
use louiselm_skills::broker::{
    conformance_inspection::ConformanceInspection,
    operator::{self, InspectError, OperatorServer},
};

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "One operator transaction checks exact evidence, unchanged authority and restart across each admission outcome."
)]
fn operator_reads_exact_large_report_or_absence_without_a_live_supervisor() {
    let mut report = observations();
    for check in &mut report.checks {
        check.confined = louiselm_skills::conformance::Outcome::Denied("\u{0001}".repeat(384));
    }
    let bytes = report.canonical_bytes().unwrap();
    assert!(bytes.len() > louiselm_skills::launch_protocol::MAX_PROTOCOL_MESSAGE_BYTES);
    for admission in [
        ConformanceEvidence::Unevaluated,
        certified(&bytes),
        ConformanceEvidence::Waived {
            condition: Condition::Missing,
            report_digest: None,
        },
        ConformanceEvidence::Waived {
            condition: Condition::Incomplete,
            report_digest: None,
        },
        ConformanceEvidence::Waived {
            condition: Condition::Stale,
            report_digest: Some(Digest::of(&bytes).to_string()),
        },
    ] {
        let root = TempDir::new().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let (service, authorization, launch, start) =
            retained_service(root.path(), &admission, &bytes);
        let audit = service.audit().unwrap();
        let pending = serde_json::to_vec(
            &service
                .authorizations()
                .consumed_for_session(&authorization.session_id)
                .unwrap(),
        )
        .unwrap();
        let expected = service
            .inspect_conformance(&authorization.session_id)
            .unwrap()
            .unwrap();
        assert_eq!(expected.admission, admission);
        assert_eq!(expected.run_id, authorization.run_id);
        assert_eq!(
            expected.waiver.as_ref().map(|waiver| (
                waiver.condition,
                waiver.expires_at_ms,
                &waiver.receipt_digest
            )),
            authorization.conformance.waiver.as_ref().map(|waiver| (
                waiver.condition,
                waiver.expires_at_ms,
                &waiver.receipt_digest
            ))
        );
        assert!(expected.last_check.is_none());
        let has_report = matches!(
            admission,
            ConformanceEvidence::Certified { .. }
                | ConformanceEvidence::Waived {
                    report_digest: Some(_),
                    ..
                }
        );
        assert_eq!(
            expected.report.as_deref().map(str::as_bytes),
            has_report.then_some(bytes.as_slice())
        );
        let socket = root.path().join("operator.sock");
        let uid = getuid().as_raw();
        let server = OperatorServer::bind(&socket, uid).unwrap();
        let worker = thread::spawn(move || {
            for _ in 0..2 {
                server
                    .serve_once(
                        |_, _| Err(louiselm_skills::broker::operator::InspectError::UnknownSession),
                        |_, _| panic!("historical inspection requires no supervisor"),
                        |id| {
                            service
                                .inspect_conformance(id)
                                .map_err(|_| InspectError::StatusUnavailable)?
                                .ok_or(InspectError::UnknownSession)
                        },
                        |_, _| panic!("inspection changes no authority"),
                        |_, _| panic!("inspection changes no Beads state"),
                        |_, _| panic!("inspection changes no retention state"),
                        |_, _, _| panic!("inspection changes no waiver"),
                    )
                    .unwrap();
            }
            (service, authorization)
        });
        let actual =
            operator::inspect_conformance(&socket, uid, "admission-status", Duration::from_secs(2))
                .unwrap();
        assert_eq!(actual, expected);
        assert_eq!(
            operator::inspect_conformance(&socket, uid, "foreign", Duration::from_secs(2)),
            Err(InspectError::UnknownSession)
        );
        let (service, authorization) = worker.join().unwrap();
        assert_eq!(service.audit().unwrap(), audit);
        assert_eq!(
            serde_json::to_vec(
                &service
                    .authorizations()
                    .consumed_for_session(&authorization.session_id)
                    .unwrap()
            )
            .unwrap(),
            pending
        );
        assert_eq!(
            service
                .receipts()
                .stored_bytes(&authorization.session_id)
                .unwrap(),
            vec![launch.canonical_bytes(), start.canonical_bytes()]
        );
        for forbidden in [
            "operator_uid",
            "authorization_id",
            "assigned_uid",
            "request_digest",
            "identity_slot",
            "\"pid\"",
            "\"path\"",
        ] {
            assert!(
                !String::from_utf8(actual.canonical_bytes().unwrap())
                    .unwrap()
                    .contains(forbidden)
            );
        }
        drop(service);
        let restarted = BrokerService::bind(
            &root.path().join("restarted.sock"),
            AuthorizationStore::open(&root.path().join("authorizations"), pool(4)).unwrap(),
            ReceiptStore::open(&root.path().join("receipts"), trusted_release()).unwrap(),
            AuditLog::open(&root.path().join("audit")).unwrap(),
            local_pin(),
        )
        .unwrap();
        assert_eq!(
            restarted.inspect_conformance("admission-status").unwrap(),
            Some(expected)
        );
    }
}

#[test]
fn operator_inspection_refuses_damaged_bound_evidence_instead_of_reporting_absence() {
    for damage in [
        "missing_report",
        "changed_report",
        "corrupt_check",
        "foreign_check",
    ] {
        let root = TempDir::new().unwrap();
        let bytes = observations().canonical_bytes().unwrap();
        let (service, authorization, _, _) =
            retained_service(root.path(), &certified(&bytes), &bytes);
        let report = root
            .path()
            .join("receipts/conformance/admission-status.json");
        match damage {
            "missing_report" => fs::remove_file(report).unwrap(),
            "changed_report" => fs::write(report, b"{}").unwrap(),
            _ => {
                let parent = root.path().join("receipts/current-conformance");
                fs::create_dir_all(&parent).unwrap();
                let mut update = current::update(&authorization, certified(&bytes));
                update.authorization_id = "foreign".into();
                let payload = if damage == "corrupt_check" {
                    b"{}".to_vec()
                } else {
                    serde_json::to_vec(&serde_json::json!({"update":update,"last_verified":[Digest::of(&bytes).to_string(),90_000]})).unwrap()
                };
                fs::write(parent.join("admission-status.json"), payload).unwrap();
            }
        }
        assert!(
            service.inspect_conformance("admission-status").is_err(),
            "{damage}"
        );
        assert!(!root.path().join("authorizations/history-failures").exists());
        assert!(service.audit().unwrap().is_empty());
    }
}

#[test]
fn inspection_parser_refuses_changed_reports_and_forged_success() {
    let root = TempDir::new().unwrap();
    let bytes = observations().canonical_bytes().unwrap();
    let (service, _, _, _) = retained_service(root.path(), &certified(&bytes), &bytes);
    let valid = service
        .inspect_conformance("admission-status")
        .unwrap()
        .unwrap();
    let canonical = valid.canonical_bytes().unwrap();
    assert_eq!(
        ConformanceInspection::parse_canonical(&canonical).unwrap(),
        valid
    );
    for damage in [
        "schema",
        "report",
        "unknown",
        "last_check",
        "oversized",
        "waiver",
    ] {
        let mut wire = serde_json::to_value(&valid).unwrap();
        match damage {
            "schema" => wire["schema"] = "foreign/1".into(),
            "report" => wire["report"] = "{}".into(),
            "unknown" => wire["operator_uid"] = 0.into(),
            "waiver" => {
                wire["waiver"] = serde_json::json!({"condition":"missing", "expires_at_ms":10, "receipt_digest":Digest::of(b"unapproved").to_string()});
            }
            "oversized" => {
                wire["report"] = "x"
                    .repeat(2 * louiselm_skills::conformance::MAX_REPORT_BYTES)
                    .into();
            }
            _ => {
                wire["last_check"] = serde_json::json!({
                    "sequence": 1, "observed_at_ms": 10, "last_success_at_ms": 11,
                    "suspended": false, "check": {"kind":"current", "evidence":{"status":"unevaluated"}}
                });
            }
        }
        assert!(
            ConformanceInspection::parse_canonical(&serde_json::to_vec(&wire).unwrap()).is_err(),
            "{damage}"
        );
        // Preserve the struct's canonical field order so these refusals prove
        // semantic validation, independently of the exact-encoding check.
        if let Ok(record) = serde_json::from_value::<ConformanceInspection>(wire) {
            assert!(record.canonical_bytes().is_err(), "{damage}");
            assert!(
                ConformanceInspection::parse_canonical(&serde_json::to_vec(&record).unwrap())
                    .is_err(),
                "{damage}"
            );
        }
    }
}
