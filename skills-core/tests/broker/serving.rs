//! Acceptance does not wait for another peer's launch handshake.
use super::*;

#[expect(
    clippy::too_many_lines,
    reason = "One ordered launch scenario checks the receipts, command refusal and non-refundable identity reuse."
)]
fn expired_command_approval(before_startup: bool, failed_launch: bool) {
    use std::time::Instant;

    let root = TempDir::new().unwrap();
    let socket = root.path().join("broker.sock");
    let request = request("expired-command");
    let authorizations =
        AuthorizationStore::open(&root.path().join("authorizations"), pool(1)).unwrap();
    let mut approval = grant(&request);
    approval.commands.as_mut().unwrap().expires_at_ms = if before_startup { 2_000 } else { 6_000 };
    authorizations.authorize(&approval, 1_000).unwrap();
    let service = Arc::new(
        BrokerService::bind(
            &socket,
            authorizations,
            ReceiptStore::open(&root.path().join("receipts"), trusted_release()).unwrap(),
            AuditLog::open(&root.path().join("audit")).unwrap(),
            local_pin(),
        )
        .unwrap(),
    );
    let broker = Arc::clone(&service);
    let started = Instant::now();
    let worker = thread::spawn(move || broker.serve_launch(2_000, verify_fixture_signature));
    let (authorization, channel) = supervisor_authorization(&socket, &request, 2_000);
    let launch = launch_receipt(&authorization);
    let mut expected = Vec::new();
    if !before_startup || !failed_launch {
        settle(|complete| channel.send(launch.canonical_bytes(), complete));
        assert_eq!(expect_acknowledgement(&channel).sequence, 0);
        expected.push(launch.canonical_bytes());
    }
    if !before_startup {
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "approval must be live at the first ACK"
        );
        // The first ACK proves that the policy was derived. Wait past its
        // original deadline before offering the second startup receipt.
        thread::sleep(Duration::from_millis(4_100));
    }
    if failed_launch {
        let terminal = startup_failure(&authorization, (!before_startup).then_some(&launch));
        settle(|complete| channel.send(terminal.canonical_bytes(), complete));
        assert_eq!(
            expect_acknowledgement(&channel).sequence,
            expected.len() as u64
        );
        expected.push(terminal.canonical_bytes());
        assert!(matches!(
            worker.join().unwrap(),
            Err(BrokerError::LaunchFailed)
        ));
        let inspection = service.inspect(&request.session_id).unwrap().unwrap();
        assert_eq!(inspection.state, SessionState::Terminal);
        assert!(inspection.start_evidence.is_none());
        assert_eq!(
            inspection.last_failure.unwrap().code,
            ErrorCode::LaunchFailed
        );
        assert_eq!(
            service
                .authorizations()
                .session_count_for_run("run-1")
                .unwrap(),
            1
        );
        let successor = service
            .authorize(
                &grant(&super::request("successor")),
                3_000,
                verify_fixture_signature,
            )
            .unwrap();
        assert_eq!(successor.identity.slot, authorization.identity_slot);
    } else {
        let start = start_receipt(&authorization, &launch);
        settle(|complete| channel.send(start.canonical_bytes(), complete));
        assert_eq!(expect_acknowledgement(&channel).sequence, 1);
        expected.push(start.canonical_bytes());
        let mut session = worker.join().unwrap().unwrap();
        assert_eq!(session.launch_head().digest, start.digest().to_string());
        assert_expired_command_rejected(&mut session, &channel);
        assert!(service.audit().unwrap().iter().all(|entry| !matches!(
            entry.decision,
            AuditDecision::EffectCommitIntent { .. } | AuditDecision::ToolGranted { .. }
        )));
    }
    assert_eq!(
        service
            .receipts()
            .stored_bytes(&request.session_id)
            .unwrap(),
        expected
    );
    assert_eq!(
        service
            .authorizations()
            .session_count_for_run("run-1")
            .unwrap(),
        if failed_launch { 2 } else { 1 }
    );
    assert!(matches!(
        service
            .authorizations()
            .consume(&request, CONTROLLER_UID, 3_000),
        Err(BrokerError::UnknownAuthorization)
    ));
    if !failed_launch {
        assert!(service.audit().unwrap().iter().any(|entry| {
            entry.session_id == request.session_id
                && entry.decision == AuditDecision::CommandApprovalExpired
        }));
    }
}

fn startup_failure(
    authorization: &LaunchAuthorization,
    launch: Option<&SignedReceipt>,
) -> SignedReceipt {
    signed(payload(
        authorization,
        if launch.is_none() {
            &authorization.request_id
        } else {
            "failed-launch"
        },
        u64::from(launch.is_some()),
        launch.map(|receipt| receipt.digest().to_string()),
        ReceiptOutcome::FailedLaunch {
            authorization: Box::new(Authorization {
                authorization_id: authorization.authorization_id.clone(),
                request_id: authorization.request_id.clone(),
                request_digest: authorization.request_digest.clone(),
            }),
            identity_slot: authorization.identity_slot,
            assigned_uid: authorization.assigned_uid,
            assigned_gid: authorization.assigned_gid,
            failure: louiselm_skills::launch_receipt::LaunchFailure {
                phase: if launch.is_none() {
                    "workspace_prepare"
                } else {
                    "agent_initialize"
                }
                .into(),
                code: "unavailable".into(),
            },
            agent_execution: louiselm_skills::launch_receipt::AgentExecution::NotStarted,
            retained_storage: louiselm_skills::launch_recovery::RetainedStorage::Sealed,
        },
        SessionState::Terminal,
    ))
}

fn assert_expired_command_rejected(
    session: &mut louiselm_skills::broker::BrokerSession,
    channel: &SeqpacketChannel,
) {
    use louiselm_skills::launch_protocol::{
        COMMAND_SCHEMA, CommandMessage, CommandOperation, CommandPrincipal, TOOL_EXECUTION_SCHEMA,
        ToolExecutionRequest,
    };
    let authorization = session.authorization();
    let command = CommandMessage {
        schema: COMMAND_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        request_id: "expired-command-request".into(),
        session_id: authorization.session_id.clone(),
        run_id: authorization.run_id.clone(),
        envelope_revision: authorization.envelope_revision,
        operation: CommandOperation::Request {
            principal: CommandPrincipal {
                channel_id: "agent-capability".into(),
                pid: 123,
                uid: authorization.assigned_uid,
                gid: authorization.assigned_gid,
            },
            command: ToolExecutionRequest {
                schema: TOOL_EXECUTION_SCHEMA.into(),
                protocol_version: PROTOCOL_VERSION,
                request_id: "expired-command-request".into(),
                session_id: authorization.session_id.clone(),
                run_id: authorization.run_id.clone(),
                envelope_revision: authorization.envelope_revision,
                sequence: 1,
                command: "printf sensitive".into(),
                timeout_ms: 1_000,
            },
        },
    };
    settle(|complete| channel.send(command.canonical_bytes(), complete));
    session.serve_command().unwrap();
    assert!(matches!(
        settle(|complete| channel.receive(complete)).packet,
        LauncherPacket::Request(ProtocolMessage::Command(CommandMessage {
            operation: CommandOperation::Reject {
                error: ErrorCode::InvalidRequest
            },
            ..
        }))
    ));
    assert!(!session.channel().is_closed());
}

#[test]
fn expired_command_before_startup_does_not_block_running_or_cleanup() {
    expired_command_approval(true, false);
    expired_command_approval(true, true);
}

#[test]
fn command_expiring_during_startup_does_not_block_running_or_cleanup() {
    expired_command_approval(false, false);
    expired_command_approval(false, true);
}

#[test]
fn stalled_and_invalid_peers_do_not_block_another_session() {
    let root = TempDir::new().unwrap();
    let (service, authorization, chain) = reconnect::fixture(root.path());
    let service = Arc::new(service);
    let connector = SeqpacketConnector::new().unwrap();
    let stalled =
        settle(|done| connector.connect(&root.path().join("broker.sock"), local_pin(), done));
    let accepted = service.accept_connection().unwrap();
    let slow_service = Arc::clone(&service);
    let slow = thread::spawn(move || {
        slow_service.serve_accepted(accepted, 90_000, verify_fixture_signature)
    });

    let offer = reconnect::checkpoint(&chain[1]);
    let peer = reconnect::peer(root.path(), &offer);
    let accepted = service.accept_connection().unwrap();
    let session = service
        .serve_accepted(accepted, 90_000, verify_fixture_signature)
        .unwrap();
    assert_eq!(session.authorization(), &authorization);
    let reply = settle(|done| peer.receive(done));
    assert!(matches!(reply.packet, LauncherPacket::Response(response)
        if matches!(response.result, ResponseResult::BrokerReconnect { .. })));

    // Invalid input fails only its own worker; the valid owner remains open.
    let invalid_opening = StatusRequest {
        schema: STATUS_REQUEST_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        request_id: "not-a-launch".into(),
        session_id: authorization.session_id.clone(),
        run_id: authorization.run_id.clone(),
    };
    settle(|done| stalled.send(invalid_opening.canonical_bytes(), done));
    assert!(slow.join().unwrap().is_err());
    assert!(!session.channel().is_closed());
    service.close();
    assert!(service.accept_connection().is_err());
}

#[test]
fn idle_session_waits_past_the_handshake_deadline() {
    let root = TempDir::new().unwrap();
    let (service, _, chain) = reconnect::fixture(root.path());
    let peer = reconnect::peer(root.path(), &reconnect::checkpoint(&chain[1]));
    let mut session = service
        .serve_connection(90_000, verify_fixture_signature)
        .unwrap();
    settle(|done| peer.receive(done));
    let (finished, result) = mpsc::channel();
    let worker = thread::spawn(move || {
        finished
            .send(
                service
                    .step(&mut session, 90_000, None, verify_fixture_signature)
                    .is_err(),
            )
            .unwrap();
    });
    // The former per-packet deadline closed a perfectly healthy idle Session.
    assert!(matches!(
        result.recv_timeout(Duration::from_secs(31)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    peer.close();
    assert!(result.recv_timeout(Duration::from_secs(2)).unwrap());
    worker.join().unwrap();
}

#[test]
fn authorized_launch_can_finish_preparation_after_the_handshake_deadline() {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("broker.sock");
    let request = request("slow-preparation");
    let authorizations =
        AuthorizationStore::open(&root.path().join("authorizations"), pool(4)).unwrap();
    let mut approval = grant(&request);
    approval.expires_at_ms = 180_000;
    approval.commands.as_mut().unwrap().expires_at_ms = 180_000;
    authorizations.authorize(&approval, 1_000).unwrap();
    let service = Arc::new(
        BrokerService::bind(
            &socket,
            authorizations,
            ReceiptStore::open(&root.path().join("receipts"), trusted_release()).unwrap(),
            AuditLog::open(&root.path().join("audit")).unwrap(),
            local_pin(),
        )
        .unwrap(),
    );
    let broker = Arc::clone(&service);
    let (finished, result) = mpsc::channel();
    let worker = thread::spawn(move || {
        finished
            .send(broker.serve_launch(2_000, verify_fixture_signature))
            .unwrap();
    });
    let (authorization, channel) = supervisor_authorization(&socket, &request, 2_000);

    // Preparation can copy a source-sized workspace before the first receipt.
    match result.recv_timeout(Duration::from_secs(31)) {
        Err(mpsc::RecvTimeoutError::Timeout) => {}
        Ok(Err(error)) => panic!("broker stopped during preparation: {error:?}"),
        Ok(Ok(_)) => panic!("broker completed before receiving launch receipts"),
        Err(error) => panic!("broker worker disappeared: {error}"),
    }
    let report = conformance_transport::report_bytes();
    let launch = conformance_transport::report_receipt(&authorization, &report);
    settle(|complete| channel.send(launch.canonical_bytes(), complete));
    for packet in conformance_transport::report_packets(&launch, &report) {
        settle(|complete| channel.send(packet, complete));
    }
    assert_eq!(expect_acknowledgement(&channel).sequence, 0);
    let start = start_receipt(&authorization, &launch);
    settle(|complete| channel.send(start.canonical_bytes(), complete));
    assert_eq!(expect_acknowledgement(&channel).sequence, 1);
    let session = result
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    assert_eq!(session.launch_head().digest, start.digest().to_string());
    assert_eq!(
        service
            .receipts()
            .stored_bytes(&request.session_id)
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        service
            .receipts()
            .conformance_report(&authorization, verify_fixture_signature)
            .unwrap(),
        Some(report)
    );
    worker.join().unwrap();
}

#[test]
fn disconnected_supervisor_releases_its_waiting_launch_worker() {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("broker.sock");
    let request = request("disconnected-preparation");
    let authorizations =
        AuthorizationStore::open(&root.path().join("authorizations"), pool(4)).unwrap();
    authorizations.authorize(&grant(&request), 1_000).unwrap();
    let service = BrokerService::bind(
        &socket,
        authorizations,
        ReceiptStore::open(&root.path().join("receipts"), trusted_release()).unwrap(),
        AuditLog::open(&root.path().join("audit")).unwrap(),
        local_pin(),
    )
    .unwrap();
    let (finished, result) = mpsc::channel();
    let worker = thread::spawn(move || {
        finished
            .send(service.serve_launch(2_000, verify_fixture_signature))
            .unwrap();
    });
    let (_, channel) = supervisor_authorization(&socket, &request, 2_000);
    channel.close();
    assert!(matches!(
        result.recv_timeout(Duration::from_secs(2)).unwrap(),
        Err(BrokerError::Transport(
            TransportError::Disconnected | TransportError::Closed
        ))
    ));
    worker.join().unwrap();
}
