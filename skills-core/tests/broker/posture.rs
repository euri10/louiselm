//! Current posture is derived from retained trusted launch facts.

use super::*;
use louiselm_skills::broker::lifecycle::LifecycleCaller;

#[path = "posture_lifetime.rs"]
mod lifetime;

#[test]
fn launch_posture_is_not_limited_by_the_operator_audit_view() {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("control.sock");
    let request = request("large-audit-posture");
    let audit = AuditLog::open(&root.path().join("audit")).unwrap();
    for at_ms in 0..4096 {
        audit
            .record(&louiselm_skills::broker::AuditEntry {
                at_ms,
                session_id: "other-session".into(),
                run_id: "other-run".into(),
                authorization_id: "other-authorization".into(),
                identity_slot: 1,
                decision: AuditDecision::AuthorizationConsumed,
            })
            .unwrap();
    }
    let service = lifecycle::bound_service(root.path(), &socket, &request);
    let peer = thread::spawn(move || {
        let (authorization, channel) = fake_supervisor(&socket, &request, 2000);
        lifecycle::answer_one_status_query(&channel, &lifecycle::status(&authorization));
    });
    let mut session = service
        .serve_launch(2000, verify_fixture_signature)
        .unwrap();
    let status = service
        .session_status(
            &mut session,
            &LifecycleCaller::Operator {
                uid: CONTROLLER_UID,
            },
            3000,
            verify_fixture_signature,
        )
        .unwrap();
    peer.join().unwrap();
    let runtime = status
        .posture
        .dimensions
        .iter()
        .find(|dimension| dimension.dimension == louiselm_skills::posture::DimensionName::Runtime)
        .unwrap();
    assert_eq!(
        runtime.state,
        louiselm_skills::posture::DimensionState::Verified
    );
    assert!(
        runtime
            .freshness
            .last_verified_at_ms
            .is_some_and(|at| at >= 2000)
    );
}

#[test]
fn session_status_uses_launch_evidence_and_reports_missing_dimensions() {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("control.sock");
    let request = request("posture-session");
    let service = lifecycle::bound_service(root.path(), &socket, &request);
    let peer = thread::spawn(move || {
        let (authorization, channel) = fake_supervisor(&socket, &request, 2000);
        lifecycle::answer_one_status_query(&channel, &lifecycle::status(&authorization));
        lifecycle::answer_one_status_query(&channel, &lifecycle::status(&authorization));
    });
    let mut session = service
        .serve_launch(2000, verify_fixture_signature)
        .unwrap();
    let admission = service.inspect("posture-session").unwrap().unwrap();
    let audit_before = service.audit().unwrap();
    let status = service
        .session_status(
            &mut session,
            &LifecycleCaller::Operator {
                uid: CONTROLLER_UID,
            },
            3000,
            verify_fixture_signature,
        )
        .unwrap();
    let later = service
        .session_status(
            &mut session,
            &LifecycleCaller::Operator {
                uid: CONTROLLER_UID,
            },
            9000,
            verify_fixture_signature,
        )
        .unwrap();
    peer.join().unwrap();
    assert_eq!(
        status.posture, later.posture,
        "a read cannot renew proof freshness"
    );
    let mut forged = status.clone();
    forged.posture.state = louiselm_skills::launch_protocol::PostureSummary::FullyVerified;
    assert!(forged.validate().is_err());

    let json = serde_json::to_value(&status).unwrap();
    assert_eq!(json["posture"]["state"], "unverified");
    let dimensions = json["posture"]["dimensions"].as_array().unwrap();
    assert_eq!(dimensions.len(), 6);
    for dimension in dimensions {
        if dimension["dimension"] == "runtime" {
            assert_eq!(dimension["state"], "verified");
            assert_eq!(dimension["failure_code"], serde_json::Value::Null);
            assert_eq!(dimension["freshness"]["basis"], "launch");
            assert!(
                dimension["freshness"]["last_verified_at_ms"]
                    .as_u64()
                    .is_some()
            );
        } else {
            assert_eq!(dimension["state"], "failed");
            assert_eq!(dimension["failure_code"], "evidence_missing");
            assert_eq!(dimension["next_action"]["id"], "collect_trusted_evidence");
            assert_eq!(
                dimension["freshness"]["last_verified_at_ms"],
                serde_json::Value::Null
            );
        }
    }
    assert_eq!(service.audit().unwrap(), audit_before);
    assert_eq!(
        service.inspect("posture-session").unwrap().unwrap(),
        admission
    );
    assert_eq!(
        louiselm_skills::launch_protocol::SessionStatus::parse_canonical(&status.canonical_bytes())
            .unwrap(),
        status,
    );
}

#[test]
fn quarantine_invalidates_current_runtime_without_rewriting_admission() {
    use louiselm_skills::{
        launch_protocol::{ChannelState, FreshnessBasis},
        posture::{DimensionName, DimensionState},
    };
    let root = TempDir::new().unwrap();
    let socket = root.path().join("control.sock");
    let request = request("quarantined-posture");
    let service = lifecycle::bound_service(root.path(), &socket, &request);
    let peer = thread::spawn(move || {
        let (authorization, channel) = fake_supervisor(&socket, &request, 2000);
        lifecycle::answer_one_status_query(&channel, &lifecycle::status(&authorization));
        let receipt = lifecycle::drive_park_peer(&authorization, &channel, true);
        let mut parked = lifecycle::status(&authorization);
        parked.state = SessionState::Parked;
        parked.channel_state = ChannelState::Revoked;
        parked.broker_head = Some(louiselm_skills::launch_receipt::ReceiptHead {
            sequence: receipt.payload.sequence,
            digest: receipt.digest().to_string(),
        });
        parked.launcher_head = parked.broker_head.clone();
        lifecycle::answer_one_status_query(&channel, &parked);
    });
    let mut session = service
        .serve_launch(2000, verify_fixture_signature)
        .unwrap();
    let original = service
        .receipts()
        .stored_bytes("quarantined-posture")
        .unwrap();
    let operator = LifecycleCaller::Operator {
        uid: CONTROLLER_UID,
    };
    let before = service
        .session_status(&mut session, &operator, 3000, verify_fixture_signature)
        .unwrap();
    let mutation = lifecycle::park(session.authorization());
    service
        .quarantine(
            &mut session,
            &operator,
            &mutation,
            3000,
            verify_fixture_signature,
        )
        .unwrap();
    let after = service
        .session_status(&mut session, &operator, 4000, verify_fixture_signature)
        .unwrap();
    peer.join().unwrap();
    for (old, current) in before
        .posture
        .dimensions
        .iter()
        .zip(&after.posture.dimensions)
    {
        if current.dimension == DimensionName::Runtime {
            assert_eq!(old.state, DimensionState::Verified);
            assert_eq!(current.state, DimensionState::Failed);
            assert_eq!(current.freshness.basis, FreshnessBasis::Invalidated);
            assert_eq!(
                serde_json::to_value(current.failure_code).unwrap(),
                "evidence_invalidated"
            );
            assert_eq!(
                old.freshness.last_verified_at_ms,
                current.freshness.last_verified_at_ms
            );
            assert_eq!(old.evidence, current.evidence);
        } else {
            assert_eq!(
                old, current,
                "unrelated dimensions retain their own evidence state"
            );
        }
    }
    assert_eq!(
        service
            .receipts()
            .stored_bytes("quarantined-posture")
            .unwrap()[..2],
        original
    );
}

#[test]
fn restart_restores_original_proof_time_without_fabricating_a_new_check() {
    use louiselm_skills::{
        launch_protocol::FreshnessBasis,
        posture::{DimensionName, DimensionState},
    };
    let root = TempDir::new().unwrap();
    let socket = root.path().join("broker.sock");
    let request = request("restored-posture");
    let service = lifecycle::bound_service(root.path(), &socket, &request);
    let peer_socket = socket.clone();
    let peer = thread::spawn(move || fake_supervisor(&peer_socket, &request, 2000));
    let session = service
        .serve_launch(2000, verify_fixture_signature)
        .unwrap();
    let (authorization, old_channel) = peer.join().unwrap();
    let checked = service
        .audit()
        .unwrap()
        .into_iter()
        .find(|entry| entry.decision == (AuditDecision::ReceiptStored { sequence: 1 }))
        .unwrap()
        .at_ms;
    let initial_bytes = service.receipts().stored_bytes("restored-posture").unwrap();
    let start = SignedReceipt::parse_canonical(&initial_bytes[1]).unwrap();
    service.close();
    drop((session, old_channel, service));
    fs::remove_file(&socket).unwrap(); // This test owns the temporary rendezvous.
    let service = BrokerService::bind(
        &socket,
        AuthorizationStore::open(&root.path().join("authorizations"), pool(4)).unwrap(),
        ReceiptStore::open(&root.path().join("receipts"), trusted_release()).unwrap(),
        AuditLog::open(&root.path().join("audit")).unwrap(),
        local_pin(),
    )
    .unwrap();
    let peer_root = root.path().to_owned();
    let peer = thread::spawn(move || {
        let channel = reconnect::peer(&peer_root, &reconnect::checkpoint(&start));
        let _ = settle(|complete| channel.receive(complete));
        lifecycle::answer_one_status_query(&channel, &lifecycle::status(&authorization));
    });
    let mut session = service
        .serve_reconnect(90_000, verify_fixture_signature)
        .unwrap();
    let status = service
        .session_status(
            &mut session,
            &LifecycleCaller::Operator {
                uid: CONTROLLER_UID,
            },
            90_000,
            verify_fixture_signature,
        )
        .unwrap();
    peer.join().unwrap();
    let runtime = status
        .posture
        .dimensions
        .iter()
        .find(|dimension| dimension.dimension == DimensionName::Runtime)
        .unwrap();
    assert_eq!(runtime.state, DimensionState::Verified);
    assert_eq!(runtime.freshness.basis, FreshnessBasis::Launch);
    assert_eq!(runtime.freshness.last_verified_at_ms, Some(checked));
    assert_eq!(
        service.receipts().stored_bytes("restored-posture").unwrap(),
        initial_bytes
    );
}
