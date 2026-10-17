//! Two real kernel boots, actual pre-admission refusal, killed lease owner,
//! distinct broker UID, installed signer and authenticated recovery transport.
use super::*;
use crate::{
    broker::{
        AuthorizationStore, BrokerError,
        operator::{
            InspectError, OperatorServer,
            consumed_recovery::{Request, Response},
        },
    },
    launcher_install::consumed_recovery::{Arm, RecoveryError, arm_at},
};

const MARKER: &str = "/var/tmp/louiselm-consumed-recovery-gate.json";
const RECOVERY_WORKER: &str = "launch_supervisor::system::installed_tests::consumed_recovery::installed_consumed_recovery_broker_worker";
const CRASH_WORKER: &str = "launch_supervisor::system::installed_tests::consumed_recovery::installed_consumed_recovery_crash_worker";

fn complete_at(
    paths: &LauncherPaths,
    operator: u32,
    id: &str,
    endpoint: &Path,
) -> Result<crate::launch_recovery::Proof, RecoveryError> {
    // Only this private fixture changes the fixed production storage location.
    let root = paths.state_root.parent().unwrap().parent().unwrap();
    crate::launcher_install::consumed_recovery::complete_at(
        paths,
        operator,
        id,
        endpoint,
        &root.join("sessions"),
    )
}

fn assert_storage_failure_keeps_reservation(root: &Path, paths: &LauncherPaths, operator: u32) {
    let original = root.join("sessions/crashed");
    let retained = root.join("sessions/crashed-preserved-fixture");
    fs::rename(&original, &retained).unwrap();
    symlink(&retained, &original).unwrap();
    let endpoint = root.join("rendezvous/recovery.sock");
    assert!(matches!(
        complete_at(paths, operator, "approval-crashed", &endpoint),
        Err(RecoveryError::Installation(_))
    ));
    assert!(matches!(
        crate::launcher_install::acquire_identity(paths, 1),
        Err(crate::launcher_install::LauncherError::Poisoned { slot: 1 })
    ));
    let Response::Snapshot { snapshot } = crate::broker::operator::consumed_recovery::exchange(
        &endpoint,
        BROKER_UID,
        &Request::Prepare {
            authorization_id: "approval-crashed".into(),
        },
    )
    .unwrap() else {
        panic!("snapshot")
    };
    assert!(snapshot.recovered.is_none());
    // Only our test symlink is removed; the retained tree is restored unchanged.
    fs::remove_file(&original).unwrap();
    fs::rename(&retained, &original).unwrap();
}

fn assert_broker_storage_failure_is_retryable(root: &Path, paths: &LauncherPaths, operator: u32) {
    let directory = root.join("state/receipts/consumed-recoveries");
    fs::create_dir(&directory).unwrap();
    chown(&directory, Some(BROKER_UID), Some(BROKER_UID)).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o500)).unwrap();
    assert!(matches!(
        complete_at(
            paths,
            operator,
            "approval",
            &root.join("rendezvous/recovery.sock")
        ),
        Err(RecoveryError::Broker(_))
    ));
    assert_eq!(fs::read_dir(&directory).unwrap().count(), 0);
    let operation = paths
        .state_root
        .join("consumed-recovery")
        .join(Digest::of(b"approval").hex());
    assert!(operation.join("outcome.json").is_file());
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
}

fn enabled() -> bool {
    std::env::var_os("LOUISELM_REQUIRE_CONSUMED_RECOVERY").is_some()
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
        .try_into()
        .unwrap()
}

fn grant(config: &LauncherConfig, name: &str) -> GrantRequest {
    let mut request = request();
    if name != "session" {
        request.session_id = name.into();
        request.request_id = format!("launch-{name}");
        request.authorization_id = format!("approval-{name}");
    }
    GrantRequest {
        role: crate::launch_protocol::LaunchRole::Agent,
        conformance: crate::launch_protocol::ConformanceAuthorization::default(),
        dependencies: None,
        skill_requests: None,
        beads_mutations: None,
        provider_requests: None,
        require_cold_recovery: false,
        request,
        controller_uid: config.operator_uid,
        expires_at_ms: now() + 3_600_000,
        broker_loss_grace_ms: 5000,
        commands: None,
    }
}

fn spawn_broker(root: &Path, initial: bool) -> (BrokerChild, mpsc::Receiver<String>) {
    for socket in [
        paths(root).broker_socket,
        root.join("rendezvous/recovery.sock"),
    ] {
        if socket.exists() {
            fs::remove_file(socket).unwrap();
        }
    }
    let child = Command::new("/usr/bin/setpriv")
        .args([
            "--reuid",
            &BROKER_UID.to_string(),
            "--regid",
            &BROKER_UID.to_string(),
            "--clear-groups",
        ])
        .arg(std::env::current_exe().unwrap())
        .args([
            RECOVERY_WORKER,
            "--exact",
            "--nocapture",
            "--test-threads=1",
            "--format=terse",
        ])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LOUISELM_RECOVERY_FIXTURE", root)
        .env(
            "LOUISELM_RECOVERY_INITIAL",
            if initial { "yes" } else { "no" },
        )
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut child = BrokerChild(child);
    let output = child.0.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(output).lines() {
            if tx.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    (child, rx)
}

#[test]
fn installed_consumed_recovery_broker_worker() {
    let Some(root) = std::env::var_os("LOUISELM_RECOVERY_FIXTURE") else {
        return;
    };
    let root = PathBuf::from(root);
    assert_eq!(rustix::process::geteuid().as_raw(), BROKER_UID);
    let broker = InstalledBroker::bind(&paths(&root), &root.join("state")).unwrap();
    let config = crate::launcher_install::public_runtime_config(&paths(&root)).unwrap();
    let store =
        AuthorizationStore::open(&root.join("state/authorizations"), config.pool.clone()).unwrap();
    if std::env::var("LOUISELM_RECOVERY_INITIAL").unwrap() == "yes" {
        let first = grant(&config, "session");
        let mut envelope = fixture_run_envelope(&first, Digest::of(b"plan").to_string(), now());
        // Later fixture children have fresh, shorter expiries within this Run.
        envelope.expires_at_ms = first.expires_at_ms + 3_600_000;
        envelope.max_sessions = 3;
        broker.authorize_run(&envelope).unwrap();
        broker.authorize(&first).unwrap();
        let crashed = grant(&config, "crashed");
        broker.authorize(&crashed).unwrap();
        store.consume_for_launcher(&crashed.request, now()).unwrap();
        println!("BROKER_READY");
        assert!(
            broker.serve_launch().is_err(),
            "actual guard refusal must not complete launch"
        );
        assert!(matches!(
            broker.authorize(&grant(&config, "exhausted")),
            Err(BrokerError::IdentityExhausted(_))
        ));
        assert_eq!(store.session_count_for_run("run").unwrap(), 2);
        println!("REFUSAL_RESERVED");
    }
    let server = OperatorServer::bind(&root.join("rendezvous/recovery.sock"), 0).unwrap();
    println!("RECOVERY_READY");
    let mut successor = false;
    loop {
        server
            .serve_recovery_once(|request| {
                if root.join("authorize-successor").exists() && !successor {
                    assert_eq!(
                        broker
                            .authorize(&grant(&config, "successor"))
                            .unwrap()
                            .identity
                            .slot,
                        0
                    );
                    assert_eq!(store.session_count_for_run("run").unwrap(), 3);
                    assert!(matches!(
                        broker.authorize(&grant(&config, "over-ceiling")),
                        Err(BrokerError::InvalidGrant)
                    ));
                    successor = true;
                }
                let result = match request {
                    Request::Prepare { authorization_id } => broker
                        .consumed_recovery_snapshot(&authorization_id)
                        .map(|snapshot| Response::Snapshot {
                            snapshot: Box::new(snapshot),
                        }),
                    Request::Commit { proof } => {
                        let ack = broker
                            .accept_consumed_recovery(&proof)
                            .map_err(|_| InspectError::StatusUnavailable)?;
                        if root.join("withhold-ack").exists() {
                            return Err(InspectError::BrokerUnavailable);
                        }
                        Ok(Response::Acknowledged {
                            acknowledgement: ack,
                        })
                    }
                };
                result.map_err(|_| InspectError::StatusUnavailable)
            })
            .unwrap();
    }
}

#[test]
fn installed_consumed_recovery_crash_worker() {
    let Some(root) = std::env::var_os("LOUISELM_RECOVERY_CRASH") else {
        return;
    };
    let lease = crate::launcher_install::acquire_identity(&paths(Path::new(&root)), 1).unwrap();
    println!("LEASE_HELD");
    // The parent kills this actual process. No destructor clears its durable fence.
    std::io::stdout().flush().unwrap();
    loop {
        thread::park();
        std::hint::black_box(&lease);
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "The first boot preserves the causal refusal, live-owner refusal, actual crash and arming evidence for the subsequent boot."
)]
fn privileged_consumed_recovery_before_reboot() {
    if !enabled() {
        return;
    }
    assert!(rustix::process::geteuid().is_root());
    assert!(
        !Path::new(MARKER).exists(),
        "fresh disposable recovery gate required"
    );
    let account = BrokerAccount::create();
    let root = tempfile::Builder::new()
        .prefix("louiselm-consumed-recovery-")
        .tempdir_in("/var/lib")
        .unwrap()
        .keep();
    let (paths, mut config, registry) = install_fixture_with_slots(&root, 2);
    config.conformance = crate::conformance::admission::Enforcement::Enforced;
    write_json(&paths.state_root.join("config.json"), &config);
    write_json(&paths.state_root.join("public-config.json"), &config);
    crate::launcher_install::require_session_policy(&paths, &config).unwrap();
    let measured = crate::conformance::installed::measure(
        &paths,
        &config,
        Instant::now() + Duration::from_secs(15),
    )
    .err();
    assert!(
        matches!(
            measured,
            Some(crate::conformance::installed::CertificationError::GuardUnavailable)
        ),
        "fixture requires actual missing guard measurement: {measured:?}"
    );
    let (broker, lines) = spawn_broker(&root, true);
    marker(&lines, "BROKER_READY");
    let mut platform =
        SystemLaunchPlatform::new(paths.clone(), config.clone(), Duration::from_secs(15)).unwrap();
    platform.registry_root = registry.clone();
    let sessions = root.join("sessions");
    fs::create_dir(&sessions).unwrap();
    fs::set_permissions(&sessions, fs::Permissions::from_mode(0o711)).unwrap();
    let supervisor = LaunchSupervisor::new(
        connect_control_broker(&config, Duration::from_secs(15)).unwrap(),
        Arc::new(InstalledLaunchSigner::open(&paths, Duration::from_secs(15)).unwrap()),
        Arc::new(platform),
        Arc::new(Registry::open_trusted(&registry).unwrap()),
        sessions,
        Duration::from_secs(15),
    );
    let (tx, rx) = mpsc::channel();
    supervisor
        .launch(
            request(),
            config.operator_uid,
            now(),
            Box::new(move |result| {
                tx.send(result).unwrap();
            }),
        )
        .unwrap();
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(45)).unwrap().err(),
        Some(SupervisorError::ConformanceRefused(
            crate::conformance::admission::Condition::GuardUnavailable
        ))
    );
    marker(&lines, "REFUSAL_RESERVED");
    marker(&lines, "RECOVERY_READY");
    let retained = root.join("sessions/crashed");
    fs::create_dir(&retained).unwrap();
    fs::set_permissions(&retained, fs::Permissions::from_mode(0o711)).unwrap();
    fs::write(
        retained.join("private-fixture"),
        b"retained private fixture",
    )
    .unwrap();
    chown(
        retained.join("private-fixture"),
        Some(AGENT_UID + 1),
        Some(AGENT_UID + 1),
    )
    .unwrap();
    fs::set_permissions(
        retained.join("private-fixture"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let accessible = Command::new("/usr/bin/setpriv")
        .args([
            "--reuid",
            &(AGENT_UID + 1).to_string(),
            "--regid",
            &(AGENT_UID + 1).to_string(),
            "--clear-groups",
            "/usr/bin/cat",
        ])
        .arg(retained.join("private-fixture"))
        .output()
        .unwrap();
    assert!(
        accessible.status.success(),
        "fixture models an unsealed crashed workspace"
    );
    let endpoint = root.join("rendezvous/recovery.sock");
    assert!(matches!(
        complete_at(&paths, config.operator_uid, "approval", &endpoint),
        Err(RecoveryError::NotArmed)
    ));
    let mut crash = BrokerChild(
        Command::new(std::env::current_exe().unwrap())
            .args([
                CRASH_WORKER,
                "--exact",
                "--nocapture",
                "--test-threads=1",
                "--format=terse",
            ])
            .env("LOUISELM_RECOVERY_CRASH", &root)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut output = BufReader::new(crash.0.stdout.take().unwrap());
    let mut line = String::new();
    loop {
        line.clear();
        assert_ne!(output.read_line(&mut line).unwrap(), 0);
        if line.contains("LEASE_HELD") {
            break;
        }
    }
    // Explicit signed-but-undelivered preparation fixture. Recovery references
    // the actual root registration; it never imports a fictitious receipt chain.
    // The separate first case exercised actual GuardUnavailable.
    let snapshot = crate::broker::operator::consumed_recovery::exchange(
        &endpoint,
        BROKER_UID,
        &Request::Prepare {
            authorization_id: "approval-crashed".into(),
        },
    )
    .unwrap();
    let Response::Snapshot { snapshot } = snapshot else {
        panic!("snapshot")
    };
    let signer = crate::launcher_install::LauncherSigner::open(&paths).unwrap();
    let payload = super::receipt_history::genesis(&signer, &snapshot.binding.authorization);
    let signature = signer
        .sign_receipt(&payload.signing_key_id, &payload.canonical_bytes())
        .unwrap();
    write_json(
        &root.join("crashed-undelivered-receipt.json"),
        &crate::launch_receipt::SignedReceipt {
            schema: crate::launch_receipt::SIGNED_RECEIPT_SCHEMA.into(),
            payload,
            signature,
        },
    );
    let binding_path = paths
        .state_root
        .join("receipt-bindings")
        .join(format!("{}.json", Digest::of(b"crashed").hex()));
    fs::copy(&binding_path, root.join("original-root-binding.json")).unwrap();
    assert!(matches!(
        arm_at(&paths, config.operator_uid, "approval-crashed", &endpoint),
        Err(RecoveryError::Busy)
    ));
    crash.0.kill().unwrap();
    crash.0.wait().unwrap();
    for id in ["approval", "approval-crashed"] {
        let armed = arm_at(&paths, config.operator_uid, id, &endpoint).unwrap();
        assert_eq!(
            arm_at(&paths, config.operator_uid, id, &endpoint).unwrap(),
            armed
        );
        assert!(matches!(
            complete_at(&paths, config.operator_uid, id, &endpoint),
            Err(RecoveryError::RebootRequired)
        ));
        write_json(&root.join(format!("{id}-arm.json")), &armed);
    }
    for entry in fs::read_dir(root.join("state/authorizations/consumed")).unwrap() {
        let entry = entry.unwrap();
        fs::copy(entry.path(), root.join(entry.file_name())).unwrap();
    }
    write_json(Path::new(MARKER), &root);
    // Account and exact persistent fixture survive the real kernel reboot.
    std::mem::forget(account);
    drop(broker);
    println!("RECOVERY_ARMED_REBOOT_REQUIRED {}", root.display());
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "The second boot checks durable proof, interrupted ACK, signing settlement and successor isolation as one recovery transaction."
)]
fn privileged_consumed_recovery_after_reboot() {
    if !enabled() {
        return;
    }
    assert!(rustix::process::geteuid().is_root());
    let root: PathBuf = serde_json::from_slice(&fs::read(MARKER).unwrap()).unwrap();
    let paths = paths(&root);
    let config = crate::launcher_install::runtime_config(&paths).unwrap();
    let armed: Arm =
        serde_json::from_slice(&fs::read(root.join("approval-arm.json")).unwrap()).unwrap();
    assert_ne!(
        armed.armed_boot_id,
        fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .unwrap()
            .trim()
    );
    let (broker, lines) = spawn_broker(&root, false);
    marker(&lines, "RECOVERY_READY");
    let endpoint = root.join("rendezvous/recovery.sock");
    assert_storage_failure_keeps_reservation(&root, &paths, config.operator_uid);
    assert_broker_storage_failure_is_retryable(&root, &paths, config.operator_uid);
    fs::write(root.join("withhold-ack"), b"test lost response").unwrap();
    assert!(matches!(
        complete_at(&paths, config.operator_uid, "approval", &endpoint),
        Err(RecoveryError::Broker(_))
    ));
    fs::remove_file(root.join("withhold-ack")).unwrap();
    drop(broker);
    let (broker, lines) = spawn_broker(&root, false);
    marker(&lines, "RECOVERY_READY");
    fs::write(root.join("authorize-successor"), b"test next Session").unwrap();
    let successor = crate::launcher_install::acquire_identity(&paths, 0).unwrap();
    let proof = complete_at(&paths, config.operator_uid, "approval", &endpoint).unwrap();
    assert_eq!(proof.payload.schema, crate::launch_recovery::SCHEMA);
    assert_eq!(
        proof.payload.retained_storage,
        crate::launch_recovery::RetainedStorage::Sealed
    );
    assert_eq!(
        complete_at(&paths, config.operator_uid, "approval", &endpoint).unwrap(),
        proof
    );
    assert!(matches!(
        crate::launcher_install::acquire_identity(&paths, 0),
        Err(crate::launcher_install::LauncherError::Occupied { slot: 0 })
    ));
    successor.release().unwrap();
    fs::write(
        root.join("withhold-ack"),
        b"test signing-reference ACK boundary",
    )
    .unwrap();
    assert!(matches!(
        complete_at(&paths, config.operator_uid, "approval-crashed", &endpoint),
        Err(RecoveryError::Broker(_))
    ));
    let ring = crate::launcher_install::public_keyring(&paths).unwrap();
    assert_eq!(
        ring.keys.iter().find_map(|key| key
            .signing_sessions
            .as_ref()
            .and_then(|sessions| sessions.get("crashed"))),
        Some(&crate::launcher_install::SessionKeyState::Live)
    );
    fs::remove_file(root.join("withhold-ack")).unwrap();
    let crashed = complete_at(&paths, config.operator_uid, "approval-crashed", &endpoint).unwrap();
    let denied = Command::new("/usr/bin/setpriv")
        .args([
            "--reuid",
            &(AGENT_UID + 1).to_string(),
            "--regid",
            &(AGENT_UID + 1).to_string(),
            "--clear-groups",
            "/usr/bin/cat",
        ])
        .arg(root.join("sessions/crashed/private-fixture"))
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    assert!(
        !denied.status.success(),
        "recovery must seal crashed storage before freeing its identity"
    );
    assert!(
        String::from_utf8(denied.stderr)
            .unwrap()
            .contains("Permission denied")
    );
    assert_eq!(
        fs::read(root.join("sessions/crashed/private-fixture")).unwrap(),
        b"retained private fixture"
    );
    assert_eq!(
        crate::sshsig::parse(&crashed.signature).unwrap().namespace,
        crate::launch_recovery::SCHEMA
    );
    assert!(crashed.payload.receipt_binding_digest.is_some());
    let binding_path = paths
        .state_root
        .join("receipt-bindings")
        .join(format!("{}.json", Digest::of(b"crashed").hex()));
    assert_eq!(
        fs::read(binding_path).unwrap(),
        fs::read(root.join("original-root-binding.json")).unwrap()
    );
    let ring = crate::launcher_install::public_keyring(&paths).unwrap();
    assert_eq!(
        ring.keys.iter().find_map(|key| key
            .signing_sessions
            .as_ref()
            .and_then(|sessions| sessions.get("crashed"))),
        Some(&crate::launcher_install::SessionKeyState::Complete)
    );
    assert_eq!(
        complete_at(&paths, config.operator_uid, "approval-crashed", &endpoint).unwrap(),
        crashed
    );
    for entry in fs::read_dir(root.join("state/authorizations/consumed")).unwrap() {
        let entry = entry.unwrap();
        assert_eq!(
            fs::read(entry.path()).unwrap(),
            fs::read(root.join(entry.file_name())).unwrap()
        );
    }
    write_json(
        Path::new("/var/tmp/louiselm-consumed-recovery-evidence.json"),
        &serde_json::json!({
            "refused_launch": proof,
            "crashed_owner": crashed,
            "pool": config.pool,
            "retained_consumed_records": 2,
            "run_session_count_after_successor": 3,
            "same_boot_refused": true,
            "live_owner_refused": true,
            "lost_ack_and_broker_restart_retried": true,
            "storage_failure_retained_reservation": true,
            "broker_storage_failure_retried_exact_saved_proof": true,
            "retained_bytes_sealed_before_identity_reuse": true,
            "historical_retry_did_not_touch_successor_lease": true
        }),
    );
    fs::set_permissions(
        "/var/tmp/louiselm-consumed-recovery-evidence.json",
        fs::Permissions::from_mode(0o444),
    )
    .unwrap();
    drop(broker);
    drop(BrokerAccount);
    println!("RECOVERY_PROVED_HISTORY_RETAINED {}", root.display());
}
