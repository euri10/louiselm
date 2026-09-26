//! Real producer effect -> protected export -> distinct configured Agent -> exact plan -> disposal.

use super::*;
use crate::{
    broker::{BrokerSession, lifecycle::LifecycleCaller, verification::VerificationStatus},
    launch_protocol::{
        LIFECYCLE_REQUEST_SCHEMA, LifecycleAction, LifecycleRequest, VERIFICATION_SCHEMA,
        VerificationOperation, VerificationRequest, VerificationStep,
    },
    launch_receipt::SessionState,
};

const VERIFY_WORKER: &str =
    "launch_supervisor::system::installed_tests::verification::installed_verification_worker";
const LITERAL_ARGUMENT: &str = "literal'$(touch INJECTED)";

#[path = "installed_promotion_tests.rs"]
mod promotion;

fn clock_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

fn verifier_launch(index: usize) -> LaunchRequest {
    LaunchRequest {
        request_id: format!("verify-launch-{index}"),
        authorization_id: format!("verify-approval-{index}"),
        session_id: format!("verifier-{index}"),
        ..request()
    }
}

fn lifecycle(
    session: &BrokerSession,
    action: LifecycleAction,
    state: SessionState,
    sequence: u64,
) -> LifecycleRequest {
    let launch = session.authorization();
    LifecycleRequest {
        schema: LIFECYCLE_REQUEST_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        request_id: format!("fixture-{sequence}"),
        authorization_id: format!("fixture-{sequence}"),
        session_id: launch.session_id.clone(),
        run_id: launch.run_id.clone(),
        action,
        expected_state: state,
        expected_receipt_sequence: Some(sequence),
        envelope_revision: launch.envelope_revision,
    }
}

fn approval(launch: LaunchRequest, uid: u32, commands: Option<ApprovedCommands>) -> GrantRequest {
    GrantRequest {
        conformance: crate::launch_protocol::ConformanceAuthorization::default(),
        dependencies: None,
        skill_requests: None,
        beads_mutations: None,
        provider_requests: None,
        require_cold_recovery: false,
        request: launch,
        controller_uid: uid,
        expires_at_ms: clock_ms() + 30000,
        broker_loss_grace_ms: 500,
        commands,
    }
}

#[test]
fn installed_verification_worker() {
    let Some(root) = std::env::var_os("LOUISELM_VERIFICATION_FIXTURE") else {
        return;
    };
    let root = PathBuf::from(root);
    let broker = InstalledBroker::bind(&paths(&root), &root.join("state")).unwrap();
    let config = crate::launcher_install::public_runtime_config(&paths(&root)).unwrap();
    let caller = LifecycleCaller::Operator {
        uid: config.operator_uid,
    };
    broker
        .authorize(&approval(
            request(),
            config.operator_uid,
            Some(ApprovedCommands {
                command_digest: Digest::of(COMMAND.as_bytes()).to_string(),
                timeout_ms: 5000,
                uses: Some(1),
                allow_delegation: false,
                expires_at_ms: clock_ms() + 60000,
            }),
        ))
        .unwrap();
    println!("PRODUCER_READY");
    let mut producer = broker.serve_launch().unwrap();
    // The producing Agent really requests and completes this governed write.
    for _ in 0..2 {
        assert!(!broker.step(&mut producer).unwrap());
    }
    let park = lifecycle(&producer, LifecycleAction::Park, SessionState::Running, 1);
    broker
        .request_lifecycle(&mut producer, &caller, &park)
        .unwrap();
    for index in 0..5 {
        run_job(
            &broker,
            &mut producer,
            &caller,
            &root,
            index,
            config.operator_uid,
        );
    }
    promotion::broker_tainted_round(&broker, &mut producer, &root);
    let disposal = lifecycle(
        &producer,
        LifecycleAction::Disposal,
        SessionState::Parked,
        2,
    );
    broker
        .request_lifecycle(&mut producer, &caller, &disposal)
        .unwrap();
    println!("VERIFICATION_DONE");
}

#[expect(
    clippy::too_many_lines,
    reason = "One installed transaction preserves its export, denial attempts, actual outcome, cleanup and replay evidence."
)]
fn run_job(
    broker: &InstalledBroker,
    producer: &mut BrokerSession,
    caller: &LifecycleCaller,
    root: &Path,
    index: usize,
    uid: u32,
) {
    let root = root.join("inputs");
    let snapshot = root.join("snapshot");
    let plan = root.join(format!("plan-{index}.json"));
    let input = broker
        .stage_verification(
            &format!("input-{index}"),
            &snapshot,
            &Digest::of(&fs::read(snapshot.join("snapshot.json")).unwrap()),
            &plan,
            &Digest::of(&fs::read(&plan).unwrap()),
        )
        .unwrap();
    let export_request = VerificationRequest {
        schema: VERIFICATION_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        request_id: format!("export-{index}"),
        launch: request(),
        head: broker
            .inspect("session")
            .unwrap()
            .unwrap()
            .broker_head
            .unwrap(),
        expires_at_ms: clock_ms() + 60000,
        operation: VerificationOperation::Export {
            input_id: format!("input-{index}"),
            input_digest: input.to_string(),
        },
    };
    let exported = broker
        .export_verification(producer, caller, &export_request)
        .unwrap();
    let launch = verifier_launch(index);
    broker
        .authorize(&approval(launch.clone(), uid, None))
        .unwrap();
    println!("VERIFIER_READY_{index}");
    let mut verifier = broker.serve_launch().unwrap();
    let run = VerificationRequest {
        schema: VERIFICATION_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        request_id: format!("run-{index}"),
        launch,
        head: verifier.launch_head().clone(),
        expires_at_ms: clock_ms() + 60000,
        operation: VerificationOperation::Run {
            producer_session_id: "session".into(),
            export_request_id: export_request.request_id,
            export_digest: exported.digest().unwrap().to_string(),
            job_digest: exported.job.job_digest.clone(),
        },
    };
    assert!(
        broker
            .run_verification(&mut verifier, &LifecycleCaller::Agent, &run)
            .is_err()
    );
    assert_eq!(
        broker.verification_status(&run.launch.session_id).unwrap(),
        VerificationStatus::NotRequested
    );
    for mutation in 0..5 {
        let mut refused = run.clone();
        match mutation {
            0 => refused.head.digest = Digest::of(b"stale").to_string(),
            1 => refused.launch.skill_generation_id = Digest::of(b"stale-generation").to_string(),
            2 => refused.expires_at_ms = 1,
            _ => {
                let VerificationOperation::Run {
                    job_digest,
                    export_digest,
                    ..
                } = &mut refused.operation
                else {
                    panic!("run");
                };
                if mutation == 3 {
                    *job_digest = Digest::of(b"substituted").to_string();
                } else {
                    *export_digest = Digest::of(b"forged").to_string();
                }
            }
        }
        assert!(
            broker
                .run_verification(&mut verifier, caller, &refused)
                .is_err()
        );
        assert!(!verifier.channel().is_closed());
        assert_eq!(
            broker.verification_status(&run.launch.session_id).unwrap(),
            VerificationStatus::NotRequested
        );
    }
    let outcome = broker.run_verification(&mut verifier, caller, &run);
    if index >= 3 {
        assert!(
            outcome.is_err(),
            "tamper and lost verifier lifetime cannot pass"
        );
        assert_eq!(
            broker.verification_status(&run.launch.session_id).unwrap(),
            VerificationStatus::Unknown
        );
        println!("VERIFIER_DONE_{index}");
        return;
    }
    let record = outcome.unwrap();
    assert_eq!(record.execution.commands_passed(), index == 0);
    assert!(record.execution.cleanup_proven);
    assert_eq!(record.terminal_head.sequence, 2);
    if index == 1 {
        assert_eq!(
            record.execution.steps,
            vec![VerificationStep::Completed {
                exit_code: 7,
                timed_out: false
            }]
        );
    }
    if index == 2 {
        assert!(matches!(
            record.execution.steps[0],
            VerificationStep::Completed {
                timed_out: true,
                ..
            }
        ));
    }
    assert_eq!(
        broker.verification_status(&run.launch.session_id).unwrap(),
        VerificationStatus::Completed(Box::new(record))
    );
    assert!(
        broker
            .run_verification(&mut verifier, caller, &run)
            .is_err(),
        "spent launch cannot be replayed"
    );
    if index == 0 {
        promotion::broker_round(broker, producer, root.parent().unwrap());
    } else if index < 3 {
        promotion::broker_denial(broker, producer, root.parent().unwrap(), index);
    }
    println!("VERIFIER_DONE_{index}");
}

fn prepare_inputs(root: &Path) {
    let root = root.join("inputs");
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
    let outside = root.join("operator-visible");
    fs::write(&outside, b"host-only").unwrap();
    fs::set_permissions(&outside, fs::Permissions::from_mode(0o444)).unwrap();
    // Positive control: the assigned UID could read this file without confinement.
    let visible = Command::new("/usr/bin/setpriv")
        .args([
            "--reuid",
            &AGENT_UID.to_string(),
            "--regid",
            &AGENT_UID.to_string(),
            "--clear-groups",
            "/bin/cat",
        ])
        .arg(&outside)
        .output()
        .unwrap();
    assert!(visible.status.success());
    assert_eq!(visible.stdout, b"host-only");
    fs::create_dir_all(root.join("snapshot/files")).unwrap();
    for name in ["snapshot", "snapshot/files"] {
        fs::set_permissions(root.join(name), fs::Permissions::from_mode(0o755)).unwrap();
    }
    // Canonical empty fixture baseline: the producing Agent creates the candidate file.
    fs::write(root.join("snapshot/snapshot.json"), format!("{{\"schema\":\"louiselm.workspace.snapshot/1\",\"base_commit\":\"{}\",\"files\":[],\"selected\":[],\"changes\":[]}}", "a".repeat(40))).unwrap();
    fs::set_permissions(
        root.join("snapshot/snapshot.json"),
        fs::Permissions::from_mode(0o444),
    )
    .unwrap();
    let plans = [
        vec![
            serde_json::json!({"argv":["sh","-c","test \"$(cat effect)\" = authorized && printf checked > effect"],"cwd":".","timeout_ms":5000}),
            serde_json::json!({"argv":["sh","-c","test \"$(cat effect)\" = checked"],"cwd":".","timeout_ms":5000}),
            serde_json::json!({"argv":["touch",LITERAL_ARGUMENT],"cwd":".","timeout_ms":5000}),
            serde_json::json!({"argv":["sh","-c",format!("test ! -e '{}' && test ! -e /tmp/louiselm-capability.sock", outside.display())],"cwd":".","timeout_ms":5000}),
        ],
        vec![
            serde_json::json!({"argv":["sh","-c","exit 7"],"cwd":".","timeout_ms":5000}),
            serde_json::json!({"argv":["touch","NOT_ALLOWED"],"cwd":".","timeout_ms":5000}),
        ],
        vec![serde_json::json!({"argv":["sleep","30"],"cwd":".","timeout_ms":250})],
        vec![serde_json::json!({"argv":["touch","NOT_ALLOWED"],"cwd":".","timeout_ms":5000})],
        vec![
            serde_json::json!({"argv":["sh","-c","touch started; sleep 30"],"cwd":".","timeout_ms":45000}),
        ],
    ];
    for (index, commands) in plans.into_iter().enumerate() {
        write_json(
            &root.join(format!("plan-{index}.json")),
            &serde_json::json!({"schema":"louiselm.workspace.verification-plan/1","commands":commands}),
        );
        fs::set_permissions(
            root.join(format!("plan-{index}.json")),
            fs::Permissions::from_mode(0o444),
        )
        .unwrap();
    }
}

fn worker(root: &Path) -> (BrokerChild, mpsc::Receiver<String>) {
    let mut child = Command::new("/usr/bin/setpriv")
        .args([
            "--reuid",
            &BROKER_UID.to_string(),
            "--regid",
            &BROKER_UID.to_string(),
            "--clear-groups",
        ])
        .arg(std::env::current_exe().unwrap())
        .args([VERIFY_WORKER, "--exact", "--nocapture"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LOUISELM_VERIFICATION_FIXTURE", root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let output = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(output).lines() {
            if tx.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    (BrokerChild(child), rx)
}

fn launch_real(
    paths: &LauncherPaths,
    config: &LauncherConfig,
    registry_root: &Path,
    sessions: &Path,
    request: LaunchRequest,
) -> crate::launch_supervisor::LaunchedSession {
    let mut platform =
        SystemLaunchPlatform::new(paths.clone(), config.clone(), Duration::from_secs(5)).unwrap();
    platform.registry_root = registry_root.into();
    let supervisor = LaunchSupervisor::new(
        connect_control_broker(config, Duration::from_secs(5)).unwrap(),
        Arc::new(InstalledLaunchSigner::open(paths, Duration::from_secs(5)).unwrap()),
        Arc::new(platform),
        Arc::new(Registry::open_trusted(registry_root).unwrap()),
        sessions.into(),
        Duration::from_secs(5),
    );
    let (tx, rx) = mpsc::channel();
    supervisor
        .launch(
            request,
            config.operator_uid,
            clock_ms(),
            Box::new(move |result| {
                tx.send(result).unwrap();
            }),
        )
        .unwrap();
    rx.recv_timeout(Duration::from_secs(20)).unwrap().unwrap()
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "One disposable fixture owns real producer/verifier lifetimes, adverse events and independent cleanup assertions."
)]
fn privileged_installed_exact_job_verification() {
    if std::env::var_os("LOUISELM_REQUIRE_BROKER_LAUNCH").is_none() {
        eprintln!("skipping: exact-job verification requires the disposable launcher VM");
        return;
    }
    assert!(rustix::process::geteuid().is_root());
    assert!(
        fs::read_to_string("/proc/self/uid_map")
            .unwrap()
            .split_whitespace()
            .eq(["0", "0", "4294967295"])
    );
    let _account = BrokerAccount::create();
    let root = tempfile::Builder::new()
        .prefix("louiselm-verification-")
        .tempdir_in("/var/lib")
        .unwrap();
    // Broker identity reconciliation is separate work: each consumed launch keeps its slot reserved.
    let (paths, config, registry) = install_fixture_with_slots(root.path(), 6);
    prepare_inputs(root.path());
    promotion::prepare(root.path(), config.operator_uid);
    let sessions = root.path().join("sessions");
    fs::create_dir(&sessions).unwrap();
    fs::set_permissions(&sessions, fs::Permissions::from_mode(0o711)).unwrap();
    let (mut child, lines) = worker(root.path());
    marker(&lines, "PRODUCER_READY");
    let producer = launch_real(&paths, &config, &registry, &sessions, request());
    let (mut input, controller_input) = std::os::unix::net::UnixStream::pair().unwrap();
    let (controller_output, output) = std::os::unix::net::UnixStream::pair().unwrap();
    output
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let relay = thread::spawn(move || {
        producer.relay_stdio(
            RelayStdio::new(
                BufReader::new(fs::File::from(OwnedFd::from(controller_input))),
                fs::File::from(OwnedFd::from(controller_output)),
            )
            .unwrap(),
        )
    });
    let command = ToolExecutionRequest {
        schema: TOOL_EXECUTION_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        request_id: "produce".into(),
        session_id: "session".into(),
        run_id: "run".into(),
        envelope_revision: 1,
        sequence: 1,
        command: COMMAND.into(),
        timeout_ms: 5000,
    };
    // Keep both controller halves alive while the producer is Parked and exported.
    let mut output = BufReader::new(output);
    let response = exchange(&mut input, &mut output, &command.canonical_bytes());
    assert!(
        matches!(response.operation, CommandOperation::Result { outcome: CommandOutcome::Completed { output } } if output.exit_code == 0)
    );
    for index in 0..5 {
        marker(&lines, &format!("VERIFIER_READY_{index}"));
        if index == 3 {
            fs::write(
                sessions.join("session/verification-exports/export-3/job/source/effect"),
                b"tampered",
            )
            .unwrap();
        }
        let verifier = launch_real(
            &paths,
            &config,
            &registry,
            &sessions,
            verifier_launch(index),
        );
        if index == 4 {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !sessions
                .join("verifier-4/verification-run/workspace/started")
                .exists()
            {
                assert!(
                    Instant::now() < deadline,
                    "verification must actually start before cancellation"
                );
                thread::sleep(Duration::from_millis(10));
            }
            let crate::launch_receipt::ReceiptOutcome::Start { evidence, .. } =
                &verifier.receipt().payload.outcome
            else {
                panic!("actual Agent identity");
            };
            rustix::process::kill_process(
                rustix::process::Pid::from_raw(i32::try_from(evidence.agent_pid).unwrap()).unwrap(),
                rustix::process::Signal::TERM,
            )
            .unwrap();
        }
        if index == 0 {
            marker(&lines, "PROMOTION_READY");
            promotion::operator_round(root.path(), config.operator_uid);
        } else if index < 3 {
            marker(&lines, &format!("PROMOTION_DENIAL_{index}"));
            promotion::operator_denial(root.path(), config.operator_uid, index);
        }
        marker(&lines, &format!("VERIFIER_DONE_{index}"));
        eprintln!("verification fixture case {index}: durable outcome checked");
        if index == 0 {
            let work = sessions.join("verifier-0/verification-run/workspace");
            assert!(work.join(LITERAL_ARGUMENT).exists());
            assert!(
                !work.join("INJECTED").exists(),
                "argv must never gain implicit shell interpretation"
            );
        }
        let retained = sessions.join(format!(
            "session/verification-exports/export-{index}/job/source/effect"
        ));
        assert_eq!(
            fs::read(retained).unwrap(),
            if index == 3 {
                b"tampered".as_slice()
            } else {
                b"authorized".as_slice()
            },
            "commands cannot mutate the retained job"
        );
        assert!(
            !sessions
                .join(format!(
                    "verifier-{index}/verification-run/workspace/NOT_ALLOWED"
                ))
                .exists()
        );
        verifier.dispose().unwrap();
    }
    marker(&lines, "TAINTED_PROMOTION_READY");
    promotion::operator_tainted_round(root.path(), config.operator_uid);
    marker(&lines, "VERIFICATION_DONE");
    assert!(child.0.wait().unwrap().success());
    assert_eq!(
        fs::read(sessions.join("session/workspace/effect")).unwrap(),
        b"authorized"
    );
    relay.join().unwrap().unwrap();
    for slot in 0..6 {
        crate::launcher_install::acquire_identity(&paths, slot)
            .unwrap()
            .release()
            .unwrap();
    }
}
