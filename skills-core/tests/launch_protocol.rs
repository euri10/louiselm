//! Behavioral coverage for launch protocol.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Test fixtures abort on setup failure and assert failures directly."
)]

//! The closed broker/supervisor lifecycle protocol.

use louiselm_skills::{
    canonical::Digest,
    launch::{LaunchRequest, REQUEST_SCHEMA},
    launch_protocol::{
        self, BROKER_RECONNECT_SCHEMA, BrokerConnection, BrokerReconnect, ChannelState,
        CompletedRequest, ErrorCode, IdentityExhaustion, LAUNCH_AUTHORIZATION_SCHEMA,
        LIFECYCLE_REQUEST_SCHEMA, LaunchAuthorization, LifecycleAction, LifecycleRequest,
        MAX_BROKER_LOSS_GRACE_MS, MAX_IDENTIFIER_BYTES, MAX_IDENTITY_OCCUPANTS,
        MAX_PROTOCOL_MESSAGE_BYTES, NextAction, OccupiedSessionIdentity, PROTOCOL_VERSION,
        PendingAction, PendingOperation, PendingPhase, PostureSummary, ProtocolError,
        ProtocolMessage, ProtocolResponse, RECEIPT_ACK_SCHEMA, RESPONSE_SCHEMA,
        ReceiptAcknowledgement, ReceiptDisposition, RequestDisposition, ResponseResult,
        SESSION_STATUS_SCHEMA, STATUS_REQUEST_SCHEMA, SUPERVISOR_STATUS_SCHEMA, SessionStatus,
        StatusRequest, SupervisorStatus,
    },
    launch_receipt::{
        Authorization, ConformanceEvidence, LaunchEvidence, ProcessExitClassification,
        RECEIPT_SCHEMA, ReceiptAuthority, ReceiptHead, ReceiptOutcome, ReceiptPayload,
        SIGNED_RECEIPT_SCHEMA, SessionState, SignedReceipt,
    },
};

#[path = "launch_protocol/posture.rs"]
mod posture;

fn digest(value: &[u8]) -> String {
    Digest::of(value).to_string()
}

// Synthetic display fixtures exercise protocol shape, not real launch authority.
fn status_posture(state: PostureSummary) -> launch_protocol::PostureStatus {
    use louiselm_skills::{
        launch_protocol::{EvidenceFreshness, FreshnessBasis, PostureStatus},
        posture::{DimensionInput, DimensionName, EvidenceKind, EvidenceRef, FailureCode, Posture},
    };
    let verified = matches!(
        state,
        PostureSummary::FullyVerified | PostureSummary::Waived
    );
    let kinds = [
        EvidenceKind::SkillGeneration,
        EvidenceKind::SessionInputManifest,
        EvidenceKind::RuntimeMeasurement,
        EvidenceKind::IsolationReceipt,
        EvidenceKind::CapabilityEnvelope,
        EvidenceKind::SessionInputManifest,
    ];
    let inputs = DimensionName::ALL
        .into_iter()
        .zip(kinds)
        .map(|(dimension, kind)| {
            if state == PostureSummary::Waived && dimension == DimensionName::ManagedSupply {
                DimensionInput::waived(
                    dimension,
                    FailureCode::WitnessMissing,
                    vec![EvidenceRef::new(kind, &digest(b"status-proof")).unwrap()],
                    EvidenceRef::new(EvidenceKind::WaiverReceipt, &digest(b"waiver")).unwrap(),
                )
            } else if verified {
                DimensionInput::verified(
                    dimension,
                    vec![EvidenceRef::new(kind, &digest(b"status-proof")).unwrap()],
                )
            } else {
                DimensionInput::failed(dimension, FailureCode::EvidenceMissing, vec![])
            }
        })
        .collect();
    let posture = Posture::evaluate("session-1", "run-1", inputs).unwrap();
    let freshness = EvidenceFreshness {
        basis: if verified {
            FreshnessBasis::Launch
        } else {
            FreshnessBasis::Missing
        },
        last_verified_at_ms: verified.then_some(1000),
    };
    PostureStatus::from_posture(&posture, [freshness; 6])
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
        broker_loss_grace_ms: MAX_BROKER_LOSS_GRACE_MS,
    }
}

fn broker_reconnect(request_id: &str) -> BrokerReconnect {
    BrokerReconnect {
        schema: BROKER_RECONNECT_SCHEMA.to_owned(),
        protocol_version: PROTOCOL_VERSION,
        request_id: request_id.to_owned(),
        session_id: "session-1".to_owned(),
        run_id: "run-1".to_owned(),
        envelope_revision: 7,
        sequence: 3,
        receipt_digest: digest(b"receipt-3"),
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
        launcher_head: Some(ReceiptHead {
            sequence: 1,
            digest: digest(b"receipt-1"),
        }),
        broker_head: Some(ReceiptHead {
            sequence: 1,
            digest: digest(b"receipt-1"),
        }),
        pending_receipt_count: 0,
        pending_operation: None,
        process_exit: None,
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
#[expect(
    clippy::too_many_lines,
    reason = "One messages are closed versioned and bounded scenario keeps its causal steps and assertions together."
)]
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
        disposition: ReceiptDisposition::DurablyStored,
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
#[expect(
    clippy::too_many_lines,
    reason = "One launch authorization is correlated closed and exactly bound scenario keeps its causal steps and assertions together."
)]
fn launch_authorization_is_correlated_closed_and_exactly_bound() {
    let request = launch_request();
    let authorization = launch_authorization(&request);
    assert_eq!(
        LAUNCH_AUTHORIZATION_SCHEMA,
        "louiselm.launch.authorization/2"
    );
    assert_eq!(MAX_BROKER_LOSS_GRACE_MS, 5_000);
    authorization
        .validate_for(&request, 1000, 1_999)
        .expect("matching unexpired authorization is usable");

    let mut zero_grace = authorization.clone();
    zero_grace.broker_loss_grace_ms = 0;
    zero_grace
        .validate()
        .expect("zero grace is an immediate fail-closed policy");
    let mut excessive_grace = authorization.clone();
    excessive_grace.broker_loss_grace_ms = MAX_BROKER_LOSS_GRACE_MS + 1;
    assert_eq!(
        excessive_grace
            .validate()
            .expect_err("broker-loss grace is capped at five seconds")
            .code,
        ErrorCode::InvalidRequest,
    );

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

    let without_grace = String::from_utf8(response.canonical_bytes())
        .expect("JSON is UTF-8")
        .replace(
            &format!(r#","broker_loss_grace_ms":{MAX_BROKER_LOSS_GRACE_MS}"#),
            "",
        );
    assert_eq!(
        ProtocolResponse::parse_canonical(without_grace.as_bytes())
            .expect_err("broker-loss grace is required authorization")
            .code,
        ErrorCode::MalformedMessage,
    );

    let mut old_schema = authorization.clone();
    old_schema.schema = "louiselm.launch.authorization/1".to_owned();
    assert_eq!(
        old_schema
            .validate()
            .expect_err("the authorization shape changed generations")
            .code,
        ErrorCode::UnsupportedSchema,
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
#[expect(
    clippy::too_many_lines,
    reason = "One broker reconnect is closed correlated and compares exact heads scenario keeps its causal steps and assertions together."
)]
fn broker_reconnect_is_closed_correlated_and_compares_exact_heads() {
    assert_eq!(
        BROKER_RECONNECT_SCHEMA,
        "louiselm.launch.broker-reconnect/1"
    );
    assert_eq!(RESPONSE_SCHEMA, "louiselm.launch.response/2");

    let launcher = broker_reconnect("reconnect-1");
    launcher
        .validate()
        .expect("the launcher advertises its exact current head");
    let launcher_bytes = launcher.canonical_bytes();
    assert_eq!(
        String::from_utf8(launcher_bytes.clone()).expect("reconnect JSON is UTF-8"),
        format!(
            concat!(
                r#"{{"schema":"louiselm.launch.broker-reconnect/1","#,
                r#""protocol_version":1,"request_id":"reconnect-1","#,
                r#""session_id":"session-1","run_id":"run-1","#,
                r#""envelope_revision":7,"sequence":3,"receipt_digest":"{}"}}"#,
            ),
            digest(b"receipt-3"),
        ),
    );
    assert_eq!(
        BrokerReconnect::parse_canonical(&launcher_bytes)
            .expect("canonical reconnect request parses"),
        launcher,
    );

    let mut broker = launcher.clone();
    broker.sequence = 1;
    broker.receipt_digest = digest(b"receipt-1");
    broker
        .validate_response_to(&launcher)
        .expect("the authenticated broker may report an older durable head");
    let response = ProtocolResponse {
        schema: RESPONSE_SCHEMA.to_owned(),
        protocol_version: PROTOCOL_VERSION,
        request_id: launcher.request_id.clone(),
        result: ResponseResult::BrokerReconnect {
            reconnect: broker.clone(),
        },
    };
    assert_eq!(
        String::from_utf8(response.canonical_bytes()).expect("response JSON is UTF-8"),
        format!(
            concat!(
                r#"{{"schema":"louiselm.launch.response/2","protocol_version":1,"#,
                r#""request_id":"reconnect-1","result":{{"kind":"broker_reconnect","#,
                r#""reconnect":{}}}}}"#,
            ),
            String::from_utf8(broker.canonical_bytes()).expect("reconnect JSON is UTF-8"),
        ),
    );
    assert_eq!(
        ProtocolResponse::parse_canonical(&response.canonical_bytes())
            .expect("canonical broker reconnect response parses"),
        response,
    );

    let mut wrong_outer = response.clone();
    wrong_outer.request_id = "another-request".to_owned();
    assert_eq!(
        wrong_outer
            .validate()
            .expect_err("outer and inner request IDs are correlated")
            .code,
        ErrorCode::InvalidRequest,
    );

    for mismatch in [
        {
            let mut value = broker.clone();
            value.request_id = "another-request".to_owned();
            value
        },
        {
            let mut value = broker.clone();
            value.session_id = "another-session".to_owned();
            value
        },
        {
            let mut value = broker.clone();
            value.run_id = "another-run".to_owned();
            value
        },
        {
            let mut value = broker.clone();
            value.envelope_revision += 1;
            value
        },
    ] {
        assert_eq!(
            mismatch
                .validate_response_to(&launcher)
                .expect_err("reconnect response must match request correlation and subject")
                .code,
            ErrorCode::InvalidRequest,
        );
    }

    let mut noncanonical_digest = broker.clone();
    noncanonical_digest.receipt_digest = noncanonical_digest.receipt_digest[7..].to_owned();
    assert_eq!(
        noncanonical_digest
            .validate()
            .expect_err("reconnect heads use canonical wire digests")
            .code,
        ErrorCode::InvalidRequest,
    );

    let mut maximum = launcher.clone();
    maximum.request_id = "a".repeat(MAX_IDENTIFIER_BYTES);
    maximum
        .validate()
        .expect("maximum reconnect identifier is accepted");
    maximum.request_id.push('a');
    assert_eq!(
        maximum
            .validate()
            .expect_err("oversized reconnect identifier is rejected")
            .code,
        ErrorCode::InvalidRequest,
    );

    let with_extra = String::from_utf8(launcher_bytes)
        .expect("reconnect JSON is UTF-8")
        .replace(
            r#""sequence":3"#,
            r#""sequence":3,"trusted_without_comparison":true"#,
        );
    assert_eq!(
        BrokerReconnect::parse_canonical(with_extra.as_bytes())
            .expect_err("reconnect fields are closed")
            .code,
        ErrorCode::MalformedMessage,
    );
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
        disposition: ReceiptDisposition::DurablyStored,
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
            LifecycleAction::Interrupt,
            SessionState::Parked,
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
fn terminal_audit_can_follow_a_mechanic_without_a_process_exit_classification() {
    for action in [
        PendingAction::Park,
        PendingAction::Resume,
        PendingAction::Interrupt,
    ] {
        for phase in [PendingPhase::Signing, PendingPhase::AwaitingDurableAck] {
            let mut status = supervisor(SessionState::Terminal);
            status.pending_receipt_count = 1;
            status.pending_operation = Some(PendingOperation {
                request_id: "earlier-mechanic".to_owned(),
                action,
                phase,
            });
            if phase == PendingPhase::AwaitingDurableAck {
                status.launcher_head = Some(ReceiptHead {
                    sequence: 2,
                    digest: digest(b"pending-receipt"),
                });
            }
            status
                .validate()
                .expect("cleanup can precede the earlier mechanic's audit");
            let composed = SessionStatus::compose(
                status.clone(),
                status_posture(PostureSummary::Unverified),
                Vec::new(),
            )
            .unwrap();
            assert!(composed.allowed_actions.is_empty());
            assert_eq!(composed.process_exit, None);
            status.pending_operation.as_mut().unwrap().phase = PendingPhase::Applying;
            assert!(
                status.validate().is_err(),
                "terminal cleanup cannot still apply a live mechanic"
            );
            status.pending_operation.as_mut().unwrap().phase = phase;
            status.channel_state = ChannelState::Enabled;
            assert!(
                status.validate().is_err(),
                "terminal means closed capabilities"
            );
        }
    }
}

#[test]
fn process_exit_classification_is_terminal_only_and_composes_without_raw_status() {
    let mut exited = supervisor(SessionState::Terminal);
    exited.process_exit = Some(ProcessExitClassification::Signaled);
    exited
        .validate()
        .expect("a terminal supervisor status may classify a confirmed process exit");
    let status = SessionStatus::compose(
        exited,
        status_posture(PostureSummary::Unverified),
        Vec::new(),
    )
    .expect("the broker preserves the sanitized classification");
    assert_eq!(
        status.process_exit,
        Some(ProcessExitClassification::Signaled)
    );
    let encoded = String::from_utf8(status.canonical_bytes()).expect("status JSON is UTF-8");
    assert!(encoded.contains(r#""process_exit":"signaled""#));
    assert!(!encoded.contains("exit_code"));
    assert!(!encoded.contains(r#""signal":"#));

    let mut running = supervisor(SessionState::Running);
    running.process_exit = Some(ProcessExitClassification::Failure);
    assert_eq!(
        running
            .validate()
            .expect_err("a live Session cannot claim a process exit")
            .code,
        ErrorCode::InvalidRequest,
    );
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "One launch status represents both receipt commit points scenario keeps its causal steps and assertions together."
)]
fn launch_status_represents_both_receipt_commit_points() {
    let mut signing_launch = supervisor(SessionState::Starting);
    signing_launch.launcher_head = None;
    signing_launch.broker_head = None;
    signing_launch.pending_receipt_count = 1;
    signing_launch.pending_operation = Some(PendingOperation {
        request_id: "launch-request".to_owned(),
        action: PendingAction::Launch,
        phase: PendingPhase::Signing,
    });
    signing_launch
        .validate()
        .expect("sequence zero may be signing while the workload is blocked");

    let mut awaiting_launch = signing_launch.clone();
    awaiting_launch.launcher_head = Some(ReceiptHead {
        sequence: 0,
        digest: digest(b"receipt-0"),
    });
    awaiting_launch.pending_operation.as_mut().unwrap().phase = PendingPhase::AwaitingDurableAck;
    awaiting_launch
        .validate()
        .expect("sequence zero may await its durable acknowledgement");

    let mut applying_start = awaiting_launch.clone();
    applying_start.pending_operation.as_mut().unwrap().phase = PendingPhase::Applying;
    applying_start.broker_head = applying_start.launcher_head.clone();
    applying_start.pending_receipt_count = 0;
    applying_start
        .validate()
        .expect("the durable Starting head may gate workload release");

    let mut signing_start = supervisor(SessionState::Running);
    signing_start.launcher_head = awaiting_launch.launcher_head.clone();
    signing_start.broker_head = awaiting_launch.launcher_head.clone();
    signing_start.pending_receipt_count = 1;
    signing_start.pending_operation = Some(PendingOperation {
        request_id: "launch-request".to_owned(),
        action: PendingAction::Launch,
        phase: PendingPhase::Signing,
    });
    signing_start
        .validate()
        .expect("a started workload may be signing its Running receipt");

    let mut awaiting_start = supervisor(SessionState::Running);
    awaiting_start.broker_head = signing_start.broker_head.clone();
    awaiting_start.pending_receipt_count = 1;
    awaiting_start.pending_operation = Some(PendingOperation {
        request_id: "launch-request".to_owned(),
        action: PendingAction::Launch,
        phase: PendingPhase::AwaitingDurableAck,
    });
    awaiting_start
        .validate()
        .expect("the Running receipt may await durable acknowledgement");

    let mut signing_launch_with_head = signing_launch.clone();
    signing_launch_with_head.launcher_head = awaiting_launch.launcher_head.clone();
    let mut awaiting_launch_with_later_head = awaiting_launch.clone();
    awaiting_launch_with_later_head.launcher_head = awaiting_start.launcher_head.clone();
    let mut signing_start_with_later_head = signing_start.clone();
    signing_start_with_later_head.launcher_head = awaiting_start.launcher_head.clone();
    let mut awaiting_start_with_earlier_head = awaiting_start.clone();
    awaiting_start_with_earlier_head.launcher_head = awaiting_launch.launcher_head.clone();
    let stable_starting_with_later_head = supervisor(SessionState::Starting);
    let mut stable_running_with_earlier_head = signing_start.clone();
    stable_running_with_earlier_head.pending_operation = None;
    stable_running_with_earlier_head.pending_receipt_count = 0;
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
        status.launcher_head.as_ref().unwrap().digest,
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
fn receipt_backlog_blocks_resume_but_not_new_narrowing() {
    let mut running = supervisor(SessionState::Running);
    running.launcher_head = Some(ReceiptHead {
        sequence: 2,
        digest: digest(b"receipt-2"),
    });
    running.pending_receipt_count = 1;
    let mut park = lifecycle(LifecycleAction::Park, SessionState::Running);
    park.expected_receipt_sequence = Some(1);
    let disposition = launch_protocol::evaluate_request(&running, None, &park)
        .expect("an audit backlog cannot block a new narrowing Park");
    let intent = disposition
        .execute_intent()
        .expect("Park remains executable");
    assert_eq!(intent.sequence, 3);
    assert_eq!(
        intent.previous_receipt_digest,
        running.launcher_head.as_ref().unwrap().digest,
    );

    let mut parked = running;
    parked.state = SessionState::Parked;
    parked.channel_state = ChannelState::Revoked;
    let mut resume = lifecycle(LifecycleAction::Resume, SessionState::Parked);
    resume.expected_receipt_sequence = Some(1);
    assert_eq!(
        launch_protocol::evaluate_request(&parked, None, &resume)
            .expect_err("unacknowledged audit history must block widening")
            .code,
        ErrorCode::DurabilityUnavailable,
    );
}

#[test]
fn pending_receipt_shape_tracks_the_active_operation_without_hiding_backlog() {
    let mut applying = supervisor(SessionState::Running);
    applying.launcher_head = Some(ReceiptHead {
        sequence: 2,
        digest: digest(b"receipt-2"),
    });
    applying.pending_receipt_count = 1;
    applying.pending_operation = Some(PendingOperation {
        request_id: "park-with-backlog".to_owned(),
        action: PendingAction::Park,
        phase: PendingPhase::Applying,
    });
    applying
        .validate()
        .expect("an existing backlog may coexist with a new narrowing mechanic");

    let mut signing_resume = supervisor(SessionState::Running);
    signing_resume.channel_state = ChannelState::Revoked;
    signing_resume.pending_receipt_count = 1;
    signing_resume.pending_operation = Some(PendingOperation {
        request_id: "resume-request".to_owned(),
        action: PendingAction::Resume,
        phase: PendingPhase::Signing,
    });
    signing_resume
        .validate()
        .expect("Resume is mechanically Running only after the thaw succeeds");

    let mut awaiting_resume = signing_resume.clone();
    awaiting_resume.launcher_head = Some(ReceiptHead {
        sequence: 2,
        digest: digest(b"resume-receipt"),
    });
    awaiting_resume.pending_operation.as_mut().unwrap().phase = PendingPhase::AwaitingDurableAck;
    awaiting_resume
        .validate()
        .expect("the truthful Running receipt may await its exact ACK");

    let mut missing_signed_receipt = signing_resume;
    missing_signed_receipt
        .pending_operation
        .as_mut()
        .unwrap()
        .phase = PendingPhase::AwaitingDurableAck;
    assert_eq!(
        missing_signed_receipt
            .validate()
            .expect_err("awaiting an ACK requires a visible signed head")
            .code,
        ErrorCode::InvalidRequest,
    );
}

#[test]
fn status_parsers_reject_other_schema_generations_before_decoding_their_body() {
    let old_supervisor = br#"{"schema":"louiselm.launch.supervisor-status/1","protocol_version":1,"session_id":"session-1","run_id":"run-1","state":"running","broker_connection":"connected","envelope_revision":7,"channel_state":"enabled","receipt_head":{"sequence":1,"digest":"sha256:fea5396a7f4325c408b1b65b33a4d77ba5486ceba941804d8889a8546cfbab96"},"pending_operation":null,"last_failure":null}"#;
    assert_eq!(
        SupervisorStatus::parse_canonical(old_supervisor)
            .expect_err("a v1 body is unsupported rather than malformed as v2")
            .code,
        ErrorCode::UnsupportedSchema,
    );

    let future_session = br#"{"schema":"louiselm.launch.session-status/99","protocol_version":1,"future_shape":true}"#;
    assert_eq!(
        SessionStatus::parse_canonical(future_session)
            .expect_err("a future body is unsupported before its fields are decoded")
            .code,
        ErrorCode::UnsupportedSchema,
    );
}

#[test]
fn identical_retry_replays_exact_receipt_before_stale_cas() {
    let request = lifecycle(LifecycleAction::Park, SessionState::Running);
    let receipt = signed_receipt(&request);
    let completed = CompletedRequest::new(&request, receipt.clone());
    let mut advanced = supervisor(SessionState::Parked);
    advanced.envelope_revision = 8;
    advanced.launcher_head = Some(ReceiptHead {
        sequence: 2,
        digest: receipt.digest().to_string(),
    });
    advanced.broker_head = advanced.launcher_head.clone();

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
#[expect(
    clippy::too_many_lines,
    reason = "One status and errors have pinned safe wire shapes scenario keeps its causal steps and assertions together."
)]
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
        status_posture(PostureSummary::FullyVerified),
        Vec::new(),
    )
    .expect("broker status is consistent");
    assert_eq!(status.schema, SESSION_STATUS_SCHEMA);
    assert_eq!(
        // Pin the surrounding canonical envelope independently of the nested
        // posture record, whose shape and validation have focused tests below.
        String::from_utf8(status.canonical_bytes())
            .expect("status JSON is UTF-8")
            .replace(
                &serde_json::to_string(&status.posture).unwrap(),
                "\"fully_verified\""
            ),
        r#"{"schema":"louiselm.launch.session-status/4","protocol_version":1,"session_id":"session-1","run_id":"run-1","state":"running","posture":"fully_verified","broker_connection":"connected","envelope_revision":7,"channel_state":"enabled","launcher_head":{"sequence":1,"digest":"sha256:fea5396a7f4325c408b1b65b33a4d77ba5486ceba941804d8889a8546cfbab96"},"broker_head":{"sequence":1,"digest":"sha256:fea5396a7f4325c408b1b65b33a4d77ba5486ceba941804d8889a8546cfbab96"},"pending_receipt_count":0,"pending_operation":{"request_id":"request-2","action":"park","phase":"applying"},"allowed_actions":[],"process_exit":null,"last_failure":{"code":"broker_unavailable","message":"control broker is unavailable","retryable":true,"current_state":"running","expected_sequence":1,"next_action":"reconnect_broker"}}"#,
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
        status_posture(PostureSummary::FullyVerified),
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
    missing_head.launcher_head = None;
    missing_head.broker_head = None;
    assert_eq!(
        missing_head
            .validate()
            .expect_err("post-launch state requires a signed receipt head")
            .code,
        ErrorCode::InvalidRequest,
    );

    let mut unacknowledgeable = supervisor(SessionState::Starting);
    unacknowledgeable.launcher_head = None;
    unacknowledgeable.broker_head = None;
    unacknowledgeable.pending_receipt_count = 1;
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
        disposition: ReceiptDisposition::DurablyStored,
    };
    assert_eq!(
        String::from_utf8(acknowledgement.canonical_bytes())
            .expect("acknowledgement JSON is UTF-8"),
        format!(
            r#"{{"schema":"louiselm.launch.receipt-ack/2","protocol_version":1,"session_id":"session-1","run_id":"run-1","sequence":3,"receipt_digest":"{}","disposition":"durably_stored"}}"#,
            expected.digest,
        ),
    );
    assert_eq!(
        acknowledgement.exact_disposition("session-1", "run-1", &expected),
        Some(ReceiptDisposition::DurablyStored),
    );

    let rejected = ReceiptAcknowledgement {
        disposition: ReceiptDisposition::Rejected,
        ..acknowledgement.clone()
    };
    assert_eq!(
        String::from_utf8(rejected.canonical_bytes()).expect("rejection JSON is UTF-8"),
        format!(
            r#"{{"schema":"louiselm.launch.receipt-ack/2","protocol_version":1,"session_id":"session-1","run_id":"run-1","sequence":3,"receipt_digest":"{}","disposition":"rejected"}}"#,
            expected.digest,
        ),
    );
    assert_eq!(
        rejected.exact_disposition("session-1", "run-1", &expected),
        Some(ReceiptDisposition::Rejected),
    );
    for exact in [&acknowledgement, &rejected] {
        assert_eq!(
            launch_protocol::decode_message(&exact.canonical_bytes())
                .expect("each closed receipt disposition decodes"),
            ProtocolMessage::ReceiptAcknowledgement(exact.clone()),
        );
    }

    let mut wrong = acknowledgement.clone();
    wrong.receipt_digest = digest(b"another-receipt");
    assert_eq!(
        wrong.exact_disposition("session-1", "run-1", &expected),
        None,
    );
    wrong = acknowledgement.clone();
    wrong.sequence += 1;
    assert_eq!(
        wrong.exact_disposition("session-1", "run-1", &expected),
        None,
    );
    assert_eq!(
        acknowledgement.exact_disposition("another-session", "run-1", &expected),
        None,
    );
    assert_eq!(
        acknowledgement.exact_disposition("session-1", "another-run", &expected),
        None,
    );

    let canonical = String::from_utf8(acknowledgement.canonical_bytes()).expect("JSON is UTF-8");
    let invalid_messages = [
        (
            canonical.replace(RECEIPT_ACK_SCHEMA, "louiselm.launch.receipt-ack/1"),
            ErrorCode::UnsupportedSchema,
            "the positive-only v1 schema is not silently treated as durable",
        ),
        (
            canonical.replace(r#","disposition":"durably_stored""#, ""),
            ErrorCode::MalformedMessage,
            "a v2 disposition is required",
        ),
        (
            canonical.replace("durably_stored", "unknown"),
            ErrorCode::MalformedMessage,
            "the disposition enum is closed",
        ),
        (
            canonical.replace(r#""sequence":3"#, r#""sequence":3,"durable":true"#),
            ErrorCode::MalformedMessage,
            "acknowledgement fields are closed",
        ),
    ];
    for (bytes, code, message) in invalid_messages {
        assert_eq!(
            launch_protocol::decode_message(bytes.as_bytes())
                .expect_err(message)
                .code,
            code,
            "case {message}",
        );
    }
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
                conformance: ConformanceEvidence::Unevaluated,
                launch_request_digest,
                runtime_measurement_digest: digest(b"runtime"),
                skill_generation_id: digest(b"generation"),
                session_input_manifest_id: digest(b"input"),
                isolation_contract: "louiselm.isolation/1".to_owned(),
                isolation_backend_id: "bubblewrap-0_12".to_owned(),
                kernel_identity: "linux-6_18".to_owned(),
                isolation_evidence_digest: digest(b"isolation"),
                broker_loss_grace_ms: MAX_BROKER_LOSS_GRACE_MS,
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

#[test]
fn identity_exhaustion_is_bounded_sorted_operator_evidence_and_redacted_from_self_status() {
    assert_eq!(MAX_IDENTITY_OCCUPANTS, 32);
    let occupants = (0..MAX_IDENTITY_OCCUPANTS + 2)
        .rev()
        .map(|slot| OccupiedSessionIdentity {
            session_id: format!("occupied-session-{slot:03}"),
            state: if slot % 2 == 0 {
                SessionState::Running
            } else {
                SessionState::Parked
            },
            slot: u32::try_from(slot).expect("test slot fits u32"),
        })
        .collect();
    let exhaustion = IdentityExhaustion::compose(occupants)
        .expect("broker composition sorts and bounds valid occupancy evidence");

    assert_eq!(exhaustion.occupied_sessions.len(), MAX_IDENTITY_OCCUPANTS);
    assert!(exhaustion.truncated);
    assert_eq!(
        exhaustion
            .occupied_sessions
            .iter()
            .map(|occupant| occupant.slot)
            .collect::<Vec<_>>(),
        (0..u32::try_from(MAX_IDENTITY_OCCUPANTS).expect("bound fits u32")).collect::<Vec<_>>(),
    );
    assert_eq!(
        exhaustion.error,
        ProtocolError::new(ErrorCode::SessionIdentityExhausted, None, None),
    );
    let invalid_assignment = ProtocolError::new(ErrorCode::IdentityAssignmentInvalid, None, None);
    assert_eq!(
        (
            invalid_assignment.message.as_str(),
            invalid_assignment.retryable,
            invalid_assignment.next_action,
        ),
        (
            "broker-assigned session identity is invalid",
            false,
            NextAction::ContactOperator,
        ),
    );

    let operator_response = ProtocolResponse {
        schema: RESPONSE_SCHEMA.to_owned(),
        protocol_version: PROTOCOL_VERSION,
        request_id: "launch-request-1".to_owned(),
        result: ResponseResult::IdentityExhaustion {
            exhaustion: exhaustion.clone(),
        },
    };
    operator_response
        .validate()
        .expect("authenticated operator response carries bounded evidence");
    let operator_json = String::from_utf8(operator_response.canonical_bytes())
        .expect("operator response JSON is UTF-8");
    assert!(operator_json.contains("occupied-session-000"));
    assert!(operator_json.contains(r#""state":"running","slot":0"#));
    for forbidden in [
        "assigned_uid",
        "assigned_gid",
        "pid",
        "path",
        "environment",
        "prompt",
    ] {
        assert!(
            !operator_json.contains(forbidden),
            "operator evidence leaked forbidden metadata: {forbidden}",
        );
    }

    let mut self_supervisor = supervisor(SessionState::Running);
    self_supervisor.last_failure = Some(exhaustion.redacted_error());
    let self_status = SessionStatus::compose(
        self_supervisor,
        status_posture(PostureSummary::FullyVerified),
        vec![LifecycleAction::Park, LifecycleAction::Disposal],
    )
    .expect("redacted failure remains valid canonical self status");
    let self_json =
        String::from_utf8(self_status.canonical_bytes()).expect("self status JSON is UTF-8");
    assert!(self_json.contains("session_identity_exhausted"));
    assert!(!self_json.contains("occupied-session-"));
    assert!(!self_json.contains(r#""slot""#));

    let mut injected_self_status = self_json;
    assert_eq!(injected_self_status.pop(), Some('}'));
    injected_self_status.push_str(
        r#","occupied_sessions":[{"session_id":"other-session","state":"running","slot":2}]}"#,
    );
    assert_eq!(
        SessionStatus::parse_canonical(injected_self_status.as_bytes())
            .expect_err("Agent/self status has no operator-evidence field")
            .code,
        ErrorCode::MalformedMessage,
    );
}

#[test]
fn identity_exhaustion_rejects_unbounded_unsorted_or_duplicate_occupancy_evidence() {
    let occupant = |session_id: &str, state, slot| OccupiedSessionIdentity {
        session_id: session_id.to_owned(),
        state,
        slot,
    };
    let exhaustion = IdentityExhaustion::compose(vec![
        occupant("session-b", SessionState::Parked, 7),
        occupant("session-a", SessionState::Running, 2),
    ])
    .expect("broker composition canonicalizes valid evidence");
    assert_eq!(
        exhaustion
            .occupied_sessions
            .iter()
            .map(|entry| entry.slot)
            .collect::<Vec<_>>(),
        [2, 7],
    );

    let mut unsorted = exhaustion.clone();
    unsorted.occupied_sessions.swap(0, 1);
    assert_eq!(
        unsorted
            .validate()
            .expect_err("received evidence must already be canonically sorted")
            .code,
        ErrorCode::InvalidRequest,
    );

    let mut duplicate_slot = exhaustion.clone();
    duplicate_slot.occupied_sessions[1].slot = duplicate_slot.occupied_sessions[0].slot;
    assert_eq!(
        duplicate_slot
            .validate()
            .expect_err("one occupied slot cannot name two Sessions")
            .code,
        ErrorCode::InvalidRequest,
    );

    let mut duplicate_session = exhaustion.clone();
    duplicate_session.occupied_sessions[1].session_id =
        duplicate_session.occupied_sessions[0].session_id.clone();
    assert_eq!(
        duplicate_session
            .validate()
            .expect_err("one Session cannot occupy two identity slots")
            .code,
        ErrorCode::InvalidRequest,
    );

    for state in [
        SessionState::Starting,
        SessionState::Running,
        SessionState::Parked,
    ] {
        IdentityExhaustion::compose(vec![occupant("nonterminal-session", state, 1)])
            .unwrap_or_else(|_| panic!("{state:?} is a nonterminal identity holder"));
    }
    assert_eq!(
        IdentityExhaustion::compose(vec![occupant(
            "terminal-session",
            SessionState::Terminal,
            1,
        )])
        .expect_err("terminal Sessions cannot be reported as live occupants")
        .code,
        ErrorCode::InvalidRequest,
    );

    let mut too_many = IdentityExhaustion::compose(
        (0..MAX_IDENTITY_OCCUPANTS)
            .map(|slot| {
                occupant(
                    &format!("session-{slot:03}"),
                    SessionState::Running,
                    u32::try_from(slot).expect("test slot fits u32"),
                )
            })
            .collect(),
    )
    .expect("the exact evidence bound is accepted");
    too_many.occupied_sessions.push(occupant(
        "session-over-bound",
        SessionState::Running,
        u32::try_from(MAX_IDENTITY_OCCUPANTS).expect("bound fits u32"),
    ));
    assert_eq!(
        too_many
            .validate()
            .expect_err("received evidence cannot exceed the fixed disclosure bound")
            .code,
        ErrorCode::InvalidRequest,
    );
}

#[test]
fn identity_exhaustion_has_one_closed_wire_shape() {
    let exhaustion = IdentityExhaustion::compose(vec![OccupiedSessionIdentity {
        session_id: "occupied-session".to_owned(),
        state: SessionState::Running,
        slot: 2,
    }])
    .expect("operator evidence is valid");
    let response = ProtocolResponse {
        schema: RESPONSE_SCHEMA.to_owned(),
        protocol_version: PROTOCOL_VERSION,
        request_id: "launch-request".to_owned(),
        result: ResponseResult::IdentityExhaustion { exhaustion },
    };
    assert_eq!(
        ProtocolResponse::parse_canonical(&response.canonical_bytes())
            .expect("closed operator evidence round trips"),
        response,
    );

    let canonical = String::from_utf8(response.canonical_bytes()).expect("response JSON is UTF-8");
    for (injected, reason) in [
        (
            canonical.replacen(r#""slot":2}"#, r#""slot":2,"uid":200002}"#, 1),
            "occupant metadata is closed",
        ),
        (
            canonical.replacen(
                r#""truncated":false}"#,
                r#""truncated":false,"path":"/secret"}"#,
                1,
            ),
            "exhaustion metadata is closed",
        ),
    ] {
        assert_eq!(
            ProtocolResponse::parse_canonical(injected.as_bytes())
                .expect_err(reason)
                .code,
            ErrorCode::MalformedMessage,
            "{reason}",
        );
    }

    let generic = ProtocolResponse {
        schema: RESPONSE_SCHEMA.to_owned(),
        protocol_version: PROTOCOL_VERSION,
        request_id: "launch-request".to_owned(),
        result: ResponseResult::Error {
            error: ProtocolError::new(ErrorCode::SessionIdentityExhausted, None, None),
        },
    };
    assert_eq!(
        ProtocolResponse::parse_canonical(&generic.canonical_bytes())
            .expect_err("generic errors cannot erase required operator evidence")
            .code,
        ErrorCode::InvalidRequest,
    );
}
