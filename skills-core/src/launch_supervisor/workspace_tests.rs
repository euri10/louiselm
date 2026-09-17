//! Production input resolution, including canonical ordering and exact launch binding.
#![allow(
    clippy::unwrap_used,
    reason = "Fixtures assert construction and refusal directly."
)]
use super::*;
use crate::{
    cache::CacheBase,
    registry::{
        AgentRegistration, EnvelopeRegistration, MeasuredFile, NetworkPolicy, Provider,
        RuntimePackage,
    },
    session_manifest::{SessionInputManifest, SessionInputs},
};

fn registry_file(root: &Path, name: &str, entries: &impl serde::Serialize) {
    fs::write(
        root.join(name),
        serde_json::to_vec(&serde_json::json!({
            "schema": crate::registry::REGISTRY_SCHEMA, "entries": entries,
        }))
        .unwrap(),
    )
    .unwrap();
}

fn registry_fixture(root: &Path) -> (Registry, AgentRegistration, RuntimePackage) {
    let runtime_root = root.join("runtime");
    fs::create_dir(&runtime_root).unwrap();
    for name in ["agent", "z", "a"] {
        fs::write(runtime_root.join(name), name.as_bytes()).unwrap();
    }
    fs::set_permissions(
        runtime_root.join("agent"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let agent = AgentRegistration {
        id: "agent".into(),
        provider: Provider::Fixed("fixture".into()),
        runtime_id: "runtime".into(),
        arguments: vec![],
        environment: std::collections::BTreeMap::new(),
        tool_integration: None,
    };
    let runtime = RuntimePackage {
        id: "runtime".into(),
        root: runtime_root,
        executable: "agent".into(),
        executable_sha256: Digest::of(b"agent").hex().into(),
        adapters: ["z", "a"]
            .into_iter()
            .map(|path| MeasuredFile {
                path: path.into(),
                sha256: Digest::of(path.as_bytes()).hex().into(),
            })
            .collect(),
        version: "1".into(),
        origin: "fixture".into(),
    };
    registry_file(root, "agents.json", &[&agent]);
    registry_file(root, "runtimes.json", &[&runtime]);
    registry_file(
        root,
        "envelopes.json",
        &[EnvelopeRegistration {
            id: "empty".into(),
            network: NetworkPolicy::Denied,
            description: "fixture".into(),
        }],
    );
    (Registry::open(root).unwrap(), agent, runtime)
}

#[test]
fn launch_loader_uses_canonical_runtime_and_rejects_foreign_bindings() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let (registry, agent, runtime) = registry_fixture(root);
    let snapshot = root.join("snapshot");
    fs::create_dir_all(snapshot.join("files")).unwrap();
    let bytes = format!(
        "{{\"schema\":\"louiselm.workspace.snapshot/1\",\"base_commit\":\"{}\",\"files\":[],\"selected\":[],\"changes\":[]}}",
        "a".repeat(40)
    );
    fs::write(snapshot.join("snapshot.json"), &bytes).unwrap();
    let cache = root.join("cache");
    fs::create_dir(&cache).unwrap();
    let identity = Digest::of(b"fixture").to_string();
    let manifest = SessionInputManifest::build(SessionInputs {
        agent: Some(agent),
        runtime: Some(runtime.measure().unwrap()),
        skill_generation_id: Some(identity.clone()),
        view_digest: Some(identity.clone()),
        project_instructions: Some(vec![]),
        tool_schemas: Some(vec![]),
        plugin_schemas: Some(vec![]),
        source_snapshot_digest: Some(Digest::of(bytes.as_bytes()).to_string()),
        source_base_digest: Some(Digest::of(b"[]").to_string()),
        cache_base_digest: Some(CacheBase::capture(&cache).unwrap().digest().to_string()),
        policy_digest: Some(identity.clone()),
        isolation_receipt: Some("isolation".into()),
        envelope_id: Some("empty".into()),
        envelope_revision: Some(1),
        acp_mcp_servers: Some(vec![]),
    })
    .unwrap();
    let input_root = root.join("inputs");
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&input_root)
        .unwrap();
    launch_inputs::stage(
        &manifest,
        &snapshot,
        &cache,
        &input_root.join(manifest.digest().hex()),
    )
    .unwrap();
    let mut request = LaunchRequest {
        schema: crate::launch::REQUEST_SCHEMA.into(),
        protocol_version: 1,
        request_id: "launch".into(),
        authorization_id: "approval".into(),
        session_id: "session".into(),
        run_id: "run".into(),
        agent_id: "agent".into(),
        envelope_id: "empty".into(),
        envelope_revision: 1,
        skill_generation_id: identity,
        session_input_manifest_id: manifest.digest().to_string(),
    };
    let owner = (
        rustix::process::geteuid().as_raw(),
        rustix::process::getegid().as_raw(),
    );
    assert!(load(&input_root, owner, &request, &registry).is_ok());
    request.envelope_revision += 1;
    assert!(matches!(
        load(&input_root, owner, &request, &registry),
        Err(SupervisorError::ResolutionFailed)
    ));
}
