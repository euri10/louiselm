//! Installed broker/launcher and real Lua Sessions; offline measured ACP peer.
use super::*;

fn own(path: &Path, uid: u32) {
    if path.is_dir() {
        for entry in fs::read_dir(path).unwrap() {
            own(&entry.unwrap().path(), uid);
        }
    }
    chown(path, Some(uid), Some(uid)).unwrap();
}

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
    let index: usize = std::env::var("LOUISELM_BEAD_EXECUTOR_CASE")
        .expect("explicit installed case")
        .parse()
        .unwrap();
    assert!((1..=5).contains(&index));
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
    let operator = root.path().join("operator");
    fs::create_dir(&operator).unwrap();
    chown(
        &operator,
        Some(config.operator_uid),
        Some(config.operator_uid),
    )
    .unwrap();
    let checkout = operator.join("run");
    fs::create_dir(&checkout).unwrap();
    for arguments in [
        vec!["init", "--quiet", "--initial-branch=run/lua-run-4"],
        vec![
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "Base",
        ],
    ] {
        assert!(
            Command::new("/usr/bin/git")
                .args(arguments)
                .current_dir(&checkout)
                .status()
                .unwrap()
                .success()
        );
    }
    let promotion_snapshot = operator.join("snapshot-4");
    let promotion_preview = crate::workspace::prepare(&checkout, &[], &promotion_snapshot).unwrap();
    let mut promotion_manifest = manifest.clone();
    promotion_manifest.source_snapshot_digest = promotion_preview.snapshot_digest.clone();
    promotion_manifest.source_base_digest = promotion_preview.base_digest.clone();
    let promotion_cache = operator.join("cache-4");
    fs::create_dir(&promotion_cache).unwrap();
    let journal = operator.join("journals");
    fs::create_dir(&journal).unwrap();
    fs::set_permissions(&journal, fs::Permissions::from_mode(0o700)).unwrap();
    own(&checkout, config.operator_uid);
    own(&promotion_snapshot, config.operator_uid);
    own(&promotion_cache, config.operator_uid);
    for directory in [&promotion_snapshot, &promotion_snapshot.join("files")] {
        fs::set_permissions(directory, fs::Permissions::from_mode(0o755)).unwrap();
    }
    fs::set_permissions(
        promotion_snapshot.join("snapshot.json"),
        fs::Permissions::from_mode(0o444),
    )
    .unwrap();
    chown(
        &journal,
        Some(config.operator_uid),
        Some(config.operator_uid),
    )
    .unwrap();
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
    for index in 1..=4 {
        let mut launch = named_request(&format!("lua-worker-{index}"));
        launch.run_id = format!("lua-run-{index}");
        launch.session_input_manifest_id = if index == 4 {
            &promotion_manifest
        } else {
            &manifest
        }
        .digest()
        .to_string();
        let grant = GrantRequest {
            role: crate::launch_protocol::LaunchRole::Agent,
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
        verifier_launch.session_input_manifest_id = grant.request.session_input_manifest_id.clone();
        let verifier_grant = GrantRequest {
            role: crate::launch_protocol::LaunchRole::FixedVerifier,
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
            "verifier_grant":verifier_grant,"snapshot":if index == 4 { &promotion_snapshot } else { &snapshot },
            "snapshot_digest":if index == 4 { &promotion_preview.snapshot_digest } else { &snapshot_digest },
            "base_commit":if index == 4 { Some(&promotion_preview.base_commit) } else { None },
            "worktree":if index == 4 { Some(&checkout) } else { None },
            "stage_inputs":if index == 4 { Some(crate::broker::operator::LaunchInputsRequest {
                manifest: Box::new(promotion_manifest.clone()), snapshot: promotion_snapshot.clone(),
                cache: promotion_cache.clone(), expected_base_commit: promotion_preview.base_commit.clone(),
            }) } else { None },
            "journal_parent":if index == 4 { Some(&journal) } else { None },
            "plan":plan,"plan_digest":plan_digest}),
        );
    }
    let input = root.path().join("controller.json");
    let selected = if index == 5 {
        operator_selection(
            &config,
            &operator,
            &checkout,
            &journal,
            &promotion_preview,
            &promotion_manifest,
            &cases[3],
        )
    } else {
        serde_json::json!({"cases":cases})
    };
    write_json(&input, &selected);
    let state = operator;
    let mut daemon = process(&manager, BROKER_UID, false);
    ready(&config);
    let project = std::env::var_os("LOUISELM_TEST_LUA_ROOT").expect("explicit Lua checkout");
    let neovim = std::env::var_os("LOUISELM_TEST_NVIM").expect("explicit stable Neovim executable");
    let groups = if index == 5 {
        vec!["--groups".to_owned(), config.broker_uid.to_string()]
    } else {
        vec!["--clear-groups".to_owned()]
    };
    let status = Command::new("/usr/bin/setpriv")
        .args([
            "--reuid",
            &config.operator_uid.to_string(),
            "--regid",
            &config.operator_uid.to_string(),
        ])
        .args(groups)
        .arg(&neovim)
        .args([
            "--headless",
            "--noplugin",
            "-u",
            "NONE",
            "-l",
            if index == 5 {
                "tests/workflow/installed_operator.lua"
            } else {
                "tests/workflow/installed_beads.lua"
            },
        ])
        .current_dir(&project)
        .env_clear()
        .env("PATH", "/usr/local/lib/louiselm/current/bin:/usr/bin:/bin")
        .env("XDG_STATE_HOME", &state)
        .env("XDG_CONFIG_HOME", state.join("config"))
        .env("XDG_DATA_HOME", state.join("data"))
        .env("LOUISELM_BEADS_FIXTURE", &input)
        .env("LOUISELM_BEADS_CASE", index.to_string())
        .status()
        .unwrap();
    assert!(
        status.success(),
        "installed Lua Bead executor case {index} failed"
    );
    if index == 4 {
        let staged = config
            .broker_socket_path
            .parent()
            .unwrap()
            .join("workspace-inputs")
            .join(promotion_manifest.digest().hex());
        let metadata = fs::metadata(&staged).unwrap();
        assert_eq!(metadata.uid(), config.broker_uid);
        assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
        let staged_preview =
            crate::workspace::launch_inputs::inspect(&staged, &promotion_manifest.digest())
                .unwrap();
        assert_eq!(
            staged_preview.source.base_commit,
            promotion_preview.base_commit
        );
        let git = |arguments: &[&str]| {
            let output = Command::new("/usr/bin/setpriv")
                .args([
                    "--reuid",
                    &config.operator_uid.to_string(),
                    "--regid",
                    &config.operator_uid.to_string(),
                    "--clear-groups",
                ])
                .arg("/usr/bin/git")
                .args(arguments)
                .current_dir(&checkout)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "operator Git readback failed: {output:?}"
            );
            String::from_utf8(output.stdout).unwrap().trim().to_owned()
        };
        let head = git(&["rev-parse", "HEAD"]);
        assert_ne!(head, promotion_preview.base_commit);
        assert_eq!(git(&["rev-parse", "HEAD^"]), promotion_preview.base_commit);
        assert_eq!(git(&["status", "--porcelain"]), "");
        assert_eq!(
            fs::read(checkout.join("accepted.txt")).unwrap(),
            b"accepted Bead output\n"
        );
        assert!(git(&["show", "-s", "--format=%B", &head]).contains(&format!("Refs {bead_id}")));
    }
    if (2..=3).contains(&index) && std::env::var_os("LOUISELM_TEST_BR").is_some() {
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
    if index == 5 {
        let run_id = selected["envelope"]["run_id"].as_str().unwrap();
        for bead in 1..=3 {
            wait_terminal(&format!("{run_id}-worker-{bead}"));
            wait_terminal(&format!("{run_id}-verifier-{bead}"));
        }
    } else {
        wait_terminal(&format!("lua-worker-{index}"));
        if index <= 2 || index == 4 {
            wait_terminal(&format!("lua-verifier-{index}"));
        }
    }
    eprintln!("installed Lua workers and verifiers: durable terminal cleanup receipts");
    terminate(&mut daemon);
}

fn operator_selection(
    config: &LauncherConfig,
    operator: &Path,
    checkout: &Path,
    journal: &Path,
    preview: &crate::workspace::SnapshotPreview,
    manifest: &crate::session_manifest::SessionInputManifest,
    case: &serde_json::Value,
) -> serde_json::Value {
    let capture = std::env::var_os("LOUISELM_TEST_CAPTURE").expect("explicit real capture CLI");
    let destination = Path::new("/usr/local/lib/louiselm/current/bin/louiselm-capture");
    fs::copy(capture, destination).unwrap();
    fs::set_permissions(destination, fs::Permissions::from_mode(0o755)).unwrap();
    // Failed fixtures may retain poisoned kernel cgroups beyond their private
    // mount namespace. A new Run must never reuse those Session identities.
    let run_id = fs::read_to_string("/proc/sys/kernel/random/uuid")
        .unwrap()
        .trim()
        .to_owned();
    assert!(
        Command::new("/usr/bin/setpriv")
            .args([
                "--reuid",
                &config.operator_uid.to_string(),
                "--regid",
                &config.operator_uid.to_string(),
                "--clear-groups",
                "/usr/bin/git",
                "branch",
                "-m",
                &format!("run/{run_id}"),
            ])
            .current_dir(checkout)
            .status()
            .unwrap()
            .success()
    );
    let snapshots = operator.join("shared-inputs");
    let cache = operator.join("shared-cache");
    for directory in [&snapshots, &cache] {
        fs::create_dir(directory).unwrap();
        chown(
            directory,
            Some(config.operator_uid),
            Some(config.broker_uid),
        )
        .unwrap();
        fs::set_permissions(directory, fs::Permissions::from_mode(0o750)).unwrap();
    }
    let mut envelope = case["envelope"].clone();
    envelope["run_id"] = serde_json::json!(run_id);
    envelope["max_sessions"] = serde_json::json!(6);
    envelope["bead_scope"]["issue_ids"] =
        serde_json::json!(["operator-1", "operator-2", "operator-3"]);
    envelope["bead_scope"]["max_mutations"] = serde_json::json!(3);
    envelope["provider_requests"]["models"] = serde_json::json!(["gpt-5.6-luna"]);
    envelope["provider_requests"]["max_effort"] = serde_json::json!("high");
    envelope["provider_requests"]["max_run_requests"] = serde_json::json!(6);
    serde_json::json!({
        "schema":"louiselm.operator.bead-run/1", "envelope":envelope,
        "beads":[{"id":"operator-1","prompt":"promote-fixture-1"},
            {"id":"operator-2","prompt":"promote-fixture-2"},
            {"id":"operator-3","prompt":"promote-fixture-3"}],
        "manifest":manifest, "cache":cache, "plan":case["plan"],
        "snapshot_parent":snapshots, "input_group":config.broker_uid,
        "worktree":{"path":checkout,"journal_parent":journal,"head":preview.base_commit},
    })
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
                    && fs::read(entry.path()).is_ok_and(|bytes| {
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
