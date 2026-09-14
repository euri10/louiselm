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
                    .step(&mut session, 90_000, verify_fixture_signature)
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
