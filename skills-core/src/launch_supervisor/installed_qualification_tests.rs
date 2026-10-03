//! Real installed paired Lua trial with the measured offline ACP peer.
use super::*;
use crate::{
    broker::provider_credentials::ProviderCredentialStore,
    conformance::{
        ReportResult,
        admission::Enforcement,
        installed::{certify, measure},
    },
    registry::Provider,
};

fn share(path: &Path, uid: u32) {
    if path.is_dir() {
        for entry in fs::read_dir(path).unwrap() {
            share(&entry.unwrap().path(), uid);
        }
    }
    chown(path, Some(uid), Some(BROKER_UID)).unwrap();
    fs::set_permissions(
        path,
        fs::Permissions::from_mode(if path.is_dir() { 0o750 } else { 0o640 }),
    )
    .unwrap();
}

#[test]
fn privileged_activated_brokered_guard_start() {
    if std::env::var_os("LOUISELM_REQUIRE_ACTIVATED_GUARD").is_none() {
        eprintln!("skipping: requires disposable VM, Neovim and private mounts");
        return;
    }
    run_installed(true);
}

#[test]
fn privileged_installed_lua_qualification() {
    if std::env::var_os("LOUISELM_REQUIRE_QUALIFICATION").is_none() {
        eprintln!("skipping: requires disposable VM, Neovim and private mounts");
        return;
    }
    run_installed(false);
}

#[expect(
    clippy::too_many_lines,
    reason = "One disposable installed transaction binds fixture inputs, explicit launch authority and proven terminal cleanup."
)]
fn run_installed(start_only: bool) {
    assert!(rustix::process::geteuid().is_root());
    let root = tempfile::Builder::new()
        .prefix("louiselm-qualification-")
        .tempdir_in("/var/lib")
        .unwrap();
    mounts(root.path());
    let _account = BrokerAccount::create();
    let (paths, config, manager) = install_unseeded_daemon(root.path(), Enforcement::Enforced, 4);
    let registry = root.path().join("registry");
    registry_record(
        &registry.join("agents.json"),
        &serde_json::json!([{
            "id":"agent","provider":"openai","runtime_id":"runtime","arguments":[],"environment":{},
            "tool_integration":crate::launch_supervisor::tool_integration::CONTRACT
        }]),
    );
    registry_record(
        &registry.join("envelopes.json"),
        &serde_json::json!([{
            "id":"envelope","network":"brokered","description":"offline qualification gate"
        }]),
    );
    fs::create_dir(SYSTEM_REGISTRY_ROOT).unwrap();
    for name in ["agents.json", "runtimes.json", "envelopes.json"] {
        fs::copy(
            registry.join(name),
            Path::new(SYSTEM_REGISTRY_ROOT).join(name),
        )
        .unwrap();
    }
    // First startup owns identity initialization while state is still empty.
    // Provision fixture custody only afterward, then restart to load it.
    let mut initialize = process(&manager, BROKER_UID, false);
    ready(&config);
    terminate(&mut initialize);
    let custody = ProviderCredentialStore::root_in(Path::new(STATE));
    chown(&custody, Some(BROKER_UID), Some(BROKER_UID)).unwrap();
    fs::set_permissions(&custody, fs::Permissions::from_mode(0o700)).unwrap();
    let credential = custody.join("openai");
    fs::write(&credential, b"qualification-fixture-secret").unwrap();
    chown(&credential, Some(BROKER_UID), Some(BROKER_UID)).unwrap();
    fs::set_permissions(&credential, fs::Permissions::from_mode(0o600)).unwrap();
    let parent = rustix::process::getppid()
        .unwrap()
        .as_raw_nonzero()
        .get()
        .cast_unsigned();
    measure(&paths, &config, Instant::now() + Duration::from_mins(3)).unwrap();
    let certificate = certify(&paths, Instant::now() + Duration::from_mins(3), parent).unwrap();
    assert_eq!(
        certificate.observations.result().unwrap(),
        ReportResult::Passed
    );

    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o755)).unwrap();
    let operator = root.path().join("operator");
    fs::create_dir(&operator).unwrap();
    let snapshot = operator.join("snapshot");
    fs::create_dir(&snapshot).unwrap();
    fs::create_dir(snapshot.join("files")).unwrap();
    let base_commit = "a".repeat(40);
    let snapshot_bytes = format!(
        "{{\"schema\":\"louiselm.workspace.snapshot/1\",\"base_commit\":\"{base_commit}\",\"files\":[],\"selected\":[],\"changes\":[]}}"
    );
    fs::write(snapshot.join("snapshot.json"), snapshot_bytes.as_bytes()).unwrap();
    let cache = operator.join("cache");
    fs::create_dir(&cache).unwrap();
    let outside = operator.join("outside-secret");
    fs::write(&outside, b"must remain outside every Session\n").unwrap();
    let plan = operator.join("plan.json");
    write_json(
        &plan,
        &serde_json::json!({
            "schema":"louiselm.workspace.verification-plan/1",
            "commands":[{"argv":["sh","-c","test -f .trial-marker"],"cwd":".","timeout_ms":5000}]
        }),
    );
    let mut inputs = workspace::fixture_manifest();
    inputs.agent.provider = Provider::Fixed("openai".into());
    inputs.provider_disclosure.providers = vec!["openai".into()];
    crate::session_manifest::SessionInputManifest::parse(&inputs.canonical_bytes()).unwrap();
    let now = u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    let mut provider = guard::approval(now);
    provider.max_run_requests = 12;
    provider.models = vec!["fixture-big".into(), "fixture-small".into()];
    provider.max_effort = crate::provider_request::ReasoningEffort::High;
    provider.expires_at_ms = now + 120_000;
    let grant = GrantRequest {
        role: crate::launch_protocol::LaunchRole::Agent,
        request: named_request("qualification"),
        controller_uid: config.operator_uid,
        expires_at_ms: now + 120_000,
        broker_loss_grace_ms: 5000,
        require_cold_recovery: false,
        conformance: crate::launch_protocol::ConformanceAuthorization::default(),
        commands: None,
        dependencies: None,
        skill_requests: None,
        beads_mutations: None,
        provider_requests: Some(provider),
    };
    let mut envelope = fixture_run_envelope(
        &grant,
        Digest::of(&fs::read(&plan).unwrap()).to_string(),
        now,
    );
    envelope.run_id = "qualification".into();
    envelope.max_sessions = 4;
    let selected = serde_json::json!({
        "schema":"louiselm.qualification-run/1","policy_revision":"installed-fixture/1",
        "workload":{"kind":"main","id":"offline-installed"},"retention_days":7,
        "manifest":{
            "routes":{
                "baseline":{"mode":"direct","agent":"agent","provider":"openai","options":{"model":"fixture-big","reasoning_effort":"high"}},
                "candidate":{"mode":"direct","agent":"agent","provider":"openai","options":{"model":"fixture-small","reasoning_effort":"low"}}
            },
            "limits":{"model_requests":12,"elapsed_seconds":120,"input_bytes":10000,"output_bytes":2000},
            "fixtures":[
                {"id":"bulk","provenance":{"reference":"installed synthetic fixture"},"acceptance":{"reference_checks":[{"answer_contains":"OFFLINE_QUALIFICATION_OK","citation":"[A]"}],"commands":[],"human_review":null}},
                {"id":"mechanical","provenance":{"reference":"installed synthetic fixture"},"acceptance":{"reference_checks":[],"commands":[["sh","-c","test -f .trial-marker"]],"human_review":null}},
                {"id":"reasoning","provenance":{"reference":"installed synthetic fixture"},"acceptance":{"reference_checks":[],"commands":[],"human_review":"explicit human acceptance remains pending"}}
            ]
        },
        "prompts":[format!("qualification-fixture-1|{}", outside.display()),"qualification-fixture-2","qualification-fixture-3"],
        "envelope":envelope,"input_manifest":inputs,"snapshot":snapshot,"snapshot_digest":inputs.source_snapshot_digest,
        "base_commit":base_commit,"plan":plan,"cache":cache
    });
    share(&operator, config.operator_uid);
    let input = operator.join("selection.json");
    write_json(&input, &selected);
    chown(&input, Some(config.operator_uid), Some(config.operator_uid)).unwrap();
    fs::set_permissions(&input, fs::Permissions::from_mode(0o600)).unwrap();
    let mut daemon = process(&manager, BROKER_UID, false);
    ready(&config);
    let project = std::env::var_os("LOUISELM_TEST_LUA_ROOT").expect("explicit Lua checkout");
    let neovim = std::env::var_os("LOUISELM_TEST_NVIM").expect("explicit trusted Neovim");
    let status = Command::new("/usr/bin/setpriv")
        .args([
            "--reuid",
            &config.operator_uid.to_string(),
            "--regid",
            &config.operator_uid.to_string(),
            "--groups",
            &BROKER_UID.to_string(),
        ])
        .arg(neovim)
        .args([
            "--headless",
            "--noplugin",
            "-u",
            "NONE",
            "-l",
            "tests/routing/installed_trial.lua",
        ])
        .current_dir(project)
        .env_clear()
        .env("PATH", "/usr/local/lib/louiselm/current/bin:/usr/bin:/bin")
        .env("LOUISELM_QUALIFICATION_FIXTURE", input)
        .env(
            "LOUISELM_QUALIFICATION_START_ONLY",
            if start_only { "1" } else { "0" },
        )
        .env("QUALIFICATION_AMBIENT_SECRET", "must-not-reach-Agent")
        .env("XDG_STATE_HOME", operator.join("state"))
        .status()
        .unwrap();
    if !status.success() {
        terminate(&mut daemon);
    }
    assert!(
        status.success(),
        "installed paired Lua qualification failed"
    );
    let deadline = Instant::now() + Duration::from_secs(15);
    let sessions = &[
        "baseline-worker",
        "baseline-verifier",
        "candidate-worker",
        "candidate-verifier",
    ];
    let mut identities = std::collections::BTreeSet::new();
    for suffix in sessions {
        let id = format!("qualification-{suffix}");
        let directory = Path::new(STATE).join("receipts/sessions").join(&id);
        loop {
            // An exited worker correctly cannot return current live status.
            // Cleanup acceptance is the authenticated terminal receipt, not
            // StatusUnavailable guessed to mean successful process death.
            let mut files: Vec<_> = fs::read_dir(&directory)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .filter(|path| path.to_string_lossy().ends_with(".receipt.json"))
                .collect();
            files.sort();
            let chain: Vec<_> = files
                .iter()
                .map(|path| {
                    crate::launch_receipt::SignedReceipt::parse_canonical(&fs::read(path).unwrap())
                        .unwrap()
                })
                .collect();
            if chain.last().is_some_and(|receipt| {
                receipt.payload.resulting_state == crate::launch_receipt::SessionState::Terminal
            }) {
                let verifier =
                    crate::launcher_install::LauncherVerifier::open(&paths, Path::new(STATE))
                        .unwrap();
                let anchor = verifier.receipt_anchor(&id).unwrap();
                crate::launch_receipt::verify_chain(&chain, &anchor, |key, payload, signature| {
                    verifier.verify(key, payload, signature).is_ok()
                })
                .unwrap();
                let crate::launch_receipt::ReceiptOutcome::Launch { evidence, .. } =
                    &chain[0].payload.outcome
                else {
                    panic!("sequence zero must bind the launch role");
                };
                assert_eq!(
                    evidence.role,
                    if suffix.ends_with("worker") {
                        crate::launch_protocol::LaunchRole::Agent
                    } else {
                        crate::launch_protocol::LaunchRole::FixedVerifier
                    }
                );
                break;
            }
            assert!(
                Instant::now() < deadline,
                "terminal cleanup missing for {id}"
            );
            thread::sleep(Duration::from_millis(25));
        }
        let start = crate::launch_receipt::SignedReceipt::parse_canonical(
            &fs::read(
                Path::new(STATE)
                    .join("receipts/sessions")
                    .join(&id)
                    .join("00000000000000000001.receipt.json"),
            )
            .unwrap(),
        )
        .unwrap();
        let crate::launch_receipt::ReceiptOutcome::Start { evidence, .. } = start.payload.outcome
        else {
            panic!("sequence one must retain Start evidence");
        };
        assert_eq!(evidence.sender_guard_required, suffix.ends_with("worker"));
        assert!(
            identities.insert(evidence.assigned_uid),
            "Session identity was reused within the Run"
        );
    }
    assert_eq!(
        fs::read(&outside).unwrap(),
        b"must remain outside every Session\n"
    );
    assert!(!snapshot.join("files/.trial-marker").exists());
    terminate(&mut daemon);
    println!(
        "INSTALLED_{}",
        if start_only {
            "ACTIVATED_GUARD_START_PASSED"
        } else {
            "QUALIFICATION_PASSED"
        }
    );
}
