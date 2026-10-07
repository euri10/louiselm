//! Real authenticated transfer, Session admission, relay and closure.
use super::*;
use louiselm_skills::launch_protocol::{BROKER_RECONNECT_SCHEMA, BrokerReconnect};
use louiselm_skills::launch_protocol::{GuardEnrollment, GuardScope};
use std::{
    fs::File,
    net::{TcpListener, TcpStream},
    os::{fd::AsFd, unix::fs::MetadataExt},
};

fn enrollment(listener: &TcpListener, pins: &File, network: &File) -> GuardEnrollment {
    let now = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    GuardEnrollment {
        scope: GuardScope {
            session_id: "session-1".into(),
            run_id: "run-1".into(),
            envelope_revision: 1,
            revision: 1,
            deadline_ns: u64::try_from(now.tv_sec).unwrap() * 1_000_000_000
                + u64::try_from(now.tv_nsec).unwrap()
                + 1_000_000_000,
        },
        guard_id: pins.metadata().unwrap().ino(),
        runtime_pid: 123,
        broker_pid: std::process::id(),
        address: listener.local_addr().unwrap(),
        listener_cookie: rustix::net::sockopt::socket_cookie(listener).unwrap(),
        network_id: network.metadata().unwrap().ino().try_into().unwrap(),
    }
}

fn response(result: ResponseResult) -> ProtocolResponse {
    ProtocolResponse {
        schema: RESPONSE_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        request_id: "guard-handoff".into(),
        result,
    }
}

#[test]
fn networking_revision_cannot_be_self_authorized() {
    let mut fixture = fixture(Some(approval(5)), Some("openai"));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let pins = File::open("/proc/self/ns/mnt").unwrap();
    let network = File::open("/proc/self/ns/net").unwrap();
    let mut enrollment = enrollment(&listener, &pins, &network);
    enrollment.scope.session_id = fixture.session.authorization().session_id.clone();
    enrollment.scope.envelope_revision = fixture.session.authorization().envelope_revision;
    enrollment.scope.revision = 2;
    let message = response(ResponseResult::SenderGuardEnrolled { enrollment });
    settle(|complete| {
        fixture.peer.send_descriptors(
            message.canonical_bytes(),
            [listener.as_fd(), pins.as_fd(), network.as_fd()],
            complete,
        )
    });
    assert!(matches!(
        fixture
            .service
            .step(&mut fixture.session, 2500, None, verify_fixture_signature,),
        Err(BrokerError::ProviderUnavailable)
    ));
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "One controlled guard-close/status race proves the closure ACK, authenticated terminal persistence and status correlation."
)]
fn posture_read_services_guard_closure_before_terminal_cleanup() {
    let mut fixture = fixture(Some(approval(5)), Some("openai"));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let pins = File::open("/proc/self/ns/mnt").unwrap();
    let network = File::open("/proc/self/ns/net").unwrap();
    let mut enrollment = enrollment(&listener, &pins, &network);
    enrollment.scope.session_id = fixture.session.authorization().session_id.clone();
    enrollment.scope.envelope_revision = fixture.session.authorization().envelope_revision;
    let enrolled = response(ResponseResult::SenderGuardEnrolled {
        enrollment: enrollment.clone(),
    });
    settle(|done| {
        fixture.peer.send_descriptors(
            enrolled.canonical_bytes(),
            [listener.as_fd(), pins.as_fd(), network.as_fd()],
            done,
        )
    });
    fixture
        .service
        .step(&mut fixture.session, 2500, None, verify_fixture_signature)
        .unwrap();
    let _accepted = settle(|done| fixture.peer.receive(done));
    drop(listener);
    let terminal = signed(payload(
        fixture.session.authorization(),
        "cleanup-after-guard-close",
        2,
        Some(
            fixture
                .current
                .launcher_head
                .as_ref()
                .unwrap()
                .digest
                .clone(),
        ),
        ReceiptOutcome::Disposal {
            authority: ReceiptAuthority::Cause {
                cause: ReceiptCause::AgentIdentityLost,
            },
        },
        SessionState::Terminal,
    ));
    thread::scope(|threads| {
        let peer = &fixture.peer;
        let mut current = fixture.current.clone();
        let enrollment = enrollment.clone();
        let terminal = &terminal;
        let driver = threads.spawn(move || {
            let packet = settle(|done| peer.receive(done));
            let LauncherPacket::Request(ProtocolMessage::Status(query)) = packet.packet else {
                panic!("expected upkeep status request");
            };
            // Disposal can start after the broker sends upkeep but before the
            // owner handles it. The owner needs this ACK before it can answer.
            let closing = response(ResponseResult::SenderGuardClosing {
                enrollment: enrollment.clone(),
            });
            settle(|done| peer.send(closing.canonical_bytes(), done));
            let closed = settle(|done| peer.receive(done));
            assert!(matches!(closed.packet, LauncherPacket::Response(ack)
                if ack.request_id == closing.request_id
                    && ack.result == ResponseResult::SenderGuardClosed { enrollment }));
            settle(|done| peer.send(terminal.canonical_bytes(), done));
            let ack = expect_acknowledgement(peer);
            assert_eq!(ack.sequence, 2);
            assert_eq!(ack.receipt_digest, terminal.digest().to_string());
            current.state = SessionState::Terminal;
            current.channel_state = louiselm_skills::launch_protocol::ChannelState::Closed;
            current.launcher_head = Some(louiselm_skills::launch_receipt::ReceiptHead {
                sequence: 2,
                digest: terminal.digest().to_string(),
            });
            current.broker_head = current.launcher_head.clone();
            let reply = ProtocolResponse {
                schema: RESPONSE_SCHEMA.into(),
                protocol_version: PROTOCOL_VERSION,
                request_id: query.request_id,
                result: ResponseResult::SupervisorStatus { status: current },
            };
            settle(|done| peer.send(reply.canonical_bytes(), done));
        });
        let result = fixture.service.project_posture_attention(
            &mut fixture.session,
            2500,
            verify_fixture_signature,
        );
        if result.is_err() {
            fixture.peer.close();
        }
        let driver = driver.join();
        assert!(
            result.is_ok(),
            "upkeep interrupted guard cleanup: {result:?}"
        );
        driver.unwrap();
    });
    assert_eq!(
        TcpStream::connect(enrollment.address).unwrap_err().kind(),
        std::io::ErrorKind::ConnectionRefused
    );
    assert!(!fixture.session.channel().is_closed());
    assert_eq!(
        fixture
            .service
            .receipts()
            .stored_bytes("provider-requests")
            .unwrap()
            .last(),
        Some(&terminal.canonical_bytes())
    );
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "One authenticated handoff, Park and Resume dispatch trace."
)]
fn operator_resume_dispatches_fresh_authority_bound_to_the_durable_park() {
    use louiselm_skills::{
        broker::lifecycle::LifecycleCaller,
        launch_protocol::{ChannelState, LifecycleAction},
    };
    let mut fixture = fixture(Some(approval(5)), Some("openai"));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let pins = File::open("/proc/self/ns/mnt").unwrap();
    let network = File::open("/proc/self/ns/net").unwrap();
    let mut original = enrollment(&listener, &pins, &network);
    original.scope.session_id = fixture.session.authorization().session_id.clone();
    original.scope.envelope_revision = fixture.session.authorization().envelope_revision;
    let enrolled = response(ResponseResult::SenderGuardEnrolled {
        enrollment: original.clone(),
    });
    settle(|complete| {
        fixture.peer.send_descriptors(
            enrolled.canonical_bytes(),
            [listener.as_fd(), pins.as_fd(), network.as_fd()],
            complete,
        )
    });
    fixture
        .service
        .step(&mut fixture.session, 2500, None, verify_fixture_signature)
        .unwrap();
    let _accepted = settle(|complete| fixture.peer.receive(complete));
    let closing = response(ResponseResult::SenderGuardClosing {
        enrollment: original.clone(),
    });
    settle(|complete| fixture.peer.send(closing.canonical_bytes(), complete));
    fixture
        .service
        .step(&mut fixture.session, 2500, None, verify_fixture_signature)
        .unwrap();
    let _closed = settle(|complete| fixture.peer.receive(complete));
    let caller = LifecycleCaller::Operator {
        uid: CONTROLLER_UID,
    };
    let park = lifecycle::park(fixture.session.authorization());
    let parked = thread::scope(|threads| {
        let peer = &fixture.peer;
        let current = &fixture.current;
        let driver = threads.spawn(move || lifecycle::drive_lifecycle_peer(peer, current));
        let receipt = fixture
            .service
            .request_lifecycle(
                &mut fixture.session,
                &caller,
                &park,
                2500,
                verify_fixture_signature,
            )
            .unwrap();
        assert_eq!(driver.join().unwrap(), receipt);
        receipt
    });
    let head = louiselm_skills::launch_receipt::ReceiptHead {
        sequence: parked.payload.sequence,
        digest: parked.digest().to_string(),
    };
    fixture.current.state = SessionState::Parked;
    fixture.current.channel_state = ChannelState::Revoked;
    fixture.current.launcher_head = Some(head.clone());
    fixture.current.broker_head = Some(head.clone());
    let mut resume = park;
    resume.request_id = "guard-resume-1".into();
    resume.authorization_id = "operator-resume".into();
    resume.action = LifecycleAction::Resume;
    resume.expected_state = SessionState::Parked;
    resume.expected_receipt_sequence = Some(head.sequence);
    let authority = thread::scope(|threads| {
        let peer = &fixture.peer;
        let current = &fixture.current;
        let request_id = resume.request_id.clone();
        let driver = threads.spawn(move || {
            lifecycle::answer_one_status_query(peer, current);
            let packet = settle(|complete| peer.receive(complete));
            let authority = match packet.packet {
                LauncherPacket::Request(ProtocolMessage::GuardResume(authority)) => {
                    Some(*authority)
                }
                _ => None,
            };
            let error = response(ResponseResult::Error {
                error: ProtocolError::new(
                    ErrorCode::LifecycleMechanicUnavailable,
                    Some(SessionState::Parked),
                    Some(2),
                ),
            });
            let error = ProtocolResponse {
                request_id,
                ..error
            };
            settle(|complete| peer.send(error.canonical_bytes(), complete));
            authority
        });
        assert!(matches!(
            fixture.service.request_lifecycle(
                &mut fixture.session,
                &caller,
                &resume,
                2500,
                verify_fixture_signature
            ),
            Err(BrokerError::Policy(_))
        ));
        driver.join().unwrap()
    })
    .expect("a guarded Resume needs broker-owned fresh networking authority");
    assert_eq!(authority.request, resume);
    assert_eq!(authority.parked_head, head);
    assert_eq!(authority.scope.revision, 2);
    assert_eq!(
        authority.scope.envelope_revision,
        original.scope.envelope_revision
    );
    assert_eq!(authority.scope.deadline_ns, original.scope.deadline_ns);
}

#[test]
fn transferred_listener_uses_production_admission_and_closes_before_ack() {
    let mut fixture = fixture(Some(approval(5)), Some("openai"));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let pins = File::open("/proc/self/ns/mnt").unwrap();
    let network = File::open("/proc/self/ns/net").unwrap();
    let mut enrollment = enrollment(&listener, &pins, &network);
    enrollment.scope.session_id = fixture.session.authorization().session_id.clone();
    enrollment.scope.envelope_revision = fixture.session.authorization().envelope_revision;
    let message = response(ResponseResult::SenderGuardEnrolled {
        enrollment: enrollment.clone(),
    });
    settle(|complete| {
        fixture.peer.send_descriptors(
            message.canonical_bytes(),
            [listener.as_fd(), pins.as_fd(), network.as_fd()],
            complete,
        )
    });
    assert!(
        !fixture
            .service
            .step(&mut fixture.session, 2500, None, verify_fixture_signature)
            .unwrap()
    );
    let packet = settle(|complete| fixture.peer.receive(complete));
    assert!(
        matches!(packet.packet, LauncherPacket::Response(ack) if ack.result == ResponseResult::SenderGuardAccepted { enrollment: enrollment.clone() })
    );
    drop(listener);
    drop(pins);
    drop(network);

    let upstream = FakeUpstream::replying(200);
    let mut client = TcpStream::connect(enrollment.address).unwrap();
    let bytes = String::from_utf8(frame())
        .unwrap()
        .replace(HOST, &enrollment.address.to_string());
    client.write_all(bytes.as_bytes()).unwrap();
    client.shutdown(std::net::Shutdown::Write).unwrap();
    thread::scope(|threads| {
        let peer = &fixture.peer;
        let current = &fixture.current;
        let status = threads.spawn(move || lifecycle::answer_one_status_query(peer, current));
        assert!(
            fixture
                .service
                .serve_guarded_provider(
                    &mut fixture.session,
                    &fixture.credentials,
                    &upstream,
                    2500,
                    verify_fixture_signature
                )
                .unwrap()
        );
        status.join().unwrap();
    });
    let mut relayed = String::new();
    client.read_to_string(&mut relayed).unwrap();
    assert!(relayed.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(relayed.contains("event: done"));
    assert_eq!(upstream.calls(), 1);
    assert_eq!(fixture.service.provider_requests_spent("run-1").unwrap(), 1);

    let closing = response(ResponseResult::SenderGuardClosing {
        enrollment: enrollment.clone(),
    });
    settle(|complete| fixture.peer.send(closing.canonical_bytes(), complete));
    fixture
        .service
        .step(&mut fixture.session, 2500, None, verify_fixture_signature)
        .unwrap();
    let packet = settle(|complete| fixture.peer.receive(complete));
    assert!(
        matches!(packet.packet, LauncherPacket::Response(ack) if ack.result == ResponseResult::SenderGuardClosed { enrollment: enrollment.clone() })
    );
    assert_eq!(
        TcpStream::connect(enrollment.address).unwrap_err().kind(),
        std::io::ErrorKind::ConnectionRefused
    );
    assert!(
        !fixture
            .service
            .serve_guarded_provider(
                &mut fixture.session,
                &fixture.credentials,
                &upstream,
                2500,
                verify_fixture_signature
            )
            .unwrap()
    );
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "One held old owner, refused closure, acknowledged retry and forbidden re-enrollment share one causal fixture."
)]
fn reconnect_cannot_ack_guard_closure_while_old_session_owns_listener() {
    let mut fixture = fixture(Some(approval(5)), Some("openai"));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let pins = File::open("/proc/self/ns/mnt").unwrap();
    let network = File::open("/proc/self/ns/net").unwrap();
    let mut enrollment = enrollment(&listener, &pins, &network);
    enrollment.scope.session_id = fixture.session.authorization().session_id.clone();
    enrollment.scope.envelope_revision = fixture.session.authorization().envelope_revision;
    let enrolled = response(ResponseResult::SenderGuardEnrolled {
        enrollment: enrollment.clone(),
    });
    settle(|complete| {
        fixture.peer.send_descriptors(
            enrolled.canonical_bytes(),
            [listener.as_fd(), pins.as_fd(), network.as_fd()],
            complete,
        )
    });
    fixture
        .service
        .step(&mut fixture.session, 2500, None, verify_fixture_signature)
        .unwrap();
    let _accepted = settle(|complete| fixture.peer.receive(complete));
    fixture.peer.close();

    let head = fixture.current.launcher_head.as_ref().unwrap();
    let offer = BrokerReconnect {
        schema: BROKER_RECONNECT_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        request_id: "guard-reconnect".into(),
        session_id: enrollment.scope.session_id.clone(),
        run_id: enrollment.scope.run_id.clone(),
        envelope_revision: enrollment.scope.envelope_revision,
        sequence: head.sequence,
        receipt_digest: head.digest.clone(),
    };
    let connector = SeqpacketConnector::new().unwrap();
    let peer = settle(|complete| {
        connector.connect(
            &fixture.root.path().join("broker.sock"),
            local_pin(),
            complete,
        )
    });
    settle(|complete| peer.send(offer.canonical_bytes(), complete));
    let mut reconnected = fixture
        .service
        .serve_reconnect(2500, verify_fixture_signature)
        .unwrap();
    let _reply = settle(|complete| peer.receive(complete));
    let closing = response(ResponseResult::SenderGuardClosing {
        enrollment: enrollment.clone(),
    });
    settle(|complete| peer.send(closing.canonical_bytes(), complete));
    assert!(matches!(
        fixture
            .service
            .step(&mut reconnected, 2500, None, verify_fixture_signature),
        Err(BrokerError::ProviderUnavailable)
    ));
    drop(reconnected);
    drop(fixture.session);
    let peer = settle(|complete| {
        connector.connect(
            &fixture.root.path().join("broker.sock"),
            local_pin(),
            complete,
        )
    });
    settle(|complete| peer.send(offer.canonical_bytes(), complete));
    let mut reconnected = fixture
        .service
        .serve_reconnect(2500, verify_fixture_signature)
        .unwrap();
    let _reply = settle(|complete| peer.receive(complete));
    settle(|complete| peer.send(closing.canonical_bytes(), complete));
    fixture
        .service
        .step(&mut reconnected, 2500, None, verify_fixture_signature)
        .unwrap();
    let packet = settle(|complete| peer.receive(complete));
    assert!(matches!(
        packet.packet,
        LauncherPacket::Response(ack)
            if ack.result == ResponseResult::SenderGuardClosed { enrollment: enrollment.clone() }
    ));
    drop(reconnected);
    let peer = settle(|complete| {
        connector.connect(
            &fixture.root.path().join("broker.sock"),
            local_pin(),
            complete,
        )
    });
    settle(|complete| peer.send(offer.canonical_bytes(), complete));
    let mut reconnected = fixture
        .service
        .serve_reconnect(2500, verify_fixture_signature)
        .unwrap();
    let _reply = settle(|complete| peer.receive(complete));
    let enrolled = response(ResponseResult::SenderGuardEnrolled { enrollment });
    settle(|complete| {
        peer.send_descriptors(
            enrolled.canonical_bytes(),
            [listener.as_fd(), pins.as_fd(), network.as_fd()],
            complete,
        )
    });
    assert!(matches!(
        fixture
            .service
            .step(&mut reconnected, 2500, None, verify_fixture_signature),
        Err(BrokerError::ProviderUnavailable)
    ));
}

#[test]
#[ignore = "requires scripts/test-sender-guard-handoff.py in disposable KVM"]
#[expect(
    clippy::too_many_lines,
    reason = "Single offline fixture dispatch keeps setup and protocol operations together."
)]
fn production_handoff_broker_worker() {
    use serde_json::{Value, json};
    use std::io::BufRead;
    fn emit(value: &Value) {
        println!("GUARD_FIXTURE {value}");
        std::io::stdout().flush().unwrap();
    }
    assert_eq!(rustix::process::getuid().as_raw(), 4_020_010);
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    let setup: Value = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
    let root = TempDir::new().unwrap();
    let socket = Path::new(setup["socket"].as_str().unwrap());
    let destination: std::net::SocketAddr = setup["destination"].as_str().unwrap().parse().unwrap();
    let authorizations = AuthorizationStore::open(
        &root.path().join("authorizations"),
        IdentityPool {
            uid_start: 4_020_000,
            gid_start: 4_020_000,
            slots: 2,
        },
    )
    .unwrap();
    let requests: Vec<_> = (0..2)
        .map(|index| request(&format!("session-{index}")))
        .collect();
    for request in &requests {
        let mut granted = grant(request);
        granted.commands = None;
        granted.expires_at_ms = 350_000;
        let mut permission = approval(10);
        permission.expires_at_ms = 350_000;
        permission.upstream = format!(
            "https://fixture.invalid:{}/v1/responses",
            destination.port()
        );
        permission.addresses = vec![destination.ip()];
        granted.provider_requests = Some(permission);
        authorizations.authorize(&granted, 1000).unwrap();
    }
    let service = BrokerService::bind(
        socket,
        authorizations,
        ReceiptStore::open(&root.path().join("receipts"), trusted_release()).unwrap(),
        AuditLog::open(&root.path().join("audit")).unwrap(),
        CredentialPin::Identity { uid: 0, gid: 0 },
    )
    .unwrap();
    let credentials = credentials(root.path(), Some("openai"));
    let mut sessions = Vec::new();
    let mut upstreams: Vec<Option<louiselm_skills::broker::GuardedUpstream>> = Vec::new();
    let upstream = FakeUpstream::replying(200);
    emit(&json!({"ready":true,"requests":requests}));
    for line in lines {
        let action: Value = serde_json::from_str(&line.unwrap()).unwrap();
        let index = usize::try_from(action["index"].as_u64().unwrap_or(0)).unwrap();
        match action["op"].as_str().unwrap() {
            "launch" => {
                sessions.push(
                    service
                        .serve_launch(2000, verify_fixture_signature)
                        .unwrap(),
                );
                emit(&json!({"launched":true}));
            }
            "enroll" => {
                let result =
                    service.step(&mut sessions[index], 2500, None, verify_fixture_signature);
                if action["refused"].as_bool().unwrap_or(false) {
                    assert!(matches!(result, Err(BrokerError::InvalidGrant)));
                    emit(&json!({"refused":true}));
                } else {
                    assert!(!result.unwrap());
                    emit(&json!({"enrolled":true}));
                }
            }
            "upstream" => {
                let result = service.receive_guarded_upstream(
                    &mut sessions[index],
                    if action["refused"].as_bool().unwrap_or(false) {
                        "wrong-request"
                    } else {
                        "guard-upstream"
                    },
                    destination,
                    2500,
                );
                if action["refused"].as_bool().unwrap_or(false) {
                    assert!(matches!(result, Err(BrokerError::RequestMismatch)));
                    emit(&json!({"refused":true}));
                    continue;
                }
                let socket = result.unwrap();
                upstreams.push(Some(socket));
                emit(&json!({"taken":upstreams.len()-1}));
            }
            "send" => {
                let result = upstreams[index]
                    .as_mut()
                    .unwrap()
                    .write_all(b"synthetic-request\n");
                emit(&match result {
                    Ok(()) => json!({"sent":true}),
                    Err(error) => json!({"sent":false,"errno":error.raw_os_error()}),
                });
            }
            "serve" => {
                assert!(
                    service
                        .serve_guarded_provider(
                            &mut sessions[index],
                            &credentials,
                            &upstream,
                            2500,
                            verify_fixture_signature
                        )
                        .unwrap()
                );
                emit(
                    &json!({"served":true,"calls":upstream.calls(),"spent":service.provider_requests_spent("run-1").unwrap()}),
                );
            }
            "close" => {
                if let Some(socket) = upstreams.get_mut(index) {
                    *socket = None;
                }
                assert!(
                    !service
                        .step(&mut sessions[index], 2500, None, verify_fixture_signature)
                        .unwrap()
                );
                emit(&json!({"closed":true}));
            }
            "halt" => {
                upstreams.clear();
                sessions.clear();
                service.close();
                emit(&json!({"halted":true}));
                break;
            }
            other => panic!("unknown broker fixture operation {other}"),
        }
    }
}
