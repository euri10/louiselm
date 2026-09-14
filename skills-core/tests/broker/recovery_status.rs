//! Canonical status reports retained broker evidence without admitting work.

use super::*;
use louiselm_skills::{
    broker::{BrokerSession, lifecycle::LifecycleCaller},
    launch_protocol::{ChannelState, RecoveryRequest, SessionStatus},
};

#[test]
fn status_reports_missing_recovery_for_both_callers_without_changing_admission() {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("control.sock");
    let request = request("recovery-status");
    let service = lifecycle::bound_service(root.path(), &socket, &request);
    let peer = thread::spawn(move || {
        let (authorization, channel) = fake_supervisor(&socket, &request, 2000);
        for _ in 0..2 {
            lifecycle::answer_one_status_query(&channel, &lifecycle::status(&authorization));
        }
    });
    let mut session = service
        .serve_launch(2000, verify_fixture_signature)
        .unwrap();
    let audit = service.audit().unwrap();
    let head = service.receipts().head("recovery-status").unwrap();
    let mut results = Vec::new();
    for caller in [
        LifecycleCaller::Operator {
            uid: CONTROLLER_UID,
        },
        LifecycleCaller::Agent,
    ] {
        let status = service
            .session_status(&mut session, &caller, 3000, verify_fixture_signature)
            .unwrap();
        assert_eq!(
            SessionStatus::parse_canonical(&status.canonical_bytes()).unwrap(),
            status
        );
        results.push(serde_json::to_value(status).unwrap());
    }
    peer.join().unwrap();
    assert_eq!(
        results[0]["recovery"],
        serde_json::json!({
            "state": "unavailable", "reason": "evidence_missing"
        })
    );
    assert_eq!(results[0]["recovery"], results[1]["recovery"]);
    assert_eq!(service.audit().unwrap(), audit);
    assert_eq!(service.receipts().head("recovery-status").unwrap(), head);
    service.admit_recovery("recovery-status", 3000).unwrap();
}

fn read_registered_status(
    service: &BrokerService,
    session: &mut BrokerSession,
    request: &RecoveryRequest,
    channel: &SeqpacketChannel,
    now_ms: u64,
) -> Result<SessionStatus, BrokerError> {
    let mut parked = lifecycle::status(session.authorization());
    parked.state = SessionState::Parked;
    parked.channel_state = ChannelState::Revoked;
    parked.launcher_head = Some(request.head.clone());
    parked.broker_head = Some(request.head.clone());
    thread::scope(|scope| {
        let peer = scope.spawn(|| lifecycle::answer_one_status_query(channel, &parked));
        let result = service.session_status(
            session,
            &LifecycleCaller::Operator {
                uid: CONTROLLER_UID,
            },
            now_ms,
            verify_fixture_signature,
        );
        peer.join().unwrap();
        result
    })
}

#[test]
fn status_reports_retained_point_and_original_expiry_without_renewing_it() {
    recovery::with_registered(|service, session, request, channel| {
        let audit = service.audit().unwrap();
        let head = service.receipts().head(&request.launch.session_id).unwrap();
        for now in [3000, 4000, request.retention.expires_at_ms] {
            let status = read_registered_status(service, session, request, channel, now).unwrap();
            assert_eq!(
                SessionStatus::parse_canonical(&status.canonical_bytes()).unwrap(),
                status
            );
            let json = serde_json::to_value(&status).unwrap();
            let expected = if now < request.retention.expires_at_ms {
                serde_json::json!({"state": "ready",
                    "operation_id": request.retention.request_id,
                    "expires_at_ms": request.retention.expires_at_ms})
            } else {
                serde_json::json!({"state": "expired"})
            };
            assert_eq!(json["recovery"], expected);
            let bytes = String::from_utf8(status.canonical_bytes()).unwrap();
            for forbidden in [
                "acp_session_id",
                "material_digest",
                "integration_digest",
                "assigned_uid",
                "assigned_gid",
                "pid",
                "path",
                "environment",
                "prompt",
            ] {
                assert!(!bytes.contains(forbidden), "leaked {forbidden}");
            }
        }
        assert_eq!(service.audit().unwrap(), audit);
        assert_eq!(
            service.receipts().head(&request.launch.session_id).unwrap(),
            head
        );
    });
}

#[test]
fn status_withholds_old_readiness_while_revalidation_is_not_durable() {
    recovery::registration(
        false,
        Some(3),
        |service, session, request, channel, root| {
            let pending = root.join("authorizations/recovery/pending-session-1.json");
            fs::write(&pending, b"true").unwrap();
            let status = read_registered_status(service, session, request, channel, 3000).unwrap();
            assert_eq!(
                serde_json::to_value(status).unwrap()["recovery"],
                serde_json::json!({
                    "state": "unavailable", "reason": "pending_durability"
                })
            );
            assert!(service.admit_recovery("session-1", 3000).is_err());
            fs::remove_file(pending).unwrap();
        },
    );
}

#[test]
fn status_refuses_corrupt_recovery_evidence() {
    recovery::registration(
        false,
        Some(3),
        |service, session, request, channel, root| {
            let path = root.join("authorizations/recovery/session-1.json");
            let original = fs::read(&path).unwrap();
            fs::write(&path, b"{\"partial\":").unwrap();
            assert!(read_registered_status(service, session, request, channel, 3000).is_err());
            assert!(service.admit_recovery("session-1", 3000).is_err());
            fs::write(path, original).unwrap();
        },
    );
}

#[test]
fn recovery_expiring_during_the_supervisor_query_is_not_reported_ready() {
    recovery::with_registered(|service, session, request, channel| {
        let mut parked = lifecycle::status(session.authorization());
        parked.state = SessionState::Parked;
        parked.channel_state = ChannelState::Revoked;
        parked.launcher_head = Some(request.head.clone());
        parked.broker_head = Some(request.head.clone());
        thread::scope(|scope| {
            let peer = scope.spawn(|| {
                thread::sleep(Duration::from_millis(20));
                lifecycle::answer_one_status_query(channel, &parked);
            });
            let status = service
                .session_status(
                    session,
                    &LifecycleCaller::Operator {
                        uid: CONTROLLER_UID,
                    },
                    request.retention.expires_at_ms - 1,
                    verify_fixture_signature,
                )
                .unwrap();
            peer.join().unwrap();
            assert_eq!(
                serde_json::to_value(status).unwrap()["recovery"],
                serde_json::json!({"state": "expired"})
            );
        });
    });
}
