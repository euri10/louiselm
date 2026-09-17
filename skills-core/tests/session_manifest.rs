//! Behavioral coverage for the Session input manifest.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Test fixtures abort on setup failure and assert failures directly."
)]

use std::collections::BTreeMap;

use louiselm_skills::{
    canonical::Digest,
    launch::{LaunchRequest, PROTOCOL_VERSION, REQUEST_SCHEMA},
    posture::{DimensionName, FailureCode, PROVIDER_DISCLOSURE_NOTICE},
    registry::{AgentRegistration, MeasuredFile, Provider, RuntimeMeasurement},
    session_manifest::{
        INPUT_MANIFEST_SCHEMA, MeasuredInput, SessionInputManifest, SessionInputs,
        SessionManifestError,
    },
};

const GENERATION: &str = "sha256:0000000000000000000000000000000000000000000000000000000000000000";
const VIEW: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
const POLICY: &str = "sha256:2222222222222222222222222222222222222222222222222222222222222222";

fn agent() -> AgentRegistration {
    AgentRegistration {
        id: "codex".to_owned(),
        provider: Provider::Fixed("openai".to_owned()),
        runtime_id: "codex-runtime".to_owned(),
        arguments: vec!["acp".to_owned()],
        environment: BTreeMap::from([("CODEX_HOME".to_owned(), "/opt/codex-home".to_owned())]),
        tool_integration: None,
    }
}

fn runtime() -> RuntimeMeasurement {
    RuntimeMeasurement {
        runtime_id: "codex-runtime".to_owned(),
        executable_sha256: "ab".repeat(32),
        adapters: vec![MeasuredFile {
            path: "lib/adapter.js".to_owned(),
            sha256: "cd".repeat(32),
        }],
        version: "1.2.3".to_owned(),
        origin: "staged release".to_owned(),
    }
}

fn measured(path: &str, content: &str) -> MeasuredInput {
    MeasuredInput::from_bytes(path, false, content.as_bytes()).unwrap()
}

fn complete_inputs() -> SessionInputs {
    SessionInputs {
        agent: Some(agent()),
        runtime: Some(runtime()),
        skill_generation_id: Some(GENERATION.to_owned()),
        view_digest: Some(VIEW.to_owned()),
        project_instructions: Some(vec![
            measured("AGENTS.md", "rules"),
            measured("docs/x.md", "x"),
        ]),
        tool_schemas: Some(vec![
            measured("tools/write.json", "{}"),
            measured("tools/read.json", "read"),
        ]),
        plugin_schemas: Some(vec![
            measured("plugin/init.lua", "return"),
            measured("plugin/extra.json", "extra"),
        ]),
        cache_base_digest: Some(Digest::of(b"cache").to_string()),
        source_snapshot_digest: Some(Digest::of(b"source snapshot").to_string()),
        source_base_digest: Some(Digest::of(b"source base").to_string()),
        policy_digest: Some(POLICY.to_owned()),
        isolation_receipt: Some("isolation-contract-1".to_owned()),
        envelope_id: Some("envelope-7".to_owned()),
        envelope_revision: Some(3),
        acp_mcp_servers: Some(Vec::new()),
    }
}

fn request_with(manifest_digest: &str) -> LaunchRequest {
    LaunchRequest {
        schema: REQUEST_SCHEMA.to_owned(),
        protocol_version: PROTOCOL_VERSION,
        request_id: "request-abc123".to_owned(),
        authorization_id: "authorization-abc123".to_owned(),
        session_id: "session-abc123".to_owned(),
        run_id: "run-xyz789".to_owned(),
        agent_id: "codex".to_owned(),
        envelope_id: "envelope-7".to_owned(),
        envelope_revision: 7,
        skill_generation_id: GENERATION.to_owned(),
        session_input_manifest_id: manifest_digest.to_owned(),
    }
}

#[test]
fn builds_and_round_trips_a_complete_manifest() {
    let manifest =
        SessionInputManifest::build(complete_inputs()).expect("complete inputs build a manifest");

    assert_eq!(manifest.schema, INPUT_MANIFEST_SCHEMA);
    assert_eq!(manifest.agent.id, "codex");
    assert_eq!(manifest.envelope.id, "envelope-7");
    assert_eq!(manifest.envelope.revision, 3);
    assert_eq!(manifest.isolation_receipt, "isolation-contract-1");
    assert_eq!(manifest.policy_digest, POLICY);
    assert_eq!(manifest.skill_generation.generation_digest, GENERATION);
    assert_eq!(manifest.skill_generation.view_digest, VIEW);
    assert_eq!(
        manifest.provider_disclosure.providers,
        vec!["openai".to_owned()]
    );
    assert_eq!(
        manifest.provider_disclosure.notice,
        PROVIDER_DISCLOSURE_NOTICE
    );

    let bytes = manifest.canonical_bytes();
    let parsed = SessionInputManifest::parse(&bytes).expect("canonical bytes round trip");
    assert_eq!(parsed, manifest);
}

#[test]
fn digest_is_accepted_by_launch_request_validation() {
    let manifest =
        SessionInputManifest::build(complete_inputs()).expect("complete inputs build a manifest");

    let request = request_with(&manifest.digest().to_string());
    request
        .validate()
        .expect("the manifest digest satisfies LaunchRequest validation");
}

#[test]
fn identical_inputs_yield_identical_digests() {
    let first = SessionInputManifest::build(complete_inputs())
        .expect("first construction succeeds")
        .digest();
    let second = SessionInputManifest::build(complete_inputs())
        .expect("second construction succeeds")
        .digest();

    assert_eq!(first, second);
}

#[test]
fn source_binding_cannot_be_omitted_from_a_launch_manifest() {
    let manifest = SessionInputManifest::build(complete_inputs()).unwrap();
    for field in ["source_snapshot_digest", "source_base_digest"] {
        let mut json = serde_json::to_value(&manifest).unwrap();
        json.as_object_mut().unwrap().remove(field);
        assert!(
            serde_json::from_value::<SessionInputManifest>(json).is_err(),
            "missing {field} must refuse launch input"
        );
    }
}

#[test]
fn source_binding_is_canonical_and_changes_launch_identity() {
    let manifest = SessionInputManifest::build(complete_inputs()).unwrap();
    for field in ["source_snapshot_digest", "source_base_digest"] {
        let mut json = serde_json::to_value(&manifest).unwrap();
        json[field] = Digest::of(b"different source").to_string().into();
        let changed: SessionInputManifest = serde_json::from_value(json.clone()).unwrap();
        assert!(SessionInputManifest::parse(&changed.canonical_bytes()).is_ok());
        assert_ne!(changed.digest(), manifest.digest());
        for invalid in ["", "unknown", "sha256:ABC", Digest::of(b"bare").hex()] {
            json[field] = invalid.into();
            let changed: SessionInputManifest = serde_json::from_value(json.clone()).unwrap();
            assert!(SessionInputManifest::parse(&changed.canonical_bytes()).is_err());
        }
    }
}

#[test]
fn cache_base_is_required_canonical_and_changes_the_session_identity() {
    let inputs = complete_inputs();
    let baseline = SessionInputManifest::build(inputs.clone())
        .unwrap()
        .digest();
    let mut changed = inputs.clone();
    changed.cache_base_digest = Some(Digest::of(b"different cache").to_string());
    assert_ne!(
        SessionInputManifest::build(changed).unwrap().digest(),
        baseline
    );
    for invalid in ["", "unknown", "sha256:ABC", Digest::of(b"bare").hex()] {
        let mut changed = inputs.clone();
        changed.cache_base_digest = Some(invalid.to_owned());
        assert!(SessionInputManifest::build(changed).is_err());
    }
    let mut json = serde_json::to_value(SessionInputManifest::build(inputs).unwrap()).unwrap();
    json.as_object_mut().unwrap().remove("cache_base_digest");
    assert!(SessionInputManifest::parse(&serde_json::to_vec(&json).unwrap()).is_err());
}

#[test]
fn shuffled_input_order_does_not_change_the_digest() {
    let mut shuffled = complete_inputs();
    shuffled.project_instructions.as_mut().unwrap().reverse();
    shuffled.tool_schemas.as_mut().unwrap().reverse();
    shuffled.plugin_schemas.as_mut().unwrap().reverse();

    let ordered = SessionInputManifest::build(complete_inputs())
        .expect("ordered construction succeeds")
        .digest();
    let reordered = SessionInputManifest::build(shuffled)
        .expect("shuffled construction succeeds")
        .digest();

    assert_eq!(ordered, reordered);
}

#[test]
fn every_changed_input_changes_the_digest() {
    let baseline = SessionInputManifest::build(complete_inputs())
        .expect("baseline construction succeeds")
        .digest();

    let changed_instruction = {
        let mut inputs = complete_inputs();
        inputs.project_instructions.as_mut().unwrap()[0] = measured("AGENTS.md", "altered");
        SessionInputManifest::build(inputs).unwrap().digest()
    };
    let changed_view = {
        let mut inputs = complete_inputs();
        inputs.view_digest = Some(
            "sha256:9999999999999999999999999999999999999999999999999999999999999999".to_owned(),
        );
        SessionInputManifest::build(inputs).unwrap().digest()
    };
    let changed_runtime = {
        let mut inputs = complete_inputs();
        let mut runtime = runtime();
        runtime.executable_sha256 = "ef".repeat(32);
        inputs.runtime = Some(runtime);
        SessionInputManifest::build(inputs).unwrap().digest()
    };
    let changed_agent = {
        let mut inputs = complete_inputs();
        let mut agent = agent();
        agent.arguments.push("--verbose".to_owned());
        inputs.agent = Some(agent);
        SessionInputManifest::build(inputs).unwrap().digest()
    };
    let changed_envelope = {
        let mut inputs = complete_inputs();
        inputs.envelope_revision = Some(4);
        SessionInputManifest::build(inputs).unwrap().digest()
    };
    let changed_provider = {
        let mut inputs = complete_inputs();
        let mut agent = agent();
        agent.provider = Provider::Fixed("anthropic".to_owned());
        inputs.agent = Some(agent);
        SessionInputManifest::build(inputs).unwrap().digest()
    };

    for (label, digest) in [
        ("project instruction content", changed_instruction),
        ("view digest", changed_view),
        ("runtime measurement", changed_runtime),
        ("agent registration", changed_agent),
        ("envelope revision", changed_envelope),
        ("provider disclosure", changed_provider),
    ] {
        assert_ne!(baseline, digest, "{label} must change the digest");
    }
}

#[test]
fn missing_required_inputs_are_refused_without_defaults() {
    type RemoveInput = fn(&mut SessionInputs);
    let cases: Vec<(&str, RemoveInput, SessionManifestError)> = vec![
        (
            "source snapshot",
            |inputs| inputs.source_snapshot_digest = None,
            SessionManifestError::Missing {
                field: "source_snapshot_digest",
            },
        ),
        (
            "source base",
            |inputs| inputs.source_base_digest = None,
            SessionManifestError::Missing {
                field: "source_base_digest",
            },
        ),
        (
            "cache base",
            |inputs| inputs.cache_base_digest = None,
            SessionManifestError::Missing {
                field: "cache_base_digest",
            },
        ),
        (
            "agent",
            |inputs| inputs.agent = None,
            SessionManifestError::Missing { field: "agent" },
        ),
        (
            "runtime",
            |inputs| inputs.runtime = None,
            SessionManifestError::UnmeasuredRuntime {
                reason: "no measured runtime is bound",
            },
        ),
        (
            "skill generation",
            |inputs| inputs.skill_generation_id = None,
            SessionManifestError::UnresolvedGeneration {
                reason: "no Skill Generation is bound",
            },
        ),
        (
            "view",
            |inputs| inputs.view_digest = None,
            SessionManifestError::UnresolvedView {
                reason: "no materialized Instruction view is bound",
            },
        ),
        (
            "policy",
            |inputs| inputs.policy_digest = None,
            SessionManifestError::Missing {
                field: "policy_digest",
            },
        ),
        (
            "isolation receipt",
            |inputs| inputs.isolation_receipt = None,
            SessionManifestError::Missing {
                field: "isolation_receipt",
            },
        ),
        (
            "envelope id",
            |inputs| inputs.envelope_id = None,
            SessionManifestError::Missing {
                field: "envelope_id",
            },
        ),
        (
            "envelope revision",
            |inputs| inputs.envelope_revision = None,
            SessionManifestError::Missing {
                field: "envelope_revision",
            },
        ),
    ];
    for (label, remove, expected) in cases {
        let mut inputs = complete_inputs();
        remove(&mut inputs);
        let error = SessionInputManifest::build(inputs)
            .err()
            .unwrap_or_else(|| panic!("{label} must be refused"));
        assert_eq!(error, expected, "{label} refusal is typed");
    }
}

#[test]
fn dimension_refusals_carry_dimension_specific_codes() {
    let mut runtime_missing = complete_inputs();
    runtime_missing.runtime = None;
    let error = SessionInputManifest::build(runtime_missing).unwrap_err();
    assert_eq!(error.dimension(), Some(DimensionName::Runtime));
    assert_eq!(error.failure_code(), Some(FailureCode::EvidenceMissing));

    let mut generation_missing = complete_inputs();
    generation_missing.skill_generation_id = None;
    let error = SessionInputManifest::build(generation_missing).unwrap_err();
    assert_eq!(error.dimension(), Some(DimensionName::ManagedSupply));
    assert_eq!(error.failure_code(), Some(FailureCode::EvidenceMissing));

    let mut view_missing = complete_inputs();
    view_missing.view_digest = None;
    let error = SessionInputManifest::build(view_missing).unwrap_err();
    assert_eq!(error.dimension(), Some(DimensionName::ManagedSupply));
    assert_eq!(error.failure_code(), Some(FailureCode::EvidenceMissing));

    let mut mcp = complete_inputs();
    mcp.acp_mcp_servers = Some(vec!["fake-mcp".to_owned()]);
    let error = SessionInputManifest::build(mcp).unwrap_err();
    assert_eq!(error.dimension(), Some(DimensionName::NativeSupply));
    assert_eq!(
        error.failure_code(),
        Some(FailureCode::NativeSupplyUncertain)
    );
}

#[test]
fn non_empty_mcp_list_is_refused_in_canonical_bytes_too() {
    let manifest = SessionInputManifest::build(complete_inputs()).unwrap();
    let with_server = String::from_utf8(manifest.canonical_bytes())
        .unwrap()
        .replace(
            "\"acp_mcp_servers\":[]",
            "\"acp_mcp_servers\":[\"fake-mcp\"]",
        );

    let error = SessionInputManifest::parse(with_server.as_bytes()).unwrap_err();
    assert!(
        matches!(error, SessionManifestError::NonEmptyMcp { .. }),
        "non-empty MCP list must be refused from bytes: {error}"
    );
}

#[test]
fn empty_runtime_and_supply_references_retain_their_failure_dimensions() {
    type ClearInput = fn(&mut SessionInputs);
    let cases: [(ClearInput, DimensionName); 3] = [
        (
            |i| i.runtime.as_mut().unwrap().executable_sha256.clear(),
            DimensionName::Runtime,
        ),
        (
            |i| i.skill_generation_id = Some(String::new()),
            DimensionName::ManagedSupply,
        ),
        (
            |i| i.view_digest = Some(String::new()),
            DimensionName::ManagedSupply,
        ),
    ];
    for (clear, dimension) in cases {
        let mut inputs = complete_inputs();
        clear(&mut inputs);
        let error = SessionInputManifest::build(inputs).unwrap_err();
        assert_eq!(error.dimension(), Some(dimension));
        assert_eq!(error.failure_code(), Some(FailureCode::EvidenceMissing));
    }
}

#[test]
fn project_instructions_are_bound_as_measured_inputs() {
    let manifest = SessionInputManifest::build(complete_inputs()).unwrap();
    let bytes = String::from_utf8(manifest.canonical_bytes()).unwrap();

    assert_eq!(manifest.project_instructions.len(), 2);
    assert_eq!(manifest.project_instructions[0].path, "AGENTS.md");
    assert_eq!(
        manifest.project_instructions[0].sha256,
        Digest::of(b"rules").hex(),
        "instruction content is content-addressed"
    );
    assert!(
        bytes.contains("\"project_instructions\""),
        "the snapshot has its own wire field"
    );
    assert!(
        !bytes.contains("package_digest") && !bytes.contains("\"members\""),
        "instruction files never masquerade as admitted packages"
    );
}

#[test]
fn malformed_inputs_are_refused() {
    let mut bad_digest = complete_inputs();
    bad_digest.skill_generation_id = Some("0000".to_owned());
    let error = SessionInputManifest::build(bad_digest).unwrap_err();
    assert!(matches!(
        error,
        SessionManifestError::Malformed {
            field: "skill_generation_id",
            ..
        }
    ));

    let mut bad_receipt = complete_inputs();
    bad_receipt.isolation_receipt = Some("not a valid identifier!".to_owned());
    let error = SessionInputManifest::build(bad_receipt).unwrap_err();
    assert!(matches!(
        error,
        SessionManifestError::Malformed {
            field: "isolation_receipt",
            ..
        }
    ));

    let mut traversal = complete_inputs();
    traversal.project_instructions.as_mut().unwrap()[0].path = "../escape".to_owned();
    let error = SessionInputManifest::build(traversal).unwrap_err();
    assert!(matches!(
        error,
        SessionManifestError::Malformed {
            field: "project_instructions",
            ..
        }
    ));

    let mut duplicate = complete_inputs();
    duplicate
        .tool_schemas
        .as_mut()
        .unwrap()
        .push(measured("tools/write.json", "{}"));
    let error = SessionInputManifest::build(duplicate).unwrap_err();
    assert!(matches!(
        error,
        SessionManifestError::Duplicate {
            field: "tool_schemas",
            ..
        }
    ));
}

#[test]
fn noncanonical_or_unknown_bytes_are_refused() {
    let manifest = SessionInputManifest::build(complete_inputs()).unwrap();

    let padded = {
        let mut bytes = manifest.canonical_bytes();
        bytes.push(b'\n');
        bytes
    };
    assert_eq!(
        SessionInputManifest::parse(&padded).unwrap_err(),
        SessionManifestError::NonCanonical
    );

    let unknown_field = String::from_utf8(manifest.canonical_bytes())
        .unwrap()
        .replace("\"schema\":", "\"smuggled\":1,\"schema\":");
    assert!(matches!(
        SessionInputManifest::parse(unknown_field.as_bytes()).unwrap_err(),
        SessionManifestError::MalformedJson(())
    ));

    let wrong_schema = String::from_utf8(manifest.canonical_bytes())
        .unwrap()
        .replace(INPUT_MANIFEST_SCHEMA, "louiselm.session.input-manifest/2");
    assert!(matches!(
        SessionInputManifest::parse(wrong_schema.as_bytes()).unwrap_err(),
        SessionManifestError::UnsupportedSchema(_)
    ));
}

#[test]
fn absent_snapshots_are_distinct_from_explicitly_empty_snapshots() {
    let removals: [fn(&mut SessionInputs); 4] = [
        |i| i.project_instructions = None,
        |i| i.tool_schemas = None,
        |i| i.plugin_schemas = None,
        |i| i.acp_mcp_servers = None,
    ];
    for remove in removals {
        let mut inputs = complete_inputs();
        remove(&mut inputs);
        assert!(matches!(
            SessionInputManifest::build(inputs),
            Err(SessionManifestError::Missing { .. })
        ));
    }
    let mut inputs = complete_inputs();
    inputs.project_instructions = Some(vec![]);
    inputs.tool_schemas = Some(vec![]);
    inputs.plugin_schemas = Some(vec![]);
    let manifest = SessionInputManifest::build(inputs).unwrap();
    assert!(manifest.project_instructions.is_empty());
    assert!(manifest.tool_schemas.is_empty());
    assert!(manifest.plugin_schemas.is_empty());
}

#[test]
fn required_values_and_nested_wire_fields_are_checked() {
    let baseline = SessionInputManifest::build(complete_inputs()).unwrap();
    for pointer in [
        "/agent/id",
        "/agent/runtime_id",
        "/agent/provider",
        "/runtime/executable_sha256",
        "/runtime/version",
        "/runtime/origin",
        "/skill_generation/generation_digest",
        "/skill_generation/view_digest",
        "/policy_digest",
        "/isolation_receipt",
        "/envelope/id",
        "/provider_disclosure/notice",
    ] {
        let mut wire = serde_json::to_value(&baseline).unwrap();
        *wire.pointer_mut(pointer).unwrap() = serde_json::json!("");
        let modified: SessionInputManifest = serde_json::from_value(wire).unwrap();
        assert!(
            SessionInputManifest::parse(&modified.canonical_bytes()).is_err(),
            "{pointer}"
        );
    }
    for pointer in [
        "/agent",
        "/runtime",
        "/runtime/adapters/0",
        "/envelope",
        "/skill_generation",
        "/provider_disclosure",
        "/project_instructions/0",
    ] {
        let mut wire = serde_json::to_value(&baseline).unwrap();
        wire.pointer_mut(pointer)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("unknown".into(), serde_json::json!("secret-marker"));
        let error = SessionInputManifest::parse(&serde_json::to_vec(&wire).unwrap()).unwrap_err();
        assert!(
            matches!(error, SessionManifestError::MalformedJson(())),
            "{pointer}"
        );
        assert!(!error.to_string().contains("secret-marker"));
    }
    let oversized = vec![b' '; louiselm_skills::session_manifest::MAX_INPUT_MANIFEST_BYTES + 1];
    assert_eq!(
        SessionInputManifest::parse(&oversized),
        Err(SessionManifestError::TooLarge)
    );
}

#[test]
fn snapshot_conflicts_runtime_mismatch_and_false_disclosure_are_refused() {
    let mut inputs = complete_inputs();
    inputs.runtime.as_mut().unwrap().runtime_id = "other".into();
    assert!(matches!(
        SessionInputManifest::build(inputs),
        Err(SessionManifestError::UnmeasuredRuntime { .. })
    ));
    for path in ["agents.md", "AGENTS.md/child"] {
        let mut inputs = complete_inputs();
        inputs
            .project_instructions
            .as_mut()
            .unwrap()
            .push(measured(path, "conflict"));
        assert!(SessionInputManifest::build(inputs).is_err());
    }
    let mut manifest = SessionInputManifest::build(complete_inputs()).unwrap();
    manifest.provider_disclosure.providers = vec!["unknown".into()];
    assert!(SessionInputManifest::parse(&manifest.canonical_bytes()).is_err());
}

#[test]
fn metadata_changes_are_bound_and_only_set_order_is_normalized() {
    let mut inputs = complete_inputs();
    inputs
        .runtime
        .as_mut()
        .unwrap()
        .adapters
        .push(MeasuredFile {
            path: "lib/second.js".into(),
            sha256: "12".repeat(32),
        });
    let baseline = SessionInputManifest::build(inputs.clone())
        .unwrap()
        .digest();
    inputs.runtime.as_mut().unwrap().adapters.reverse();
    assert_eq!(
        SessionInputManifest::build(inputs.clone())
            .unwrap()
            .digest(),
        baseline
    );
    let changes: [fn(&mut SessionInputs); 12] = [
        |i| i.project_instructions.as_mut().unwrap()[0].executable = true,
        |i| i.project_instructions.as_mut().unwrap()[0].size += 1,
        |i| i.tool_schemas.as_mut().unwrap()[0].sha256 = "34".repeat(32),
        |i| i.plugin_schemas.as_mut().unwrap()[0].path = "different.json".into(),
        |i| i.policy_digest = Some(Digest::of(b"new policy").to_string()),
        |i| i.skill_generation_id = Some(Digest::of(b"new generation").to_string()),
        |i| i.isolation_receipt = Some("new-receipt".into()),
        |i| i.envelope_id = Some("new-envelope".into()),
        |i| i.runtime.as_mut().unwrap().version = "2".into(),
        |i| i.runtime.as_mut().unwrap().origin = "other origin".into(),
        |i| {
            i.agent
                .as_mut()
                .unwrap()
                .environment
                .insert("EXTRA".into(), "yes".into());
        },
        |i| i.agent.as_mut().unwrap().tool_integration = Some("integration/1".into()),
    ];
    for change in changes {
        let mut variant = inputs.clone();
        change(&mut variant);
        assert_ne!(
            SessionInputManifest::build(variant).unwrap().digest(),
            baseline
        );
    }
    inputs.agent.as_mut().unwrap().arguments = vec!["first".into(), "second".into()];
    let ordered = SessionInputManifest::build(inputs.clone())
        .unwrap()
        .digest();
    inputs.agent.as_mut().unwrap().arguments.reverse();
    assert_ne!(
        SessionInputManifest::build(inputs).unwrap().digest(),
        ordered
    );
}
