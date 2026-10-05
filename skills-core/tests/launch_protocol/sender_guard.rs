use louiselm_skills::launch_protocol::{
    self, ErrorCode, GUARD_SOCKET_REQUEST_SCHEMA, GUARD_SOCKET_RETIRE_SCHEMA, GuardEnrollment,
    GuardScope, GuardSocketRequest, GuardSocketRetire, PROTOCOL_VERSION, ProtocolMessage,
};

fn request() -> GuardSocketRequest {
    GuardSocketRequest {
        schema: GUARD_SOCKET_REQUEST_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        request_id: "guard-upstream-1".into(),
        enrollment: GuardEnrollment {
            scope: GuardScope {
                session_id: "session".into(),
                run_id: "run".into(),
                envelope_revision: 1,
                revision: 1,
                deadline_ns: 1,
            },
            guard_id: 1,
            runtime_pid: 2,
            broker_pid: 3,
            address: "127.0.0.1:40773".parse().unwrap(),
            listener_cookie: 4,
            network_id: 5,
        },
        destination: "192.0.2.1:443".parse().unwrap(),
    }
}

#[test]
fn guarded_socket_request_is_closed_and_bound_to_one_enrollment() {
    let request = request();
    assert_eq!(
        launch_protocol::decode_message(&request.canonical_bytes()).unwrap(),
        ProtocolMessage::GuardSocketRequest(request.clone())
    );
    let mut changed = serde_json::to_value(&request).unwrap();
    changed["destination"] = serde_json::json!("0.0.0.0:443");
    assert_eq!(
        launch_protocol::decode_message(&serde_json::to_vec(&changed).unwrap())
            .unwrap_err()
            .code,
        ErrorCode::InvalidRequest
    );
    changed["destination"] = serde_json::json!("192.0.2.1:443");
    changed["unknown"] = serde_json::json!(true);
    assert_eq!(
        launch_protocol::decode_message(&serde_json::to_vec(&changed).unwrap())
            .unwrap_err()
            .code,
        ErrorCode::MalformedMessage
    );
}

#[test]
fn guarded_socket_retirement_is_closed_and_scoped() {
    let request = request();
    let retire = GuardSocketRetire {
        schema: GUARD_SOCKET_RETIRE_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        enrollment: request.enrollment,
        socket_cookie: 9,
    };
    assert_eq!(
        launch_protocol::decode_message(&retire.canonical_bytes()).unwrap(),
        ProtocolMessage::GuardSocketRetire(retire.clone())
    );
    let mut changed = serde_json::to_value(&retire).unwrap();
    changed["socket_cookie"] = serde_json::json!(0);
    assert_eq!(
        launch_protocol::decode_message(&serde_json::to_vec(&changed).unwrap())
            .unwrap_err()
            .code,
        ErrorCode::InvalidRequest
    );
    changed["socket_cookie"] = serde_json::json!(9);
    changed["unknown"] = serde_json::json!(true);
    assert_eq!(
        launch_protocol::decode_message(&serde_json::to_vec(&changed).unwrap())
            .unwrap_err()
            .code,
        ErrorCode::MalformedMessage
    );
}

#[test]
fn guarded_resume_binds_fresh_network_authority_to_the_original_envelope_and_park() {
    let message = serde_json::json!({
        "schema": "louiselm.launch.guard-resume/1",
        "protocol_version": PROTOCOL_VERSION,
        "request": {
            "schema": launch_protocol::LIFECYCLE_REQUEST_SCHEMA,
            "protocol_version": PROTOCOL_VERSION,
            "request_id": "resume", "session_id": "session", "run_id": "run",
            "authorization_id": "operator-resume", "action": "resume",
            "expected_state": "parked", "expected_receipt_sequence": 2,
            "envelope_revision": 7,
        },
        "parked_head": {"sequence": 2, "digest": super::digest(b"durable-park")},
        "scope": {
            "session_id": "session", "run_id": "run", "envelope_revision": 7,
            "revision": 3, "deadline_ns": 10,
        },
    });
    let decoded = launch_protocol::decode_message(&serde_json::to_vec(&message).unwrap());
    assert!(decoded.is_ok(), "fresh authority refused: {decoded:?}");
    for (pointer, value) in [
        ("/scope/session_id", serde_json::json!("other")),
        ("/scope/run_id", serde_json::json!("other")),
        ("/scope/envelope_revision", serde_json::json!(8)),
        ("/scope/revision", serde_json::json!(0)),
        ("/scope/deadline_ns", serde_json::json!(0)),
        ("/request/action", serde_json::json!("park")),
        ("/request/expected_state", serde_json::json!("running")),
        ("/parked_head/sequence", serde_json::json!(3)),
        ("/parked_head/digest", serde_json::json!("not-a-digest")),
    ] {
        let mut changed = message.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        assert_eq!(
            launch_protocol::decode_message(&serde_json::to_vec(&changed).unwrap())
                .unwrap_err()
                .code,
            ErrorCode::InvalidRequest,
            "invalid binding {pointer} was not refused",
        );
    }
    let mut changed = message;
    changed["scope"]["unknown"] = serde_json::json!(true);
    assert_eq!(
        launch_protocol::decode_message(&serde_json::to_vec(&changed).unwrap())
            .unwrap_err()
            .code,
        ErrorCode::MalformedMessage,
    );
}
