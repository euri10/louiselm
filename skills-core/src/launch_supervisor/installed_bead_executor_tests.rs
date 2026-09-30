//! Installed broker/launcher and real Lua Sessions; offline measured ACP peer.
use super::*;

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "One installed transaction provisions exact child grants and runs the real Lua controller under the operator identity."
)]
fn privileged_installed_lua_bead_executor() {
    if std::env::var_os("LOUISELM_REQUIRE_BEAD_EXECUTOR").is_none() {
        eprintln!("skipping: requires disposable VM, Neovim and private mounts");
        return;
    }
    assert!(rustix::process::geteuid().is_root());
    let root = tempfile::Builder::new()
        .prefix("louiselm-lua-beads-")
        .tempdir_in("/var/lib")
        .unwrap();
    mounts(root.path());
    let _account = BrokerAccount::create();
    let (_, config, manager) = install_unseeded_daemon(
        root.path(),
        crate::conformance::admission::Enforcement::PreCutover,
        6,
    );
    let (program, tracker, bead_id) = beads::provision(root.path());
    let manifest = workspace::fixture_manifest();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o755)).unwrap();
    fs::create_dir(SYSTEM_REGISTRY_ROOT).unwrap();
    for name in ["agents.json", "runtimes.json", "envelopes.json"] {
        fs::copy(
            root.path().join("registry").join(name),
            Path::new(SYSTEM_REGISTRY_ROOT).join(name),
        )
        .unwrap();
    }
    let now = u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    let mut cases = Vec::new();
    let verification_root = Path::new("/var/lib/louiselm/verification-fixture");
    fs::create_dir(verification_root).unwrap();
    fs::set_permissions(verification_root, fs::Permissions::from_mode(0o755)).unwrap();
    let snapshot = verification_root.join("snapshot");
    fs::create_dir(&snapshot).unwrap();
    fs::set_permissions(&snapshot, fs::Permissions::from_mode(0o755)).unwrap();
    let snapshot_bytes = format!(
        "{{\"schema\":\"louiselm.workspace.snapshot/1\",\"base_commit\":\"{}\",\"files\":[],\"selected\":[],\"changes\":[]}}",
        "a".repeat(40)
    );
    fs::write(snapshot.join("snapshot.json"), snapshot_bytes.as_bytes()).unwrap();
    fs::set_permissions(
        snapshot.join("snapshot.json"),
        fs::Permissions::from_mode(0o444),
    )
    .unwrap();
    let snapshot_digest = Digest::of(snapshot_bytes.as_bytes()).to_string();
    for index in 1..=3 {
        let mut launch = named_request(&format!("lua-worker-{index}"));
        launch.run_id = format!("lua-run-{index}");
        launch.session_input_manifest_id = manifest.digest().to_string();
        let grant = GrantRequest {
            request: launch,
            controller_uid: config.operator_uid,
            expires_at_ms: now + 120_000,
            broker_loss_grace_ms: crate::launch::MAX_BROKER_LOSS_GRACE_MS,
            require_cold_recovery: false,
            conformance: crate::launch_protocol::ConformanceAuthorization::default(),
            dependencies: None,
            commands: None,
            skill_requests: None,
            provider_requests: None,
            beads_mutations: Some(crate::beads_mutation::ApprovedBeadsMutations {
                project_digest: Digest::of(tracker.as_os_str().as_encoded_bytes()).to_string(),
                role: crate::beads_mutation::BeadsRole::Worker,
                issue_ids: vec![bead_id.clone()],
                effects: vec![crate::beads_mutation::BeadsEffect::CommentAdd],
                max_mutations: 1,
                expires_at_ms: now + 120_000,
            }),
        };
        let mut verifier_launch = named_request(&format!("lua-verifier-{index}"));
        verifier_launch.run_id = grant.request.run_id.clone();
        verifier_launch.session_input_manifest_id = manifest.digest().to_string();
        let verifier_grant = GrantRequest {
            request: verifier_launch,
            controller_uid: config.operator_uid,
            expires_at_ms: now + 120_000,
            broker_loss_grace_ms: crate::launch::MAX_BROKER_LOSS_GRACE_MS,
            require_cold_recovery: false,
            conformance: crate::launch_protocol::ConformanceAuthorization::default(),
            dependencies: None,
            commands: None,
            skill_requests: None,
            provider_requests: None,
            beads_mutations: None,
        };
        let plan = verification_root.join(format!("plan-{index}.json"));
        let command = if index == 2 {
            serde_json::json!({"argv":["sh","-c","exit 7"],"cwd":".","timeout_ms":5000})
        } else {
            serde_json::json!({"argv":["true"],"cwd":".","timeout_ms":5000})
        };
        write_json(
            &plan,
            &serde_json::json!({"schema":"louiselm.workspace.verification-plan/1","commands":[command]}),
        );
        fs::set_permissions(&plan, fs::Permissions::from_mode(0o444)).unwrap();
        let plan_digest = Digest::of(&fs::read(&plan).unwrap()).to_string();
        let mut envelope = fixture_run_envelope(&grant, plan_digest.clone(), now);
        envelope.bead_scope = grant.beads_mutations.clone().unwrap();
        envelope.bead_scope.role = crate::beads_mutation::BeadsRole::Coordinator;
        envelope.max_sessions = 2;
        cases.push(
            serde_json::json!({"bead_id":bead_id,"envelope":envelope,"grant":grant,
            "verifier_grant":verifier_grant,"snapshot":snapshot,
            "snapshot_digest":snapshot_digest,"plan":plan,"plan_digest":plan_digest}),
        );
    }
    let input = root.path().join("controller.json");
    write_json(&input, &serde_json::json!({"cases":cases}));
    let state = root.path().join("operator");
    fs::create_dir(&state).unwrap();
    chown(&state, Some(config.operator_uid), Some(config.operator_uid)).unwrap();
    let mut daemon = process(&manager, BROKER_UID, false);
    ready(&config);
    let project = std::env::var_os("LOUISELM_TEST_LUA_ROOT").expect("explicit Lua checkout");
    let neovim = std::env::var_os("LOUISELM_TEST_NVIM").expect("explicit stable Neovim executable");
    let index: usize = std::env::var("LOUISELM_BEAD_EXECUTOR_CASE")
        .expect("explicit installed case")
        .parse()
        .unwrap();
    assert!((1..=3).contains(&index));
    let status = Command::new("/usr/bin/setpriv")
        .args([
            "--reuid",
            &config.operator_uid.to_string(),
            "--regid",
            &config.operator_uid.to_string(),
            "--clear-groups",
        ])
        .arg(&neovim)
        .args([
            "--headless",
            "--noplugin",
            "-u",
            "NONE",
            "-l",
            "tests/workflow/installed_beads.lua",
        ])
        .current_dir(&project)
        .env_clear()
        .env("PATH", "/usr/local/lib/louiselm/current/bin:/usr/bin:/bin")
        .env("XDG_STATE_HOME", &state)
        .env("LOUISELM_BEADS_FIXTURE", &input)
        .env("LOUISELM_BEADS_CASE", index.to_string())
        .status()
        .unwrap();
    assert!(
        status.success(),
        "installed Lua Bead executor case {index} failed"
    );
    if index > 1 && std::env::var_os("LOUISELM_TEST_BR").is_some() {
        let read = |arguments: &[&str]| {
            let output = Command::new("/usr/bin/setpriv")
                .args([
                    "--reuid",
                    &BROKER_UID.to_string(),
                    "--regid",
                    &BROKER_UID.to_string(),
                    "--clear-groups",
                ])
                .arg(&program)
                .args(arguments)
                .current_dir(&tracker)
                .env_clear()
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "real Beads readback failed: {output:?}"
            );
            serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()
        };
        let comments = read(&["comments", "list", &bead_id, "--json"]);
        let expected = if index == 2 {
            "verification_failed"
        } else {
            "worker_failed"
        };
        assert!(comments.as_array().unwrap().iter().any(|comment| {
            comment["text"].as_str().is_some_and(|text| {
                text.starts_with(&format!("Run lua-run-{index}: {expected}; observations: "))
                    && !text.contains("exit 7")
            })
        }));
        let issue = read(&["show", &bead_id, "--json"]);
        assert_eq!(issue[0]["status"], "open");
    }
    wait_terminal(&format!("lua-worker-{index}"));
    if index <= 2 {
        wait_terminal(&format!("lua-verifier-{index}"));
    }
    eprintln!("installed Lua workers and verifiers: durable terminal cleanup receipts");
    terminate(&mut daemon);
}

fn wait_terminal(id: &str) {
    // Operator inspection is live-only; its worker disappears after disposal.
    // Read the actual broker's validated durable receipt in this private fixture.
    let directory = Path::new(STATE).join("receipts/sessions").join(id);
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if fs::read_dir(&directory)
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".receipt.json")
                    && fs::read(entry.path()).ok().is_some_and(|bytes| {
                        crate::launch_receipt::SignedReceipt::parse_canonical(&bytes).is_ok_and(
                            |receipt| {
                                receipt.payload.session_id == id
                                    && receipt.payload.resulting_state
                                        == crate::launch_receipt::SessionState::Terminal
                            },
                        )
                    })
            })
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "no terminal cleanup receipt for {id}"
        );
        thread::sleep(Duration::from_millis(25));
    }
}
