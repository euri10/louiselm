//! Trusted preparation consumes real fixture owners without authorizing a Run.
use super::*;
use louiselm_skills::{
    cache::CacheBase,
    session_manifest::SessionInputManifest,
    workspace::{
        self, WorkspaceError,
        launch_inputs::{self, Preparation, RunProposal},
    },
};
use std::{path::PathBuf, process::Command};

struct PreparedFixture {
    supply: Supply,
    registry: Registry,
    root: PathBuf,
    snapshot: PathBuf,
    cache: PathBuf,
    source_digest: Digest,
    cache_digest: Digest,
}

impl PreparedFixture {
    fn new() -> Self {
        let supply = Supply::new();
        let generation = supply.admit(vec![supply.member("prepared", &["demo"])]);
        supply.activate(&generation);
        let root = supply.fixture.path("preparation");
        let runtime = root.join("runtime");
        write_file(&runtime.join("bin/agent"), "measured, never executed");
        write_file(&runtime.join("lib/adapter.js"), "measured adapter");
        let registry_root = root.join("registry");
        support::write_registry(&registry_root, &runtime);
        write_file(
            &registry_root.join("agents.json"),
            r#"{"schema":"louiselm.launch.registry/1","entries":[{"id":"demo","provider":"demo-provider","runtime_id":"demo-runtime","arguments":[],"environment":{},"tool_integration":"louiselm.test-tool-integration/1"}]}"#,
        );
        let registry = Registry::open(&registry_root).unwrap();
        let repository = root.join("repo");
        write_file(&repository.join("AGENTS.md"), "approved rules\n");
        for args in [
            vec!["init", "--quiet"],
            vec!["add", "AGENTS.md"],
            vec![
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--quiet",
                "-m",
                "Fixture",
            ],
        ] {
            assert!(
                Command::new("git")
                    .args(args)
                    .current_dir(&repository)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let snapshot = root.join("snapshot");
        let source = workspace::prepare(&repository, &[], &snapshot).unwrap();
        let cache = root.join("cache");
        fs::create_dir(&cache).unwrap();
        let source_digest = Digest::parse(&source.snapshot_digest).unwrap();
        let cache_digest = CacheBase::capture(&cache).unwrap().digest().clone();
        Self {
            supply,
            registry,
            root,
            snapshot,
            cache,
            source_digest,
            cache_digest,
        }
    }

    fn selection<'a>(&'a self, instructions: &'a [String]) -> Preparation<'a> {
        Preparation {
            agent_id: "demo",
            envelope_id: "denied",
            snapshot: &self.snapshot,
            snapshot_digest: &self.source_digest,
            cache: &self.cache,
            cache_digest: &self.cache_digest,
            project_instructions: instructions,
        }
    }

    fn prepare(
        &self,
        request: &Preparation<'_>,
        output: &str,
    ) -> Result<RunProposal, WorkspaceError> {
        launch_inputs::prepare(
            &self.supply.fixture.store(),
            &Policy::embedded(),
            &self.registry,
            request,
            &self.root.join(output),
        )
    }
}

#[test]
fn prepares_complete_measured_inputs_and_an_inactive_fresh_run() {
    let fixture = PreparedFixture::new();
    let instructions = vec!["AGENTS.md".to_owned()];
    let request = fixture.selection(&instructions);
    let output = fixture.root.join("prepared");
    let proposal = fixture.prepare(&request, "prepared").unwrap();
    assert_eq!(proposal.envelope.revision, 1);
    let manifest =
        SessionInputManifest::parse(&fs::read(output.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest.digest().to_string(), proposal.manifest_digest);
    assert_eq!(
        manifest.skill_generation.generation_digest,
        louiselm_skills::session_manifest::SessionInputs::resolve(
            &fixture.supply.fixture.store(),
            &Policy::embedded(),
            &fixture.registry,
            "demo"
        )
        .unwrap()
        .skill_generation_id
        .unwrap()
    );
    assert_eq!(
        manifest.project_instructions[0].sha256,
        Digest::of(b"approved rules\n").hex()
    );
    assert!(
        manifest.tool_schemas.is_empty()
            && manifest.plugin_schemas.is_empty()
            && manifest.acp_mcp_servers.is_empty()
    );
    assert_eq!(
        launch_inputs::inspect_proposal(&output, &manifest.digest()).unwrap(),
        proposal
    );
    let second = fixture.prepare(&request, "second").unwrap();
    assert_ne!(second.run_id, proposal.run_id);
    assert_eq!(second.manifest_digest, proposal.manifest_digest);
    assert!(matches!(
        fixture.prepare(&request, "prepared"),
        Err(WorkspaceError::Invalid(
            "output already exists; choose a new destination"
        ))
    ));
    assert_eq!(proposal.schema, "louiselm.run.input-proposal/1");
    let inspected = Command::new(env!("CARGO_BIN_EXE_louiselm-skills"))
        .args(["workspace", "launch-inputs", "inspect-proposal", "--input"])
        .arg(&output)
        .args(["--digest", &proposal.manifest_digest, "--robot-json"])
        .output()
        .unwrap();
    assert!(
        inspected.status.success(),
        "{}",
        String::from_utf8_lossy(&inspected.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<RunProposal>(&inspected.stdout).unwrap(),
        proposal
    );
    assert!(
        !String::from_utf8(inspected.stdout)
            .unwrap()
            .contains("approved rules")
    );
}

#[test]
fn changed_or_unresolved_selection_publishes_nothing() {
    let fixture = PreparedFixture::new();
    let missing = vec!["missing.md".to_owned()];
    assert!(matches!(
        fixture.prepare(&fixture.selection(&missing), "missing"),
        Err(WorkspaceError::Invalid(
            "selected project instruction missing"
        ))
    ));
    assert!(!fixture.root.join("missing").exists());
    let instructions = vec!["AGENTS.md".to_owned()];
    let foreign = Digest::of(b"other");
    let mut request = fixture.selection(&instructions);
    request.snapshot_digest = &foreign;
    assert!(matches!(
        fixture.prepare(&request, "snapshot-changed"),
        Err(WorkspaceError::Invalid(
            "snapshot digest mismatch; inspect the selected snapshot"
        ))
    ));
    request.snapshot_digest = &fixture.source_digest;
    request.cache_digest = &foreign;
    assert!(matches!(
        fixture.prepare(&request, "cache-changed"),
        Err(WorkspaceError::Invalid("selected cache digest mismatch"))
    ));
    assert!(!fixture.root.join("snapshot-changed").exists());
    assert!(!fixture.root.join("cache-changed").exists());
    let duplicates = vec!["AGENTS.md".to_owned(), "AGENTS.md".to_owned()];
    assert!(matches!(
        fixture.prepare(&fixture.selection(&duplicates), "duplicate"),
        Err(WorkspaceError::Input(
            louiselm_skills::session_manifest::SessionManifestError::Duplicate {
                field: "project_instructions"
            }
        ))
    ));
    let empty = fixture
        .prepare(&fixture.selection(&[]), "explicit-empty")
        .unwrap();
    assert_eq!(empty.envelope.revision, 1);
    let path = fixture.snapshot.join("files/AGENTS.md");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(&path, "changed after selection").unwrap();
    assert!(matches!(
        fixture.prepare(&fixture.selection(&instructions), "tampered"),
        Err(WorkspaceError::Invalid(
            "snapshot file differs from its inventory"
        ))
    ));
    assert!(!fixture.root.join("tampered").exists());
}

#[test]
fn stored_proposal_tampering_is_not_an_approval_or_an_input_binding() {
    let fixture = PreparedFixture::new();
    let instructions = vec!["AGENTS.md".to_owned()];
    let proposal = fixture
        .prepare(&fixture.selection(&instructions), "prepared")
        .unwrap();
    let input = fixture.root.join("prepared");
    let path = input.join("proposal.json");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let mutations: [fn(&mut RunProposal); 5] = [
        |p| p.run_id = "not-a-fresh-run".into(),
        |p| p.envelope.revision = 2,
        |p| p.envelope.id = "another".into(),
        |p| p.manifest_digest = Digest::of(b"other").to_string(),
        |p| p.base_commit = "a".repeat(40),
    ];
    for mutate in mutations {
        let mut changed = proposal.clone();
        mutate(&mut changed);
        fs::write(&path, serde_json::to_vec(&changed).unwrap()).unwrap();
        assert!(matches!(
            launch_inputs::inspect_proposal(
                &input,
                &Digest::parse(&proposal.manifest_digest).unwrap()
            ),
            Err(WorkspaceError::Invalid("Run proposal binding mismatch"))
        ));
    }
}

#[test]
#[ignore = "requires disposable VM root with TMPDIR=/var/lib, never host sudo"]
fn privileged_preparation_cli_measures_trusted_owners_without_launch() {
    assert!(rustix::process::geteuid().is_root());
    assert_eq!(std::env::var("TMPDIR").unwrap(), "/var/lib");
    let fixture = PreparedFixture::new();
    Registry::open_trusted(fixture.registry.root()).unwrap();
    let instructions = fixture.root.join("instructions.json");
    fs::write(&instructions, br#"["AGENTS.md"]"#).unwrap();
    let destination = fixture.root.join("cli-prepared");
    let prepare = || {
        Command::new(env!("CARGO_BIN_EXE_louiselm-skills"))
            .args(["workspace", "launch-inputs", "prepare", "--store"])
            .arg(fixture.supply.fixture.store_root())
            .arg("--registry")
            .arg(fixture.registry.root())
            .args(["--agent", "demo", "--envelope", "denied", "--snapshot"])
            .arg(&fixture.snapshot)
            .args(["--snapshot-digest", &fixture.source_digest.to_string()])
            .arg("--cache")
            .arg(&fixture.cache)
            .args(["--cache-digest", &fixture.cache_digest.to_string()])
            .arg("--instructions")
            .arg(&instructions)
            .arg("--output")
            .arg(&destination)
            .arg("--robot-json")
            .output()
            .unwrap()
    };
    fs::write(&instructions, br#"{"implicit":"instructions"}"#).unwrap();
    let refused = prepare();
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr)
            .contains("project instructions must be an explicit JSON array")
    );
    assert!(!destination.exists());
    fs::write(&instructions, br#"["AGENTS.md"]"#).unwrap();
    let output = prepare();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let proposal: RunProposal = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        launch_inputs::inspect_proposal(
            &destination,
            &Digest::parse(&proposal.manifest_digest).unwrap()
        )
        .unwrap(),
        proposal
    );
    assert_eq!(proposal.envelope.revision, 1);
    assert!(!prepare().status.success(), "existing output is refused");
}
