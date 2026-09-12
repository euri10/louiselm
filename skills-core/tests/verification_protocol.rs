//! Closed exact-job authority and normalized observations, not self-attested success.
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "Tests assert fixture construction and refusal contracts."
)]

use louiselm_skills::{
    Digest,
    launch::{LaunchRequest, PROTOCOL_VERSION, REQUEST_SCHEMA},
    launch_protocol::{
        self, ProtocolMessage, ProtocolResponse, RESPONSE_SCHEMA, ResponseResult,
        VERIFICATION_SCHEMA, VerificationExecution, VerificationOperation, VerificationRequest,
        VerificationStep,
    },
    launch_receipt::ReceiptHead,
    workspace::verification::JobPreview,
};

fn request() -> VerificationRequest {
    VerificationRequest {
        schema: VERIFICATION_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        request_id: "verify".into(),
        launch: LaunchRequest {
            schema: REQUEST_SCHEMA.into(),
            protocol_version: PROTOCOL_VERSION,
            request_id: "launch".into(),
            authorization_id: "authorization".into(),
            session_id: "verifier".into(),
            run_id: "run".into(),
            agent_id: "agent".into(),
            envelope_id: "envelope".into(),
            envelope_revision: 1,
            skill_generation_id: Digest::of(b"generation").to_string(),
            session_input_manifest_id: Digest::of(b"input").to_string(),
        },
        head: ReceiptHead {
            sequence: 1,
            digest: Digest::of(b"receipt").to_string(),
        },
        expires_at_ms: 10000,
        operation: VerificationOperation::Run {
            producer_session_id: "producer".into(),
            export_request_id: "export".into(),
            export_digest: Digest::of(b"export").to_string(),
            job_digest: Digest::of(b"job").to_string(),
        },
    }
}

fn evidence() -> VerificationExecution {
    VerificationExecution {
        request: request(),
        job: JobPreview {
            schema: "louiselm.workspace.verification-preview/1".into(),
            state: "prepared".into(),
            job_digest: Digest::of(b"job").to_string(),
            snapshot_digest: Digest::of(b"snapshot").to_string(),
            bundle_digest: Digest::of(b"bundle").to_string(),
            base_digest: Digest::of(b"base").to_string(),
            result_digest: Digest::of(b"result").to_string(),
            plan_digest: Digest::of(b"plan").to_string(),
            command_count: 2,
        },
        integration_digest: Digest::of(b"integration").to_string(),
        steps: vec![
            VerificationStep::Completed {
                exit_code: 0,
                timed_out: false
            };
            2
        ],
        cleanup_proven: true,
        interrupted: false,
    }
}

#[test]
fn exact_operation_roundtrips_but_same_session_stale_head_and_open_fields_refuse() {
    let request = request();
    assert!(
        matches!(launch_protocol::decode_message(&request.canonical_bytes()).unwrap(), ProtocolMessage::Verification(parsed) if parsed == request)
    );
    let mut same = request.clone();
    same.launch.session_id = "producer".into();
    assert!(same.validate().is_err());
    for sequence in [0, 2, u64::MAX] {
        let mut stale = request.clone();
        stale.head.sequence = sequence;
        assert!(stale.validate().is_err());
    }
    let mut wire = serde_json::to_value(&request).unwrap();
    wire["operation"]["command"] = "arbitrary host effects".into();
    assert!(launch_protocol::decode_message(&serde_json::to_vec(&wire).unwrap()).is_err());
}

#[test]
fn every_required_step_and_cleanup_are_necessary_and_responses_are_correlated() {
    let evidence = evidence();
    assert!(evidence.commands_passed());
    for mutation in 0..7 {
        let mut failed = evidence.clone();
        match mutation {
            0 => {
                failed.steps.pop();
            }
            1 => failed.steps[0] = VerificationStep::Unknown,
            2 => {
                failed.steps[0] = VerificationStep::Completed {
                    exit_code: 1,
                    timed_out: false,
                }
            }
            3 => {
                failed.steps[0] = VerificationStep::Completed {
                    exit_code: 0,
                    timed_out: true,
                }
            }
            4 => failed.cleanup_proven = false,
            5 => failed.interrupted = true,
            _ => failed.job.job_digest = Digest::of(b"substituted").to_string(),
        }
        assert!(!failed.commands_passed());
    }
    let mut response = ProtocolResponse {
        schema: RESPONSE_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        request_id: "verify".into(),
        result: ResponseResult::VerificationExecution { evidence },
    };
    assert!(response.validate().is_ok());
    response.request_id = "another".into();
    assert!(response.validate().is_err());
}

#[test]
fn transfer_selects_only_retained_ids_and_never_accepts_a_host_destination() {
    let mut transfer = request();
    transfer.operation = VerificationOperation::Transfer {
        export_request_id: "export".into(),
        export_digest: Digest::of(b"export").to_string(),
        job_digest: Digest::of(b"job").to_string(),
    };
    assert!(
        matches!(launch_protocol::decode_message(&transfer.canonical_bytes()).unwrap(), ProtocolMessage::Verification(value) if value == transfer)
    );
    let mut wire = serde_json::to_value(&transfer).unwrap();
    wire["operation"]["destination"] = "/operator/checkout".into();
    assert!(launch_protocol::decode_message(&serde_json::to_vec(&wire).unwrap()).is_err());
    for invalid in ["../export", "/export", "", "export/file"] {
        let mut rejected = transfer.clone();
        if let VerificationOperation::Transfer {
            export_request_id, ..
        } = &mut rejected.operation
        {
            *export_request_id = invalid.into();
        }
        assert!(rejected.validate().is_err());
    }
    let mut response = ProtocolResponse {
        schema: RESPONSE_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        request_id: transfer.request_id.clone(),
        result: ResponseResult::VerificationTransfer {
            request: Box::new(transfer),
            directory: "/fixed/transfer".into(),
        },
    };
    assert!(response.validate().is_ok());
    response.request_id = "foreign".into();
    assert!(response.validate().is_err());
}
