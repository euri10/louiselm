//! Report-bearing admission receipts require the exact retained observations.

use super::*;
use louiselm_skills::conformance::{
    Check, Cleanup, MAX_REPORT_BYTES, Outcome, REPORT_SCHEMA, REQUIRED_CHECKS, Report, Scope,
    admission::Condition,
};

#[path = "conformance_status.rs"]
mod status;

fn observations() -> Report {
    Report {
        schema: REPORT_SCHEMA.into(),
        scope: Scope::InstalledHost,
        checks: REQUIRED_CHECKS
            .iter()
            .map(|name| Check {
                name: (*name).into(),
                control: Outcome::Allowed,
                confined: Outcome::Denied("private probe observation".into()),
            })
            .collect(),
        completed: true,
        cleanup: Cleanup::Confirmed,
    }
}

fn certified(bytes: &[u8]) -> ConformanceEvidence {
    ConformanceEvidence::Certified {
        report_digest: Digest::of(bytes).to_string(),
    }
}

fn authorized_admission(
    root: &Path,
    request: &LaunchRequest,
    decision: &ConformanceEvidence,
) -> LaunchAuthorization {
    let mut grant = grant(request);
    if let ConformanceEvidence::Waived { condition, .. } = decision {
        grant.conformance.attendance =
            louiselm_skills::conformance::admission::Attendance::Interactive;
        grant.conformance.waiver = Some(louiselm_skills::launch_protocol::ConformanceWaiver {
            preparation: None,
            session_id: request.session_id.clone(),
            request_digest: request.digest().to_string(),
            operator_uid: CONTROLLER_UID,
            condition: *condition,
            expires_at_ms: 30_000,
            receipt_digest: Digest::of(b"approved waiver").to_string(),
        });
    }
    let store = AuthorizationStore::open(&root.join("authorizations"), pool(4)).unwrap();
    store.authorize(&grant, 1000).unwrap();
    store.consume(request, CONTROLLER_UID, 2000).unwrap()
}

fn admission_receipt(
    authorization: &LaunchAuthorization,
    conformance: ConformanceEvidence,
) -> SignedReceipt {
    let mut receipt = launch_receipt(authorization);
    let ReceiptOutcome::Launch { evidence, .. } = &mut receipt.payload.outcome else {
        panic!("fixture is a launch receipt");
    };
    evidence.conformance = conformance;
    signed(receipt.payload)
}

#[test]
fn report_digest_without_report_cannot_receive_a_durable_acknowledgement() {
    for conformance in [
        ConformanceEvidence::Certified {
            report_digest: Digest::of(b"missing report").to_string(),
        },
        ConformanceEvidence::Waived {
            condition: Condition::Stale,
            report_digest: Some(Digest::of(b"missing report").to_string()),
        },
    ] {
        let root = TempDir::new().unwrap();
        let authorization =
            authorized_admission(root.path(), &request("missing-report"), &conformance);
        let receipts =
            ReceiptStore::open(&root.path().join("receipts"), trusted_release()).unwrap();
        let receipt = admission_receipt(&authorization, conformance);
        assert!(
            receipts
                .append(
                    &authorization,
                    &receipt.canonical_bytes(),
                    None,
                    verify_fixture_signature
                )
                .is_err(),
            "a signed digest does not prove that the broker retained the report"
        );
        assert!(receipts.head(&authorization.session_id).unwrap().is_none());
    }
}

#[test]
fn exact_admission_reports_survive_start_and_restart() {
    let bytes = observations().canonical_bytes().unwrap();
    for decision in [
        certified(&bytes),
        ConformanceEvidence::Waived {
            condition: Condition::Stale,
            report_digest: Some(Digest::of(&bytes).to_string()),
        },
        ConformanceEvidence::Waived {
            condition: Condition::Missing,
            report_digest: None,
        },
        ConformanceEvidence::Unevaluated,
    ] {
        let root = TempDir::new().unwrap();
        let authorization =
            authorized_admission(root.path(), &request("retained-report"), &decision);
        let path = root.path().join("receipts");
        let receipts = ReceiptStore::open(&path, trusted_release()).unwrap();
        let supplied = match &decision {
            ConformanceEvidence::Certified { .. }
            | ConformanceEvidence::Waived {
                report_digest: Some(_),
                ..
            } => Some(bytes.as_slice()),
            _ => None,
        };
        let launch = admission_receipt(&authorization, decision);
        receipts
            .append(
                &authorization,
                &launch.canonical_bytes(),
                supplied,
                verify_fixture_signature,
            )
            .unwrap();
        if supplied.is_some() {
            let report_path = path.join("conformance/retained-report.json");
            assert_eq!(fs::read(&report_path).unwrap(), bytes);
            assert_eq!(
                fs::metadata(report_path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let start = start_receipt(&authorization, &launch);
        receipts
            .append(
                &authorization,
                &start.canonical_bytes(),
                None,
                verify_fixture_signature,
            )
            .unwrap();
        drop(receipts);
        let reopened = ReceiptStore::open(&path, trusted_release()).unwrap();
        assert_eq!(
            reopened
                .conformance_report(&authorization, verify_fixture_signature)
                .unwrap()
                .as_deref(),
            supplied
        );
        assert_eq!(
            reopened.stored_bytes(&authorization.session_id).unwrap(),
            vec![launch.canonical_bytes(), start.canonical_bytes()]
        );
        let mut foreign = authorization.clone();
        foreign.run_id = "different-run".into();
        assert!(
            reopened
                .conformance_report(&foreign, verify_fixture_signature)
                .is_err()
        );
    }
}

#[test]
fn contradictory_or_malformed_reports_never_reach_receipt_storage() {
    let passing = observations().canonical_bytes().unwrap();
    let mut guest = observations();
    guest.scope = Scope::DisposableGuest;
    let guest = guest.canonical_bytes().unwrap();
    let mut incomplete = observations();
    incomplete.completed = false;
    let incomplete = incomplete.canonical_bytes().unwrap();
    let mut failed = observations();
    failed.checks[0].confined = Outcome::Allowed;
    let failed = failed.canonical_bytes().unwrap();
    let mut noncanonical = passing.clone();
    noncanonical.push(b'\n');
    let oversized = vec![b'x'; MAX_REPORT_BYTES + 1];
    for (decision, bytes) in [
        (certified(b"other bytes"), passing.clone()),
        (certified(&guest), guest),
        (certified(&incomplete), incomplete),
        (certified(&failed), failed.clone()),
        (certified(&noncanonical), noncanonical),
        (certified(b"{}"), b"{}".to_vec()),
        (certified(&oversized), oversized),
        (ConformanceEvidence::Unevaluated, passing.clone()),
        (
            ConformanceEvidence::Waived {
                condition: Condition::Missing,
                report_digest: Some(Digest::of(&passing).to_string()),
            },
            passing,
        ),
        (
            ConformanceEvidence::Waived {
                condition: Condition::Stale,
                report_digest: Some(Digest::of(&failed).to_string()),
            },
            failed,
        ),
    ] {
        let root = TempDir::new().unwrap();
        let authorization =
            authorized_admission(root.path(), &request("refused-report"), &decision);
        let path = root.path().join("receipts");
        let receipts = ReceiptStore::open(&path, trusted_release()).unwrap();
        let launch = admission_receipt(&authorization, decision);
        assert!(matches!(
            receipts.append(
                &authorization,
                &launch.canonical_bytes(),
                Some(&bytes),
                verify_fixture_signature
            ),
            Err(BrokerError::ConformanceReport(_))
        ));
        assert!(receipts.head(&authorization.session_id).unwrap().is_none());
        assert!(!path.join("conformance").exists());
    }
}

#[test]
fn nonwaivable_failure_and_unverified_signatures_cannot_publish_reports() {
    let root = TempDir::new().unwrap();
    let authorization = consumed_authorization(root.path(), &request("untrusted-report"));
    let path = root.path().join("receipts");
    let receipts = ReceiptStore::open(&path, trusted_release()).unwrap();
    let bytes = observations().canonical_bytes().unwrap();
    let launch = admission_receipt(&authorization, certified(&bytes));
    assert!(
        receipts
            .append(
                &authorization,
                &launch.canonical_bytes(),
                Some(&bytes),
                |_, _, _| false
            )
            .is_err()
    );
    let waived = admission_receipt(
        &authorization,
        ConformanceEvidence::Waived {
            condition: Condition::ContainmentFailure,
            report_digest: None,
        },
    );
    // The authenticated authorization now rejects this impossible waiver
    // before report validation; containment failures still cannot publish.
    assert!(matches!(
        receipts.append(
            &authorization,
            &waived.canonical_bytes(),
            None,
            verify_fixture_signature
        ),
        Err(BrokerError::ReceiptUnauthorized)
    ));
    assert!(!path.join("conformance").exists());
    assert!(receipts.head(&authorization.session_id).unwrap().is_none());
}

#[test]
fn report_write_failure_withholds_ack_and_exact_orphan_can_be_reused() {
    let root = TempDir::new().unwrap();
    let authorization = consumed_authorization(root.path(), &request("interrupted-report"));
    let path = root.path().join("receipts");
    let receipts = ReceiptStore::open(&path, trusted_release()).unwrap();
    let bytes = observations().canonical_bytes().unwrap();
    let launch = admission_receipt(&authorization, certified(&bytes));
    let directory = path.join("conformance");
    fs::write(&directory, b"test-owned storage obstruction").unwrap();
    assert!(
        receipts
            .append(
                &authorization,
                &launch.canonical_bytes(),
                Some(&bytes),
                verify_fixture_signature
            )
            .is_err()
    );
    assert!(receipts.head(&authorization.session_id).unwrap().is_none());
    fs::remove_file(&directory).unwrap();

    // Reconstruct the durable state of an interruption between report and
    // receipt publication. Recovery must neither overwrite nor reject it.
    fs::create_dir(&directory).unwrap();
    let report = path.join("conformance/interrupted-report.json");
    fs::write(&report, &bytes).unwrap();
    drop(receipts);
    let receipts = ReceiptStore::open(&path, trusted_release()).unwrap();
    receipts
        .append(
            &authorization,
            &launch.canonical_bytes(),
            Some(&bytes),
            verify_fixture_signature,
        )
        .unwrap();
    assert_eq!(
        receipts
            .conformance_report(&authorization, verify_fixture_signature)
            .unwrap(),
        Some(bytes)
    );
}

#[test]
fn damaged_retained_reports_cannot_be_inspected_or_extended() {
    for damage in ["missing", "changed", "oversized", "symlink", "fifo"] {
        let root = TempDir::new().unwrap();
        let authorization = consumed_authorization(root.path(), &request("damaged-report"));
        let path = root.path().join("receipts");
        let receipts = ReceiptStore::open(&path, trusted_release()).unwrap();
        let bytes = observations().canonical_bytes().unwrap();
        let launch = admission_receipt(&authorization, certified(&bytes));
        receipts
            .append(
                &authorization,
                &launch.canonical_bytes(),
                Some(&bytes),
                verify_fixture_signature,
            )
            .unwrap();
        drop(receipts);
        let report = path.join("conformance/damaged-report.json");
        fs::remove_file(&report).unwrap();
        match damage {
            "missing" => (),
            "changed" => fs::write(&report, b"different observations").unwrap(),
            "oversized" => fs::write(&report, vec![b'x'; MAX_REPORT_BYTES + 1]).unwrap(),
            "symlink" => {
                let target = root.path().join("same-bytes.json");
                fs::write(&target, &bytes).unwrap();
                std::os::unix::fs::symlink(target, &report).unwrap();
            }
            "fifo" => {
                rustix::fs::mkfifoat(rustix::fs::CWD, &report, rustix::fs::Mode::RUSR).unwrap();
            }
            _ => unreachable!(),
        }
        let reopened = ReceiptStore::open(&path, trusted_release()).unwrap();
        assert!(
            reopened
                .conformance_report(&authorization, verify_fixture_signature)
                .is_err(),
            "{damage}"
        );
        let start = start_receipt(&authorization, &launch);
        assert!(
            reopened
                .append(
                    &authorization,
                    &start.canonical_bytes(),
                    None,
                    verify_fixture_signature
                )
                .is_err(),
            "{damage}"
        );
        assert_eq!(
            reopened.stored_bytes(&authorization.session_id).unwrap(),
            vec![launch.canonical_bytes()]
        );
    }
}

#[test]
fn reconnect_retains_report_reference_without_claiming_current_host_proof() {
    use louiselm_skills::{
        broker::lifecycle::LifecycleCaller, launch_receipt::ReceiptHead, posture::DimensionName,
    };
    let root = TempDir::new().unwrap();
    let authorization = consumed_authorization(root.path(), &request("report-posture"));
    let bytes = observations().canonical_bytes().unwrap();
    let launch = admission_receipt(&authorization, certified(&bytes));
    let start = start_receipt(&authorization, &launch);
    let receipts = ReceiptStore::open(&root.path().join("receipts"), trusted_release()).unwrap();
    receipts
        .append(
            &authorization,
            &launch.canonical_bytes(),
            Some(&bytes),
            verify_fixture_signature,
        )
        .unwrap();
    receipts
        .append(
            &authorization,
            &start.canonical_bytes(),
            None,
            verify_fixture_signature,
        )
        .unwrap();
    let service = BrokerService::bind(
        &root.path().join("broker.sock"),
        AuthorizationStore::open(&root.path().join("authorizations"), pool(4)).unwrap(),
        receipts,
        AuditLog::open(&root.path().join("audit")).unwrap(),
        local_pin(),
    )
    .unwrap();
    let offer = reconnect::checkpoint(&start);
    let mut status = lifecycle::status(&authorization);
    let head = ReceiptHead {
        sequence: 1,
        digest: start.digest().to_string(),
    };
    status.broker_head = Some(head.clone());
    status.launcher_head = Some(head);
    let peer_root = root.path().to_owned();
    let peer = thread::spawn(move || {
        let channel = reconnect::peer(&peer_root, &offer);
        settle(|complete| channel.receive(complete));
        lifecycle::answer_one_status_query(&channel, &status);
    });
    let mut session = service
        .serve_reconnect(90_000, verify_fixture_signature)
        .unwrap();
    let operator = LifecycleCaller::Operator {
        uid: CONTROLLER_UID,
    };
    let status = service
        .session_status(&mut session, &operator, 90_000, verify_fixture_signature)
        .unwrap();
    peer.join().unwrap();
    let isolation = status
        .posture
        .dimensions
        .iter()
        .find(|row| row.dimension == DimensionName::Isolation)
        .unwrap();
    assert_eq!(
        serde_json::to_value(isolation).unwrap()["evidence"][0]["id"],
        Digest::of(&bytes).to_string()
    );
    assert_eq!(
        serde_json::to_value(isolation).unwrap()["failure_code"],
        "evidence_missing"
    );
    assert!(
        isolation.freshness.last_verified_at_ms.is_none(),
        "retention and restart are not successful currentness checks"
    );
    assert!(
        !String::from_utf8(status.canonical_bytes())
            .unwrap()
            .contains("private probe observation")
    );
    fs::write(
        root.path().join("receipts/conformance/report-posture.json"),
        b"damaged retained history",
    )
    .unwrap();
    assert!(
        service
            .session_status(&mut session, &operator, 90_001, verify_fixture_signature)
            .is_err(),
        "cached posture cannot bypass corrupted report history"
    );
}

#[test]
fn historical_report_cannot_be_primary_isolation_evidence() {
    use louiselm_skills::{
        launch_protocol::{EvidenceFreshness, FreshnessBasis, PostureStatus},
        posture::{
            DimensionInput, DimensionName, DimensionState, EvidenceKind, EvidenceRef, FailureCode,
            Posture,
        },
    };
    let report = EvidenceRef::new(
        EvidenceKind::ConformanceReport,
        &Digest::of(b"report").to_string(),
    )
    .unwrap();
    let inputs = |claim_verified| {
        DimensionName::ALL
            .into_iter()
            .map(|dimension| {
                let evidence = if dimension == DimensionName::Isolation {
                    vec![report.clone()]
                } else {
                    Vec::new()
                };
                if claim_verified && dimension == DimensionName::Isolation {
                    DimensionInput::verified(dimension, evidence)
                } else {
                    DimensionInput::failed(dimension, FailureCode::EvidenceMissing, evidence)
                }
            })
            .collect()
    };
    assert!(Posture::evaluate("session", "run", inputs(true)).is_err());
    let posture = Posture::evaluate("session", "run", inputs(false)).unwrap();
    let mut status = PostureStatus::from_posture(
        &posture,
        [EvidenceFreshness {
            basis: FreshnessBasis::Missing,
            last_verified_at_ms: None,
        }; 6],
    );
    status.validate().unwrap();
    let row = status
        .dimensions
        .iter_mut()
        .find(|row| row.dimension == DimensionName::Isolation)
        .unwrap();
    row.state = DimensionState::Verified;
    row.failure_code = None;
    row.next_action = louiselm_skills::dossier::NextAction {
        id: "none".into(),
        detail: "No action is required.".into(),
    };
    row.freshness = EvidenceFreshness {
        basis: FreshnessBasis::Launch,
        last_verified_at_ms: Some(90_000),
    };
    assert!(
        status.validate().is_err(),
        "wire posture cannot promote historical evidence either"
    );
}
