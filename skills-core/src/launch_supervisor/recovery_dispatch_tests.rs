//! Delayed retention uses the actual socket callback and serialized owner.
//! Mechanical storage is held here; the installed VM covers its real boundary.

use super::*;
use crate::{
    launch::{LaunchRequest, REQUEST_SCHEMA},
    launch_protocol::{
        RECOVERY_REQUEST_SCHEMA, RETENTION_EVIDENCE_SCHEMA, RecoveryRequest, ResponseResult,
        RetentionEvidence, RetentionRequest,
    },
    launch_receipt::{Authorization, LaunchEvidence, ReceiptHead},
};

fn parked() -> (Harness, RecoveryRequest) {
    let mut harness = Harness::new("printf exact", false);
    let launch = LaunchRequest {
        schema: REQUEST_SCHEMA.into(),
        protocol_version: 1,
        request_id: "launch".into(),
        authorization_id: "launch-grant".into(),
        session_id: "session-1".into(),
        run_id: "run-1".into(),
        agent_id: "fixture".into(),
        envelope_id: "empty".into(),
        envelope_revision: 1,
        skill_generation_id: Digest::of(b"generation").to_string(),
        session_input_manifest_id: Digest::of(b"input").to_string(),
    };
    let mut genesis = harness.owner.receipts[0].clone();
    genesis.payload.sequence = 0;
    genesis.payload.previous_receipt_digest = None;
    genesis.payload.resulting_state = SessionState::Starting;
    genesis.payload.outcome = ReceiptOutcome::Launch {
        authorization: Authorization {
            authorization_id: launch.authorization_id.clone(),
            request_id: launch.request_id.clone(),
            request_digest: launch.digest().to_string(),
        },
        evidence: Box::new(LaunchEvidence {
            launch_request_digest: launch.digest().to_string(),
            runtime_measurement_digest: Digest::of(b"runtime").to_string(),
            skill_generation_id: launch.skill_generation_id.clone(),
            session_input_manifest_id: launch.session_input_manifest_id.clone(),
            isolation_contract: "louiselm.isolation/1".into(),
            isolation_backend_id: "fixture".into(),
            kernel_identity: "fixture".into(),
            isolation_evidence_digest: Digest::of(b"isolation").to_string(),
            broker_loss_grace_ms: 0,
            capability_channel_ids: vec!["acp".into(), "broker".into()],
        }),
    };
    harness.owner.receipts[0].payload.previous_receipt_digest = Some(genesis.digest().to_string());
    harness.owner.receipts.insert(0, genesis);
    let mut park = harness.owner.receipts[1].clone();
    park.payload.sequence = 2;
    park.payload.previous_receipt_digest = Some(harness.owner.receipts[1].digest().to_string());
    park.payload.outcome = ReceiptOutcome::Park {
        authority: ReceiptAuthority::Authorized(Authorization {
            authorization_id: "park".into(),
            request_id: "park".into(),
            request_digest: Digest::of(b"park").to_string(),
        }),
    };
    park.payload.resulting_state = SessionState::Parked;
    harness.owner.broker_head = ReceiptHead {
        sequence: 2,
        digest: park.digest().to_string(),
    };
    harness.owner.receipts.push(park);
    harness.owner.state = SessionState::Parked;
    harness.owner.channel_state = ChannelState::Revoked;
    let request = RecoveryRequest {
        schema: RECOVERY_REQUEST_SCHEMA.into(),
        protocol_version: 1,
        launch,
        head: harness.owner.broker_head.clone(),
        retention: RetentionRequest {
            request_id: "retain".into(),
            acp_session_id: "acp".into(),
            expires_at_ms: u64::MAX,
        },
    };
    (harness, request)
}

#[test]
fn asynchronous_retention_completes_only_at_its_original_park_checkpoint() {
    for late in [false, true] {
        let (mut harness, request) = parked();
        settle(|complete| harness.broker.send(request.canonical_bytes(), complete));
        while harness.recovery.lock().unwrap().is_none() {
            harness.tick();
        }
        let (retention, complete) = harness.recovery.lock().unwrap().take().unwrap();
        assert_eq!(retention, request.retention);
        if late {
            harness.owner.state = SessionState::Terminal;
        }
        let evidence = RetentionEvidence {
            schema: RETENTION_EVIDENCE_SCHEMA.into(),
            launch: request.launch,
            request: retention,
            contract: "louiselm.test-recovery/1".into(),
            integration_digest: Digest::of(b"fixture-tool-isolation").to_string(),
            material_digest: Digest::of(b"material").to_string(),
        };
        std::thread::spawn(move || complete(Ok(evidence)))
            .join()
            .unwrap();
        harness.tick();
        let reply = settle(|complete| harness.broker.receive(complete));
        let LauncherPacket::Response(reply) = reply.packet else {
            panic!("retention response");
        };
        assert_eq!(reply.request_id, "retain");
        assert_eq!(
            matches!(reply.result, ResponseResult::RecoveryRetention { .. }),
            !late
        );
        assert_eq!(
            harness.owner.state,
            if late {
                SessionState::Terminal
            } else {
                SessionState::Parked
            }
        );
    }
}

#[test]
fn cold_restore_completion_cannot_revive_disposal_or_a_replaced_connection() {
    for late in ["none", "terminal", "connection", "process"] {
        let (mut harness, target) = parked();
        let mut source = target.launch.clone();
        source.session_id = "source".into();
        source.authorization_id = "source-authorization".into();
        source.request_id = "source-request".into();
        let request = crate::launch_protocol::RecoveryRestoreRequest {
            schema: crate::launch_protocol::RECOVERY_RESTORE_SCHEMA.into(),
            protocol_version: 1,
            request_id: "restore".into(),
            target: target.launch,
            head: target.head,
            source: RetentionEvidence {
                schema: RETENTION_EVIDENCE_SCHEMA.into(),
                launch: source,
                request: target.retention,
                contract: "louiselm.test-recovery/1".into(),
                integration_digest: Digest::of(b"integration").to_string(),
                material_digest: Digest::of(b"material").to_string(),
            },
        };
        settle(|complete| harness.broker.send(request.canonical_bytes(), complete));
        while harness.restore.lock().unwrap().is_none() {
            harness.tick();
        }
        let (observed, complete) = harness.restore.lock().unwrap().take().unwrap();
        assert_eq!(observed, request);
        match late {
            "terminal" => harness.owner.state = SessionState::Terminal,
            "connection" => harness.owner.connection_epoch += 1,
            "process" => harness.owner.process_epoch += 1,
            _ => {}
        }
        let head = harness.owner.broker_head.clone();
        std::thread::spawn(move || complete(Ok(observed)))
            .join()
            .unwrap();
        harness.tick();
        if matches!(late, "none" | "terminal") {
            let reply = settle(|complete| harness.broker.receive(complete));
            let LauncherPacket::Response(reply) = reply.packet else {
                panic!("restore response")
            };
            assert_eq!(
                matches!(reply.result, ResponseResult::RecoveryRestored { .. }),
                late == "none"
            );
        }
        assert_eq!(harness.owner.broker_head, head);
        assert_eq!(harness.owner.channel_state, ChannelState::Revoked);
        assert_eq!(
            harness.owner.state,
            if late == "terminal" {
                SessionState::Terminal
            } else {
                SessionState::Parked
            }
        );
    }
}
