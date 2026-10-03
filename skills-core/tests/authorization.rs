//! Launch grants must be valid before creating durable authority or retention.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "Disposable authorization fixtures abort on setup or assertion failure."
)]

use louiselm_skills::{
    Digest,
    broker::{AuthorizationStore, BrokerError, GrantRequest},
    launch::{LaunchRequest, MAX_BROKER_LOSS_GRACE_MS, PROTOCOL_VERSION, REQUEST_SCHEMA},
    launch_protocol::ConformanceAuthorization,
    launcher_install::IdentityPool,
};

fn grant(broker_loss_grace_ms: u32) -> GrantRequest {
    GrantRequest {
        role: louiselm_skills::launch_protocol::LaunchRole::Agent,
        request: LaunchRequest {
            schema: REQUEST_SCHEMA.into(),
            protocol_version: PROTOCOL_VERSION,
            request_id: "request-1".into(),
            authorization_id: "authorization-1".into(),
            session_id: "session-1".into(),
            run_id: "run-1".into(),
            agent_id: "demo".into(),
            envelope_id: "envelope-1".into(),
            envelope_revision: 7,
            skill_generation_id: Digest::of(b"generation").to_string(),
            session_input_manifest_id: Digest::of(b"input").to_string(),
        },
        controller_uid: 1501,
        expires_at_ms: 30_000,
        broker_loss_grace_ms,
        conformance: ConformanceAuthorization::default(),
        require_cold_recovery: false,
        commands: None,
        dependencies: None,
        skill_requests: None,
        beads_mutations: None,
        provider_requests: None,
    }
}

fn pool() -> IdentityPool {
    IdentityPool {
        uid_start: 2_000_000,
        gid_start: 3_000_000,
        slots: 1,
    }
}

#[test]
fn invalid_broker_loss_grace_leaves_no_durable_state() {
    for grace in [MAX_BROKER_LOSS_GRACE_MS + 1, 10_000, u32::MAX] {
        let root = tempfile::tempdir().unwrap();
        let store = AuthorizationStore::open(root.path(), pool()).unwrap();
        let mut approval = grant(grace);
        let result = store.authorize(&approval, 1_000);
        assert!(
            matches!(result, Err(BrokerError::InvalidGrant)),
            "{result:?}"
        );
        for directory in ["pending", "consumed", "workspace-retention"] {
            assert_eq!(
                std::fs::read_dir(root.path().join(directory))
                    .unwrap()
                    .count(),
                0,
                "invalid grace {grace} created {directory} state",
            );
        }
        assert_eq!(store.session_count_for_run("run-1").unwrap(), 0);
        assert!(matches!(
            store.consume(&approval.request, approval.controller_uid, 2_000),
            Err(BrokerError::UnknownAuthorization)
        ));
        drop(store);

        // Rejection must not spend the request identity or its only pool slot.
        let reopened = AuthorizationStore::open(root.path(), pool()).unwrap();
        approval.broker_loss_grace_ms = MAX_BROKER_LOSS_GRACE_MS;
        reopened.authorize(&approval, 2_000).unwrap();
        reopened
            .consume(&approval.request, approval.controller_uid, 3_000)
            .unwrap();
    }
}

#[test]
fn broker_loss_grace_boundaries_survive_restart_and_consumption() {
    for grace in [0, MAX_BROKER_LOSS_GRACE_MS] {
        let root = tempfile::tempdir().unwrap();
        let store = AuthorizationStore::open(root.path(), pool()).unwrap();
        let approval = grant(grace);
        let pending = store.authorize(&approval, 1_000).unwrap();
        assert_eq!(pending.broker_loss_grace_ms, grace);
        drop(store);

        let reopened = AuthorizationStore::open(root.path(), pool()).unwrap();
        let authorization = reopened
            .consume(&approval.request, approval.controller_uid, 2_000)
            .unwrap();
        authorization
            .validate_for(&approval.request, approval.controller_uid, 2_000)
            .unwrap();
        assert_eq!(authorization.broker_loss_grace_ms, grace);
        assert_eq!(authorization.identity_slot, 0);
    }
}

fn verifier_grant() -> GrantRequest {
    let mut value = serde_json::to_value(grant(0)).unwrap();
    value["role"] = "fixed_verifier".into();
    serde_json::from_value(value).expect("explicit fixed-verifier role must be accepted")
}

#[test]
fn fixed_verifier_role_survives_restart_and_signed_consumption() {
    let root = tempfile::tempdir().unwrap();
    let approval = verifier_grant();
    let store = AuthorizationStore::open(root.path(), pool()).unwrap();
    let pending = store.authorize(&approval, 1_000).unwrap();
    assert_eq!(
        serde_json::to_value(&pending).unwrap()["role"],
        "fixed_verifier"
    );
    drop(store);
    let reopened = AuthorizationStore::open(root.path(), pool()).unwrap();
    let authorization = reopened
        .consume(&approval.request, approval.controller_uid, 2_000)
        .unwrap();
    assert_eq!(
        serde_json::to_value(&authorization).unwrap()["role"],
        "fixed_verifier"
    );
    authorization
        .validate_for(&approval.request, approval.controller_uid, 2_000)
        .unwrap();
    assert!(matches!(
        reopened.consume(&approval.request, approval.controller_uid, 2_001),
        Err(BrokerError::UnknownAuthorization)
    ));
}

#[test]
fn fixed_verifier_refuses_effects_before_spending_launch_authority() {
    for (field, effect) in [
        (
            "commands",
            serde_json::json!({
                "command_digest": Digest::of(b"arbitrary command").to_string(),
                "timeout_ms": 100, "uses": 1, "allow_delegation": false, "expires_at_ms": 30_000,
            }),
        ),
        ("require_cold_recovery", true.into()),
        (
            "skill_requests",
            serde_json::json!({"agents":["demo"],"allow_run":false,"expires_at_ms":30_000}),
        ),
        (
            "beads_mutations",
            serde_json::json!({
                "project_digest":Digest::of(b"project").to_string(),"role":"worker",
                "issue_ids":["b-1"],"effects":[{"kind":"comment_add"}],"max_mutations":1,"expires_at_ms":30_000,
            }),
        ),
        (
            "dependencies",
            serde_json::json!({
                "input_manifest_digest":grant(0).request.session_input_manifest_id,
                "lockfile_path":"Cargo.lock","lockfile_digest":Digest::of(b"lockfile").to_string(),
                "registries":[],"preapproved":[],"max_fetches":1,"max_bytes":1024,"expires_at_ms":30_000,
            }),
        ),
        (
            "provider_requests",
            serde_json::json!({
                "disclosure_profile":louiselm_skills::provider_request::disclosure::profile_digest(),
                "provider":"openai","upstream":"https://example.invalid/v1/responses","addresses":["192.0.2.1"],
                "max_run_requests":1,"models":["fixture"],"max_effort":"low","expires_at_ms":30_000,
            }),
        ),
    ] {
        let root = tempfile::tempdir().unwrap();
        let store = AuthorizationStore::open(root.path(), pool()).unwrap();
        let mut value = serde_json::to_value(verifier_grant()).unwrap();
        value[field] = effect;
        let invalid: GrantRequest = serde_json::from_value(value).unwrap();
        if let Some(scope) = &invalid.dependencies {
            scope.validate(1_000).unwrap();
        }
        if let Some(scope) = &invalid.skill_requests {
            assert!(scope.valid(1_000));
        }
        if let Some(scope) = &invalid.beads_mutations {
            assert!(scope.valid(1_000));
        }
        if let Some(scope) = &invalid.provider_requests {
            assert!(scope.valid(1_000));
        }
        let result = store.authorize(&invalid, 1_000);
        assert!(
            matches!(result, Err(BrokerError::InvalidGrant)),
            "{field}: {result:?}"
        );
        assert_eq!(store.session_count_for_run("run-1").unwrap(), 0);
        assert_eq!(
            std::fs::read_dir(root.path().join("workspace-retention"))
                .unwrap()
                .count(),
            0
        );
        store.authorize(&verifier_grant(), 1_001).unwrap();
    }
}

#[test]
fn launch_role_is_required_and_closed() {
    let mut value = serde_json::to_value(grant(0)).unwrap();
    value.as_object_mut().unwrap().remove("role");
    assert!(serde_json::from_value::<GrantRequest>(value.clone()).is_err());
    value["role"] = "coordinator".into();
    assert!(serde_json::from_value::<GrantRequest>(value).is_err());
}
