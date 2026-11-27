//! Installed broker/launcher and real Lua Sessions; fixture or pinned stock ACP.
use super::*;
use std::cell::Cell;

#[path = "installed_saved_attention_tests.rs"]
mod saved_attention;

#[path = "installed_stock_operator_tests.rs"]
mod stock;

fn stale_staging_is_absent(path: &Path) -> std::io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(error),
    }
}

fn report_stale_staging(ready: &Path, result: &Path, staged: &Path) {
    let deadline = Instant::now() + Duration::from_secs(90);
    while !ready.exists() {
        assert!(
            Instant::now() < deadline,
            "stale staging check was not requested"
        );
        thread::sleep(Duration::from_millis(10));
    }
    let verdict = match stale_staging_is_absent(staged) {
        Ok(true) => "absent".to_owned(),
        Ok(false) => "published".to_owned(),
        Err(error) => format!("inspection_error:{error}"),
    };
    let temporary = result.with_extension("tmp");
    fs::write(&temporary, verdict).unwrap();
    fs::set_permissions(&temporary, fs::Permissions::from_mode(0o644)).unwrap();
    fs::rename(temporary, result).unwrap();
}

#[test]
fn stale_staging_check_distinguishes_absence_publication_and_errors() {
    let root = tempfile::tempdir().unwrap();
    let staged = root.path().join("staged");
    assert!(stale_staging_is_absent(&staged).unwrap());
    fs::create_dir(&staged).unwrap();
    assert!(!stale_staging_is_absent(&staged).unwrap());
    fs::remove_dir(&staged).unwrap();
    std::os::unix::fs::symlink("missing-target", &staged).unwrap();
    assert!(!stale_staging_is_absent(&staged).unwrap());
    let file = root.path().join("file");
    fs::write(&file, b"not a directory").unwrap();
    assert!(stale_staging_is_absent(&file.join("child")).is_err());
}

#[test]
fn stale_staging_observer_waits_for_the_request_and_reports_publication() {
    let root = tempfile::tempdir().unwrap();
    let ready = root.path().join("ready");
    let result = root.path().join("result");
    let staged = root.path().join("staged");
    thread::scope(|scope| {
        let (finished, receiver) = mpsc::channel();
        let (observer_ready, observer_result, observer_staged) = (&ready, &result, &staged);
        let observer = scope.spawn(move || {
            report_stale_staging(observer_ready, observer_result, observer_staged);
            finished.send(()).unwrap();
        });
        let premature = receiver.recv_timeout(Duration::from_millis(100));
        fs::create_dir(&staged).unwrap();
        fs::write(&ready, b"").unwrap();
        observer.join().unwrap();
        assert!(matches!(premature, Err(mpsc::RecvTimeoutError::Timeout)));
        assert_eq!(fs::read_to_string(&result).unwrap(), "published");
    });
}

#[derive(Default)]
struct FailureObservation {
    daemon_pid: Cell<Option<i32>>,
    tracker_pid: Cell<Option<i32>>,
}

fn own(path: &Path, uid: u32) {
    if path.is_dir() {
        for entry in fs::read_dir(path).unwrap() {
            own(&entry.unwrap().path(), uid);
        }
    }
    chown(path, Some(uid), Some(uid)).unwrap();
}

fn inject_saved_review_observation(
    fixture: &Path,
    owner: (u32, u32),
    completed: bool,
) -> thread::JoinHandle<()> {
    let fixture = fixture.to_owned();
    thread::spawn(move || {
        let input = fixture.join("uncertain-request.json");
        // Case 15 deliberately waits for its 60-second Run deadline first.
        let deadline = Instant::now() + Duration::from_secs(180);
        while !input.exists() {
            assert!(
                Instant::now() < deadline,
                "saved review never requested the observation fixture"
            );
            thread::sleep(Duration::from_millis(10));
        }
        // This is the same durable request/grant observation used by the
        // broker interruption tests, attached to the real saved publication.
        let request: crate::broker::promotion::PromotionRequest =
            serde_json::from_slice(&fs::read(&input).unwrap()).unwrap();
        assert!(
            !request.request_id.is_empty()
                && request.request_id.len() <= 128
                && request
                    .request_id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        );
        let parent = Path::new(STATE).join("authorizations/promotions");
        fs::create_dir_all(&parent).unwrap();
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).unwrap();
        chown(&parent, Some(owner.0), Some(owner.1)).unwrap();
        let journal = parent.join(&request.request_id);
        fs::create_dir(&journal).unwrap();
        fs::set_permissions(&journal, fs::Permissions::from_mode(0o700)).unwrap();
        write_json(&journal.join("request.json"), &request);
        write_json(&journal.join("0.grant"), &0usize);
        if completed {
            write_json(&journal.join("0.done"), &0usize);
            write_json(
                &journal.join("result.json"),
                &crate::workspace::promotion::ApplicationResult {
                    complete: true,
                    completed_steps: 1,
                    output_provenance: crate::workspace::provenance::OutputProvenance::untainted(),
                },
            );
        }
        chown(&journal, Some(owner.0), Some(owner.1)).unwrap();
        for entry in fs::read_dir(&journal).unwrap() {
            let path = entry.unwrap().path();
            chown(&path, Some(owner.0), Some(owner.1)).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            fs::File::open(path).unwrap().sync_all().unwrap();
        }
        fs::File::open(&journal).unwrap().sync_all().unwrap();
        fs::File::open(&parent).unwrap().sync_all().unwrap();
        fs::write(fixture.join("uncertain-ready"), b"").unwrap();
    })
}

struct FixtureBrokerGroup(crate::sandbox::Cgroup);

impl FixtureBrokerGroup {
    fn new() -> Self {
        Self(
            crate::sandbox::Cgroup::create(
                Path::new("/sys/fs/cgroup"),
                &format!("bead-fixture-{}", std::process::id()),
            )
            .unwrap(),
        )
    }

    fn cleanup(&self) -> Result<(), String> {
        self.0.kill_all().map_err(|error| error.to_string())?;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if self
                .0
                .processes()
                .map_err(|error| error.to_string())?
                .is_empty()
            {
                break;
            }
            if Instant::now() >= deadline {
                return Err("fixture broker cgroup did not become empty".into());
            }
            thread::sleep(Duration::from_millis(10));
        }
        fs::remove_dir(self.0.path()).map_err(|error| error.to_string())
    }
}

impl Drop for FixtureBrokerGroup {
    fn drop(&mut self) {
        if let Err(error) = self.cleanup() {
            if std::thread::panicking() {
                eprintln!("fixture broker cleanup failed: {error}");
            } else {
                panic!("fixture broker cleanup failed: {error}");
            }
        }
    }
}

#[test]
fn privileged_installed_lua_bead_executor_failure_cleanup() {
    if std::env::var_os("LOUISELM_REQUIRE_BEAD_EXECUTOR").is_none() {
        eprintln!("skipping: requires disposable VM, Neovim and private mounts");
        return;
    }
    assert_eq!(std::env::var("LOUISELM_BEAD_EXECUTOR_CASE").unwrap(), "1");
    assert!(std::env::var_os("LOUISELM_TEST_BR").is_none());
    let previous_maps = super::super::guard_cleanup::policy_maps();
    let observation = FailureObservation::default();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        installed_lua_bead_executor(Some(&observation));
    }));
    let failure = result.unwrap_err();
    let message = failure
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| failure.downcast_ref::<&str>().copied());
    assert_eq!(message, Some("installed Lua Bead executor case 1 failed"));
    let daemon = rustix::process::Pid::from_raw(observation.daemon_pid.get().unwrap()).unwrap();
    assert!(matches!(
        rustix::process::waitpid(Some(daemon), rustix::process::WaitOptions::NOHANG),
        Err(rustix::io::Errno::CHILD)
    ));
    let tracker = observation.tracker_pid.get().unwrap();
    assert!(
        !Path::new(&format!("/proc/{tracker}")).exists(),
        "fixture tracker process {tracker} survived teardown"
    );
    for database in ["passwd", "group"] {
        let entry = Command::new("/usr/bin/getent")
            .args([database, ACCOUNT])
            .output()
            .unwrap();
        assert_eq!(
            entry.status.code(),
            Some(2),
            "fixture {database} entry survived teardown"
        );
    }
    assert!(
        !Path::new("/sys/fs/cgroup")
            .join(format!(
                "louiselm-session-bead-fixture-{}",
                std::process::id()
            ))
            .exists(),
        "fixture broker cgroup survived teardown"
    );
    super::super::launcher_cleanup::assert_exited(&previous_maps);
    // A secondary account-removal error must also preserve an earlier panic.
    eprintln!("forcing an expected cleanup error for the already removed fixture account");
    let secondary = std::panic::catch_unwind(|| {
        let _already_removed = BrokerAccount;
        panic!("original fixture failure");
    })
    .unwrap_err();
    assert_eq!(
        secondary.downcast_ref::<&str>().copied(),
        Some("original fixture failure")
    );
    eprintln!("fixture failure preserved; broker reaped, tracker exited and account removed");
}

#[test]
fn privileged_installed_lua_bead_executor() {
    installed_lua_bead_executor(None);
}

#[expect(
    clippy::too_many_lines,
    reason = "One installed transaction provisions exact child grants and runs the real Lua controller under the operator identity."
)]
fn installed_lua_bead_executor(failure: Option<&FailureObservation>) {
    if std::env::var_os("LOUISELM_REQUIRE_BEAD_EXECUTOR").is_none() {
        eprintln!("skipping: requires disposable VM, Neovim and private mounts");
        return;
    }
    assert!(rustix::process::geteuid().is_root());
    let previous_maps = super::super::guard_cleanup::policy_maps();
    let index: usize = std::env::var("LOUISELM_BEAD_EXECUTOR_CASE")
        .expect("explicit installed case")
        .parse()
        .unwrap();
    assert!((1..=15).contains(&index));
    let saved_refresh = index == 14;
    let saved_review = index == 15;
    let send_coexistence = std::env::var_os("LOUISELM_TEST_SEND_COEXIST").is_some();
    assert!(!send_coexistence || saved_review);
    let saved_review_decision =
        std::env::var("LOUISELM_TEST_REVIEW_DECISION").unwrap_or_else(|_| "pending".into());
    assert!(
        [
            "pending",
            "accepted",
            "discarded",
            "application_unknown",
            "application_completed"
        ]
        .contains(&saved_review_decision.as_str())
    );
    let rehearsal_fault = match index {
        12 => "replica",
        13 => "pre_acp",
        _ => "",
    };
    let stock_operator = matches!(index, 10..=13);
    let startup_only = matches!(index, 11..=13);
    let startup_failure = index == 9;
    // Case 8 reuses the passing promotion fixture, with an actual human-review
    // delay crossing the original Run expiry before Accept reaches the real CLI.
    let delayed_promotion = index == 8;
    let index = if stock_operator {
        5
    } else if startup_failure {
        1
    } else if delayed_promotion || saved_refresh || saved_review {
        4
    } else {
        index
    };
    let root = tempfile::Builder::new()
        .prefix("louiselm-lua-beads-")
        .tempdir_in("/var/lib")
        .unwrap();
    mounts(root.path());
    let _account = BrokerAccount::create();
    let (paths, config, manager) = install_unseeded_daemon(
        root.path(),
        if stock_operator {
            crate::conformance::admission::Enforcement::Enforced
        } else {
            crate::conformance::admission::Enforcement::PreCutover
        },
        6,
    );
    if index == 1 && std::env::var_os("LOUISELM_TEST_BR").is_none() {
        // louiselm-3hdk7: the real 1,593-issue tracker took 5.683s to export
        // and rebuild before the broker could answer the launch authorization.
        // Replay that ordering beyond the old 5s launcher wait, within the
        // replica import's existing 10s bound. No Provider or real Agent runs.
        fs::write(root.path().join("slow-replica-import"), b"").unwrap();
    }
    let import_running = root.path().join("slow-replica-import-running");
    if failure.is_some() {
        fs::write(&import_running, b"").unwrap();
        chown(&import_running, Some(BROKER_UID), Some(BROKER_UID)).unwrap();
    }
    let (program, tracker, bead_id) = beads::provision(root.path());
    if !rehearsal_fault.is_empty() {
        stock::inject_rehearsal_failure(root.path(), &config, &program, rehearsal_fault);
    }
    let upstream = if stock_operator {
        stock::install(root.path(), &paths, &config, startup_only)
    } else {
        None
    };
    let manifest = if stock_operator {
        workspace::fixture_manifest_for(
            super::super::codex_chain::runtime_directory(root.path()).as_deref(),
            crate::registry::NetworkPolicy::Brokered,
        )
    } else {
        workspace::fixture_manifest()
    };
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o755)).unwrap();
    let operator = root.path().join("operator");
    fs::create_dir(&operator).unwrap();
    chown(
        &operator,
        Some(config.operator_uid),
        Some(config.operator_uid),
    )
    .unwrap();
    if saved_review {
        // The broker reads the explicitly shared snapshot; private phone state
        // and the destination remain protected by their own directories.
        fs::set_permissions(&operator, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let checkout = operator.join(if saved_review { "checkout" } else { "run" });
    let source_repository = if saved_review {
        operator.join("project")
    } else {
        checkout.clone()
    };
    fs::create_dir(&source_repository).unwrap();
    let source_files =
        workspace::copy_snapshot_files(&workspace::snapshot_bytes(), &source_repository);
    for arguments in [
        vec![
            "init",
            "--quiet",
            "--initial-branch=run/44444444-4444-4444-8444-444444444444",
        ],
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
        if arguments.contains(&"commit") && !source_files.is_empty() {
            assert!(
                Command::new("/usr/bin/git")
                    .args(["add", "--force", "--"])
                    .args(&source_files)
                    .current_dir(&source_repository)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        assert!(
            Command::new("/usr/bin/git")
                .args(arguments)
                .current_dir(&source_repository)
                .status()
                .unwrap()
                .success()
        );
    }
    if saved_review {
        own(&source_repository, config.operator_uid);
        let capture_test = std::env::var_os("LOUISELM_TEST_CAPTURE_TEST")
            .expect("explicit candidate capture library test");
        let output = Command::new("/usr/bin/setpriv")
            .args([
                "--reuid",
                &config.operator_uid.to_string(),
                "--regid",
                &config.operator_uid.to_string(),
                "--clear-groups",
            ])
            .arg(capture_test)
            .args([
                "phone_control::http::installed_saved_review::installed_saved_review",
                "--exact",
                "--ignored",
                "--nocapture",
            ])
            .env_clear()
            .env("LOUISELM_TEST_REVIEW_ROOT", &operator)
            .env("LOUISELM_TEST_REVIEW_PHASE", "prepare")
            .env(
                "LOUISELM_TEST_SEND_COEXIST",
                if send_coexistence { "1" } else { "" },
            )
            .output()
            .unwrap();
        assert!(
            output.status.success()
                && String::from_utf8_lossy(&output.stdout).contains("1 passed")
                && String::from_utf8_lossy(&output.stdout)
                    .contains("SAVED_REVIEW_SOURCE_AND_RUN_BOUND"),
            "capture source preparation failed: {output:?}"
        );
    }
    let promotion_snapshot = operator.join("snapshot-4");
    let promotion_preview = if saved_review {
        prepare_review_snapshot(&paths, &checkout, &promotion_snapshot, config.operator_uid)
    } else {
        crate::workspace::prepare(&checkout, &[], &promotion_snapshot).unwrap()
    };
    assert_eq!(
        promotion_preview.base_digest, manifest.source_base_digest,
        "operator checkout must contain the selected fixture source, not an empty repository"
    );
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
    if saved_review {
        let readable_by_broker = |path: &Path| {
            Command::new("/usr/bin/setpriv")
                .args([
                    "--reuid",
                    &config.broker_uid.to_string(),
                    "--regid",
                    &config.broker_gid.to_string(),
                    "--clear-groups",
                    "/usr/bin/test",
                    "-r",
                ])
                .arg(path)
                .status()
                .unwrap()
                .success()
        };
        assert!(readable_by_broker(
            &promotion_snapshot.join("snapshot.json")
        ));
        let private_source = operator.join(if send_coexistence {
            "louiselm/capture/phone-control/run-sources/44444444-4444-4444-8444-444444444444.json"
        } else {
            "phone/run-sources/44444444-4444-4444-8444-444444444444.json"
        });
        assert!(private_source.is_file());
        assert!(!readable_by_broker(&private_source));
    }
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
    for index in 1..=7 {
        if index == 5 {
            cases.push(serde_json::Value::Null);
            continue;
        }
        let mut launch = named_request(&format!("lua-worker-{index}"));
        launch.run_id = if index == 4 {
            "44444444-4444-4444-8444-444444444444".into()
        } else {
            format!("lua-run-{index}")
        };
        launch.session_input_manifest_id = if index == 4 {
            &promotion_manifest
        } else {
            &manifest
        }
        .digest()
        .to_string();
        let expiry = now
            + if index == 4 && (delayed_promotion || saved_refresh || saved_review) {
                60_000
            } else {
                120_000
            };
        let grant = GrantRequest {
            role: crate::launch_protocol::LaunchRole::Agent,
            request: launch,
            controller_uid: config.operator_uid,
            expires_at_ms: expiry,
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
                expires_at_ms: expiry,
            }),
        };
        let mut verifier_launch = named_request(&format!("lua-verifier-{index}"));
        verifier_launch.run_id = grant.request.run_id.clone();
        verifier_launch.session_input_manifest_id = grant.request.session_input_manifest_id.clone();
        let verifier_grant = GrantRequest {
            role: crate::launch_protocol::LaunchRole::FixedVerifier,
            request: verifier_launch,
            controller_uid: config.operator_uid,
            expires_at_ms: expiry,
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
        if index == 4 {
            own(&plan, config.operator_uid);
        }
        let plan_digest = Digest::of(&fs::read(&plan).unwrap()).to_string();
        let mut envelope = fixture_run_envelope(&grant, plan_digest.clone(), now);
        envelope.expires_at_ms = expiry;
        envelope.bead_scope.clone_from(&grant.beads_mutations);
        envelope.bead_scope.as_mut().unwrap().role = crate::beads_mutation::BeadsRole::Coordinator;
        envelope.max_sessions = 2;
        if index >= 6 {
            envelope.max_sessions = 4;
            let scope = envelope.bead_scope.as_mut().unwrap();
            scope.issue_ids.push("held-successor".into());
            scope.max_mutations = 2;
        }
        if index == 4 && delayed_promotion {
            envelope.max_sessions = 4;
            let scope = envelope.bead_scope.as_mut().unwrap();
            scope.issue_ids.push("promotion-successor".into());
            scope.max_mutations = 2;
        }
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
            "plan":plan,"plan_digest":plan_digest,
            "saved_ready":operator.join("saved-ready"),"saved_cleaned":operator.join("saved-cleaned"),
            "hold_ready":operator.join("hold-ready"),"hold_published":operator.join("hold-published"),
            "stale_check_ready":operator.join("stale-check-ready"),
            "stale_check_result":operator.join("stale-check-result")}),
        );
    }
    let input = root.path().join("controller.json");
    let mut selected = if index == 5 {
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
    if let Some(upstream) = &upstream {
        selected["envelope"]["provider_requests"]["upstream"] = serde_json::json!(format!(
            "https://api.openai.com:{}/v1/responses",
            upstream.port
        ));
        selected["envelope"]["provider_requests"]["addresses"] = serde_json::json!(["127.0.0.1"]);
        // Six expected HTTPS exchanges, with one unused unit so the Run does
        // not deliberately enter its exhaustion hold before the final verifier.
        selected["envelope"]["provider_requests"]["max_run_requests"] = serde_json::json!(7);
    }
    if stock_operator {
        for (index, bead) in selected["beads"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .enumerate()
        {
            bead["prompt"] = serde_json::json!(format!(
                "Create accepted-{}.txt for this offline fixture.",
                index + 1
            ));
        }
    }
    write_json(&input, &selected);
    if startup_failure {
        // Exact private fixture input, never system or retained live storage.
        // The broker may consume the grant, but privileged preparation must
        // refuse missing staged source before any Agent can start.
        fs::remove_dir_all(
            config
                .broker_socket_path
                .parent()
                .unwrap()
                .join("workspace-inputs")
                .join(manifest.digest().hex()),
        )
        .unwrap();
    }
    let state = operator;
    // The broker's bounded tracker runner creates its own process group. Keep
    // every fixture broker descendant in one owned cgroup, even on unwind.
    let broker_group = FixtureBrokerGroup::new();
    let mut daemon = process_with_cgroup(
        &manager,
        BROKER_UID,
        false,
        &BROKER_UID.to_string(),
        Some(broker_group.0.path()),
        None,
    );
    ready(&config);
    if upstream.is_some() {
        // The first production startup establishes pristine state identity.
        // Custody is loaded once at startup, so install the dummy credential
        // between daemons, not after the serving daemon has loaded its store.
        terminate(&mut daemon);
        stock::provision_credential();
        daemon = process_with_cgroup(
            &manager,
            BROKER_UID,
            false,
            &BROKER_UID.to_string(),
            Some(broker_group.0.path()),
            Some((stock::BROKER_ENTRY, root.path())),
        );
        ready(&config);
    }
    if let Some(observation) = failure {
        observation
            .daemon_pid
            .set(Some(i32::try_from(daemon.0.id()).unwrap()));
    }
    assert!(broker_group.0.processes().unwrap().contains(&daemon.0.id()));
    let hold_injector = (index >= 6).then(|| inject_provider_hold(&state, index, now));
    let staged_inputs = config
        .broker_socket_path
        .parent()
        .unwrap()
        .join("workspace-inputs")
        .join(promotion_manifest.digest().hex());
    if saved_review {
        assert!(
            stale_staging_is_absent(&staged_inputs).unwrap(),
            "saved-review fixture inputs must not already be published"
        );
    }
    let saved_cleanup = (index == 4).then(|| {
        expire_saved_result_sessions(
            &state,
            (config.broker_uid, config.broker_gid),
            now,
            saved_review,
            send_coexistence,
            &staged_inputs,
        )
    });
    let saved_observation = (saved_review && saved_review_decision.starts_with("application_"))
        .then(|| {
            inject_saved_review_observation(
                &state,
                (config.broker_uid, config.broker_gid),
                saved_review_decision == "application_completed",
            )
        });
    let project = std::env::var_os("LOUISELM_TEST_LUA_ROOT").expect("explicit Lua checkout");
    let neovim = std::env::var_os("LOUISELM_TEST_NVIM").expect("explicit stable Neovim executable");
    let groups = if index == 5 {
        vec!["--groups".to_owned(), config.broker_uid.to_string()]
    } else {
        vec!["--clear-groups".to_owned()]
    };
    let mut command = Command::new("/usr/bin/setpriv");
    command
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
        .env("LOUISELM_TEST_REHEARSAL_FAILURE", rehearsal_fault)
        .env(
            "LOUISELM_TEST_STOCK_OPERATOR",
            if stock_operator { "1" } else { "0" },
        )
        .env(
            "LOUISELM_TEST_STOCK_STARTUP_ONLY",
            if startup_only { "1" } else { "0" },
        )
        .env(
            "PATH",
            "/usr/local/lib/louiselm/current/bin:/usr/local/lib/louiselm-tools:/usr/bin:/bin",
        )
        .env("XDG_STATE_HOME", &state)
        .env("XDG_CONFIG_HOME", state.join("config"))
        .env("XDG_DATA_HOME", state.join("data"))
        .env("LOUISELM_BEADS_FIXTURE", &input)
        .env("LOUISELM_BEADS_CASE", index.to_string())
        .env(
            "LOUISELM_TEST_SOURCE_SIZED",
            if std::env::var_os("LOUISELM_TEST_WORKSPACE_SNAPSHOT").is_some() {
                "1"
            } else {
                "0"
            },
        )
        .env(
            "LOUISELM_TEST_STARTUP_FAILURE",
            if startup_failure { "1" } else { "0" },
        )
        .env(
            "LOUISELM_TEST_PROMOTION_DELAY",
            if delayed_promotion { "1" } else { "0" },
        )
        .env(
            "LOUISELM_TEST_SAVED_REFRESH",
            if saved_refresh { "1" } else { "0" },
        )
        .env(
            "LOUISELM_TEST_SAVED_REVIEW",
            if saved_review { "1" } else { "0" },
        );
    if saved_review {
        command.env(
            "LOUISELM_TEST_CAPTURE_TEST",
            std::env::var_os("LOUISELM_TEST_CAPTURE_TEST").unwrap(),
        );
        command.env("LOUISELM_TEST_REVIEW_DECISION", &saved_review_decision);
        if send_coexistence {
            assert_eq!(saved_review_decision, "pending");
            command.env("LOUISELM_TEST_SEND_COEXIST", "1");
            command.env("LOUISELM_TEST_NVIM", &neovim);
            command.env(
                "LOUISELM_TEST_CAPTURE",
                std::env::var_os("LOUISELM_TEST_CAPTURE").unwrap(),
            );
            command.env("LOUISELM_TEST_LUA_ROOT", &project);
        }
    }
    let status = if let Some(observation) = failure {
        let mut neovim = BrokerChild(command.spawn().unwrap());
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let marker = fs::read_to_string(&import_running).unwrap();
            if !marker.is_empty() {
                let pid = marker.parse::<i32>().unwrap();
                assert!(Path::new(&format!("/proc/{pid}")).exists());
                assert!(
                    broker_group
                        .0
                        .processes()
                        .unwrap()
                        .contains(&u32::try_from(pid).unwrap()),
                    "tracker import escaped the fixture broker cgroup"
                );
                observation.tracker_pid.set(Some(pid));
                eprintln!("forcing fixture failure during tracker import {pid}");
                break;
            }
            assert!(
                neovim.0.try_wait().unwrap().is_none(),
                "Neovim exited before tracker import"
            );
            assert!(Instant::now() < deadline, "tracker import did not start");
            thread::sleep(Duration::from_millis(10));
        }
        neovim.0.kill().unwrap();
        neovim.0.wait().unwrap()
    } else {
        if stock_operator {
            stock::verify_rehearsal_refusal(config.operator_uid);
        }
        command.status().unwrap()
    };
    let pending_before = saved_cleanup.and_then(|cleanup| cleanup.join().unwrap());
    if let Some(observation) = saved_observation {
        observation.join().unwrap();
    }
    if let Some(injector) = hold_injector {
        injector.join().unwrap();
        let store = crate::broker::AuthorizationStore::open(
            &Path::new(STATE).join("authorizations"),
            config.pool.clone(),
        )
        .unwrap();
        assert_eq!(
            store
                .session_count_for_run(&format!("lua-run-{index}"))
                .unwrap(),
            1
        );
    }
    assert!(
        status.success(),
        "installed Lua Bead executor case {index} failed"
    );
    if index == 4 && !saved_review {
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
        assert_eq!(git(&["status", "--porcelain"]), "");
        assert_ne!(head, promotion_preview.base_commit);
        assert_eq!(
            git(&["rev-parse", if saved_refresh { "HEAD~3" } else { "HEAD^" }]),
            promotion_preview.base_commit
        );
        assert_eq!(
            fs::read(checkout.join("accepted.txt")).unwrap(),
            b"accepted Bead output\n"
        );
        assert!(git(&["show", "-s", "--format=%B", &head]).contains(&format!("Refs {bead_id}")));
        assert_eq!(
            fs::read_dir(&journal).unwrap().count(),
            1,
            "double Accept must create only one application journal"
        );
        saved_attention::assert_saved_decision("44444444-4444-4444-8444-444444444444", true);
    }
    if saved_review {
        if saved_review_decision == "pending" || saved_review_decision.starts_with("application_") {
            let pending_after =
                saved_attention::assert_saved_pending("44444444-4444-4444-8444-444444444444");
            assert_eq!(Some(pending_after), pending_before);
            assert_eq!(fs::read_dir(&journal).unwrap().count(), 0);
        } else {
            let accepted = saved_review_decision == "accepted";
            saved_attention::assert_saved_decision(
                "44444444-4444-4444-8444-444444444444",
                accepted,
            );
            assert_eq!(
                fs::read_dir(&journal).unwrap().count(),
                usize::from(accepted)
            );
        }
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
    if startup_failure {
        let store = crate::broker::AuthorizationStore::open(
            &Path::new(STATE).join("authorizations"),
            config.pool.clone(),
        )
        .unwrap();
        assert_eq!(store.session_count_for_run("lua-run-1").unwrap(), 1);
        assert!(
            store
                .consumed_for_session("lua-worker-1")
                .unwrap()
                .is_some()
        );
        let held = store.consumed_for_session("lua-worker-1").unwrap().unwrap();
        let scratch = root.path().join("failure-verifier");
        fs::create_dir(&scratch).unwrap();
        fs::set_permissions(&scratch, fs::Permissions::from_mode(0o700)).unwrap();
        let verifier =
            Arc::new(crate::launcher_install::LauncherVerifier::open(&paths, &scratch).unwrap());
        let receipts = crate::broker::ReceiptStore::installed(
            &Path::new(STATE).join("receipts"),
            Arc::clone(&verifier),
        )
        .unwrap();
        let bytes = receipts.stored_bytes("lua-worker-1").unwrap();
        assert_eq!(bytes.len(), 1);
        let receipt = crate::launch_receipt::SignedReceipt::parse_canonical(&bytes[0]).unwrap();
        crate::launch_receipt::verify_chain(
            std::slice::from_ref(&receipt),
            &verifier.receipt_anchor("lua-worker-1").unwrap(),
            |key, payload, signature| verifier.verify(key, payload, signature).is_ok(),
        )
        .unwrap();
        assert!(matches!(&receipt.payload.outcome,
            crate::launch_receipt::ReceiptOutcome::FailedLaunch {
                failure, agent_execution: crate::launch_receipt::AgentExecution::NotStarted, ..
            } if failure.phase == "workspace_prepare" && failure.code == "unavailable"));
        let lease = crate::launcher_install::acquire_identity(&paths, held.identity.slot).unwrap();
        lease.release().unwrap();
        eprintln!(
            "startup cause preserved; one consumed authorization; authenticated terminal failed launch; no successor"
        );
    } else if !rehearsal_fault.is_empty() {
        let report: serde_json::Value =
            serde_json::from_slice(&fs::read(state.join("rehearsal.json")).unwrap()).unwrap();
        assert_ne!(report["status"], "passed");
        let store = crate::broker::AuthorizationStore::open(
            &Path::new(STATE).join("authorizations"),
            config.pool.clone(),
        )
        .unwrap();
        assert_eq!(
            store
                .session_count_for_run(selected["envelope"]["run_id"].as_str().unwrap())
                .unwrap(),
            0
        );
        eprintln!("REHEARSAL_CHARACTERIZATION fault={rehearsal_fault} no_paid_authority=true");
    } else if startup_only {
        let run_id = selected["envelope"]["run_id"].as_str().unwrap();
        wait_terminal(&format!("{run_id}-worker-1"));
        let store = crate::broker::AuthorizationStore::open(
            &Path::new(STATE).join("authorizations"),
            config.pool.clone(),
        )
        .unwrap();
        assert_eq!(store.session_count_for_run(run_id).unwrap(), 1);
        assert_eq!(fs::read_dir(&journal).unwrap().count(), 0);
        eprintln!(
            "STOCK_OPERATOR production-daemon startup, real capture attachment and terminal cleanup proven; no prompt"
        );
    } else if index == 5 {
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
    if stock_operator && rehearsal_fault.is_empty() {
        stock::verify_run(
            root.path(),
            &paths,
            &config,
            &state,
            &selected,
            startup_only,
        );
    }
    super::super::launcher_cleanup::assert_exited(&previous_maps);
    if let Some(upstream) = upstream {
        upstream.finish();
    }
    eprintln!("installed Lua launcher exit and guard cleanup proven");
    terminate(&mut daemon);
}

fn prepare_review_snapshot(
    paths: &LauncherPaths,
    checkout: &Path,
    output: &Path,
    operator_uid: u32,
) -> crate::workspace::SnapshotPreview {
    // Characterize the actual fixture checkout without relaxing Git ownership.
    let diagnostic = Command::new("/usr/bin/git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(checkout)
        .env_clear()
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    if !diagnostic.status.success() {
        eprintln!(
            "SAVED_REVIEW_ROOT_GIT_REFUSAL: {}",
            String::from_utf8_lossy(&diagnostic.stderr).trim()
        );
    }
    let result = Command::new("/usr/bin/setpriv")
        .args([
            "--reuid",
            &operator_uid.to_string(),
            "--regid",
            &operator_uid.to_string(),
            "--clear-groups",
        ])
        .arg(paths.release_prefix.join("current/bin/louiselm-skills"))
        .args(["workspace", "prepare", "--repository"])
        .arg(checkout)
        .arg("--output")
        .arg(output)
        .arg("--robot-json")
        .env_clear()
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "operator snapshot failed: {result:?}"
    );
    let preview: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(preview["schema"], "louiselm.workspace.preview/1");
    crate::workspace::SnapshotPreview {
        schema: "louiselm.workspace.preview/1",
        snapshot_digest: preview["snapshot_digest"].as_str().unwrap().into(),
        base_digest: preview["base_digest"].as_str().unwrap().into(),
        base_commit: preview["base_commit"].as_str().unwrap().into(),
        file_count: usize::try_from(preview["file_count"].as_u64().unwrap()).unwrap(),
        total_bytes: preview["total_bytes"].as_u64().unwrap(),
        changes: serde_json::from_value(preview["changes"].clone()).unwrap(),
    }
}

fn expire_saved_result_sessions(
    operator: &Path,
    owner: (u32, u32),
    now: u64,
    saved_review: bool,
    send_coexistence: bool,
    staged_inputs: &Path,
) -> thread::JoinHandle<Option<crate::broker::attention::Projection>> {
    let ready = operator.join("saved-ready");
    let cleaned = operator.join("saved-cleaned");
    let stale_ready = operator.join("stale-check-ready");
    let stale_result = operator.join("stale-check-result");
    let operator = operator.to_owned();
    let staged_inputs = staged_inputs.to_owned();
    thread::spawn(move || {
        if saved_review {
            report_stale_staging(&stale_ready, &stale_result, &staged_inputs);
        }
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let sealed = ["lua-worker-4", "lua-verifier-4"].iter().all(|id| {
                let root = Path::new(crate::launch_supervisor::SYSTEM_SESSIONS_ROOT).join(id);
                fs::metadata(&root).is_ok_and(|meta| meta.mode() & 0o7777 == 0o700)
                    && fs::read(root.join("workspace-retention.json"))
                        .ok()
                        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
                        .is_some_and(|marker| marker["disposed"] == true)
            });
            if ready.exists() && sealed {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "saved producer/verifier never reached sealed Disposal"
            );
            thread::sleep(Duration::from_millis(10));
        }
        let report = crate::workspace::retention::cleanup_expired(
            owner,
            now + crate::workspace::retention::DEFAULT_RETENTION_MS + 86_400_000,
        )
        .unwrap();
        assert_eq!(report.failed, 0, "expired Session cleanup failed");
        assert_eq!(
            report.removed, 2,
            "cleanup must actually remove both finished Session trees"
        );
        for id in ["lua-worker-4", "lua-verifier-4"] {
            assert!(
                !Path::new(crate::launch_supervisor::SYSTEM_SESSIONS_ROOT)
                    .join(id)
                    .join("workspace")
                    .exists()
            );
        }
        let pending = saved_review
            .then(|| saved_attention::wait_saved_pending("44444444-4444-4444-8444-444444444444"));
        if send_coexistence {
            let projection = operator.join("coexist-projection.json");
            let mut entries: Vec<crate::broker::attention::Projection> =
                fs::read_dir(Path::new(STATE).join("authorizations/attention-outbox/entries"))
                    .unwrap()
                    .map(|entry| {
                        serde_json::from_slice(&fs::read(entry.unwrap().path()).unwrap()).unwrap()
                    })
                    .collect();
            entries.sort_by_key(|entry| entry.sequence);
            write_json(
                &projection,
                &entries
                    .iter()
                    .map(crate::broker::attention::Projection::wire)
                    .collect::<Vec<_>>(),
            );
            chown(&projection, Some(owner.0), Some(owner.1)).unwrap();
        }
        fs::write(cleaned, b"").unwrap();
        println!("SAVED_RESULT_EXPIRED_SESSION_STORAGE_REMOVED");
        if send_coexistence {
            for phase in ["before", "after", "restart"] {
                let marker = operator.join(format!("coexist-{phase}"));
                let deadline = Instant::now() + Duration::from_secs(90);
                while !marker.exists() {
                    if operator.join("coexist-abort").exists() {
                        return pending;
                    }
                    assert!(
                        Instant::now() < deadline,
                        "missing coexistence {phase} checkpoint"
                    );
                    thread::sleep(Duration::from_millis(10));
                }
                assert_eq!(
                    saved_attention::assert_saved_pending("44444444-4444-4444-8444-444444444444"),
                    *pending.as_ref().unwrap()
                );
                fs::write(operator.join(format!("coexist-{phase}.ack")), b"").unwrap();
            }
        }
        pending
    })
}

fn inject_provider_hold(state: &Path, index: usize, now: u64) -> thread::JoinHandle<()> {
    let ready = state.join("hold-ready");
    let published = state.join("hold-published");
    thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(90);
        while !ready.exists() {
            assert!(
                Instant::now() < deadline,
                "Lua did not reach hold injection boundary"
            );
            thread::sleep(Duration::from_millis(10));
        }
        // Offline double only for Provider spending: publish the captured durable
        // hold at the observed prompt-completion boundary (case 6), or after a
        // clear status but before verifier authorization (case 7). No upstream.
        let run_id = format!("lua-run-{index}");
        let directory = Path::new(STATE)
            .join("authorizations/provider-requests/holds")
            .join(Digest::of(run_id.as_bytes()).hex());
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        let record = directory.join("0.json");
        write_json(
            &record,
            &crate::broker::provider_requests::ProviderHold {
                run_id,
                reason: crate::broker::provider_requests::HoldReason::Exhausted,
                held_at_ms: now,
            },
        );
        fs::set_permissions(&record, fs::Permissions::from_mode(0o600)).unwrap();
        own(&directory, BROKER_UID);
        fs::File::open(&record).unwrap().sync_all().unwrap();
        fs::File::open(&directory).unwrap().sync_all().unwrap();
        fs::write(published, b"").unwrap();
    })
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
    let destination = Path::new("/usr/local/lib/louiselm-tools/louiselm-capture");
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
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
    let now = envelope["expires_at_ms"].as_u64().unwrap() - 120_000;
    let duration_ms = if std::env::var_os("LOUISELM_TEST_WORKSPACE_SNAPSHOT").is_some() {
        300_000
    } else {
        120_000
    };
    // This operator workflow really uses Provider authority. The generic Run
    // fixture now denies it, so select the full positive scope explicitly here.
    envelope["provider_requests"] = serde_json::to_value(guard::approval(now)).unwrap();
    envelope["provider_requests"]["models"] = serde_json::json!(["gpt-5.6-luna"]);
    envelope["provider_requests"]["max_effort"] = serde_json::json!("high");
    envelope["provider_requests"]["max_run_requests"] = serde_json::json!(6);
    envelope["expires_at_ms"] = 0.into();
    envelope["bead_scope"]["expires_at_ms"] = 0.into();
    envelope["provider_requests"]["expires_at_ms"] = 0.into();
    serde_json::json!({
        "schema":"louiselm.operator.bead-run/2", "envelope":envelope,
        "execution":{"kind":"duration", "duration_ms":duration_ms},
        "beads":[{"id":"operator-3","prompt":"promote-fixture-1"},
            {"id":"operator-1","prompt":"promote-fixture-2"},
            {"id":"operator-2","prompt":"promote-fixture-3"}],
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
