//! Recovery admission consumes broker evidence, never advertised ACP support.

use super::*;
use louiselm_skills::launch_protocol::{RecoveryReadiness, RecoveryUnavailableReason};
use louiselm_skills::{
    broker::lifecycle::LifecycleCaller,
    launch_protocol::{
        BrokerConnection, ChannelState, RECOVERY_REQUEST_SCHEMA, RETENTION_EVIDENCE_SCHEMA,
        RecoveryRequest, RetentionEvidence, RetentionRequest, SUPERVISOR_STATUS_SCHEMA,
        SupervisorStatus,
    },
    launch_receipt::ReceiptHead,
};

#[test]
fn ordinary_work_remains_usable_without_recovery_but_required_work_is_refused() {
    let root = TempDir::new().unwrap();
    let launch = request("session-1");
    let authorizations =
        AuthorizationStore::open(&root.path().join("authorizations"), pool(4)).unwrap();
    authorizations.authorize(&grant(&launch), 1000).unwrap();
    authorizations.consume_for_launcher(&launch, 2000).unwrap();
    let mut required = grant(&request("required"));
    required.require_cold_recovery = true;
    authorizations.authorize(&required, 1000).unwrap();
    authorizations
        .consume_for_launcher(&required.request, 2000)
        .unwrap();
    let service = BrokerService::bind(
        &root.path().join("broker.sock"),
        authorizations,
        ReceiptStore::open(&root.path().join("receipts"), trusted_release()).unwrap(),
        AuditLog::open(&root.path().join("audit")).unwrap(),
        local_pin(),
    )
    .unwrap();
    assert_eq!(
        service.recovery_readiness("session-1", 2000).unwrap(),
        RecoveryReadiness::Unavailable {
            reason: RecoveryUnavailableReason::EvidenceMissing
        }
    );
    assert!(service.admit_recovery("session-1", 2000).is_ok());
    assert!(service.admit_recovery("required", 2000).is_err());
}

fn status_reply(channel: &SeqpacketChannel, request: &RecoveryRequest) {
    let packet = settle(|complete| channel.receive(complete));
    let LauncherPacket::Request(ProtocolMessage::Status(query)) = packet.packet else {
        panic!("status request");
    };
    let response = ProtocolResponse {
        schema: RESPONSE_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        request_id: query.request_id,
        result: ResponseResult::SupervisorStatus {
            status: SupervisorStatus {
                schema: SUPERVISOR_STATUS_SCHEMA.into(),
                protocol_version: PROTOCOL_VERSION,
                session_id: request.launch.session_id.clone(),
                run_id: request.launch.run_id.clone(),
                state: SessionState::Parked,
                broker_connection: BrokerConnection::Connected,
                envelope_revision: request.launch.envelope_revision,
                channel_state: ChannelState::Revoked,
                launcher_head: Some(request.head.clone()),
                broker_head: Some(request.head.clone()),
                pending_receipt_count: 0,
                pending_operation: None,
                process_exit: None,
                last_failure: None,
            },
        },
    };
    settle(|complete| channel.send(response.canonical_bytes(), complete));
}

#[test]
fn controller_registration_binds_retention_and_survives_restart_without_renewal() {
    registration(false, Some(3), |_, _, _, _, _| {});
}

#[test]
fn an_authenticated_supervisor_cannot_substitute_another_measured_integration() {
    registration(true, Some(3), |_, _, _, _, _| {});
}

pub(super) fn with_registered(
    check: impl FnOnce(
        &BrokerService,
        &mut louiselm_skills::broker::BrokerSession,
        &RecoveryRequest,
        &SeqpacketChannel,
    ),
) {
    registration(false, Some(3), |service, session, request, channel, _| {
        check(service, session, request, channel);
    });
}

#[expect(
    clippy::too_many_lines,
    reason = "One socket-level transaction follows controller authorization, Park, retention, replay, restart and expiry without resetting its evidence."
)]
pub(super) fn registration(
    wrong_integration: bool,
    uses: Option<u32>,
    check: impl FnOnce(
        &BrokerService,
        &mut louiselm_skills::broker::BrokerSession,
        &RecoveryRequest,
        &SeqpacketChannel,
        &Path,
    ),
) {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("broker.sock");
    let launch = request("session-1");
    let authorizations =
        AuthorizationStore::open(&root.path().join("authorizations"), pool(4)).unwrap();
    let mut approval = grant(&launch);
    approval.require_cold_recovery = true;
    approval.commands.as_mut().unwrap().uses = uses;
    authorizations.authorize(&approval, 1000).unwrap();
    let service = BrokerService::bind(
        &socket,
        authorizations,
        ReceiptStore::open(&root.path().join("receipts"), trusted_release()).unwrap(),
        AuditLog::open(&root.path().join("audit")).unwrap(),
        local_pin(),
    )
    .unwrap();
    let service = Arc::new(service);
    let peer_launch = launch.clone();
    let peer = thread::spawn(move || {
        let (authorization, channel) = fake_supervisor(&socket, &peer_launch, 2000);
        let receipt = super::lifecycle::drive_park_peer(&authorization, &channel, false);
        (channel, receipt)
    });
    let mut session = service
        .serve_launch(2000, verify_fixture_signature)
        .unwrap();
    let caller = LifecycleCaller::Operator {
        uid: CONTROLLER_UID,
    };
    let park = super::lifecycle::park(session.authorization());
    let receipt = service
        .request_lifecycle(&mut session, &caller, &park, 2000, verify_fixture_signature)
        .unwrap();
    let (channel, _) = peer.join().unwrap();
    let request = RecoveryRequest {
        schema: RECOVERY_REQUEST_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        launch,
        head: ReceiptHead {
            sequence: receipt.payload.sequence,
            digest: receipt.digest().to_string(),
        },
        retention: RetentionRequest {
            request_id: "recovery-1".into(),
            acp_session_id: "acp-1".into(),
            expires_at_ms: 60000,
        },
    };
    for caller in [
        LifecycleCaller::Agent,
        LifecycleCaller::Operator {
            uid: CONTROLLER_UID + 1,
        },
        LifecycleCaller::Coordinator {
            session_id: "coordinator".into(),
            run_id: "run-1".into(),
            descendants: vec!["session-1".into()],
            envelope_revision: 7,
            expires_at_ms: 60000,
        },
    ] {
        assert!(matches!(
            service.register_recovery(
                &mut session,
                &caller,
                &request,
                2000,
                verify_fixture_signature
            ),
            Err(BrokerError::ControllerMismatch)
        ));
        assert!(!session.channel().is_closed());
    }
    assert!(service.admit_recovery("session-1", 2000).is_err());
    let peer_request = request.clone();
    let observer = Arc::clone(&service);
    let peer = thread::spawn(move || {
        for _ in 0..if wrong_integration { 1 } else { 2 } {
            status_reply(&channel, &peer_request);
            let packet = settle(|complete| channel.receive(complete));
            assert_eq!(
                packet.packet,
                LauncherPacket::Request(ProtocolMessage::Recovery(peer_request.clone()))
            );
            assert!(
                observer.admit_recovery("session-1", 3000).is_err(),
                "even an identical retry is unavailable until supervisor revalidation is durable"
            );
            let response = ProtocolResponse {
                schema: RESPONSE_SCHEMA.into(),
                protocol_version: PROTOCOL_VERSION,
                request_id: peer_request.retention.request_id.clone(),
                result: ResponseResult::RecoveryRetention {
                    evidence: RetentionEvidence {
                        schema: RETENTION_EVIDENCE_SCHEMA.into(),
                        launch: peer_request.launch.clone(),
                        request: peer_request.retention.clone(),
                        contract: "louiselm.test-recovery/1".into(),
                        integration_digest: Digest::of(if wrong_integration {
                            b"wrong"
                        } else {
                            b"fixture-tool-isolation"
                        })
                        .to_string(),
                        material_digest: Digest::of(b"retained material").to_string(),
                    },
                },
            };
            settle(|complete| channel.send(response.canonical_bytes(), complete));
            if !wrong_integration {
                status_reply(&channel, &peer_request);
            }
        }
        channel
    });
    let registered = service.register_recovery(
        &mut session,
        &caller,
        &request,
        2000,
        verify_fixture_signature,
    );
    if wrong_integration {
        assert!(registered.is_err());
        assert_eq!(
            service.recovery_readiness("session-1", 2000).unwrap(),
            RecoveryReadiness::Unavailable {
                reason: RecoveryUnavailableReason::PendingDurability
            }
        );
        peer.join().unwrap();
        return;
    }
    let evidence = registered.unwrap();
    assert_eq!(
        service
            .register_recovery(
                &mut session,
                &caller,
                &request,
                3000,
                verify_fixture_signature
            )
            .unwrap(),
        evidence
    );
    let channel = peer.join().unwrap();
    check(&service, &mut session, &request, &channel, root.path());
    let mut conflict = request.clone();
    conflict.retention.expires_at_ms += 1;
    assert!(matches!(
        service.register_recovery(
            &mut session,
            &caller,
            &conflict,
            3000,
            verify_fixture_signature
        ),
        Err(BrokerError::RequestMismatch)
    ));
    assert!(service.admit_recovery("session-1", 3000).is_ok());
    assert_eq!(
        service.recovery_readiness("session-1", 60000).unwrap(),
        RecoveryReadiness::Expired {}
    );
    assert!(service.admit_recovery("session-1", 60000).is_err());
    drop(channel);
    drop(session);
    drop(service);
    let service = BrokerService::bind(
        &root.path().join("restarted.sock"),
        AuthorizationStore::open(&root.path().join("authorizations"), pool(4)).unwrap(),
        ReceiptStore::open(&root.path().join("receipts"), trusted_release()).unwrap(),
        AuditLog::open(&root.path().join("audit")).unwrap(),
        local_pin(),
    )
    .unwrap();
    assert!(service.admit_recovery("session-1", 3000).is_ok());
    assert!(service.admit_recovery("session-1", 60000).is_err());
    fs::write(
        root.path().join("authorizations/recovery/session-1.json"),
        b"{\"partial\":",
    )
    .unwrap();
    assert!(service.admit_recovery("session-1", 3000).is_err());
}
