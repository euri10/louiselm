//! Durable, payload-free native phase evidence.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "test fixtures and assertions"
)]

use louiselm_capture::{ActionEvidenceError, ActionIdentity, ActionOutcome, ActionReservation};
use std::{
    io::Write,
    process::{Command, Stdio},
};

#[path = "support/phone_evidence.rs"]
mod phone_evidence;

fn owner(root: &std::path::Path, message: &serde_json::Value) -> serde_json::Value {
    let mut child = Command::new(env!("CARGO_BIN_EXE_louiselm-capture"))
        .args(["phone-control", "owner-journal"])
        .env_clear()
        .env("LOUISELM_CAPTURE_CONFIG_DIR", root)
        .env("LOUISELM_CAPTURE_DATA_DIR", root)
        .env("LOUISELM_CAPTURE_STATE_DIR", root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(message.to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    serde_json::from_slice(&output.stdout).unwrap()
}

fn identity(root: &std::path::Path) -> ActionIdentity {
    ActionIdentity {
        operation_id: "01".repeat(16),
        receiver_identity: "02".repeat(32),
        controller_id: "03".repeat(16),
        subject_id: "04".repeat(16),
        action: 3,
        payload_hash: "05".repeat(32),
        grant_id: "06".repeat(16),
        grant_revision: 1,
        period_id: "07".repeat(16),
        expected_revision: 2,
        project: root.canonicalize().unwrap().to_str().unwrap().into(),
        agent: "mock".into(),
        recorded_at_ms: 1000,
    }
}

#[test]
fn owner_phases_survive_restart_and_late_reports_cannot_downgrade() {
    let root = tempfile::tempdir().unwrap();
    let request = identity(root.path());
    let path = root.path().join("evidence");
    phone_evidence::initialize_store(&path);
    let accepted = ActionOutcome::Accepted {
        evidence: "11".repeat(32),
    };
    let dispatched = ActionOutcome::Dispatched {
        evidence: "22".repeat(32),
    };
    let completed = ActionOutcome::Completed {
        evidence: "33".repeat(32),
    };
    {
        let store = phone_evidence::open(&path).unwrap();
        store.reserve(&"09".repeat(16), &request).unwrap();
        store.report(&request, &accepted, 2000).unwrap();
        store.report(&request, &dispatched, 3000).unwrap();
        store.report(&request, &completed, 4000).unwrap();
        store.report(&request, &accepted, 5000).unwrap();
    }
    let store = phone_evidence::open(&path).unwrap();
    let retained = store.lookup(&request.operation_id).unwrap();
    assert_eq!(retained.outcome, completed);
    assert_eq!(retained.phases, vec![accepted, dispatched, completed]);
}

#[test]
fn unknown_after_acceptance_preserves_evidence_and_cannot_be_fenced() {
    let root = tempfile::tempdir().unwrap();
    let request = identity(root.path());
    let path = root.path().join("evidence");
    phone_evidence::initialize_store(&path);
    let store = phone_evidence::open(&path).unwrap();
    let accepted = ActionOutcome::Accepted {
        evidence: "11".repeat(32),
    };
    store.reserve(&"09".repeat(16), &request).unwrap();
    store.report(&request, &accepted, 2000).unwrap();
    store
        .report(&request, &ActionOutcome::Unknown, 3000)
        .unwrap();
    let retained = store.lookup(&request.operation_id).unwrap();
    assert_eq!(retained.outcome, ActionOutcome::Unknown);
    assert_eq!(retained.phases, vec![accepted]);
    assert_eq!(
        store.report(
            &request,
            &ActionOutcome::FencedNoEffect {
                evidence: "44".repeat(32)
            },
            4000
        ),
        Err(ActionEvidenceError::Conflict)
    );
    let mut independent = request.clone();
    independent.operation_id = "08".repeat(16);
    independent.subject_id = "09".repeat(16);
    assert_eq!(
        store.reserve_session_prompt(&"09".repeat(16), &independent, &[]),
        Err(ActionEvidenceError::Unresolved)
    );
    assert_eq!(
        store.reserve_session_prompt(
            &"09".repeat(16),
            &independent,
            std::slice::from_ref(&request.operation_id)
        ),
        Ok(ActionReservation::New)
    );
}

#[test]
fn native_owner_inspection_returns_every_durable_phase() {
    let root = tempfile::tempdir().unwrap();
    let tls = root.path().join("louiselm/capture/tls");
    let receiver = louiselm_capture::TlsIdentity::load_or_create(&tls).unwrap();
    let mut request = identity(root.path());
    request.receiver_identity = receiver.public_key_sha256().to_owned();
    phone_evidence::initialize(
        &root.path().join("louiselm/workflow/phone-action-owner"),
        &tls.join("phone-action-owner.json"),
        &request.receiver_identity,
        louiselm_capture::ActionEvidenceKind::Owner,
    );
    phone_evidence::initialize_barrier(&tls, &request.receiver_identity);
    phone_evidence::bind_barrier_to_owner(&tls);
    let inspect = serde_json::json!({"v":1,"type":"inspect_phases","identity":request});
    assert_eq!(owner(root.path(), &inspect)["code"], "unavailable");
    assert_eq!(
        owner(
            root.path(),
            &serde_json::json!({"v":2,"type":"begin","generation":"09".repeat(16),"identity":request})
        )["type"],
        "begun"
    );
    let phases = [
        serde_json::json!({"accepted":{"evidence":"11".repeat(32)}}),
        serde_json::json!({"dispatched":{"evidence":"22".repeat(32)}}),
        serde_json::json!({"completed":{"evidence":"33".repeat(32)}}),
    ];
    for (index, phase) in phases.iter().enumerate() {
        let report = serde_json::json!({"v":1,"type":"report","identity":request,"outcome":phase,"at_ms":2000 + index});
        assert_eq!(owner(root.path(), &report)["type"], "recorded");
    }
    let result = owner(root.path(), &inspect);
    assert_eq!(result["type"], "action_phases");
    assert_eq!(result["outcome"], phases[2]);
    assert_eq!(result["accepted_evidence"], "11".repeat(32));
    assert_eq!(result["dispatched_evidence"], "22".repeat(32));
    assert_eq!(result["completed_evidence"], "33".repeat(32));
    let mut other = request;
    other.payload_hash = "99".repeat(32);
    let altered = serde_json::json!({"v":1,"type":"inspect_phases","identity":other});
    assert_eq!(owner(root.path(), &altered)["code"], "conflict");
}
