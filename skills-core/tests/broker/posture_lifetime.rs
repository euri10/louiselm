//! Receipt ordering and source-owned validity through the canonical status path.

use super::*;
use louiselm_skills::{
    launch_protocol::{BrokerConnection, ChannelState, FreshnessBasis, SessionStatus},
    launch_receipt::ReceiptHead,
    posture::{DimensionName, DimensionState},
};

fn runtime(status: &SessionStatus) -> &louiselm_skills::launch_protocol::DimensionStatus {
    status
        .posture
        .dimensions
        .iter()
        .find(|d| d.dimension == DimensionName::Runtime)
        .unwrap()
}

#[test]
fn terminal_receipt_cannot_be_overridden_by_a_running_status() {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("control.sock");
    let request = request("ended-posture");
    let service = lifecycle::bound_service(root.path(), &socket, &request);
    let peer = thread::spawn(move || {
        let (authorization, channel) = fake_supervisor(&socket, &request, 2000);
        let query = settle(|done| channel.receive(done));
        let LauncherPacket::Request(ProtocolMessage::Status(query)) = query.packet else {
            panic!("expected status request");
        };
        let start = start_receipt(&authorization, &launch_receipt(&authorization));
        let ended = signed(payload(
            &authorization,
            "agent-exit",
            2,
            Some(start.digest().to_string()),
            ReceiptOutcome::Disposal {
                authority: ReceiptAuthority::Cause {
                    cause: ReceiptCause::AgentIdentityLost,
                },
            },
            SessionState::Terminal,
        ));
        settle(|done| channel.send(ended.canonical_bytes(), done));
        assert_eq!(expect_acknowledgement(&channel).sequence, 2);
        let mut stale = lifecycle::status(&authorization);
        stale.broker_head = Some(ReceiptHead {
            sequence: 2,
            digest: ended.digest().to_string(),
        });
        stale.launcher_head = stale.broker_head.clone();
        let response = ProtocolResponse {
            schema: RESPONSE_SCHEMA.into(),
            protocol_version: PROTOCOL_VERSION,
            request_id: query.request_id,
            result: ResponseResult::SupervisorStatus { status: stale },
        };
        settle(|done| channel.send(response.canonical_bytes(), done));
    });
    let mut session = service
        .serve_launch(2000, verify_fixture_signature)
        .unwrap();
    let result = service.session_status(
        &mut session,
        &LifecycleCaller::Agent,
        3000,
        verify_fixture_signature,
    );
    peer.join().unwrap();
    assert!(
        result.is_err(),
        "a matching receipt digest cannot revive its terminal subject"
    );
    assert_eq!(
        service.inspect("ended-posture").unwrap().unwrap().state,
        SessionState::Terminal
    );
}

#[test]
fn reads_apply_source_validity_without_renewing_or_expiring_launch_proof() {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("control.sock");
    let request = request("validity-posture");
    let service = lifecycle::bound_service(root.path(), &socket, &request);
    let peer = thread::spawn(move || {
        let (authorization, channel) = fake_supervisor(&socket, &request, 2000);
        for connection in [
            BrokerConnection::Connected,
            BrokerConnection::Connected,
            BrokerConnection::Grace,
            BrokerConnection::Reconciling,
            BrokerConnection::Disconnected,
            BrokerConnection::Connected,
        ] {
            let mut status = lifecycle::status(&authorization);
            status.broker_connection = connection;
            if connection != BrokerConnection::Connected {
                status.channel_state = ChannelState::Revoked;
            }
            lifecycle::answer_one_status_query(&channel, &status);
        }
    });
    let mut session = service
        .serve_launch(2000, verify_fixture_signature)
        .unwrap();
    let original = service.receipts().stored_bytes("validity-posture").unwrap();
    let audit = service.audit().unwrap();
    let mut statuses = Vec::new();
    // Before the original check, then across launch permission expiry and loss.
    // None of these reads owns a timer or creates a new successful check.
    for now in [0, 90_000, 90_001, 90_002, 90_003, 90_004] {
        statuses.push(
            service
                .session_status(
                    &mut session,
                    &LifecycleCaller::Agent,
                    now,
                    verify_fixture_signature,
                )
                .unwrap(),
        );
    }
    peer.join().unwrap();
    for (index, status) in statuses.iter().enumerate() {
        let current = runtime(status);
        let valid = matches!(index, 1 | 5);
        assert_eq!(
            current.state,
            if valid {
                DimensionState::Verified
            } else {
                DimensionState::Failed
            }
        );
        assert_eq!(
            current.freshness.basis,
            if valid {
                FreshnessBasis::Launch
            } else {
                FreshnessBasis::Invalidated
            }
        );
        assert_eq!(
            current.freshness.last_verified_at_ms,
            runtime(&statuses[1]).freshness.last_verified_at_ms
        );
        assert!(status.allowed_actions.is_empty());
        for dimension in &status.posture.dimensions {
            if dimension.dimension != DimensionName::Runtime {
                assert_eq!(dimension.freshness.basis, FreshnessBasis::Missing);
                assert_eq!(dimension.freshness.last_verified_at_ms, None);
            }
        }
    }
    assert_eq!(service.audit().unwrap(), audit);
    assert_eq!(
        service.receipts().stored_bytes("validity-posture").unwrap(),
        original
    );
}

fn disconnected_launch(root: &Path) -> (BrokerService, LaunchAuthorization, SignedReceipt) {
    let socket = root.join("broker.sock");
    let request = request("lifetime-restart");
    let service = lifecycle::bound_service(root, &socket, &request);
    let peer = thread::spawn(move || fake_supervisor(&socket, &request, 2000));
    let session = service
        .serve_launch(2000, verify_fixture_signature)
        .unwrap();
    let (authorization, channel) = peer.join().unwrap();
    let start = SignedReceipt::parse_canonical(
        &service
            .receipts()
            .stored_bytes(&authorization.session_id)
            .unwrap()[1],
    )
    .unwrap();
    drop((session, channel));
    (service, authorization, start)
}

fn reopen(root: &Path, service: BrokerService) -> BrokerService {
    service.close();
    drop(service);
    let socket = root.join("broker.sock");
    fs::remove_file(&socket).unwrap(); // Only this fixture's temporary socket.
    BrokerService::bind(
        &socket,
        AuthorizationStore::open(&root.join("authorizations"), pool(4)).unwrap(),
        ReceiptStore::open(&root.join("receipts"), trusted_release()).unwrap(),
        AuditLog::open(&root.join("audit")).unwrap(),
        local_pin(),
    )
    .unwrap()
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "One two-case replay fixture keeps each signed outcome and its freshness assertions together."
)]
fn restart_consumes_loss_park_and_terminal_suffix_without_refreshing_launch() {
    for ended in [false, true] {
        let root = TempDir::new().unwrap();
        let (service, authorization, start) = disconnected_launch(root.path());
        let admission = service
            .receipts()
            .stored_bytes(&authorization.session_id)
            .unwrap();
        let checked = service
            .audit()
            .unwrap()
            .into_iter()
            .find(|entry| entry.decision == (AuditDecision::ReceiptStored { sequence: 1 }))
            .unwrap()
            .at_ms;
        let state = if ended {
            SessionState::Terminal
        } else {
            SessionState::Parked
        };
        let outcome = if ended {
            ReceiptOutcome::Disposal {
                authority: ReceiptAuthority::Cause {
                    cause: ReceiptCause::AgentIdentityLost,
                },
            }
        } else {
            ReceiptOutcome::Park {
                authority: ReceiptAuthority::Cause {
                    cause: ReceiptCause::BrokerLost,
                },
            }
        };
        let suffix = signed(payload(
            &authorization,
            "loss-outcome",
            2,
            Some(start.digest().to_string()),
            outcome,
            state,
        ));
        let suffix_bytes = suffix.canonical_bytes();
        let service = reopen(root.path(), service);
        let peer_root = root.path().to_owned();
        let peer = thread::spawn(move || {
            let channel = reconnect::peer(&peer_root, &reconnect::checkpoint(&suffix));
            let _ = settle(|done| channel.receive(done));
            settle(|done| channel.send(suffix.canonical_bytes(), done));
            assert_eq!(expect_acknowledgement(&channel).sequence, 2);
            let mut status = lifecycle::status(&authorization);
            status.state = state;
            status.channel_state = if ended {
                ChannelState::Closed
            } else {
                ChannelState::Revoked
            };
            status.broker_head = Some(ReceiptHead {
                sequence: 2,
                digest: suffix.digest().to_string(),
            });
            status.launcher_head = status.broker_head.clone();
            lifecycle::answer_one_status_query(&channel, &status);
            lifecycle::answer_one_status_query(&channel, &status);
        });
        let mut session = service
            .serve_reconnect(90_000, verify_fixture_signature)
            .unwrap();
        for now in [90_000, 900_000] {
            let status = service
                .session_status(
                    &mut session,
                    &LifecycleCaller::Agent,
                    now,
                    verify_fixture_signature,
                )
                .unwrap();
            assert_eq!(status.state, state, "reconnect/read must not Resume");
            assert!(status.allowed_actions.is_empty());
            let runtime = runtime(&status);
            assert_eq!(
                runtime.state,
                if ended {
                    DimensionState::Failed
                } else {
                    DimensionState::Verified
                }
            );
            assert_eq!(runtime.freshness.last_verified_at_ms, Some(checked));
            assert_eq!(
                runtime.freshness.basis,
                if ended {
                    FreshnessBasis::Invalidated
                } else {
                    FreshnessBasis::Launch
                }
            );
            SessionStatus::parse_canonical(&status.canonical_bytes()).unwrap();
        }
        peer.join().unwrap();
        let stored = service.receipts().stored_bytes("lifetime-restart").unwrap();
        assert_eq!(stored[..2], admission);
        assert_eq!(stored[2], suffix_bytes);
    }
}

#[test]
fn restart_refuses_corrupt_or_unreadable_proof_instead_of_using_cached_success() {
    for unreadable in [false, true] {
        let root = TempDir::new().unwrap();
        let (service, _, start) = disconnected_launch(root.path());
        let proof = root
            .path()
            .join("receipts/sessions/lifetime-restart/00000000000000000001.receipt.json");
        if unreadable {
            fs::remove_file(&proof).unwrap();
            fs::create_dir(&proof).unwrap();
        } else {
            fs::write(&proof, b"corrupt").unwrap();
        }
        let service = reopen(root.path(), service);
        let channel = reconnect::peer(root.path(), &reconnect::checkpoint(&start));
        assert!(
            service
                .serve_reconnect(90_000, verify_fixture_signature)
                .is_err()
        );
        expect_disconnect(&channel);
    }
}
