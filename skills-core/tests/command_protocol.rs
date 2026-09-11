//! Closed broker/supervisor command records, independent of process mechanics.
#![allow(
    clippy::unwrap_used,
    reason = "Tests assert fixture construction and wire results."
)]

use louiselm_skills::launch_protocol::{
    COMMAND_SCHEMA, CommandMessage, CommandOperation, CommandPrincipal, ProtocolMessage,
    TOOL_EXECUTION_SCHEMA, ToolExecutionRequest, decode_message,
};

fn request() -> CommandMessage {
    CommandMessage {
        schema: COMMAND_SCHEMA.to_owned(),
        protocol_version: 1,
        request_id: "command-1".to_owned(),
        session_id: "session-1".to_owned(),
        run_id: "run-1".to_owned(),
        envelope_revision: 1,
        operation: CommandOperation::Request {
            principal: CommandPrincipal {
                channel_id: "agent-capability".to_owned(),
                pid: 123,
                uid: 1001,
                gid: 1001,
            },
            command: ToolExecutionRequest {
                schema: TOOL_EXECUTION_SCHEMA.to_owned(),
                protocol_version: 1,
                request_id: "command-1".to_owned(),
                session_id: "session-1".to_owned(),
                run_id: "run-1".to_owned(),
                envelope_revision: 1,
                sequence: 1,
                command: "printf private".to_owned(),
                timeout_ms: 1000,
            },
        },
    }
}

#[test]
fn attributed_command_round_trips_on_the_existing_transport_protocol() {
    let request = request();
    assert_eq!(
        decode_message(&request.canonical_bytes()).unwrap(),
        ProtocolMessage::Command(request)
    );
}

#[test]
fn neither_peer_identity_fields_nor_contradictory_nested_subjects_are_accepted() {
    let wire = serde_json::to_value(request()).unwrap();
    for pointer in [
        "/extra",
        "/operation/extra",
        "/operation/principal/claimed_pid",
    ] {
        let mut value = wire.clone();
        let (parent, field) = pointer.rsplit_once('/').unwrap();
        value.pointer_mut(parent).unwrap()[field] = true.into();
        assert!(decode_message(&serde_json::to_vec(&value).unwrap()).is_err());
    }
    for (field, changed) in [
        ("request_id", serde_json::json!("other")),
        ("session_id", serde_json::json!("other")),
        ("run_id", serde_json::json!("other")),
        ("envelope_revision", serde_json::json!(2)),
        ("sequence", serde_json::json!(0)),
    ] {
        let mut value = wire.clone();
        value["operation"]["command"][field] = changed;
        assert!(
            decode_message(&serde_json::to_vec(&value).unwrap()).is_err(),
            "{field}"
        );
    }
}

#[test]
fn authorization_has_bounded_lifetime_and_separate_dispatch_sequence() {
    let mut message = request();
    message.operation = CommandOperation::Authorize {
        principal: CommandPrincipal {
            channel_id: "agent-capability".to_owned(),
            pid: 123,
            uid: 1001,
            gid: 1001,
        },
        principal_sequence: 1,
        dispatch_sequence: 7,
        command_digest: louiselm_skills::Digest::of(b"printf private").to_string(),
        timeout_ms: 1000,
        valid_for_ms: 500,
    };
    assert!(decode_message(&message.canonical_bytes()).is_ok());
    let wire = serde_json::to_value(message).unwrap();
    for (field, value) in [
        ("principal_sequence", 0),
        ("dispatch_sequence", 0),
        ("valid_for_ms", 0),
        ("valid_for_ms", 30_001),
        ("timeout_ms", 0),
    ] {
        let mut changed = wire.clone();
        changed["operation"][field] = value.into();
        assert!(
            decode_message(&serde_json::to_vec(&changed).unwrap()).is_err(),
            "{field}"
        );
    }
}
