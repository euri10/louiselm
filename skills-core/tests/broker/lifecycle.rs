//! Broker lifecycle authorization and durable request identity.

use super::*;
use louiselm_skills::{
    broker::{
        BrokerSession,
        lifecycle::{LifecycleCaller, LifecycleStore},
    },
    launch_protocol::{
        BrokerConnection, COMMAND_SCHEMA, ChannelState, CommandMessage, CommandOperation,
        LIFECYCLE_REQUEST_SCHEMA, LifecycleAction, LifecycleRequest, PendingAction,
        PendingOperation, PendingPhase, PostureSummary, SUPERVISOR_STATUS_SCHEMA, SessionStatus,
        SupervisorStatus,
    },
    launch_receipt::ReceiptHead,
};

#[path = "agent_status_lifecycle.rs"]
mod status_continuity;

pub(super) fn status(authorization: &LaunchAuthorization) -> SupervisorStatus {
    let start = start_receipt(authorization, &launch_receipt(authorization));
    let head = ReceiptHead {
        sequence: 1,
        digest: start.digest().to_string(),
    };
    SupervisorStatus {
        schema: SUPERVISOR_STATUS_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        session_id: authorization.session_id.clone(),
        run_id: authorization.run_id.clone(),
        state: SessionState::Running,
        broker_connection: BrokerConnection::Connected,
        envelope_revision: authorization.envelope_revision,
        channel_state: ChannelState::Enabled,
        launcher_head: Some(head.clone()),
        broker_head: Some(head),
        pending_receipt_count: 0,
        pending_operation: None,
        process_exit: None,
        last_failure: None,
    }
}

pub(super) fn park(authorization: &LaunchAuthorization) -> LifecycleRequest {
    LifecycleRequest {
        schema: LIFECYCLE_REQUEST_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        request_id: "park-1".into(),
        session_id: authorization.session_id.clone(),
        run_id: authorization.run_id.clone(),
        authorization_id: "operator-park-1".into(),
        action: LifecycleAction::Park,
        expected_state: SessionState::Running,
        expected_receipt_sequence: Some(1),
        envelope_revision: authorization.envelope_revision,
    }
}

#[test]
fn broker_authorizes_only_the_operator_or_current_descendant_scope() {
    let root = TempDir::new().unwrap();
    let launch = consumed_authorization(root.path(), &request("session-1"));
    let store = LifecycleStore::open(&root.path().join("lifecycle")).unwrap();
    let current = status(&launch);
    let mutation = park(&launch);
    for caller in [
        LifecycleCaller::Agent,
        LifecycleCaller::Operator {
            uid: CONTROLLER_UID + 1,
        },
    ] {
        let error = store
            .prepare(&launch, &current, &caller, &mutation, 2000, &[])
            .unwrap_err();
        assert!(
            matches!(error, BrokerError::Policy(error) if error.code == ErrorCode::InvalidRequest)
        );
    }
    let scope = LifecycleCaller::Coordinator {
        session_id: "coordinator".into(),
        run_id: launch.run_id.clone(),
        descendants: vec![launch.session_id.clone()],
        envelope_revision: launch.envelope_revision,
        expires_at_ms: 3000,
    };
    assert!(
        store
            .prepare(&launch, &current, &scope, &mutation, 3000, &[])
            .is_err()
    );
    let mut foreign = mutation.clone();
    foreign.session_id = "sibling".into();
    assert!(
        store
            .prepare(&launch, &current, &scope, &foreign, 2000, &[])
            .is_err()
    );
    assert!(
        store
            .prepare(&launch, &current, &scope, &mutation, 2000, &[])
            .is_ok()
    );
}

#[test]
fn coordinators_cannot_resume_or_control_a_foreign_revision_or_run() {
    let root = TempDir::new().unwrap();
    let launch = consumed_authorization(root.path(), &request("session-1"));
    let store = LifecycleStore::open(&root.path().join("lifecycle")).unwrap();
    let mut current = status(&launch);
    current.state = SessionState::Parked;
    current.channel_state = ChannelState::Revoked;
    let mut mutation = park(&launch);
    mutation.action = LifecycleAction::Resume;
    mutation.expected_state = SessionState::Parked;
    let scope = LifecycleCaller::Coordinator {
        session_id: "coordinator".into(),
        run_id: launch.run_id.clone(),
        descendants: vec![launch.session_id.clone()],
        envelope_revision: launch.envelope_revision,
        expires_at_ms: 3000,
    };
    assert!(
        store
            .prepare(&launch, &current, &scope, &mutation, 2000, &[])
            .is_err()
    );
    mutation.action = LifecycleAction::Disposal;
    mutation.envelope_revision += 1;
    assert!(
        store
            .prepare(&launch, &current, &scope, &mutation, 2000, &[])
            .is_err()
    );
    mutation.envelope_revision = launch.envelope_revision;
    mutation.run_id = "another-run".into();
    assert!(
        store
            .prepare(&launch, &current, &scope, &mutation, 2000, &[])
            .is_err()
    );
}

#[test]
fn quarantine_persists_and_refuses_resume_even_for_the_operator() {
    let root = TempDir::new().unwrap();
    let launch = consumed_authorization(root.path(), &request("session-1"));
    let path = root.path().join("lifecycle");
    let store = LifecycleStore::open(&path).unwrap();
    store.quarantine(&launch.session_id).unwrap();
    drop(store);
    let store = LifecycleStore::open(&path).unwrap();
    let mut current = status(&launch);
    current.state = SessionState::Parked;
    current.channel_state = ChannelState::Revoked;
    let mut mutation = park(&launch);
    mutation.action = LifecycleAction::Resume;
    mutation.expected_state = SessionState::Parked;
    assert!(
        store
            .prepare(
                &launch,
                &current,
                &LifecycleCaller::Operator {
                    uid: CONTROLLER_UID
                },
                &mutation,
                2000,
                &[]
            )
            .is_err()
    );
}

#[test]
fn accepted_request_survives_restart_and_replays_the_exact_durable_receipt() {
    let root = TempDir::new().unwrap();
    let launch = consumed_authorization(root.path(), &request("session-1"));
    let path = root.path().join("lifecycle");
    let store = LifecycleStore::open(&path).unwrap();
    let current = status(&launch);
    let mutation = park(&launch);
    let caller = LifecycleCaller::Operator {
        uid: CONTROLLER_UID,
    };
    let accepted = store
        .prepare(&launch, &current, &caller, &mutation, 2000, &[])
        .unwrap();
    let intent = accepted.execute_intent().unwrap();
    let receipt = signed(
        intent
            .receipt_payload(
                &trusted_release().release_id,
                &trusted_release().signing_key_id,
            )
            .unwrap(),
    );
    drop(store);
    let store = LifecycleStore::open(&path).unwrap();
    let mut after = current.clone();
    after.state = SessionState::Parked;
    after.channel_state = ChannelState::Revoked;
    after.broker_head = Some(ReceiptHead {
        sequence: 2,
        digest: receipt.digest().to_string(),
    });
    after.launcher_head = after.broker_head.clone();
    let replay = store
        .prepare(
            &launch,
            &after,
            &caller,
            &mutation,
            4000,
            std::slice::from_ref(&receipt),
        )
        .unwrap();
    assert_eq!(
        replay.replayed_receipt().unwrap().canonical_bytes(),
        receipt.canonical_bytes()
    );
    let mut conflict = mutation.clone();
    conflict.action = LifecycleAction::Disposal;
    assert!(
        matches!(store.prepare(&launch, &after, &caller, &conflict, 4000, &[receipt]),
        Err(BrokerError::Policy(error)) if error.code == ErrorCode::RequestIdConflict)
    );
}

#[test]
fn distinct_requests_cannot_spend_the_same_observed_state() {
    let root = TempDir::new().unwrap();
    let launch = consumed_authorization(root.path(), &request("session-1"));
    let store = LifecycleStore::open(&root.path().join("lifecycle")).unwrap();
    let current = status(&launch);
    let first = park(&launch);
    let mut second = first.clone();
    second.request_id = "second-park".into();
    let caller = LifecycleCaller::Operator {
        uid: CONTROLLER_UID,
    };
    store
        .prepare(&launch, &current, &caller, &first, 2000, &[])
        .unwrap();
    assert!(
        matches!(store.prepare(&launch, &current, &caller, &second, 2000, &[]),
        Err(BrokerError::Policy(error)) if error.code == ErrorCode::OperationPending)
    );
}

#[test]
fn a_durable_refusal_replays_and_releases_the_pending_request() {
    let root = TempDir::new().unwrap();
    let launch = consumed_authorization(root.path(), &request("session-1"));
    let path = root.path().join("lifecycle");
    let store = LifecycleStore::open(&path).unwrap();
    let current = status(&launch);
    let first = park(&launch);
    let caller = LifecycleCaller::Operator {
        uid: CONTROLLER_UID,
    };
    store
        .prepare(&launch, &current, &caller, &first, 2000, &[])
        .unwrap();
    let failure = ProtocolError::new(
        ErrorCode::LifecycleMechanicUnavailable,
        Some(SessionState::Running),
        Some(1),
    );
    store.record_failure(&first, &failure).unwrap();
    drop(store);
    let store = LifecycleStore::open(&path).unwrap();
    assert!(
        matches!(store.prepare(&launch, &current, &caller, &first, 2000, &[]),
        Err(BrokerError::Policy(error)) if error == failure)
    );
    let mut next = first;
    next.request_id = "retry-with-new-id".into();
    store
        .prepare(&launch, &current, &caller, &next, 2000, &[])
        .unwrap();
}

#[test]
fn concurrent_lifecycle_reservations_have_exactly_one_winner() {
    let root = TempDir::new().unwrap();
    let launch = consumed_authorization(root.path(), &request("session-1"));
    let store = Arc::new(LifecycleStore::open(&root.path().join("lifecycle")).unwrap());
    let current = status(&launch);
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let attempts = (0..2)
        .map(|index| {
            let store = Arc::clone(&store);
            let barrier = Arc::clone(&barrier);
            let current = current.clone();
            let launch = launch.clone();
            thread::spawn(move || {
                let mut mutation = park(&launch);
                mutation.request_id = format!("contender-{index}");
                barrier.wait();
                store
                    .prepare(
                        &launch,
                        &current,
                        &LifecycleCaller::Operator {
                            uid: CONTROLLER_UID,
                        },
                        &mutation,
                        2000,
                        &[],
                    )
                    .is_ok()
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(
        attempts
            .into_iter()
            .filter_map(|attempt| attempt.join().unwrap().then_some(()))
            .count(),
        1
    );
}

#[test]
fn signed_lifecycle_outcomes_must_answer_an_existing_exact_intent() {
    let root = TempDir::new().unwrap();
    let launch = consumed_authorization(root.path(), &request("session-1"));
    let store = LifecycleStore::open(&root.path().join("lifecycle")).unwrap();
    let current = status(&launch);
    let mutation = park(&launch);
    let disposition =
        louiselm_skills::launch_protocol::evaluate_request(&current, None, &mutation).unwrap();
    let receipt = signed(
        disposition
            .execute_intent()
            .unwrap()
            .receipt_payload(
                &trusted_release().release_id,
                &trusted_release().signing_key_id,
            )
            .unwrap(),
    );
    assert!(store.check_receipt(&receipt).is_err());
    store
        .prepare(
            &launch,
            &current,
            &LifecycleCaller::Operator {
                uid: CONTROLLER_UID,
            },
            &mutation,
            2000,
            &[],
        )
        .unwrap();
    store.check_receipt(&receipt).unwrap();
    let mut forged = receipt.payload.clone();
    if let ReceiptOutcome::Park {
        authority: ReceiptAuthority::Authorized(authorization),
    } = &mut forged.outcome
    {
        authorization.authorization_id = "not-approved".into();
    }
    assert!(store.check_receipt(&signed(forged)).is_err());
}

#[test]
fn operator_park_stores_the_signed_outcome_before_acknowledgement() {
    park_exchange(false);
}

#[test]
fn quarantine_revokes_commands_before_requesting_park() {
    park_exchange(true);
}

fn park_exchange(quarantine: bool) {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("broker.sock");
    let request = request("session-1");
    let authorizations =
        AuthorizationStore::open(&root.path().join("authorizations"), pool(4)).unwrap();
    authorizations.authorize(&grant(&request), 1000).unwrap();
    let service = BrokerService::bind(
        &socket,
        authorizations,
        ReceiptStore::open(&root.path().join("receipts"), trusted_release()).unwrap(),
        AuditLog::open(&root.path().join("audit")).unwrap(),
        local_pin(),
    )
    .unwrap();
    let peer = thread::spawn(move || {
        let (authorization, channel) = fake_supervisor(&socket, &request, 2000);
        drive_park_peer(&authorization, &channel, quarantine)
    });
    let mut session = service
        .serve_launch(2000, verify_fixture_signature)
        .unwrap();
    let mutation = park(session.authorization());
    let caller = LifecycleCaller::Operator {
        uid: CONTROLLER_UID,
    };
    let receipt = if quarantine {
        service.quarantine(
            &mut session,
            &caller,
            &mutation,
            2000,
            verify_fixture_signature,
        )
    } else {
        service.request_lifecycle(
            &mut session,
            &caller,
            &mutation,
            2000,
            verify_fixture_signature,
        )
    }
    .unwrap();
    if quarantine {
        assert!(session.command_revocation_complete());
    }
    assert_eq!(receipt, peer.join().unwrap());
    assert_eq!(
        service.receipts().stored_bytes("session-1").unwrap().last(),
        Some(&receipt.canonical_bytes())
    );
}

pub(super) fn drive_park_peer(
    authorization: &LaunchAuthorization,
    channel: &SeqpacketChannel,
    quarantine: bool,
) -> SignedReceipt {
    let mut current = status(authorization);
    if quarantine {
        let packet = settle(|complete| channel.receive(complete));
        let LauncherPacket::Request(ProtocolMessage::Command(mut message)) = packet.packet else {
            panic!("command revocation before Park")
        };
        assert!(matches!(
            message.operation,
            louiselm_skills::launch_protocol::CommandOperation::Revoke
        ));
        message.operation =
            louiselm_skills::launch_protocol::CommandOperation::Revoked { enforced: true };
        settle(|complete| channel.send(message.canonical_bytes(), complete));
        current.channel_state = ChannelState::Revoked;
    }
    drive_lifecycle_peer(channel, &current)
}

fn drive_lifecycle_peer(channel: &SeqpacketChannel, current: &SupervisorStatus) -> SignedReceipt {
    drive_lifecycle_peer_before_reply(channel, current, || {})
}

fn drive_lifecycle_peer_before_reply(
    channel: &SeqpacketChannel,
    current: &SupervisorStatus,
    before_reply: impl FnOnce(),
) -> SignedReceipt {
    answer_one_status_query(channel, current);
    let packet = settle(|complete| channel.receive(complete));
    let LauncherPacket::Request(ProtocolMessage::Lifecycle(mutation)) = packet.packet else {
        panic!("lifecycle request")
    };
    let disposition =
        louiselm_skills::launch_protocol::evaluate_request(current, None, &mutation).unwrap();
    let receipt = signed(
        disposition
            .execute_intent()
            .unwrap()
            .receipt_payload(
                &trusted_release().release_id,
                &trusted_release().signing_key_id,
            )
            .unwrap(),
    );
    settle(|complete| channel.send(receipt.canonical_bytes(), complete));
    assert_eq!(
        expect_acknowledgement(channel).receipt_digest,
        receipt.digest().to_string()
    );
    let response = ProtocolResponse {
        schema: RESPONSE_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        request_id: mutation.request_id,
        result: ResponseResult::Receipt {
            receipt: receipt.clone(),
        },
    };
    before_reply();
    settle(|complete| channel.send(response.canonical_bytes(), complete));
    receipt
}

#[test]
fn late_lifecycle_reply_cannot_outlive_its_key_authority() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let root = TempDir::new().unwrap();
    let socket = root.path().join("control.sock");
    let request = request("session-1");
    let service = bound_service(root.path(), &socket, &request);
    let trusted = Arc::new(AtomicBool::new(true));
    let signer_trust = Arc::clone(&trusted);
    let peer = thread::spawn(move || {
        let (authorization, channel) = fake_supervisor(&socket, &request, 2000);
        drive_lifecycle_peer_before_reply(&channel, &status(&authorization), || {
            signer_trust.store(false, Ordering::SeqCst);
        })
    });
    let mut session = service
        .serve_launch(2000, verify_fixture_signature)
        .unwrap();
    let mutation = park(session.authorization());
    let result = service.request_lifecycle(
        &mut session,
        &operator(),
        &mutation,
        2000,
        |key, bytes, signature| {
            trusted.load(Ordering::SeqCst) && verify_fixture_signature(key, bytes, signature)
        },
    );
    let receipt = peer.join().unwrap();
    assert!(
        matches!(result, Err(BrokerError::Policy(error)) if error.code == ErrorCode::ReceiptChainInvalid)
    );
    assert!(session.channel().is_closed());
    assert_eq!(
        service
            .receipts()
            .stored_bytes("session-1")
            .unwrap()
            .last()
            .unwrap(),
        &receipt.canonical_bytes()
    );
}

#[test]
fn allowed_actions_follow_caller_scope_session_state_and_quarantine() {
    let root = TempDir::new().unwrap();
    let launch = consumed_authorization(root.path(), &request("session-1"));
    let operator = LifecycleCaller::Operator {
        uid: CONTROLLER_UID,
    };
    let coordinator = LifecycleCaller::Coordinator {
        session_id: "coordinator".into(),
        run_id: launch.run_id.clone(),
        descendants: vec![launch.session_id.clone()],
        envelope_revision: launch.envelope_revision,
        expires_at_ms: 3_000,
    };

    // Mechanically valid transitions, narrowed by who is asking.
    assert_eq!(
        operator.allowed_actions(&launch, SessionState::Running, false, 2_000),
        vec![
            LifecycleAction::Park,
            LifecycleAction::Interrupt,
            LifecycleAction::Disposal,
        ],
    );
    assert_eq!(
        operator.allowed_actions(&launch, SessionState::Parked, false, 2_000),
        vec![
            LifecycleAction::Resume,
            LifecycleAction::Interrupt,
            LifecycleAction::Disposal,
        ],
    );

    // Only operators Resume, so a coordinator never sees it offered.
    assert_eq!(
        coordinator.allowed_actions(&launch, SessionState::Parked, false, 2_000),
        vec![LifecycleAction::Interrupt, LifecycleAction::Disposal],
    );

    // A durable quarantine marker withdraws Resume without touching the rest.
    assert_eq!(
        operator.allowed_actions(&launch, SessionState::Parked, true, 2_000),
        vec![LifecycleAction::Interrupt, LifecycleAction::Disposal],
    );

    // Terminal state offers nothing to anyone.
    assert!(
        operator
            .allowed_actions(&launch, SessionState::Terminal, false, 2_000)
            .is_empty()
    );

    // An Agent capability channel has no lifecycle authority at any state.
    for state in [
        SessionState::Running,
        SessionState::Parked,
        SessionState::Terminal,
    ] {
        assert!(
            LifecycleCaller::Agent
                .allowed_actions(&launch, state, false, 2_000)
                .is_empty()
        );
    }

    // Expired scope, foreign Run and a non-descendant target all offer nothing.
    assert!(
        coordinator
            .allowed_actions(&launch, SessionState::Running, false, 3_000)
            .is_empty()
    );
    let foreign = LifecycleCaller::Coordinator {
        session_id: "coordinator".into(),
        run_id: launch.run_id.clone(),
        descendants: vec!["sibling".into()],
        envelope_revision: launch.envelope_revision,
        expires_at_ms: 3_000,
    };
    assert!(
        foreign
            .allowed_actions(&launch, SessionState::Running, false, 2_000)
            .is_empty()
    );
    assert!(
        LifecycleCaller::Operator {
            uid: CONTROLLER_UID + 1,
        }
        .allowed_actions(&launch, SessionState::Running, false, 2_000)
        .is_empty()
    );
}

pub(super) fn bound_service(root: &Path, socket: &Path, request: &LaunchRequest) -> BrokerService {
    let authorizations = AuthorizationStore::open(&root.join("authorizations"), pool(4)).unwrap();
    authorizations.authorize(&grant(request), 1000).unwrap();
    BrokerService::bind(
        socket,
        authorizations,
        ReceiptStore::open(&root.join("receipts"), trusted_release()).unwrap(),
        AuditLog::open(&root.join("audit")).unwrap(),
        local_pin(),
    )
    .unwrap()
}

pub(super) fn answer_one_status_query(channel: &SeqpacketChannel, current: &SupervisorStatus) {
    let packet = settle(|complete| channel.receive(complete));
    let LauncherPacket::Request(ProtocolMessage::Status(query)) = packet.packet else {
        panic!("status query")
    };
    let response = ProtocolResponse {
        schema: RESPONSE_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        request_id: query.request_id,
        result: ResponseResult::SupervisorStatus {
            status: current.clone(),
        },
    };
    settle(|complete| channel.send(response.canonical_bytes(), complete));
}

fn agent_status_query(authorization: &LaunchAuthorization, session_id: &str) -> Vec<u8> {
    CommandMessage {
        schema: COMMAND_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        request_id: "agent-status-1".into(),
        session_id: session_id.to_owned(),
        run_id: authorization.run_id.clone(),
        envelope_revision: authorization.envelope_revision,
        operation: CommandOperation::StatusRequest {},
    }
    .canonical_bytes()
}

fn expect_response(channel: &SeqpacketChannel) -> CommandMessage {
    let packet = settle(|complete| channel.receive(complete));
    let LauncherPacket::Request(ProtocolMessage::Command(response)) = packet.packet else {
        panic!("broker answers the capability channel with a response")
    };
    response
}

fn operator() -> LifecycleCaller {
    LifecycleCaller::Operator {
        uid: CONTROLLER_UID,
    }
}

#[test]
fn session_status_composes_supervisor_mechanics_with_broker_posture_and_caller_scope() {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("control.sock");
    let request = request("session-1");
    let service = bound_service(root.path(), &socket, &request);
    let peer = thread::spawn(move || {
        let (authorization, channel) = fake_supervisor(&socket, &request, 2000);
        let running = status(&authorization);
        answer_one_status_query(&channel, &running);
        answer_one_status_query(&channel, &running);
        let mut pending = running;
        pending.pending_operation = Some(PendingOperation {
            request_id: "park-7".into(),
            action: PendingAction::Park,
            phase: PendingPhase::Applying,
        });
        answer_one_status_query(&channel, &pending);
    });
    let mut session = service
        .serve_launch(2000, verify_fixture_signature)
        .unwrap();
    let read = |session: &mut BrokerSession, caller: &LifecycleCaller| {
        service
            .session_status(session, caller, 2000, verify_fixture_signature)
            .unwrap()
    };

    // Mechanical facts come from the supervisor; posture and actions from the broker.
    let status = read(&mut session, &operator());
    assert_eq!(status.session_id, "session-1");
    assert_eq!(status.state, SessionState::Running);
    assert_eq!(status.posture.state, PostureSummary::Unverified);
    assert_eq!(
        status.allowed_actions,
        vec![
            LifecycleAction::Park,
            LifecycleAction::Interrupt,
            LifecycleAction::Disposal,
        ],
    );
    assert_eq!(
        status.broker_head,
        service.receipts().head("session-1").unwrap()
    );
    assert_eq!(
        SessionStatus::parse_canonical(&status.canonical_bytes()).unwrap(),
        status,
    );

    // The same mechanical state offers an Agent channel nothing at all.
    let agent = read(&mut session, &LifecycleCaller::Agent);
    assert!(agent.allowed_actions.is_empty());
    assert_eq!(agent.state, status.state);

    // A serialized operation in flight withdraws every advertised action.
    let pending = read(&mut session, &operator());
    assert!(pending.pending_operation.is_some());
    assert!(pending.allowed_actions.is_empty());
    peer.join().unwrap();
}

#[test]
fn an_agent_reads_only_its_own_session_and_never_learns_of_another() {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("control.sock");
    let request = request("session-1");
    let service = bound_service(root.path(), &socket, &request);
    let peer = thread::spawn(move || {
        let (authorization, channel) = fake_supervisor(&socket, &request, 2000);
        let current = status(&authorization);

        // The Agent asks about its own Session; the broker asks the supervisor.
        settle(|complete| channel.send(agent_status_query(&authorization, "session-1"), complete));
        answer_one_status_query(&channel, &current);
        let own = expect_response(&channel);

        // The same Agent naming a different Session gets no answer at all.
        settle(|complete| {
            channel.send(
                agent_status_query(&authorization, "other-session"),
                complete,
            )
        });
        (own, expect_response(&channel))
    });
    let mut session = service
        .serve_launch(2000, verify_fixture_signature)
        .unwrap();
    let audit_before = service.audit().unwrap().len();
    let head_before = service.receipts().head("session-1").unwrap();

    service
        .serve_agent_status(&mut session, 2000, verify_fixture_signature)
        .unwrap();
    assert!(
        service
            .serve_agent_status(&mut session, 2000, verify_fixture_signature,)
            .is_err()
    );
    // Reading status grants nothing: no durable authority moved either way.
    assert_eq!(service.audit().unwrap().len(), audit_before);
    assert_eq!(service.receipts().head("session-1").unwrap(), head_before);

    let (own, foreign) = peer.join().unwrap();
    assert_eq!(own.request_id, "agent-status-1");
    let CommandOperation::StatusResult { status } = own.operation else {
        panic!("an Agent reading its own Session gets canonical Session status")
    };
    assert_eq!(status.session_id, "session-1");
    assert_eq!(status.state, SessionState::Running);
    // An Agent capability channel has no lifecycle authority, at any state.
    assert!(status.allowed_actions.is_empty());
    assert_eq!(
        SessionStatus::parse_canonical(&status.canonical_bytes()).unwrap(),
        *status,
    );

    // The refusal discloses nothing about the Session that was asked for,
    // including whether it exists at all.
    assert_eq!(foreign.request_id, "agent-status-1");
    let refusal_json = String::from_utf8(foreign.canonical_bytes()).unwrap();
    let CommandOperation::StatusRefused { error } = foreign.operation else {
        panic!("a foreign subject is refused, never answered")
    };
    assert_eq!(error, ErrorCode::SubjectMismatch);
    assert!(!refusal_json.contains("other-session"));
    let own_json = String::from_utf8(status.canonical_bytes()).unwrap();
    for forbidden in [
        "assigned_uid",
        "assigned_gid",
        "pid",
        "slot",
        "occupied",
        "path",
    ] {
        assert!(
            !refusal_json.contains(forbidden),
            "refusal leaked {forbidden}"
        );
        assert!(
            !own_json.contains(forbidden),
            "self status leaked {forbidden}"
        );
    }
}

#[test]
fn an_agent_and_its_operator_read_one_session_differing_only_by_scope() {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("control.sock");
    let request = request("session-1");
    let service = bound_service(root.path(), &socket, &request);
    let peer = thread::spawn(move || {
        let (authorization, channel) = fake_supervisor(&socket, &request, 2000);
        let current = status(&authorization);
        settle(|complete| channel.send(agent_status_query(&authorization, "session-1"), complete));
        answer_one_status_query(&channel, &current);
        let own = expect_response(&channel);
        answer_one_status_query(&channel, &current);
        own
    });
    let mut session = service
        .serve_launch(2000, verify_fixture_signature)
        .unwrap();
    // Both observations follow the actual launch-proof time, including any
    // elapsed handshake time. Reading at the launch's initial clock can instead
    // ask one caller about evidence that still lies in its future.
    let observed_at_ms = service
        .audit()
        .unwrap()
        .iter()
        .map(|entry| entry.at_ms)
        .max()
        .unwrap();
    assert!(
        !service
            .step(&mut session, observed_at_ms, verify_fixture_signature)
            .unwrap()
    );
    let operator = service
        .session_status(
            &mut session,
            &operator(),
            observed_at_ms,
            verify_fixture_signature,
        )
        .unwrap();
    let reply = peer.join().unwrap();
    let CommandOperation::StatusResult { status: agent } = reply.operation else {
        panic!("normal worker dispatch answers Agent status")
    };

    assert!(agent.allowed_actions.is_empty());
    assert_eq!(
        operator.allowed_actions,
        vec![
            LifecycleAction::Park,
            LifecycleAction::Interrupt,
            LifecycleAction::Disposal,
        ],
    );
    // Identical Session, identical answer, except for what scope decides.
    assert_eq!(
        SessionStatus {
            allowed_actions: agent.allowed_actions.clone(),
            ..operator
        },
        *agent,
    );
}

#[test]
fn agent_status_refusals_do_not_disclose_subjects_or_close_the_worker() {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("control.sock");
    let request = request("session-1");
    let service = bound_service(root.path(), &socket, &request);
    // The first foreign subject exists in this broker, the second never did.
    AuthorizationStore::open(&root.path().join("authorizations"), pool(4))
        .unwrap()
        .authorize(&grant(&super::request("sibling")), 1000)
        .unwrap();
    let peer = thread::spawn(move || {
        let (authorization, channel) = fake_supervisor(&socket, &request, 2000);
        let mut replies = Vec::new();
        for subject in ["sibling", "absent"] {
            settle(|complete| channel.send(agent_status_query(&authorization, subject), complete));
            replies.push(expect_response(&channel));
        }
        // Foreign Run and stale revision also leave the worker usable.
        for field in ["run_id", "envelope_revision"] {
            let mut query: serde_json::Value =
                serde_json::from_slice(&agent_status_query(&authorization, "session-1")).unwrap();
            query[field] = if field == "run_id" {
                serde_json::json!("foreign-run")
            } else {
                serde_json::json!(0)
            };
            settle(|complete| channel.send(serde_json::to_vec(&query).unwrap(), complete));
            replies.push(expect_response(&channel));
        }
        settle(|complete| channel.send(agent_status_query(&authorization, "session-1"), complete));
        answer_one_status_query(&channel, &status(&authorization));
        replies.push(expect_response(&channel));
        replies
    });
    let mut session = service
        .serve_launch(2000, verify_fixture_signature)
        .unwrap();
    let audit = service.audit().unwrap();
    let head = service.receipts().head("session-1").unwrap();
    for _ in 0..5 {
        assert!(
            !service
                .step(&mut session, 2000, verify_fixture_signature)
                .unwrap()
        );
    }
    let replies = peer.join().unwrap();
    assert_eq!(replies[0].canonical_bytes(), replies[1].canonical_bytes());
    assert_eq!(replies[0].canonical_bytes(), replies[2].canonical_bytes());
    assert!(matches!(
        replies[0].operation,
        CommandOperation::StatusRefused {
            error: ErrorCode::SubjectMismatch
        }
    ));
    assert!(matches!(
        replies[3].operation,
        CommandOperation::StatusRefused {
            error: ErrorCode::InvalidRequest
        }
    ));
    assert!(matches!(
        replies[4].operation,
        CommandOperation::StatusResult { .. }
    ));
    assert_eq!(service.audit().unwrap(), audit);
    assert_eq!(service.receipts().head("session-1").unwrap(), head);
}

#[test]
fn nested_agent_status_is_retryable_without_competing_for_the_supervisor_reply() {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("control.sock");
    let request = request("session-1");
    let service = bound_service(root.path(), &socket, &request);
    let peer = thread::spawn(move || {
        let (authorization, channel) = fake_supervisor(&socket, &request, 2000);
        settle(|complete| channel.send(agent_status_query(&authorization, "session-1"), complete));
        answer_one_status_query(&channel, &status(&authorization));
        expect_response(&channel)
    });
    let mut session = service
        .serve_launch(2000, verify_fixture_signature)
        .unwrap();
    let status = service
        .session_status(&mut session, &operator(), 2000, verify_fixture_signature)
        .unwrap();
    assert_eq!(status.state, SessionState::Running);
    assert!(matches!(
        peer.join().unwrap().operation,
        CommandOperation::StatusRefused {
            error: ErrorCode::OperationPending
        }
    ));
}

#[test]
fn status_after_a_park_reports_the_state_the_transition_actually_left_behind() {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("control.sock");
    let request = request("session-1");
    let service = bound_service(root.path(), &socket, &request);
    let peer = thread::spawn(move || {
        let (authorization, channel) = fake_supervisor(&socket, &request, 2000);
        let receipt = drive_park_peer(&authorization, &channel, false);
        // Mechanics moved; the supervisor now reports the head Park actually wrote.
        let head = ReceiptHead {
            sequence: receipt.payload.sequence,
            digest: receipt.digest().to_string(),
        };
        let parked = SupervisorStatus {
            state: SessionState::Parked,
            // A Park disables the capability channel; the shape rules require it.
            channel_state: ChannelState::Disabled,
            launcher_head: Some(head.clone()),
            broker_head: Some(head),
            ..status(&authorization)
        };
        answer_one_status_query(&channel, &parked);
        answer_one_status_query(&channel, &parked);
    });
    let mut session = service
        .serve_launch(2000, verify_fixture_signature)
        .unwrap();
    let mutation = park(session.authorization());
    let receipt = service
        .request_lifecycle(
            &mut session,
            &operator(),
            &mutation,
            2000,
            verify_fixture_signature,
        )
        .unwrap();
    assert_eq!(receipt.payload.resulting_state, SessionState::Parked);

    let parked = service
        .session_status(&mut session, &operator(), 2000, verify_fixture_signature)
        .unwrap();
    assert_eq!(parked.state, SessionState::Parked);
    // Resume becomes offerable only now, and only to the operator.
    assert_eq!(
        parked.allowed_actions,
        vec![
            LifecycleAction::Resume,
            LifecycleAction::Interrupt,
            LifecycleAction::Disposal,
        ],
    );
    assert_eq!(
        parked.broker_head,
        service.receipts().head("session-1").unwrap()
    );

    let scope = LifecycleCaller::Coordinator {
        session_id: "coordinator".into(),
        run_id: session.authorization().run_id.clone(),
        descendants: vec!["session-1".into()],
        envelope_revision: session.authorization().envelope_revision,
        expires_at_ms: 3_000,
    };
    let coordinator = service
        .session_status(&mut session, &scope, 2000, verify_fixture_signature)
        .unwrap();
    assert_eq!(
        coordinator.allowed_actions,
        vec![LifecycleAction::Interrupt, LifecycleAction::Disposal],
    );
    peer.join().unwrap();
}
