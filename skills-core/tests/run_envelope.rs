//! A Run approval must bound every later Session grant across broker restarts.
#![allow(
    clippy::unwrap_used,
    reason = "The fixture asserts exact policy outcomes."
)]

use louiselm_skills::{
    Digest,
    beads_mutation::{ApprovedBeadsMutations, BeadsEffect, BeadsRole},
    broker::{
        GrantRequest,
        run_envelope::{RunEnvelope, RunEnvelopeStore},
    },
    launch::{LaunchRequest, PROTOCOL_VERSION},
    launch_protocol::ConformanceAuthorization,
    launch_protocol::{VERIFICATION_SCHEMA, VerificationOperation, VerificationRequest},
    launch_receipt::ReceiptHead,
    provider_request::{ApprovedProviderRequests, ReasoningEffort},
};

fn envelope() -> RunEnvelope {
    RunEnvelope {
        schema: "louiselm.broker.run-envelope/1".into(),
        run_id: "run-1".into(),
        envelope_id: "envelope-1".into(),
        envelope_revision: 1,
        controller_uid: 1000,
        bead_scope: ApprovedBeadsMutations {
            project_digest: Digest::of(b"project").to_string(),
            role: BeadsRole::Coordinator,
            issue_ids: vec!["louiselm-a".into(), "louiselm-b".into()],
            effects: vec![
                BeadsEffect::CommentAdd,
                BeadsEffect::Claim,
                BeadsEffect::Close,
            ],
            max_mutations: 8,
            expires_at_ms: 10_000,
        },
        provider_requests: ApprovedProviderRequests {
            disclosure_profile: louiselm_skills::provider_request::disclosure::profile_digest(),
            provider: "openai".into(),
            upstream: "https://api.openai.com/v1/responses".into(),
            addresses: vec!["127.0.0.1".parse().unwrap()],
            max_run_requests: 4,
            models: vec!["gpt-5.6-luna".into()],
            max_effort: ReasoningEffort::High,
            expires_at_ms: 10_000,
        },
        commands: None,
        verification_plan_digest: Digest::of(b"fixed plan").to_string(),
        max_sessions: 3,
        expires_at_ms: 10_000,
    }
}

fn grant() -> GrantRequest {
    let mut scope = envelope().bead_scope;
    scope.role = BeadsRole::Worker;
    scope.issue_ids = vec!["louiselm-a".into()];
    scope.effects = vec![BeadsEffect::CommentAdd, BeadsEffect::Claim];
    scope.max_mutations = 2;
    GrantRequest {
        dependencies: None,
        conformance: ConformanceAuthorization::default(),
        require_cold_recovery: true,
        request: LaunchRequest {
            schema: "louiselm.launch.request/1".into(),
            protocol_version: PROTOCOL_VERSION,
            request_id: "request-1".into(),
            authorization_id: "authorization-1".into(),
            session_id: "session-1".into(),
            run_id: "run-1".into(),
            agent_id: "codex".into(),
            envelope_id: "envelope-1".into(),
            envelope_revision: 1,
            skill_generation_id: Digest::of(b"generation").to_string(),
            session_input_manifest_id: Digest::of(b"inputs").to_string(),
        },
        controller_uid: 1000,
        expires_at_ms: 9_000,
        broker_loss_grace_ms: 500,
        commands: None,
        skill_requests: None,
        beads_mutations: Some(scope),
        provider_requests: Some(envelope().provider_requests),
    }
}

#[test]
fn run_approval_persists_and_rejects_widened_child_grants() {
    let root = tempfile::tempdir().unwrap();
    let store = RunEnvelopeStore::open(root.path()).unwrap();
    let approval = store.approve(&envelope(), 1_000).unwrap();
    assert_eq!(
        approval.envelope_digest,
        Digest::of(&envelope().canonical_bytes()).to_string()
    );
    assert!(store.check_child(&grant(), 1_000).is_ok());

    let reopened = RunEnvelopeStore::open(root.path()).unwrap();
    assert!(reopened.check_child(&grant(), 1_000).is_ok());
    let mut wrong_bead = grant();
    wrong_bead.beads_mutations.as_mut().unwrap().issue_ids = vec!["louiselm-c".into()];
    assert!(wrong_bead.beads_mutations.as_ref().unwrap().valid(1_000));
    assert!(reopened.check_child(&wrong_bead, 1_000).is_err());
    let mut reset_budget = grant();
    reset_budget
        .provider_requests
        .as_mut()
        .unwrap()
        .max_run_requests = 5;
    assert!(reopened.check_child(&reset_budget, 1_000).is_err());
    let mut stale = grant();
    stale.request.envelope_revision = 2;
    assert!(reopened.check_child(&stale, 1_000).is_err());

    let request = VerificationRequest {
        schema: VERIFICATION_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        request_id: "verify-1".into(),
        launch: grant().request,
        head: ReceiptHead {
            sequence: 1,
            digest: Digest::of(b"head").to_string(),
        },
        expires_at_ms: 9_000,
        operation: VerificationOperation::Export {
            input_id: "inputs-1".into(),
            input_digest: Digest::of(b"inputs").to_string(),
        },
    };
    assert!(
        reopened
            .check_verification(&request, &envelope().verification_plan_digest, 1_000)
            .is_ok()
    );
    assert!(
        reopened
            .check_verification(&request, &Digest::of(b"other plan").to_string(), 1_000)
            .is_err()
    );

    let mut revised = envelope();
    revised.envelope_revision = 2;
    let second = reopened.approve(&revised, 1_000).unwrap();
    assert_eq!(second.envelope_revision, 2);
    assert!(reopened.check_child(&grant(), 1_000).is_err());
    assert!(
        reopened
            .check_verification(&request, &envelope().verification_plan_digest, 1_000)
            .is_err()
    );
}

#[test]
fn closed_run_envelope_refuses_missing_unknown_and_expired_authority() {
    let root = tempfile::tempdir().unwrap();
    let store = RunEnvelopeStore::open(root.path()).unwrap();
    let mut invalid = envelope();
    invalid.verification_plan_digest.clear();
    assert!(store.approve(&invalid, 1_000).is_err());
    invalid = envelope();
    invalid.expires_at_ms = 1_000;
    assert!(store.approve(&invalid, 1_000).is_err());
    let mut serialized = serde_json::to_value(envelope()).unwrap();
    serialized["unapproved_extension"] = serde_json::json!(true);
    assert!(serde_json::from_value::<RunEnvelope>(serialized).is_err());
    assert!(store.check_child(&grant(), 1_000).is_err());
    store.approve(&envelope(), 1_000).unwrap();
    assert!(store.check_child(&grant(), 10_000).is_err());
    let mut coordinator = grant();
    coordinator.beads_mutations.as_mut().unwrap().role = BeadsRole::Coordinator;
    coordinator.beads_mutations.as_mut().unwrap().effects = vec![BeadsEffect::CommentAdd];
    let mut worker_only = envelope();
    worker_only.envelope_revision = 2;
    worker_only.bead_scope.role = BeadsRole::Worker;
    worker_only.bead_scope.effects = vec![BeadsEffect::CommentAdd];
    store.approve(&worker_only, 1_000).unwrap();
    coordinator.request.envelope_revision = 2;
    assert!(store.check_child(&coordinator, 1_000).is_err());
}

#[test]
fn missing_earlier_run_revision_fails_closed() {
    let root = tempfile::tempdir().unwrap();
    let store = RunEnvelopeStore::open(root.path()).unwrap();
    store.approve(&envelope(), 1_000).unwrap();
    let mut revision = envelope();
    revision.envelope_revision = 2;
    store.approve(&revision, 1_000).unwrap();
    std::fs::remove_file(root.path().join("run-1/1.json")).unwrap();
    let mut child = grant();
    child.request.envelope_revision = 2;
    assert!(store.check_child(&child, 1_000).is_err());
}
