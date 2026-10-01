use super::*;
use louiselm_skills::{
    Digest,
    broker::operator::{LaunchInputBinding, LaunchInputsRequest, stage_launch_inputs},
    registry::{AgentRegistration, Provider, RuntimeMeasurement},
    session_manifest::{SessionInputManifest, SessionInputs},
};

fn request() -> LaunchInputsRequest {
    let digest = |bytes: &[u8]| Some(Digest::of(bytes).to_string());
    LaunchInputsRequest {
        manifest: Box::new(
            SessionInputManifest::build(SessionInputs {
                agent: Some(AgentRegistration {
                    id: "agent".into(),
                    provider: Provider::Fixed("fixture".into()),
                    runtime_id: "runtime".into(),
                    arguments: vec![],
                    environment: std::collections::BTreeMap::new(),
                    tool_integration: None,
                }),
                runtime: Some(RuntimeMeasurement {
                    runtime_id: "runtime".into(),
                    executable_sha256: Digest::of(b"runtime").hex().into(),
                    adapters: vec![],
                    version: "1".into(),
                    origin: "fixture".into(),
                }),
                skill_generation_id: digest(b"generation"),
                view_digest: digest(b"view"),
                project_instructions: Some(vec![]),
                tool_schemas: Some(vec![]),
                plugin_schemas: Some(vec![]),
                source_snapshot_digest: digest(b"snapshot"),
                source_base_digest: digest(b"source"),
                cache_base_digest: digest(b"cache"),
                policy_digest: digest(b"policy"),
                isolation_receipt: Some("isolation".into()),
                envelope_id: Some("envelope".into()),
                envelope_revision: Some(1),
                acp_mcp_servers: Some(vec![]),
            })
            .unwrap(),
        ),
        snapshot: "/operator/snapshot".into(),
        cache: "/operator/cache".into(),
        expected_base_commit: "a".repeat(40),
    }
}

fn binding(request: &LaunchInputsRequest) -> LaunchInputBinding {
    LaunchInputBinding {
        schema: "louiselm.launch-inputs.staged/1".into(),
        manifest_digest: request.manifest.digest().to_string(),
        source_snapshot_digest: request.manifest.source_snapshot_digest.clone(),
        source_base_digest: request.manifest.source_base_digest.clone(),
        cache_base_digest: request.manifest.cache_base_digest.clone(),
        base_commit: request.expected_base_commit.clone(),
    }
}

#[test]
fn launch_inputs_client_requires_every_exact_binding_and_authenticates_the_operator() {
    for scenario in [
        "valid", "schema", "manifest", "snapshot", "source", "cache", "head", "foreign",
    ] {
        let root = private_root();
        let path = root.path().join("inspect.sock");
        let uid = rustix::process::geteuid().as_raw();
        let server = OperatorServer::bind(&path, uid + u32::from(scenario == "foreign")).unwrap();
        let input = request();
        let worker = thread::spawn(move || {
            server
                .serve_once(
                    |received| {
                        assert_ne!(scenario, "foreign", "authentication precedes source lookup");
                        let mut reply = binding(received);
                        let replacement = Digest::of(b"substituted").to_string();
                        match scenario {
                            "schema" => reply.schema = "wrong".into(),
                            "manifest" => reply.manifest_digest = replacement,
                            "snapshot" => reply.source_snapshot_digest = replacement,
                            "source" => reply.source_base_digest = replacement,
                            "cache" => reply.cache_base_digest = replacement,
                            "head" => reply.base_commit = "b".repeat(40),
                            _ => {}
                        }
                        Ok(reply)
                    },
                    |_, _| panic!("not verification"),
                    |_| panic!("not authorization"),
                    |_, _| panic!("not dependencies"),
                    |_, _| panic!("not inspection"),
                    |_| panic!("not conformance"),
                    |_, _| panic!("not skill"),
                    |_, _| panic!("not Beads"),
                    |_, _| panic!("not retention"),
                    |_, _, _| panic!("not waiver"),
                    |_, _, _| panic!("not extension"),
                )
                .unwrap();
        });
        let result = stage_launch_inputs(&path, uid, &input, Duration::from_secs(2));
        worker.join().unwrap();
        match scenario {
            "valid" => assert_eq!(result.unwrap(), binding(&input)),
            "foreign" => assert_eq!(result.unwrap_err(), InspectError::AuthenticationRefused),
            _ => assert_eq!(result.unwrap_err(), InspectError::StatusUnavailable),
        }
    }
}

#[test]
fn launch_inputs_refuse_malformed_manifest_relative_paths_and_invalid_head_before_connecting() {
    for scenario in ["manifest", "snapshot", "cache", "head"] {
        let mut input = request();
        match scenario {
            "manifest" => input.manifest.source_base_digest = "bad".into(),
            "snapshot" => input.snapshot = "relative".into(),
            "cache" => input.cache = "relative".into(),
            _ => input.expected_base_commit = "bad".into(),
        }
        assert_eq!(
            stage_launch_inputs(
                std::path::Path::new("/absent.sock"),
                1,
                &input,
                Duration::from_secs(1)
            )
            .unwrap_err(),
            InspectError::InvalidRequest,
            "{scenario}",
        );
    }
    let mut raw = serde_json::to_value(request()).unwrap();
    raw["provider_key"] = "must not be accepted".into();
    assert_eq!(
        LaunchInputsRequest::parse(&serde_json::to_vec(&raw).unwrap()).unwrap_err(),
        InspectError::InvalidRequest,
    );
    let mut oversized = request();
    oversized.manifest.agent.arguments = vec!["x".repeat(64 * 1024)];
    assert_eq!(
        stage_launch_inputs(
            std::path::Path::new("/absent.sock"),
            1,
            &oversized,
            Duration::from_secs(1)
        )
        .unwrap_err(),
        InspectError::InvalidRequest,
    );
}
