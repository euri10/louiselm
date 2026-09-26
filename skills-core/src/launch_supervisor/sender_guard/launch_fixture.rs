//! Offline signed-launch/status double for the privileged handoff composition.
use super::tests::number;
use super::*;
use crate::launch_transport::LauncherPacket;
use crate::{Digest, launch_protocol::*, launch_receipt::*};
use serde_json::Value;
use std::sync::mpsc;

fn send(channel: &SeqpacketChannel, bytes: Vec<u8>) {
    let (tx, rx) = mpsc::channel();
    channel
        .send(bytes, Box::new(move |value| tx.send(value).unwrap()))
        .unwrap();
    rx.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
}

fn receive(channel: &SeqpacketChannel) -> LauncherPacket {
    let (tx, rx) = mpsc::channel();
    channel
        .receive(Box::new(move |value| tx.send(value).unwrap()))
        .unwrap();
    rx.recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap()
        .packet
}

fn signed(payload: ReceiptPayload) -> SignedReceipt {
    SignedReceipt {
        schema: SIGNED_RECEIPT_SCHEMA.into(),
        signature: Digest::of(&payload.canonical_bytes()).to_string(),
        payload,
    }
}

pub(super) fn launch(channel: &SeqpacketChannel, setup: &Value) -> Option<SupervisorStatus> {
    let request = setup.get("launch_request")?;
    let request: crate::launch::LaunchRequest = serde_json::from_value(request.clone()).unwrap();
    send(channel, request.canonical_bytes());
    let LauncherPacket::Response(reply) = receive(channel) else {
        panic!("authorization")
    };
    let ResponseResult::LaunchAuthorization { authorization } = reply.result else {
        panic!("authorization")
    };
    let launch = signed(ReceiptPayload {
        schema: RECEIPT_SCHEMA.into(),
        session_id: authorization.session_id.clone(),
        run_id: authorization.run_id.clone(),
        request_id: authorization.request_id.clone(),
        envelope_revision: authorization.envelope_revision,
        sequence: 0,
        previous_receipt_digest: None,
        release_id: Digest::of(b"release").to_string(),
        signing_key_id: Digest::of(b"launcher-key").to_string(),
        outcome: ReceiptOutcome::Launch {
            authorization: Authorization {
                authorization_id: authorization.authorization_id.clone(),
                request_id: authorization.request_id.clone(),
                request_digest: authorization.request_digest.clone(),
            },
            evidence: Box::new(LaunchEvidence {
                conformance: ConformanceEvidence::Unevaluated,
                launch_request_digest: authorization.request_digest.clone(),
                runtime_measurement_digest: Digest::of(b"runtime").to_string(),
                skill_generation_id: Digest::of(b"generation").to_string(),
                session_input_manifest_id: Digest::of(b"input").to_string(),
                isolation_contract: "louiselm.isolation/2".into(),
                isolation_backend_id: "bubblewrap-0_12".into(),
                kernel_identity: "linux-6_12".into(),
                isolation_evidence_digest: Digest::of(b"isolation").to_string(),
                broker_loss_grace_ms: authorization.broker_loss_grace_ms,
                capability_channel_ids: vec!["acp".into(), "broker".into()],
            }),
        },
        resulting_state: SessionState::Starting,
    });
    send(channel, launch.canonical_bytes());
    assert!(matches!(
        receive(channel),
        LauncherPacket::Request(ProtocolMessage::ReceiptAcknowledgement(_))
    ));
    let mut start = launch.payload.clone();
    start.request_id = "start-after-launch".into();
    start.sequence = 1;
    start.previous_receipt_digest = Some(launch.digest().to_string());
    start.resulting_state = SessionState::Running;
    start.outcome = ReceiptOutcome::Start {
        evidence: StartEvidence {
            agent_pid: number(setup, "runtime"),
            assigned_uid: authorization.assigned_uid,
            assigned_gid: authorization.assigned_gid,
            tool_isolation_digest: Digest::of(b"fixture-tool-isolation").to_string(),
        },
        authority: ReceiptAuthority::Cause {
            cause: ReceiptCause::LaunchAcknowledged,
        },
    };
    let start = signed(start);
    send(channel, start.canonical_bytes());
    assert!(matches!(
        receive(channel),
        LauncherPacket::Request(ProtocolMessage::ReceiptAcknowledgement(_))
    ));
    let head = ReceiptHead {
        sequence: 1,
        digest: start.digest().to_string(),
    };
    Some(SupervisorStatus {
        schema: SUPERVISOR_STATUS_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        session_id: authorization.session_id,
        run_id: authorization.run_id,
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
    })
}

pub(super) fn status(channel: &SeqpacketChannel, current: &SupervisorStatus) {
    let LauncherPacket::Request(ProtocolMessage::Status(query)) = receive(channel) else {
        panic!("status request")
    };
    send(
        channel,
        ProtocolResponse {
            schema: RESPONSE_SCHEMA.into(),
            protocol_version: PROTOCOL_VERSION,
            request_id: query.request_id,
            result: ResponseResult::SupervisorStatus {
                status: current.clone(),
            },
        }
        .canonical_bytes(),
    );
}
