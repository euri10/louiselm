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

#[derive(Clone, Copy)]
enum TrialCase {
    Startup,
    Paired,
    RefusedVerification,
}

#[test]
fn privileged_activated_brokered_guard_start() {
    if std::env::var_os("LOUISELM_REQUIRE_ACTIVATED_GUARD").is_none() {
        eprintln!("skipping: requires disposable VM, Neovim and private mounts");
        return;
    }
    run_installed(TrialCase::Startup);
}

#[test]
fn privileged_installed_lua_qualification() {
    if std::env::var_os("LOUISELM_REQUIRE_QUALIFICATION").is_none() {
        eprintln!("skipping: requires disposable VM, Neovim and private mounts");
        return;
    }
    let refused = Command::new("/usr/bin/unshare")
        .args(["--mount", "--net", "--propagation", "private"])
        .arg(std::env::current_exe().unwrap())
        .args([
            "launch_supervisor::system::installed_tests::daemon::qualification::installed_refused_qualification_worker",
            "--exact", "--nocapture", "--test-threads=1",
        ])
        .env("LOUISELM_REQUIRE_QUALIFICATION_REFUSAL", "1")
        .stderr(Stdio::inherit())
        .output()
        .unwrap();
    std::io::stdout().write_all(&refused.stdout).unwrap();
    assert!(
        refused.status.success()
            && String::from_utf8(refused.stdout)
                .unwrap()
                .lines()
                .any(|line| line.ends_with("INSTALLED_QUALIFICATION_REFUSAL_CLEANUP_PASSED")),
        "refused qualification must retain Unknown and finish authenticated cleanup"
    );
    run_installed(TrialCase::Paired);
}

#[test]
fn installed_refused_qualification_worker() {
    if std::env::var_os("LOUISELM_REQUIRE_QUALIFICATION_REFUSAL").is_none() {
        return;
    }
    run_installed(TrialCase::RefusedVerification);
}

#[expect(
    clippy::too_many_lines,
    reason = "One disposable installed transaction binds fixture inputs, explicit launch authority and proven terminal cleanup."
)]
fn run_installed(case: TrialCase) {
    let start_only = matches!(case, TrialCase::Startup);
    let refused = matches!(case, TrialCase::RefusedVerification);
    assert!(rustix::process::geteuid().is_root());
    let previous_maps = super::super::guard_cleanup::policy_maps();
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
    inputs.isolation.network = crate::registry::NetworkPolicy::Brokered;
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
    if refused {
        // This invocation owns a private mount namespace. Refuse the mechanic
        // without changing any trusted tool bytes or accepting an unknown job.
        assert!(
            Command::new("/usr/bin/mount")
                .args([
                    "--bind",
                    "/bin/false",
                    "/usr/lib/louiselm/verifier/bin/verifier-toolchain"
                ])
                .status()
                .unwrap()
                .success()
        );
    }
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
        .envs(refused.then_some(("LOUISELM_QUALIFICATION_EXPECT_UNKNOWN", "1")))
        .env("QUALIFICATION_AMBIENT_SECRET", "must-not-reach-Agent")
        .env("XDG_STATE_HOME", operator.join("state"))
        .status()
        .unwrap();
    // Controller disposal completes through the live broker even when the
    // trial refuses a verifier outcome. Do not strand its unused verifiers.
    if refused {
        assert!(
            Command::new("/usr/bin/umount")
                .arg("/usr/lib/louiselm/verifier/bin/verifier-toolchain")
                .status()
                .unwrap()
                .success()
        );
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    let sessions: &[&str] = if refused {
        &["baseline-worker", "baseline-verifier", "candidate-verifier"]
    } else {
        &[
            "baseline-worker",
            "baseline-verifier",
            "candidate-worker",
            "candidate-verifier",
        ]
    };
    let mut identities = Vec::with_capacity(sessions.len());
    for suffix in sessions {
        let id = format!("qualification-{suffix}");
        let directory = Path::new(STATE).join("receipts/sessions").join(&id);
        if !status.success() && !directory.try_exists().unwrap() {
            // A failed baseline need not authorize the candidate worker. The
            // outer-process barrier below still checks every launcher started.
            continue;
        }
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
                if !start_only
                    && suffix.ends_with("verifier")
                    && (status.success() || (refused && *suffix == "baseline-verifier"))
                {
                    let terminal = chain.last().unwrap();
                    assert!(matches!(
                        terminal.payload.outcome,
                        crate::launch_receipt::ReceiptOutcome::Disposal { .. }
                    ));
                    assert_eq!(
                        terminal.payload.request_id,
                        format!(
                            "verification-cleanup-{}",
                            Digest::of(format!("qualification-{suffix}-verify").as_bytes()).hex()
                        )
                    );
                }
                if refused && *suffix == "candidate-verifier" {
                    assert!(matches!(
                        chain.last().unwrap().payload.outcome,
                        crate::launch_receipt::ReceiptOutcome::Disposal {
                            authority: crate::launch_receipt::ReceiptAuthority::Cause {
                                cause: crate::launch_receipt::ReceiptCause::ControllerLost,
                            },
                        }
                    ));
                }
                break;
            }
            assert!(
                Instant::now() < deadline,
                "terminal cleanup missing for {id}; signed head: {:?}",
                chain
                    .last()
                    .map(|receipt| (receipt.payload.sequence, receipt.payload.resulting_state))
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
        identities.push(evidence.assigned_uid);
    }
    super::super::launcher_cleanup::assert_exited(&previous_maps);
    terminate(&mut daemon);
    assert!(
        if refused {
            status.code() == Some(6)
        } else {
            status.success()
        },
        "installed paired Lua qualification failed"
    );
    if refused {
        assert!(
            !Path::new(STATE)
                .join("receipts/sessions/qualification-candidate-worker")
                .try_exists()
                .unwrap()
        );
        assert_eq!(
            identities
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            3
        );
    } else {
        assert_qualification_identities(start_only, identities.try_into().unwrap());
    }
    assert_eq!(
        fs::read(&outside).unwrap(),
        b"must remain outside every Session\n"
    );
    assert!(!snapshot.join("files/.trial-marker").exists());
    println!(
        "INSTALLED_{}",
        match case {
            TrialCase::Startup => "ACTIVATED_GUARD_START_PASSED",
            TrialCase::Paired => "QUALIFICATION_PASSED",
            TrialCase::RefusedVerification => "QUALIFICATION_REFUSAL_CLEANUP_PASSED",
        }
    );
}

fn assert_qualification_identities(
    start_only: bool,
    [
        baseline_worker,
        baseline_verifier,
        candidate_worker,
        candidate_verifier,
    ]: [u32; 4],
) {
    // Both verifiers initialize before the baseline worker. Completed baseline
    // verification disposes its verifier before candidate worker authorization;
    // the parked baseline worker and candidate verifier still hold their UIDs.
    for phase in [
        [baseline_verifier, candidate_verifier, baseline_worker],
        [candidate_verifier, baseline_worker, candidate_worker],
    ] {
        assert_eq!(
            std::collections::BTreeSet::from(phase).len(),
            phase.len(),
            "live qualification Sessions share a host identity"
        );
    }
    // Startup-only executes no verifier job, so all four identities remain live.
    if start_only {
        assert_ne!(
            baseline_verifier, candidate_worker,
            "live qualification Sessions share a host identity"
        );
    }
}

#[test]
fn qualification_accepts_distinct_live_identities() {
    for start_only in [false, true] {
        assert_qualification_identities(start_only, [4_020_002, 4_020_000, 4_020_003, 4_020_001]);
    }
}

#[test]
fn paired_qualification_accepts_disposed_verifier_uid_reuse() {
    // Observed order/UIDs: owned guest /var/tmp/ogjcv-evidence/65ezh-diagnostic.log,
    // codex/01a1350d-f4a7-7192-9820-6e8b0dbc2f5b. Baseline verifier Disposal
    // preceded candidate worker Launch. Completed verification publishes only
    // after that authenticated cleanup, before trial.lua advances the arm.
    assert_qualification_identities(false, [4_020_002, 4_020_000, 4_020_000, 4_020_001]);
}

#[test]
#[should_panic(expected = "live qualification Sessions share a host identity")]
fn startup_only_qualification_rejects_the_same_uid_reuse() {
    assert_qualification_identities(true, [4_020_002, 4_020_000, 4_020_000, 4_020_001]);
}

#[test]
fn qualification_rejects_every_overlapping_identity_pair() {
    for (left, right) in [(0, 1), (0, 2), (0, 3), (1, 3), (2, 3)] {
        let mut identities = [4_020_002, 4_020_000, 4_020_003, 4_020_001];
        identities[right] = identities[left];
        let failure = std::panic::catch_unwind(|| {
            assert_qualification_identities(false, identities);
        })
        .expect_err("an overlapping identity pair must be refused");
        let message = failure
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| failure.downcast_ref::<&str>().copied());
        assert!(
            message.is_some_and(|message| {
                message.contains("live qualification Sessions share a host identity")
            }),
            "unexpected refusal for pair {left}/{right}"
        );
    }
}
