//! Restart uses only the exact verified prefix, without rebuilding grants.
use super::*;
use louiselm_skills::launch_protocol::{BROKER_RECONNECT_SCHEMA, BrokerReconnect};

pub(super) fn fixture(root: &Path) -> (BrokerService, LaunchAuthorization, Vec<SignedReceipt>) {
    fixture_on(root, None)
}

fn fixture_on(
    root: &Path,
    listener: Option<louiselm_skills::launch_transport::SeqpacketListener>,
) -> (BrokerService, LaunchAuthorization, Vec<SignedReceipt>) {
    let authorizations = AuthorizationStore::open(&root.join("authorizations"), pool(4)).unwrap();
    let request = request("reconnect-session");
    authorizations.authorize(&grant(&request), 1000).unwrap();
    let authorization = authorizations.consume_for_launcher(&request, 2000).unwrap();
    let receipts = ReceiptStore::open(&root.join("receipts"), trusted_release()).unwrap();
    let launch = launch_receipt(&authorization);
    let start = start_receipt(&authorization, &launch);
    for receipt in [&launch, &start] {
        receipts
            .append(
                &authorization,
                &receipt.canonical_bytes(),
                verify_fixture_signature,
            )
            .unwrap();
    }
    let listener = listener.unwrap_or_else(|| {
        louiselm_skills::launch_transport::SeqpacketListener::bind(&root.join("broker.sock"))
            .unwrap()
    });
    let service = BrokerService::over(
        listener,
        &root.join("broker.sock"),
        authorizations,
        receipts,
        AuditLog::open(&root.join("audit")).unwrap(),
        local_pin(),
    )
    .unwrap();
    (service, authorization, vec![launch, start])
}

#[test]
fn inherited_rendezvous_serves_an_authenticated_reconnection() {
    use louiselm_skills::launch_transport::SeqpacketListener;
    use rustix::net::{
        AddressFamily, SocketAddrUnix, SocketFlags, SocketType, bind, listen, socket_with,
    };
    let root = TempDir::new().unwrap();
    let fd = socket_with(
        AddressFamily::UNIX,
        SocketType::SEQPACKET,
        SocketFlags::CLOEXEC,
        None,
    )
    .unwrap();
    bind(
        &fd,
        &SocketAddrUnix::new(root.path().join("broker.sock")).unwrap(),
    )
    .unwrap();
    listen(&fd, 8).unwrap();
    let (service, authorization, chain) =
        fixture_on(root.path(), Some(SeqpacketListener::adopt(fd).unwrap()));
    let offer = checkpoint(&chain[1]);
    let channel = peer(root.path(), &offer);
    let session = service
        .serve_connection(90_000, verify_fixture_signature)
        .unwrap();
    assert_eq!(session.authorization(), &authorization);
    let reply = settle(|complete| channel.receive(complete));
    assert!(matches!(reply.packet, LauncherPacket::Response(response)
        if matches!(&response.result, ResponseResult::BrokerReconnect { reconnect } if reconnect == &offer)));
}

pub(super) fn checkpoint(receipt: &SignedReceipt) -> BrokerReconnect {
    BrokerReconnect {
        schema: BROKER_RECONNECT_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        request_id: "reconnect-1".into(),
        session_id: receipt.payload.session_id.clone(),
        run_id: receipt.payload.run_id.clone(),
        envelope_revision: receipt.payload.envelope_revision,
        sequence: receipt.payload.sequence,
        receipt_digest: receipt.digest().to_string(),
    }
}

pub(super) fn peer(root: &Path, offer: &BrokerReconnect) -> SeqpacketChannel {
    let connector = SeqpacketConnector::new().unwrap();
    let channel =
        settle(|complete| connector.connect(&root.join("broker.sock"), local_pin(), complete));
    settle(|complete| channel.send(offer.canonical_bytes(), complete));
    channel
}

#[test]
fn equal_prefix_reattaches_after_authorization_expiry_without_reconsumption() {
    let root = TempDir::new().unwrap();
    let (service, authorization, chain) = fixture(root.path());
    let offer = checkpoint(&chain[1]);
    let channel = peer(root.path(), &offer);
    let session = service
        .serve_reconnect(90_000, verify_fixture_signature)
        .unwrap();
    assert_eq!(session.authorization(), &authorization);
    let reply = settle(|complete| channel.receive(complete));
    assert!(matches!(reply.packet, LauncherPacket::Response(response)
        if matches!(&response.result, ResponseResult::BrokerReconnect { reconnect } if reconnect == &offer)));
    assert_eq!(
        service
            .receipts()
            .stored_bytes(&authorization.session_id)
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn supervisor_ahead_appends_only_the_exact_signed_suffix() {
    let root = TempDir::new().unwrap();
    let (service, authorization, chain) = fixture(root.path());
    let park = signed(payload(
        &authorization,
        "broker-loss-park",
        2,
        Some(chain[1].digest().to_string()),
        ReceiptOutcome::Park {
            authority: louiselm_skills::launch_receipt::ReceiptAuthority::Cause {
                cause: louiselm_skills::launch_receipt::ReceiptCause::BrokerLost,
            },
        },
        SessionState::Parked,
    ));
    let channel = peer(root.path(), &checkpoint(&park));
    let expected = park.canonical_bytes();
    let worker = thread::spawn(move || {
        let reply = settle(|complete| channel.receive(complete));
        assert!(matches!(reply.packet, LauncherPacket::Response(response)
            if matches!(&response.result, ResponseResult::BrokerReconnect { reconnect } if reconnect.sequence == 1)));
        settle(|complete| channel.send(park.canonical_bytes(), complete));
        assert_eq!(expect_acknowledgement(&channel).sequence, 2);
        channel
    });
    let _session = service
        .serve_reconnect(90_000, verify_fixture_signature)
        .unwrap();
    let _channel = worker.join().unwrap();
    assert_eq!(
        service
            .receipts()
            .stored_bytes(&authorization.session_id)
            .unwrap()[2],
        expected
    );
}

#[test]
fn conflicting_or_broker_ahead_checkpoints_refuse_without_changing_receipts() {
    for kind in ["digest", "ahead", "run", "revision"] {
        let root = TempDir::new().unwrap();
        let (service, authorization, chain) = fixture(root.path());
        let mut offer = checkpoint(&chain[1]);
        match kind {
            "digest" => offer.receipt_digest = Digest::of(b"foreign").to_string(),
            "ahead" => offer = checkpoint(&chain[0]),
            "run" => offer.run_id = "foreign".into(),
            _ => offer.envelope_revision += 1,
        }
        let channel = peer(root.path(), &offer);
        assert!(
            service
                .serve_reconnect(90_000, verify_fixture_signature)
                .is_err(),
            "{kind}"
        );
        expect_disconnect(&channel);
        assert_eq!(
            service
                .receipts()
                .stored_bytes(&authorization.session_id)
                .unwrap()
                .len(),
            2
        );
        let outbox = louiselm_skills::broker::attention::Outbox::open(
            &root.path().join("authorizations/attention-outbox"),
        )
        .unwrap();
        assert!(
            outbox.next().unwrap().is_some(),
            "refusal queues local Attention"
        );
    }
}

#[test]
fn stored_receipt_gap_cannot_silently_shorten_the_verified_prefix() {
    let root = TempDir::new().unwrap();
    let (service, authorization, chain) = fixture(root.path());
    let directory = root.path().join("receipts/sessions/reconnect-session");
    // A torn/corrupt store has a later record despite its missing predecessor.
    fs::write(
        directory.join("00000000000000000003.receipt.json"),
        chain[1].canonical_bytes(),
    )
    .unwrap();
    assert!(
        service
            .receipts()
            .stored_bytes(&authorization.session_id)
            .is_err()
    );
}

#[test]
fn invalid_suffix_is_not_acknowledged_or_spliced() {
    for kind in ["gap", "signature", "prefix", "head"] {
        let root = TempDir::new().unwrap();
        let (service, authorization, chain) = fixture(root.path());
        let mut park = signed(payload(
            &authorization,
            "lost",
            2,
            Some(chain[1].digest().to_string()),
            ReceiptOutcome::Park {
                authority: louiselm_skills::launch_receipt::ReceiptAuthority::Cause {
                    cause: louiselm_skills::launch_receipt::ReceiptCause::BrokerLost,
                },
            },
            SessionState::Parked,
        ));
        match kind {
            "gap" => {
                park.payload.sequence = 3;
                park = signed(park.payload);
            }
            "prefix" => {
                park.payload.previous_receipt_digest = Some(Digest::of(b"wrong").to_string());
                park = signed(park.payload);
            }
            "signature" => park.signature = Digest::of(b"wrong").to_string(),
            _ => {}
        }
        let mut offer = checkpoint(&park);
        if kind == "head" {
            offer.receipt_digest = Digest::of(b"wrong").to_string();
        }
        let channel = peer(root.path(), &offer);
        let worker = thread::spawn(move || {
            let _ = settle(|complete| channel.receive(complete));
            settle(|complete| channel.send(park.canonical_bytes(), complete));
            expect_disconnect(&channel);
        });
        assert!(
            service
                .serve_reconnect(90_000, verify_fixture_signature)
                .is_err(),
            "{kind}"
        );
        worker.join().unwrap();
        assert_eq!(
            service
                .receipts()
                .stored_bytes(&authorization.session_id)
                .unwrap()
                .len(),
            2
        );
    }
}

#[test]
fn stored_signature_is_reverified_before_replying_with_a_checkpoint() {
    let root = TempDir::new().unwrap();
    let (service, _, chain) = fixture(root.path());
    let channel = peer(root.path(), &checkpoint(&chain[1]));
    assert!(service.serve_reconnect(90_000, |_, _, _| false).is_err());
    expect_disconnect(&channel);
}

#[test]
fn status_is_answerable_after_a_broker_restart_reattaches_the_exact_prefix() {
    use louiselm_skills::{
        broker::lifecycle::LifecycleCaller,
        launch_protocol::{LifecycleAction, PostureSummary},
    };

    let root = TempDir::new().unwrap();
    let (service, authorization, chain) = fixture(root.path());
    let offer = checkpoint(&chain[1]);
    let peer_root = root.path().to_owned();
    let peer_authorization = authorization.clone();
    let peer = thread::spawn(move || {
        let channel = peer(&peer_root, &checkpoint(&chain[1]));
        // Drain the reattachment reply, then answer the broker's status query.
        let _ = settle(|complete| channel.receive(complete));
        super::lifecycle::answer_one_status_query(
            &channel,
            &super::lifecycle::status(&peer_authorization),
        );
        channel
    });
    let mut session = service
        .serve_reconnect(90_000, verify_fixture_signature)
        .unwrap();
    assert_eq!(session.authorization(), &authorization);

    // A reattached worker holds no command authority, but status still answers.
    let status = service
        .session_status(
            &mut session,
            &LifecycleCaller::Operator {
                uid: CONTROLLER_UID,
            },
            PostureSummary::Pending,
            90_000,
            verify_fixture_signature,
        )
        .unwrap();
    assert_eq!(status.session_id, offer.session_id);
    assert_eq!(status.state, SessionState::Running);
    assert_eq!(
        status.broker_head,
        service.receipts().head(&authorization.session_id).unwrap()
    );
    assert_eq!(
        status.allowed_actions,
        vec![
            LifecycleAction::Park,
            LifecycleAction::Interrupt,
            LifecycleAction::Disposal,
        ],
    );
    peer.join().unwrap();
}

#[test]
fn one_rendezvous_routes_launches_reattachments_and_refuses_anything_else() {
    use louiselm_skills::launch_protocol::{STATUS_REQUEST_SCHEMA, StatusRequest};

    // A peer opening with a launch request is served as a new launch.
    let root = TempDir::new().unwrap();
    let socket = root.path().join("broker.sock");
    let request = request("session-1");
    let service = super::lifecycle::bound_service(root.path(), &socket, &request);
    let supervisor = thread::spawn(move || fake_supervisor(&socket, &request, 2000));
    let launched = service
        .serve_connection(2000, verify_fixture_signature)
        .unwrap();
    assert_eq!(launched.authorization().session_id, "session-1");
    let (authorization, channel) = supervisor.join().unwrap();
    assert_eq!(launched.authorization(), &authorization);
    drop(channel);
    service.close();

    // A peer opening with a reconnect offer is reattached on the same rendezvous.
    let restarted_root = TempDir::new().unwrap();
    let (restarted, authorization, chain) = fixture(restarted_root.path());
    let offer = checkpoint(&chain[1]);
    let peer_root = restarted_root.path().to_owned();
    let reattaching = thread::spawn(move || peer(&peer_root, &checkpoint(&chain[1])));
    let session = restarted
        .serve_connection(90_000, verify_fixture_signature)
        .unwrap();
    assert_eq!(session.authorization(), &authorization);
    let reattached = reattaching.join().unwrap();
    let reply = settle(|complete| reattached.receive(complete));
    assert!(matches!(reply.packet, LauncherPacket::Response(response)
        if matches!(&response.result, ResponseResult::BrokerReconnect { reconnect } if reconnect == &offer)));

    // Any other opening packet is refused, with no response to correlate.
    let stray_root = TempDir::new().unwrap();
    let (stray, stray_authorization, _) = fixture(stray_root.path());
    let stray_path = stray_root.path().to_owned();
    let opener = thread::spawn(move || {
        let connector = SeqpacketConnector::new().unwrap();
        let channel = settle(|complete| {
            connector.connect(&stray_path.join("broker.sock"), local_pin(), complete)
        });
        settle(|complete| {
            channel.send(
                StatusRequest {
                    schema: STATUS_REQUEST_SCHEMA.into(),
                    protocol_version: PROTOCOL_VERSION,
                    request_id: "status-1".into(),
                    session_id: stray_authorization.session_id.clone(),
                    run_id: stray_authorization.run_id.clone(),
                }
                .canonical_bytes(),
                complete,
            )
        });
        channel
    });
    let refused = stray.serve_connection(90_000, verify_fixture_signature);
    assert!(matches!(refused, Err(BrokerError::InvalidGrant)));
    expect_disconnect(&opener.join().unwrap());
}
