//! Launch staging freezes the complete selected source/cache input before launch.

use super::*;
use louiselm_skills::{
    Digest,
    cache::CacheBase,
    registry::{AgentRegistration, Provider, RuntimeMeasurement},
    session_manifest::{SessionInputManifest, SessionInputs},
};

fn inputs(temp: &TempDir) -> SessionInputManifest {
    fs::write(temp.path().join("repo/tracked.txt"), b"selected dirty\n").unwrap();
    fs::write(temp.path().join("repo/.env"), b"excluded secret").unwrap();
    let snapshot = prepare(temp, "snapshot", &["tracked.txt"]);
    fs::create_dir(temp.path().join("cache")).unwrap();
    fs::write(temp.path().join("cache/dependency"), b"warm cache").unwrap();
    let digest = |bytes: &[u8]| Some(Digest::of(bytes).to_string());
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
        source_snapshot_digest: Some(snapshot["snapshot_digest"].as_str().unwrap().into()),
        source_base_digest: Some(snapshot["base_digest"].as_str().unwrap().into()),
        cache_base_digest: Some(
            CacheBase::capture(&temp.path().join("cache"))
                .unwrap()
                .digest()
                .to_string(),
        ),
        policy_digest: digest(b"policy"),
        isolation_receipt: Some("isolation".into()),
        envelope_id: Some("envelope".into()),
        envelope_revision: Some(1),
        acp_mcp_servers: Some(vec![]),
    })
    .unwrap()
}

fn stage(temp: &TempDir, manifest: &SessionInputManifest) -> Output {
    fs::write(
        temp.path().join("manifest.json"),
        manifest.canonical_bytes(),
    )
    .unwrap();
    run(&[
        "launch-inputs",
        "stage",
        "--manifest",
        temp.path().join("manifest.json").to_str().unwrap(),
        "--snapshot",
        temp.path().join("snapshot").to_str().unwrap(),
        "--cache",
        temp.path().join("cache").to_str().unwrap(),
        "--output",
        temp.path().join("staged").to_str().unwrap(),
        "--robot-json",
    ])
}

#[test]
fn launch_staging_freezes_source_cache_and_selection_preview() {
    let temp = fixture();
    let manifest = inputs(&temp);
    let preview = successful(&stage(&temp, &manifest));
    assert_eq!(preview["manifest_digest"], manifest.digest().to_string());
    assert!(
        preview["source"]["changes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["path"] == ".env" && p["included"] == false)
    );
    assert_eq!(
        fs::read(temp.path().join("staged/snapshot/files/tracked.txt")).unwrap(),
        b"selected dirty\n"
    );
    assert!(!temp.path().join("staged/snapshot/files/.env").exists());
    fs::write(temp.path().join("repo/tracked.txt"), b"later checkout").unwrap();
    fs::write(temp.path().join("cache/dependency"), b"poisoned later").unwrap();
    let inspect = || {
        run(&[
            "launch-inputs",
            "inspect",
            "--input",
            temp.path().join("staged").to_str().unwrap(),
            "--digest",
            &manifest.digest().to_string(),
            "--robot-json",
        ])
    };
    assert_eq!(successful(&inspect()), preview);
    assert_eq!(
        fs::read(temp.path().join("staged/cache/dependency")).unwrap(),
        b"warm cache"
    );
    assert!(
        !stage(&temp, &manifest).status.success(),
        "existing inputs are never replaced"
    );
    let cached = temp.path().join("staged/cache/dependency");
    fs::set_permissions(&cached, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(cached, b"tampered").unwrap();
    assert!(
        !inspect().status.success(),
        "remeasure staged bytes before use"
    );
}

#[test]
fn launch_staging_refuses_substitution_and_links_before_publication() {
    let temp = fixture();
    let mut manifest = inputs(&temp);
    let correct = manifest.source_base_digest.clone();
    manifest.source_base_digest = Digest::of(b"substitute").to_string();
    assert!(!stage(&temp, &manifest).status.success());
    assert!(!temp.path().join("staged").exists());
    manifest.source_base_digest = correct;
    symlink("/etc/passwd", temp.path().join("cache/link")).unwrap();
    assert!(!stage(&temp, &manifest).status.success());
    assert!(!temp.path().join("staged").exists());
}
