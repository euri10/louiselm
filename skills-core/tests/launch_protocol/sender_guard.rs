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
