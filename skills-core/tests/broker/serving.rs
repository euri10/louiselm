//! Acceptance does not wait for another peer's launch handshake.
use super::*;

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
