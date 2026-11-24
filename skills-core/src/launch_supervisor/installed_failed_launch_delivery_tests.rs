//! Exact protected evidence outlives its original supervisor process.
use super::*;

const PRODUCER: &str = "launch_supervisor::system::installed_tests::failed_launch_delivery::installed_failed_launch_producer";

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
        .try_into()
        .unwrap()
}

fn successor_request() -> LaunchRequest {
    let mut request = request();
    request.session_id = "successor".into();
    request.authorization_id = "successor-approval".into();
    request.request_id = "successor-launch".into();
    request
}

fn supervisor(root: &Path, failed: bool) -> LaunchSupervisor {
    let paths = paths(root);
    let config = crate::launcher_install::runtime_config(&paths).unwrap();
    let mut platform =
        SystemLaunchPlatform::new(paths.clone(), config.clone(), Duration::from_secs(5)).unwrap();
    platform.registry_root = root.join("registry");
    let scenario = fs::read_to_string(root.join("failure-case")).unwrap();
    let signer: Arc<dyn LaunchSigner> = if failed {
        Arc::new(failures::FaultSigner::saved_delivery(
            root,
            &paths,
            if scenario == "terminal-storage0" {
                "terminal-storage0"
            } else {
                "terminal-storage1"
            },
        ))
    } else {
        Arc::new(InstalledLaunchSigner::open(&paths, Duration::from_secs(5)).unwrap())
    };
    LaunchSupervisor::new(
        connect_control_broker(&config, Duration::from_secs(5)).unwrap(),
        signer,
        Arc::new(platform),
        Arc::new(Registry::open_trusted(&root.join("registry")).unwrap()),
        root.join("sessions"),
        Duration::from_secs(5),
    )
}

#[test]
fn installed_failed_launch_producer() {
    let Some(root) = std::env::var_os("LOUISELM_FAILED_PRODUCER") else {
        return;
    };
    assert!(rustix::process::geteuid().is_root());
    let root = PathBuf::from(root);
    let launch = supervisor(&root, true);
    let config = crate::launcher_install::public_runtime_config(&paths(&root)).unwrap();
    if fs::read_to_string(root.join("failure-case")).unwrap() == "terminal-storage0" {
        let input = config
            .broker_socket_path
            .parent()
            .unwrap()
            .join("workspace-inputs");
        fs::set_permissions(input, fs::Permissions::from_mode(0o500)).unwrap();
    }
    let (sender, receiver) = mpsc::channel();
    launch
        .launch(
            request(),
            config.operator_uid,
            now(),
            Box::new(move |result| {
                sender.send(result.map(|_| ())).unwrap();
            }),
        )
        .unwrap();
    let error = receiver
        .recv_timeout(Duration::from_secs(30))
        .unwrap()
        .unwrap_err();
    assert_eq!(
        error,
        SupervisorError::StartupDeliveryPending {
            failure: Box::new(
                if fs::read_to_string(root.join("failure-case")).unwrap() == "terminal-storage0" {
                    SupervisorError::ResolutionFailed.during("workspace_prepare")
                } else {
                    SupervisorError::ToolIsolationUnproven
                }
            )
        }
    );
    println!("ORIGINAL_SUPERVISOR_EXITING_WITH_SAVED_PROOF");
}

pub(super) fn broker_worker(root: &Path, broker: &Arc<InstalledBroker>) {
    println!("DELIVERY_BROKER_READY");
    assert!(matches!(
        broker.serve_connection(),
        Err(crate::broker::BrokerError::LaunchFailed)
    ));
    assert_eq!(
        broker
            .inspect("session")
            .unwrap()
            .unwrap()
            .last_failure
            .unwrap()
            .code,
        crate::launch_protocol::ErrorCode::LaunchFailed
    );
    let config = crate::launcher_install::public_runtime_config(&paths(root)).unwrap();
    let grant = GrantRequest {
        role: crate::launch_protocol::LaunchRole::Agent,
        request: successor_request(),
        controller_uid: config.operator_uid,
        expires_at_ms: now() + 60_000,
        broker_loss_grace_ms: 500,
        conformance: crate::launch_protocol::ConformanceAuthorization::default(),
        dependencies: None,
        skill_requests: None,
        beads_mutations: None,
        provider_requests: None,
        commands: None,
        require_cold_recovery: false,
    };
    assert_eq!(broker.authorize(&grant).unwrap().identity.slot, 0);
    println!("SUCCESSOR_AUTHORIZED");
    thread::scope(|scope| {
        let (running, ready) = mpsc::channel();
        let live = scope.spawn(move || {
            let mut successor = broker.serve_launch().unwrap();
            println!("SUCCESSOR_RUNNING");
            running.send(()).unwrap();
            while !broker.step(&mut successor).unwrap() {}
        });
        ready.recv_timeout(Duration::from_secs(30)).unwrap();
        assert!(matches!(
            broker.serve_connection(),
            Err(crate::broker::BrokerError::LaunchFailed)
        ));
        println!("EXACT_REPLAY_ACCEPTED_WITH_SUCCESSOR");
        live.join().unwrap();
    });
    let store =
        crate::broker::AuthorizationStore::open(&root.join("state/authorizations"), config.pool)
            .unwrap();
    assert_eq!(store.session_count_for_run("run").unwrap(), 2);
    println!("DELIVERY_BROKER_DONE");
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "The installed two-process failure, broker restart, live successor and exact retry share one ordered fixture lifecycle."
)]
fn privileged_installed_failed_launch_delivery() {
    if std::env::var_os("LOUISELM_REQUIRE_BROKER_LAUNCH").is_none() {
        eprintln!("skipping: requires the disposable launcher VM");
        return;
    }
    assert!(rustix::process::geteuid().is_root());
    let _account = BrokerAccount::create();
    for scenario in ["terminal-storage0", "terminal-storage1"] {
        let root = tempfile::Builder::new()
            .prefix("louiselm-failed-delivery-")
            .tempdir_in("/var/lib")
            .unwrap();
        let (paths, config, registry) = install_fixture(root.path());
        fs::create_dir(root.path().join("sessions")).unwrap();
        fs::set_permissions(
            root.path().join("sessions"),
            fs::Permissions::from_mode(0o711),
        )
        .unwrap();
        fs::write(root.path().join("failure-case"), scenario).unwrap();
        let registry_path = registry.join("agents.json");
        let original_registry = fs::read(&registry_path).unwrap();
        let (mut original_broker, lines) = broker_process(root.path(), Some(scenario));
        marker(&lines, "BROKER_READY");
        let producer = Command::new(std::env::current_exe().unwrap())
            .args([PRODUCER, "--exact", "--nocapture"])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LOUISELM_FAILED_PRODUCER", root.path())
            .output()
            .unwrap();
        assert!(producer.status.success(), "{producer:?}");
        marker(&lines, "BROKER_REJECTED");
        assert!(original_broker.0.wait().unwrap().success());
        assert!(config.broker_socket_path.starts_with(root.path()));
        fs::remove_file(&config.broker_socket_path).unwrap();
        fs::write(&registry_path, original_registry).unwrap();
        fs::set_permissions(
            config
                .broker_socket_path
                .parent()
                .unwrap()
                .join("workspace-inputs"),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        let directory = root.path().join("state/receipts/sessions");
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        if directory.join("session").exists() {
            fs::set_permissions(directory.join("session"), fs::Permissions::from_mode(0o700))
                .unwrap();
        }
        let saved = paths.state_root.join("startup-receipts/session.json");
        let original = fs::read(&saved).unwrap();
        let records: Vec<crate::launcher_install::startup_receipts::StartupReceipt> =
            serde_json::from_slice(&original).unwrap();
        assert_eq!(
            records.len(),
            if scenario == "terminal-storage0" {
                1
            } else {
                2
            }
        );
        let terminal = crate::launch_receipt::SignedReceipt::parse_canonical(
            &records.last().unwrap().receipt_bytes,
        )
        .unwrap();
        assert!(terminal.payload.outcome.is_failed_launch());
        assert!(matches!(terminal.payload.outcome,
            crate::launch_receipt::ReceiptOutcome::FailedLaunch { agent_execution, .. }
            if agent_execution == if scenario == "terminal-storage0" {
                crate::launch_receipt::AgentExecution::NotStarted
            } else { crate::launch_receipt::AgentExecution::Started }));
        fs::write(root.path().join("saved-delivery"), b"").unwrap();
        let (mut restarted, lines) = broker_process(root.path(), None);
        marker(&lines, "DELIVERY_BROKER_READY");
        assert!(matches!(
            crate::launcher_install::startup_receipts::deliver_failed_launch(
                &paths,
                config.operator_uid + 1,
                "session"
            ),
            Err(crate::launcher_install::startup_receipts::DeliveryError::MissingProof)
        ));
        let deliver = || {
            crate::launcher_install::startup_receipts::deliver_failed_launch(
                &paths,
                config.operator_uid,
                "session",
            )
            .unwrap()
        };
        let first = deliver();
        marker(&lines, "SUCCESSOR_AUTHORIZED");
        let launch = supervisor(root.path(), false);
        let (sender, receiver) = mpsc::channel();
        launch
            .launch(
                successor_request(),
                config.operator_uid,
                now(),
                Box::new(move |result| {
                    sender.send(result).unwrap();
                }),
            )
            .unwrap();
        let successor = receiver
            .recv_timeout(Duration::from_secs(20))
            .unwrap()
            .unwrap();
        marker(&lines, "SUCCESSOR_RUNNING");
        let crate::launch_receipt::ReceiptOutcome::Start { evidence, .. } =
            &successor.receipt().payload.outcome
        else {
            panic!("Start")
        };
        let process = PathBuf::from(format!("/proc/{}", evidence.agent_pid));
        assert!(process.exists());
        assert_eq!(deliver(), first);
        marker(&lines, "EXACT_REPLAY_ACCEPTED_WITH_SUCCESSOR");
        assert!(process.exists());
        assert_eq!(fs::read(&saved).unwrap(), original);
        let keyring: serde_json::Value =
            serde_json::from_slice(&fs::read(paths.state_root.join("keyring.json")).unwrap())
                .unwrap();
        assert!(
            keyring["keys"]
                .as_array()
                .unwrap()
                .iter()
                .any(|key| key["signing_sessions"]["session"] == "complete")
        );
        successor.dispose().unwrap();
        marker(&lines, "DELIVERY_BROKER_DONE");
        assert!(restarted.0.wait().unwrap().success());
        assert!(!process.exists());
        eprintln!(
            "FAILED_LAUNCH_DELIVERY {scenario}: original process exited, broker restarted, exact replay preserved live successor"
        );
    }
}
