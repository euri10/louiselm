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
    );
    let (_, tracker, _) = beads::provision(root.path());
    let manifest = workspace::fixture_manifest();
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
    let mut grants = Vec::new();
    for index in 1..=3 {
        let mut launch = named_request(&format!("lua-worker-{index}"));
        launch.run_id = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee".into();
        launch.session_input_manifest_id = manifest.digest().to_string();
        grants.push(GrantRequest {
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
                issue_ids: vec![format!("fixture-{index}")],
                effects: vec![crate::beads_mutation::BeadsEffect::CommentAdd],
                max_mutations: 1,
                expires_at_ms: now + 120_000,
            }),
        });
    }
    let mut envelope = fixture_run_envelope(&grants[0], Digest::of(b"lua-plan").to_string(), now);
    envelope.bead_scope = grants[0].beads_mutations.clone().unwrap();
    envelope.bead_scope.role = crate::beads_mutation::BeadsRole::Coordinator;
    envelope.bead_scope.issue_ids.push("fixture-2".into());
    envelope.bead_scope.issue_ids.push("fixture-3".into());
    envelope.max_sessions = 3;
    let input = root.path().join("controller.json");
    write_json(
        &input,
        &serde_json::json!({"envelope":envelope,"grants":grants}),
    );
    let state = root.path().join("operator");
    fs::create_dir(&state).unwrap();
    chown(&state, Some(config.operator_uid), Some(config.operator_uid)).unwrap();
    let mut daemon = process(&manager, BROKER_UID, false);
    ready(&config);
    let project = std::env::var_os("LOUISELM_TEST_LUA_ROOT").expect("explicit Lua checkout");
    let neovim = std::env::var_os("LOUISELM_TEST_NVIM").expect("explicit stable Neovim executable");
    let status = Command::new("/usr/bin/setpriv")
        .args([
            "--reuid",
            &config.operator_uid.to_string(),
            "--regid",
            &config.operator_uid.to_string(),
            "--clear-groups",
        ])
        .arg(neovim)
        .args([
            "--headless",
            "--noplugin",
            "-u",
            "NONE",
            "-l",
            "tests/workflow/installed_beads.lua",
        ])
        .current_dir(project)
        .env_clear()
        .env("PATH", "/usr/local/lib/louiselm/current/bin:/usr/bin:/bin")
        .env("XDG_STATE_HOME", state)
        .env("LOUISELM_BEADS_FIXTURE", input)
        .status()
        .unwrap();
    if status.success() {
        for index in 1..=3 {
            wait_terminal(&format!("lua-worker-{index}"));
        }
        eprintln!("installed Lua workers: three durable terminal cleanup receipts");
    }
    terminate(&mut daemon);
    assert!(status.success(), "installed Lua Bead executor failed");
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
