//! Exact source/cache fixtures shared by installed launch acceptance tests.

use super::*;
use crate::{
    cache::CacheBase,
    registry::{AgentRegistration, Provider, RuntimeMeasurement},
    session_manifest::{SessionInputManifest, SessionInputs},
    workspace::launch_inputs,
};

fn snapshot_bytes() -> Vec<u8> {
    format!("{{\"schema\":\"louiselm.workspace.snapshot/1\",\"base_commit\":\"{}\",\"files\":[],\"selected\":[],\"changes\":[]}}", "a".repeat(40)).into_bytes()
}

pub(in crate::launch_supervisor) fn fixture_manifest() -> SessionInputManifest {
    let binaries = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_owned();
    let mut cache_bytes = b"louiselm.cache.base/1\n".to_vec();
    cache_bytes.extend(
        crate::Manifest::new(vec![], false)
            .unwrap()
            .canonical_bytes(),
    );
    SessionInputManifest::build(SessionInputs {
        agent: Some(AgentRegistration {
            id: "agent".into(),
            provider: Provider::Fixed("fixture".into()),
            runtime_id: "runtime".into(),
            arguments: vec![],
            environment: std::collections::BTreeMap::new(),
            tool_integration: Some(super::super::super::tool_integration::CONTRACT.into()),
        }),
        runtime: Some(RuntimeMeasurement {
            runtime_id: "runtime".into(),
            executable_sha256: Digest::of(
                &fs::read(binaries.join("louiselm-tool-test-agent")).unwrap(),
            )
            .hex()
            .into(),
            adapters: vec![],
            version: "fixture".into(),
            origin: "fixture".into(),
        }),
        skill_generation_id: Some(Digest::of(b"fixture-generation").to_string()),
        view_digest: Some(Digest::of(b"fixture-view").to_string()),
        project_instructions: Some(vec![]),
        tool_schemas: Some(vec![]),
        plugin_schemas: Some(vec![]),
        source_snapshot_digest: Some(Digest::of(&snapshot_bytes()).to_string()),
        source_base_digest: Some(Digest::of(b"[]").to_string()),
        cache_base_digest: Some(Digest::of(&cache_bytes).to_string()),
        policy_digest: Some(crate::Policy::embedded().digest().to_string()),
        isolation_receipt: Some("fixture-isolation".into()),
        envelope_id: Some("envelope".into()),
        envelope_revision: Some(1),
        acp_mcp_servers: Some(vec![]),
    })
    .unwrap()
}

pub(super) fn stage_fixture(config: &LauncherConfig) {
    stage_manifest(config, &fixture_manifest());
}

pub(in crate::launch_supervisor) fn stage_manifest(
    config: &LauncherConfig,
    inputs: &SessionInputManifest,
) {
    let staging = tempfile::tempdir().unwrap();
    fs::create_dir_all(staging.path().join("snapshot/files")).unwrap();
    fs::write(
        staging.path().join("snapshot/snapshot.json"),
        snapshot_bytes(),
    )
    .unwrap();
    fs::create_dir(staging.path().join("cache")).unwrap();
    assert_eq!(
        CacheBase::capture(&staging.path().join("cache"))
            .unwrap()
            .digest()
            .to_string(),
        inputs.cache_base_digest
    );
    let root = config
        .broker_socket_path
        .parent()
        .unwrap()
        .join("workspace-inputs");
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    launch_inputs::stage(
        inputs,
        &staging.path().join("snapshot"),
        &staging.path().join("cache"),
        &root.join(inputs.digest().hex()),
    )
    .unwrap();
    own(&root, config.broker_uid, config.broker_gid);
}

fn own(path: &Path, uid: u32, gid: u32) {
    if path.is_dir() {
        for entry in fs::read_dir(path).unwrap() {
            own(&entry.unwrap().path(), uid, gid);
        }
    }
    chown(path, Some(uid), Some(gid)).unwrap();
}
