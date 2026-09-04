//! The closed broker/supervisor lifecycle protocol.

use louiselm_skills::{
    canonical::Digest,
    launch::{LaunchRequest, REQUEST_SCHEMA},
    launch_protocol::{
        self, BrokerConnection, ChannelState, CompletedRequest, ErrorCode,
        LAUNCH_AUTHORIZATION_SCHEMA, LIFECYCLE_REQUEST_SCHEMA, LaunchAuthorization,
        LifecycleAction, LifecycleRequest, MAX_IDENTIFIER_BYTES, MAX_PROTOCOL_MESSAGE_BYTES,
        NextAction, PROTOCOL_VERSION, PendingAction, PendingOperation, PendingPhase,
        PostureSummary, ProtocolError, ProtocolMessage, ProtocolResponse, RECEIPT_ACK_SCHEMA,
        RESPONSE_SCHEMA, ReceiptAcknowledgement, RequestDisposition, ResponseResult,
        SESSION_STATUS_SCHEMA, STATUS_REQUEST_SCHEMA, SUPERVISOR_STATUS_SCHEMA, SessionStatus,
        StatusRequest, SupervisorStatus,
    },
    launch_receipt::{
        Authorization, LaunchEvidence, RECEIPT_SCHEMA, ReceiptAuthority, ReceiptHead,
        ReceiptOutcome, ReceiptPayload, SIGNED_RECEIPT_SCHEMA, SessionState, SignedReceipt,
    },
};

fn digest(value: &[u8]) -> String {
    Digest::of(value).to_string()
}

fn launch_request() -> LaunchRequest {
    LaunchRequest {
        schema: REQUEST_SCHEMA.to_owned(),
        protocol_version: PROTOCOL_VERSION,
        request_id: "launch-request-1".to_owned(),
        authorization_id: "launch-authorization-1".to_owned(),
        session_id: "session-1".to_owned(),
        run_id: "run-1".to_owned(),
        agent_id: "agent-1".to_owned(),
        envelope_id: "envelope-1".to_owned(),
        envelope_revision: 7,
        skill_generation_id: digest(b"generation"),
        session_input_manifest_id: digest(b"input"),
    }
}

fn launch_authorization(request: &LaunchRequest) -> LaunchAuthorization {
    LaunchAuthorization {
        schema: LAUNCH_AUTHORIZATION_SCHEMA.to_owned(),
        protocol_version: PROTOCOL_VERSION,
        authorization_id: request.authorization_id.clone(),
        request_id: request.request_id.clone(),
        request_digest: request.digest().to_string(),
        controller_uid: 1000,
        session_id: request.session_id.clone(),
        run_id: request.run_id.clone(),
        envelope_revision: request.envelope_revision,
        identity_slot: 3,
        assigned_uid: 200_003,
        assigned_gid: 300_003,
        expires_at_ms: 2_000,
    }
}

fn lifecycle(action: LifecycleAction, expected_state: SessionState) -> LifecycleRequest {
    LifecycleRequest {
        schema: LIFECYCLE_REQUEST_SCHEMA.to_owned(),
        protocol_version: PROTOCOL_VERSION,
        request_id: "request-1".to_owned(),
        session_id: "session-1".to_owned(),
        run_id: "run-1".to_owned(),
        authorization_id: "authorization-1".to_owned(),
        action,
        expected_state,
        expected_receipt_sequence: Some(1),
        envelope_revision: 7,
    }
}

fn supervisor(state: SessionState) -> SupervisorStatus {
    SupervisorStatus {
        schema: SUPERVISOR_STATUS_SCHEMA.to_owned(),
        protocol_version: PROTOCOL_VERSION,
        session_id: "session-1".to_owned(),
        run_id: "run-1".to_owned(),
        state,
        broker_connection: BrokerConnection::Connected,
        envelope_revision: 7,
        channel_state: match state {
            SessionState::Starting => ChannelState::Disabled,
            SessionState::Running => ChannelState::Enabled,
            SessionState::Parked => ChannelState::Revoked,
            SessionState::Terminal => ChannelState::Closed,
        },
        receipt_head: Some(ReceiptHead {
            sequence: 1,
            digest: digest(b"receipt-1"),
        }),
        pending_operation: None,
        last_failure: None,
    }
}

fn signed_receipt(request: &LifecycleRequest) -> SignedReceipt {
    let payload = ReceiptPayload {
        schema: RECEIPT_SCHEMA.to_owned(),
        session_id: request.session_id.clone(),
        run_id: request.run_id.clone(),
        request_id: request.request_id.clone(),
        envelope_revision: request.envelope_revision,
        sequence: 2,
        previous_receipt_digest: Some(digest(b"receipt-1")),
        release_id: digest(b"release"),
        signing_key_id: digest(b"launcher-key"),
        outcome: ReceiptOutcome::Park {
            authority: ReceiptAuthority::Authorized(Authorization {
                authorization_id: request.authorization_id.clone(),
                request_id: request.request_id.clone(),
                request_digest: request.digest().to_string(),
            }),
        },
        resulting_state: SessionState::Parked,
    };
    SignedReceipt {
        schema: SIGNED_RECEIPT_SCHEMA.to_owned(),
        signature: digest(&payload.canonical_bytes()),
        payload,
    }
}

#[test]
fn messages_are_closed_versioned_and_bounded() {
    let launch_request = launch_request();
    assert_eq!(
        launch_protocol::decode_message(&launch_request.canonical_bytes())
            .expect("launch authorization query decodes"),
        ProtocolMessage::LaunchAuthorization(launch_request),
    );

    let status = StatusRequest {
        schema: STATUS_REQUEST_SCHEMA.to_owned(),
        protocol_version: PROTOCOL_VERSION,
        request_id: "status-1".to_owned(),
        session_id: "session-1".to_owned(),
        run_id: "run-1".to_owned(),
    };
    let status_bytes = status.canonical_bytes();
    assert_eq!(
        launch_protocol::decode_message(&status_bytes).expect("status request decodes"),
        ProtocolMessage::Status(status),
    );

    let lifecycle_request = lifecycle(LifecycleAction::Park, SessionState::Running);
    assert_eq!(
        launch_protocol::decode_message(&lifecycle_request.canonical_bytes())
            .expect("lifecycle request decodes"),
        ProtocolMessage::Lifecycle(lifecycle_request),
    );

    let acknowledgement = ReceiptAcknowledgement {
        schema: RECEIPT_ACK_SCHEMA.to_owned(),
        protocol_version: PROTOCOL_VERSION,
        session_id: "session-1".to_owned(),
        run_id: "run-1".to_owned(),
        sequence: 0,
        receipt_digest: digest(b"receipt-0"),
    };
    assert_eq!(
        launch_protocol::decode_message(&acknowledgement.canonical_bytes())
            .expect("receipt acknowledgement decodes"),
        ProtocolMessage::ReceiptAcknowledgement(acknowledgement),
    );

    let malformed = [
        b"".as_slice(),
        b"{".as_slice(),
        br#"{"schema":"louiselm.launch.status-request/1"} trailing"#.as_slice(),
    ];
    for bytes in malformed {
        assert_eq!(
            launch_protocol::decode_message(bytes)
                .expect_err("malformed JSON is rejected")
                .code,
            ErrorCode::MalformedMessage,
        );
    }

    let mut oversized = vec![b' '; MAX_PROTOCOL_MESSAGE_BYTES + 1];
    oversized[0] = b'{';
    assert_eq!(
        launch_protocol::decode_message(&oversized)
            .expect_err("size is checked before JSON")
            .code,
        ErrorCode::MessageTooLarge,
    );

    let wrong_version = String::from_utf8(status_bytes.clone())
        .expect("JSON is UTF-8")
        .replace(r#""protocol_version":1"#, r#""protocol_version":2"#);
    assert_eq!(
        launch_protocol::decode_message(wrong_version.as_bytes())
            .expect_err("unknown versions fail closed")
            .code,
        ErrorCode::UnsupportedVersion,
    );

    for hostile in [
        String::from_utf8(status_bytes.clone())
            .expect("JSON is UTF-8")
            .replace(STATUS_REQUEST_SCHEMA, "louiselm.launch.unknown/1"),
        String::from_utf8(status_bytes.clone())
            .expect("JSON is UTF-8")
            .replace(r#""run_id":"run-1""#, r#""run_id":"run-1","extra":true"#),
        String::from_utf8(status_bytes)
            .expect("JSON is UTF-8")
            .replace(r#""request_id":"status-1","#, ""),
    ] {
        assert!(
            matches!(
                launch_protocol::decode_message(hostile.as_bytes())
                    .expect_err("closed message is rejected")
                    .code,
                ErrorCode::UnsupportedSchema | ErrorCode::MalformedMessage
            ),
            "hostile message was {hostile}",
        );
    }

    let lifecycle_json = String::from_utf8(
        lifecycle(LifecycleAction::Park, SessionState::Running).canonical_bytes(),
    )
    .expect("JSON is UTF-8");
    for (hostile, expected) in [
        (
            lifecycle_json.replace(r#""action":"park""#, r#""action":"launch""#),
            ErrorCode::MalformedMessage,
        ),
        (
            lifecycle_json.replace(
                r#""request_id":"request-1""#,
                r#""request_id":"request-1","request_id":"request-2""#,
            ),
            ErrorCode::MalformedMessage,
        ),
        (
            lifecycle_json.replace(r#""envelope_revision":7"#, r#""envelope_revision":"7""#),
            ErrorCode::MalformedMessage,
        ),
        (
            lifecycle_json.replace(
                r#""expected_receipt_sequence":1"#,
                r#""expected_receipt_sequence":null"#,
            ),
            ErrorCode::InvalidRequest,
        ),
    ] {
        assert_eq!(
            launch_protocol::decode_message(hostile.as_bytes())
                .expect_err("malformed lifecycle shape is rejected")
                .code,
            expected,
        );
    }
}

#[test]
fn launch_authorization_is_correlated_closed_and_exactly_bound() {
    let request = launch_request();
    let authorization = launch_authorization(&request);
    authorization
        .validate_for(&request, 1000, 1_999)
        .expect("matching unexpired authorization is usable");

    let response = ProtocolResponse {
        schema: RESPONSE_SCHEMA.to_owned(),
        protocol_version: PROTOCOL_VERSION,
        request_id: request.request_id.clone(),
        result: ResponseResult::LaunchAuthorization {
            authorization: authorization.clone(),
        },
    };
    assert_eq!(
        ProtocolResponse::parse_canonical(&response.canonical_bytes())
            .expect("canonical launch authorization response parses"),
        response,
    );

    let mut wrong_response = response.clone();
    wrong_response.request_id = "another-request".to_owned();
    assert_eq!(
        wrong_response
            .validate()
            .expect_err("the response request ID is correlated")
            .code,
        ErrorCode::InvalidRequest,
    );

    let mut with_extra = String::from_utf8(response.canonical_bytes()).expect("JSON is UTF-8");
    with_extra = with_extra.replace(
        r#""expires_at_ms":2000"#,
        r#""expires_at_ms":2000,"consumed":false"#,
    );
    assert_eq!(
        ProtocolResponse::parse_canonical(with_extra.as_bytes())
            .expect_err("authorization fields are closed")
            .code,
        ErrorCode::MalformedMessage,
    );

    assert_eq!(
        authorization
            .validate_for(&request, 1000, authorization.expires_at_ms)
            .expect_err("expiry is exclusive")
            .code,
        ErrorCode::InvalidRequest,
    );

    let mismatches = [
        {
            let mut value = authorization.clone();
            value.authorization_id = "another-authorization".to_owned();
            value
        },
        {
            let mut value = authorization.clone();
            value.request_id = "another-request".to_owned();
            value
        },
        {
            let mut value = authorization.clone();
            value.request_digest = digest(b"another-request");
            value
        },
        {
            let mut value = authorization.clone();
            value.controller_uid = 1001;
            value
        },
        {
            let mut value = authorization.clone();
            value.session_id = "another-session".to_owned();
            value
        },
        {
            let mut value = authorization.clone();
            value.run_id = "another-run".to_owned();
            value
        },
        {
            let mut value = authorization.clone();
            value.envelope_revision += 1;
            value
        },
    ];
    for mismatch in mismatches {
        assert_eq!(
            mismatch
                .validate_for(&request, 1000, 1_999)
                .expect_err("every authorization binding is exact")
                .code,
            ErrorCode::InvalidRequest,
        );
    }

    for invalid_identity in [
        {
            let mut value = authorization.clone();
            value.assigned_uid = 0;
            value
        },
        {
            let mut value = authorization;
            value.assigned_gid = 0;
            value
        },
    ] {
        assert_eq!(
            invalid_identity
                .validate()
                .expect_err("root is not an assigned Session identity")
                .code,
            ErrorCode::InvalidRequest,
        );
    }
}

#[test]
fn identifiers_and_wire_digests_are_strictly_canonical() {
    let base = lifecycle(LifecycleAction::Park, SessionState::Running);
    for hostile in ["", "../escape", "has space", "non-ascii-é"] {
        let mut request = base.clone();
        request.request_id = hostile.to_owned();
        assert_eq!(
            request
                .validate()
                .expect_err("hostile identifier is rejected")
                .code,
            ErrorCode::InvalidRequest,
        );
    }

    let mut maximum = base.clone();
    maximum.request_id = "a".repeat(MAX_IDENTIFIER_BYTES);
    maximum.validate().expect("maximum identifier is accepted");
    maximum.request_id.push('a');
    assert_eq!(
        maximum
            .validate()
            .expect_err("oversized identifier is rejected")
            .code,
        ErrorCode::InvalidRequest,
    );

    let mut acknowledgement = ReceiptAcknowledgement {
        schema: RECEIPT_ACK_SCHEMA.to_owned(),
        protocol_version: PROTOCOL_VERSION,
        session_id: "session-1".to_owned(),
        run_id: "run-1".to_owned(),
        sequence: 0,
        receipt_digest: digest(b"receipt-0"),
    };
    for noncanonical in [
        acknowledgement.receipt_digest[7..].to_owned(),
        acknowledgement
            .receipt_digest
            .replacen("sha256:", "sha256-", 1),
    ] {
        acknowledgement.receipt_digest = noncanonical;
        assert_eq!(
            acknowledgement
                .validate()
                .expect_err("digest aliases are not canonical protocol values")
                .code,
            ErrorCode::InvalidRequest,
        );
    }
}

#[test]
fn lifecycle_transition_matrix_is_closed() {
    let allowed = [
        (
            SessionState::Running,
            LifecycleAction::Park,
            SessionState::Parked,
        ),
        (
            SessionState::Running,
            LifecycleAction::Interrupt,
            SessionState::Running,
        ),
        (
            SessionState::Running,
            LifecycleAction::Disposal,
            SessionState::Terminal,
        ),
        (
            SessionState::Parked,
            LifecycleAction::Resume,
            SessionState::Running,
        ),
        (
            SessionState::Parked,
            LifecycleAction::Disposal,
            SessionState::Terminal,
        ),
    ];
    for state in [
        SessionState::Starting,
        SessionState::Running,
        SessionState::Parked,
        SessionState::Terminal,
    ] {
        for action in [
            LifecycleAction::Park,
            LifecycleAction::Resume,
            LifecycleAction::Interrupt,
            LifecycleAction::Disposal,
        ] {
            let expected = allowed
                .iter()
                .find(|(from, candidate, _)| *from == state && *candidate == action)
                .map(|(_, _, result)| *result);
            assert_eq!(
                launch_protocol::transition(state, action).ok(),
                expected,
                "unexpected transition for {state:?}/{action:?}",
            );
        }
    }
}

#[test]
fn launch_status_represents_both_receipt_commit_points() {
    let mut signing_launch = supervisor(SessionState::Starting);
    signing_launch.receipt_head = None;
    signing_launch.pending_operation = Some(PendingOperation {
        request_id: "launch-request".to_owned(),
        action: PendingAction::Launch,
        phase: PendingPhase::Signing,
    });
    signing_launch
        .validate()
        .expect("sequence zero may be signing while the workload is blocked");

    let mut awaiting_launch = signing_launch.clone();
    awaiting_launch.receipt_head = Some(ReceiptHead {
        sequence: 0,
        digest: digest(b"receipt-0"),
    });
    awaiting_launch.pending_operation.as_mut().unwrap().phase = PendingPhase::AwaitingDurableAck;
    awaiting_launch
        .validate()
        .expect("sequence zero may await its durable acknowledgement");

    let mut applying_start = awaiting_launch.clone();
    applying_start.pending_operation.as_mut().unwrap().phase = PendingPhase::Applying;
    applying_start
        .validate()
        .expect("the durable Starting head may gate workload release");

    let mut signing_start = supervisor(SessionState::Running);
    signing_start.receipt_head = awaiting_launch.receipt_head.clone();
    signing_start.pending_operation = Some(PendingOperation {
        request_id: "launch-request".to_owned(),
        action: PendingAction::Launch,
        phase: PendingPhase::Signing,
    });
    signing_start
        .validate()
        .expect("a started workload may be signing its Running receipt");

    let mut awaiting_start = supervisor(SessionState::Running);
    awaiting_start.pending_operation = Some(PendingOperation {
        request_id: "launch-request".to_owned(),
        action: PendingAction::Launch,
        phase: PendingPhase::AwaitingDurableAck,
    });
    awaiting_start
        .validate()
        .expect("the Running receipt may await durable acknowledgement");

    let mut signing_launch_with_head = signing_launch.clone();
    signing_launch_with_head.receipt_head = awaiting_launch.receipt_head.clone();
    let mut awaiting_launch_with_later_head = awaiting_launch.clone();
    awaiting_launch_with_later_head.receipt_head = awaiting_start.receipt_head.clone();
    let mut signing_start_with_later_head = signing_start.clone();
    signing_start_with_later_head.receipt_head = awaiting_start.receipt_head.clone();
    let mut awaiting_start_with_earlier_head = awaiting_start.clone();
    awaiting_start_with_earlier_head.receipt_head = awaiting_launch.receipt_head.clone();
    let stable_starting_with_later_head = supervisor(SessionState::Starting);
    let mut stable_running_with_earlier_head = signing_start.clone();
    stable_running_with_earlier_head.pending_operation = None;
    for (label, status) in [
        (
            "Starting/signing with a signed head",
            signing_launch_with_head,
        ),
        (
            "Starting/awaiting with the Running head",
            awaiting_launch_with_later_head,
        ),
        (
            "Running/signing with the Running head",
            signing_start_with_later_head,
        ),
        (
            "Running/awaiting with only the Starting head",
            awaiting_start_with_earlier_head,
        ),
        (
            "stable Starting after sequence zero",
            stable_starting_with_later_head,
        ),
        (
            "stable Running before sequence one",
            stable_running_with_earlier_head,
        ),
    ] {
        assert_eq!(
            status
                .validate()
                .expect_err("an impossible launch receipt head must be rejected")
                .code,
            ErrorCode::InvalidRequest,
            "case {label}",
        );
    }

    let mut request_from_sequence_zero =
        lifecycle(LifecycleAction::Disposal, SessionState::Starting);
    request_from_sequence_zero.expected_receipt_sequence = Some(0);
    request_from_sequence_zero
        .validate()
        .expect("Starting may already have the durable sequence-zero head");
    request_from_sequence_zero.expected_receipt_sequence = Some(1);
    assert_eq!(
        request_from_sequence_zero
            .validate()
            .expect_err("Starting cannot have advanced beyond sequence zero")
            .code,
        ErrorCode::InvalidRequest,
    );
}

#[test]
fn evaluation_is_stateless_and_applies_pending_then_cas_precedence() {
    let status = supervisor(SessionState::Running);
    let request = lifecycle(LifecycleAction::Park, SessionState::Running);
    let disposition = launch_protocol::evaluate_request(&status, None, &request)
        .expect("matching CAS request is executable");
    let intent = disposition
        .execute_intent()
        .expect("new request returns an execution intent");
    assert_eq!(intent.sequence, 2);
    assert_eq!(
        intent.previous_receipt_digest,
        status.receipt_head.as_ref().unwrap().digest,
    );
    assert_eq!(intent.resulting_state, SessionState::Parked);
    assert_eq!(status.state, SessionState::Running, "evaluation is pure");
    let receipt_payload = intent
        .receipt_payload(&digest(b"release"), &digest(b"launcher-key"))
        .expect("an execution intent becomes one valid receipt payload");
    assert_eq!(receipt_payload.request_id, request.request_id);
    assert_eq!(
        receipt_payload.outcome,
        signed_receipt(&request).payload.outcome
    );

    let mut pending_status = status.clone();
    pending_status.pending_operation = Some(PendingOperation {
        request_id: "other-request".to_owned(),
        action: PendingAction::Interrupt,
        phase: PendingPhase::Applying,
    });
    let mut stale_everything = request.clone();
    stale_everything.expected_state = SessionState::Parked;
    stale_everything.expected_receipt_sequence = Some(99);
    stale_everything.envelope_revision = 99;
    assert_eq!(
        launch_protocol::evaluate_request(&pending_status, None, &stale_everything)
            .expect_err("pending work wins over stale CAS")
            .code,
        ErrorCode::OperationPending,
    );

    let stale_cases = [
        (
            {
                let mut value = request.clone();
                value.expected_state = SessionState::Parked;
                value
            },
            ErrorCode::StateMismatch,
        ),
        (
            {
                let mut value = request.clone();
                value.expected_receipt_sequence = Some(9);
                value
            },
            ErrorCode::ReceiptSequenceMismatch,
        ),
        (
            {
                let mut value = request.clone();
                value.envelope_revision = 9;
                value
            },
            ErrorCode::EnvelopeRevisionMismatch,
        ),
    ];
    for (request, code) in stale_cases {
        assert_eq!(
            launch_protocol::evaluate_request(&status, None, &request)
                .expect_err("stale CAS is rejected")
                .code,
            code,
        );
    }
}

#[test]
fn identical_retry_replays_exact_receipt_before_stale_cas() {
    let request = lifecycle(LifecycleAction::Park, SessionState::Running);
    let receipt = signed_receipt(&request);
    let completed = CompletedRequest::new(&request, receipt.clone());
    let mut advanced = supervisor(SessionState::Parked);
    advanced.envelope_revision = 8;
    advanced.receipt_head = Some(ReceiptHead {
        sequence: 2,
        digest: receipt.digest().to_string(),
    });

    let replay = launch_protocol::evaluate_request(&advanced, Some(&completed), &request)
        .expect("an exact retry ignores now-stale CAS values");
    assert_eq!(
        replay.replayed_receipt().unwrap().canonical_bytes(),
        receipt.canonical_bytes(),
    );

    let mut mismatched_completion = completed.clone();
    mismatched_completion.receipt.payload.envelope_revision = 99;
    assert_eq!(
        launch_protocol::evaluate_request(&advanced, Some(&mismatched_completion), &request)
            .expect_err("a stored receipt about different request fields is not replayed")
            .code,
        ErrorCode::ReceiptChainInvalid,
    );

    for conflicting in [
        {
            let mut value = request.clone();
            value.authorization_id = "authorization-2".to_owned();
            value
        },
        {
            let mut value = request.clone();
            value.action = LifecycleAction::Interrupt;
            value
        },
        {
            let mut value = request.clone();
            value.expected_state = SessionState::Parked;
            value
        },
        {
            let mut value = request.clone();
            value.expected_receipt_sequence = Some(9);
            value
        },
        {
            let mut value = request.clone();
            value.envelope_revision = 9;
            value
        },
    ] {
        assert_eq!(
            launch_protocol::evaluate_request(&advanced, Some(&completed), &conflicting)
                .expect_err("same request id with different bytes conflicts")
                .code,
            ErrorCode::RequestIdConflict,
        );
    }

    assert!(matches!(replay, RequestDisposition::Replay(_)));
}

#[test]
fn status_and_errors_have_pinned_safe_wire_shapes() {
    let pending_supervisor = SupervisorStatus {
        pending_operation: Some(PendingOperation {
            request_id: "request-2".to_owned(),
            action: PendingAction::Park,
            phase: PendingPhase::Applying,
        }),
        last_failure: Some(ProtocolError::new(
            ErrorCode::BrokerUnavailable,
            Some(SessionState::Running),
            Some(1),
        )),
        ..supervisor(SessionState::Running)
    };
    let status = SessionStatus::compose(
        pending_supervisor,
        PostureSummary::FullyVerified,
        Vec::new(),
    )
    .expect("broker status is consistent");
    assert_eq!(status.schema, SESSION_STATUS_SCHEMA);
    assert_eq!(
        String::from_utf8(status.canonical_bytes()).expect("status JSON is UTF-8"),
        r#"{"schema":"louiselm.launch.session-status/1","protocol_version":1,"session_id":"session-1","run_id":"run-1","state":"running","posture":"fully_verified","broker_connection":"connected","envelope_revision":7,"channel_state":"enabled","receipt_head":{"sequence":1,"digest":"sha256:fea5396a7f4325c408b1b65b33a4d77ba5486ceba941804d8889a8546cfbab96"},"pending_operation":{"request_id":"request-2","action":"park","phase":"applying"},"allowed_actions":[],"last_failure":{"code":"broker_unavailable","message":"control broker is unavailable","retryable":true,"current_state":"running","expected_sequence":1,"next_action":"reconnect_broker"}}"#,
    );

    let response = ProtocolResponse {
        schema: RESPONSE_SCHEMA.to_owned(),
        protocol_version: PROTOCOL_VERSION,
        request_id: "status-1".to_owned(),
        result: ResponseResult::SessionStatus {
            status: status.clone(),
        },
    };
    assert_eq!(
        ProtocolResponse::parse_canonical(&response.canonical_bytes())
            .expect("canonical closed response parses"),
        response,
    );
    let supervisor_response = ProtocolResponse {
        schema: RESPONSE_SCHEMA.to_owned(),
        protocol_version: PROTOCOL_VERSION,
        request_id: "status-2".to_owned(),
        result: ResponseResult::SupervisorStatus {
            status: supervisor(SessionState::Running),
        },
    };
    assert_eq!(
        ProtocolResponse::parse_canonical(&supervisor_response.canonical_bytes())
            .expect("supervisor answers status without authoring posture or policy"),
        supervisor_response,
    );
    let mut noncanonical = response.canonical_bytes();
    noncanonical.push(b'\n');
    assert_eq!(
        ProtocolResponse::parse_canonical(&noncanonical)
            .expect_err("response whitespace changes canonical bytes")
            .code,
        ErrorCode::InvalidRequest,
    );

    let ready = SessionStatus::compose(
        supervisor(SessionState::Running),
        PostureSummary::FullyVerified,
        vec![LifecycleAction::Disposal, LifecycleAction::Park],
    )
    .expect("broker status sorts the currently actionable subset");
    assert_eq!(
        ready.allowed_actions,
        vec![LifecycleAction::Park, LifecycleAction::Disposal]
    );

    let mut contradictory = ready.clone();
    contradictory.allowed_actions = vec![LifecycleAction::Resume];
    assert_eq!(
        contradictory
            .validate()
            .expect_err("status cannot advertise a mechanically impossible action")
            .code,
        ErrorCode::InvalidRequest,
    );

    contradictory = ready;
    contradictory.pending_operation = status.pending_operation.clone();
    assert_eq!(
        contradictory
            .validate()
            .expect_err("pending status cannot advertise another executable mutation")
            .code,
        ErrorCode::InvalidRequest,
    );

    let mut missing_head = supervisor(SessionState::Parked);
    missing_head.receipt_head = None;
    assert_eq!(
        missing_head
            .validate()
            .expect_err("post-launch state requires a signed receipt head")
            .code,
        ErrorCode::InvalidRequest,
    );

    let mut unacknowledgeable = supervisor(SessionState::Starting);
    unacknowledgeable.receipt_head = None;
    unacknowledgeable.pending_operation = Some(PendingOperation {
        request_id: "launch-request".to_owned(),
        action: PendingAction::Launch,
        phase: PendingPhase::AwaitingDurableAck,
    });
    assert_eq!(
        unacknowledgeable
            .validate()
            .expect_err("an ACK wait must expose the exact receipt head")
            .code,
        ErrorCode::InvalidRequest,
    );

    for (state, action) in [
        (SessionState::Starting, PendingAction::Resume),
        (SessionState::Parked, PendingAction::Launch),
        (SessionState::Terminal, PendingAction::Interrupt),
    ] {
        let mut impossible = supervisor(state);
        impossible.pending_operation = Some(PendingOperation {
            request_id: "pending-request".to_owned(),
            action,
            phase: PendingPhase::Applying,
        });
        assert_eq!(
            impossible
                .validate()
                .expect_err("pending operation must agree with mechanical state")
                .code,
            ErrorCode::InvalidRequest,
        );
    }

    let invalid = ProtocolError {
        code: ErrorCode::BrokerUnavailable,
        message: "hostile injected text".to_owned(),
        retryable: false,
        current_state: None,
        expected_sequence: None,
        next_action: NextAction::None,
    };
    assert!(
        invalid.validate().is_err(),
        "error metadata is derived, not trusted"
    );

    let exhausted = ProtocolError::new(ErrorCode::SessionIdentityExhausted, None, None);
    assert_eq!(exhausted.message, "no session identity is available");
    assert!(exhausted.retryable);
    assert_eq!(exhausted.next_action, NextAction::Wait);

    for stale in [
        ErrorCode::StateMismatch,
        ErrorCode::ReceiptSequenceMismatch,
        ErrorCode::EnvelopeRevisionMismatch,
    ] {
        let error = ProtocolError::new(stale, Some(SessionState::Running), Some(4));
        assert!(
            !error.retryable,
            "stale CAS bytes cannot become valid without changing the request",
        );
        assert_eq!(error.next_action, NextAction::RefreshStatus);
    }
}

#[test]
fn receipt_acknowledgement_binds_the_exact_pending_head() {
    let expected = ReceiptHead {
        sequence: 3,
        digest: digest(b"receipt-3"),
    };
    let acknowledgement = ReceiptAcknowledgement {
        schema: RECEIPT_ACK_SCHEMA.to_owned(),
        protocol_version: PROTOCOL_VERSION,
        session_id: "session-1".to_owned(),
        run_id: "run-1".to_owned(),
        sequence: expected.sequence,
        receipt_digest: expected.digest.clone(),
    };
    assert!(acknowledgement.matches("session-1", "run-1", &expected));

    let mut wrong = acknowledgement.clone();
    wrong.receipt_digest = digest(b"another-receipt");
    assert!(!wrong.matches("session-1", "run-1", &expected));
    wrong = acknowledgement.clone();
    wrong.sequence += 1;
    assert!(!wrong.matches("session-1", "run-1", &expected));
    assert!(!acknowledgement.matches("another-session", "run-1", &expected));

    let with_extra_field = String::from_utf8(acknowledgement.canonical_bytes())
        .expect("JSON is UTF-8")
        .replace(r#""sequence":3"#, r#""sequence":3,"durable":true"#);
    assert_eq!(
        launch_protocol::decode_message(with_extra_field.as_bytes())
            .expect_err("acknowledgement fields are closed")
            .code,
        ErrorCode::MalformedMessage,
    );
}

#[test]
fn responses_enforce_request_correlation_and_the_encoded_size_limit() {
    let request = lifecycle(LifecycleAction::Park, SessionState::Running);
    let mismatched = ProtocolResponse {
        schema: RESPONSE_SCHEMA.to_owned(),
        protocol_version: PROTOCOL_VERSION,
        request_id: "another-request".to_owned(),
        result: ResponseResult::Receipt {
            receipt: signed_receipt(&request),
        },
    };
    assert_eq!(
        mismatched
            .validate()
            .expect_err("a receipt response must answer the same request")
            .code,
        ErrorCode::InvalidRequest,
    );

    let launch_request_digest = digest(b"launch-request");
    let channels = (0..390)
        .map(|index| format!("channel-{index:04}-{}", "x".repeat(110)))
        .collect();
    let payload = ReceiptPayload {
        schema: RECEIPT_SCHEMA.to_owned(),
        session_id: "session-1".to_owned(),
        run_id: "run-1".to_owned(),
        request_id: "launch-request".to_owned(),
        envelope_revision: 1,
        sequence: 0,
        previous_receipt_digest: None,
        release_id: digest(b"release"),
        signing_key_id: digest(b"launcher-key"),
        outcome: ReceiptOutcome::Launch {
            authorization: Authorization {
                authorization_id: "authorization-1".to_owned(),
                request_id: "launch-request".to_owned(),
                request_digest: launch_request_digest.clone(),
            },
            evidence: Box::new(LaunchEvidence {
                launch_request_digest,
                runtime_measurement_digest: digest(b"runtime"),
                skill_generation_id: digest(b"generation"),
                session_input_manifest_id: digest(b"input"),
                isolation_contract: "louiselm.isolation/1".to_owned(),
                isolation_backend_id: "bubblewrap-0_12".to_owned(),
                kernel_identity: "linux-6_18".to_owned(),
                isolation_evidence_digest: digest(b"isolation"),
                capability_channel_ids: channels,
            }),
        },
        resulting_state: SessionState::Starting,
    };
    let mut receipt = SignedReceipt {
        schema: SIGNED_RECEIPT_SCHEMA.to_owned(),
        payload,
        signature: "x".to_owned(),
    };
    let probe = ProtocolResponse {
        schema: RESPONSE_SCHEMA.to_owned(),
        protocol_version: PROTOCOL_VERSION,
        request_id: "launch-request".to_owned(),
        result: ResponseResult::Receipt {
            receipt: receipt.clone(),
        },
    };
    let wrapper_bytes = probe.canonical_bytes().len() - receipt.canonical_bytes().len();
    let target_receipt_bytes = MAX_PROTOCOL_MESSAGE_BYTES - wrapper_bytes + 1;
    let signature_bytes = target_receipt_bytes - receipt.canonical_bytes().len() + 1;
    receipt.signature = "x".repeat(signature_bytes);
    receipt
        .validate()
        .expect("the nested receipt remains within its own size limit");

    let oversized_response = ProtocolResponse {
        schema: RESPONSE_SCHEMA.to_owned(),
        protocol_version: PROTOCOL_VERSION,
        request_id: "launch-request".to_owned(),
        result: ResponseResult::Receipt { receipt },
    };
    assert!(oversized_response.canonical_bytes().len() > MAX_PROTOCOL_MESSAGE_BYTES);
    assert_eq!(
        oversized_response
            .validate()
            .expect_err("sender rejects a response its decoder would reject")
            .code,
        ErrorCode::MessageTooLarge,
    );
}
