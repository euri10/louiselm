//! Real installed authority invalidates old snapshots and preserves unaffected history.

use super::*;
use crate::launcher_install::{KeyContainment, revoke_key};

#[test]
fn privileged_installed_revocation_freezes_with_or_without_broker() {
    if std::env::var_os("LOUISELM_REQUIRE_BROKER_LAUNCH").is_none() {
        eprintln!("skipping: installed live revocation requires the disposable launcher VM");
        return;
    }
    let _account = BrokerAccount::create();
    for lose_broker in [false, true] {
        live_revocation(lose_broker);
    }
}

fn live_revocation(lose_broker: bool) {
    let root = tempfile::Builder::new()
        .prefix("louiselm-live-revocation-")
        .tempdir_in("/var/lib")
        .unwrap();
    let (paths, config, registry_root) = install_fixture(root.path());
    fs::write(root.path().join("key-revocation"), b"").unwrap();
    let (mut broker, lines) = broker_process(root.path(), None);
    marker(&lines, "BROKER_READY");
    let mut platform =
        SystemLaunchPlatform::new(paths.clone(), config.clone(), Duration::from_secs(5)).unwrap();
    platform.registry_root = registry_root.clone();
    let sessions = root.path().join("sessions");
    fs::create_dir(&sessions).unwrap();
    fs::set_permissions(&sessions, fs::Permissions::from_mode(0o711)).unwrap();
    let signer = Arc::new(InstalledLaunchSigner::open(&paths, Duration::from_secs(5)).unwrap());
    let key = signer.signing_key_id().to_owned();
    let supervisor = LaunchSupervisor::new(
        connect_control_broker(&config, Duration::from_secs(5)).unwrap(),
        signer,
        Arc::new(platform),
        Arc::new(Registry::open_trusted(&registry_root).unwrap()),
        sessions,
        Duration::from_secs(5),
    );
    let (tx, rx) = mpsc::channel();
    let now = u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    supervisor
        .launch(
            request(),
            config.operator_uid,
            now,
            Box::new(move |result| {
                tx.send(result).unwrap();
            }),
        )
        .unwrap();
    let session = rx.recv_timeout(Duration::from_secs(20)).unwrap().unwrap();
    let agent_pid = marker(&lines, "BROKER_RUNNING ");
    let agent_pid = agent_pid.split_whitespace().nth(1).unwrap();
    let cgroup = fs::read_to_string(format!("/proc/{agent_pid}/cgroup")).unwrap();
    let cgroup = cgroup
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .unwrap();
    let events = Path::new("/sys/fs/cgroup")
        .join(cgroup.trim_start_matches('/'))
        .join("cgroup.events");
    assert!(fs::read_to_string(&events).unwrap().contains("frozen 0"));
    let original = fs::read(
        root.path()
            .join("state/receipts/sessions/session/00000000000000000001.receipt.json"),
    )
    .unwrap();
    revoke_key(&paths, &key, now + 1).unwrap();
    if lose_broker {
        broker.0.kill().unwrap();
        broker.0.wait().unwrap();
    }
    let verifier = LauncherVerifier::open(&paths, &root.path().join("state")).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(report) = verifier
            .key_revocation("session")
            .unwrap()
            .unwrap()
            .observation
        {
            assert_eq!(report.containment, KeyContainment::Frozen);
            break;
        }
        assert!(
            Instant::now() < deadline,
            "installed supervisor did not report containment"
        );
        thread::sleep(Duration::from_millis(10));
    }
    assert!(fs::read_to_string(&events).unwrap().contains("frozen 1"));
    assert_eq!(
        fs::read(
            root.path()
                .join("state/receipts/sessions/session/00000000000000000001.receipt.json")
        )
        .unwrap(),
        original
    );
    if !lose_broker {
        marker(&lines, "BROKER_REVOKED");
        assert!(broker.0.wait().unwrap().success());
    }
    assert!(session.dispose().is_err());
    assert!(!Path::new(&format!("/proc/{agent_pid}")).exists());
}

#[test]
fn privileged_installed_broker_revocation_survives_restart() {
    if std::env::var_os("LOUISELM_REQUIRE_BROKER_LAUNCH").is_none() {
        eprintln!("skipping: installed broker revocation requires the disposable launcher VM");
        return;
    }
    let _account = BrokerAccount::create();
    let root = tempfile::Builder::new()
        .prefix("louiselm-broker-revocation-")
        .tempdir_in("/var/lib")
        .unwrap();
    let (paths, config, _) = install_fixture_with_slots(root.path(), 4);
    let old = LauncherSigner::open(&paths).unwrap();
    for subject in isolation::SUBJECTS {
        let auth = authorization(root.path(), &config, subject);
        fs::write(
            root.path().join(format!("{subject}.authorization")),
            serde_json::to_vec(&auth).unwrap(),
        )
        .unwrap();
        let receipt = if subject == "healthy" {
            rotate(
                &paths,
                &SystemCommandRunner,
                &RotationRequest {
                    rotation_id: "healthy".into(),
                    expected_active_key_id: old.active_key_id().into(),
                },
                3000,
            )
            .unwrap();
            let current = LauncherSigner::open(&paths).unwrap();
            sign(&current, genesis(&current, &auth))
        } else {
            sign(&old, genesis(&old, &auth))
        };
        fs::write(
            root.path().join(format!("{subject}.receipt")),
            receipt.canonical_bytes(),
        )
        .unwrap();
    }
    isolation::inspect_process(root.path(), "seed");
    let binding = paths.state_root.join("receipt-bindings").join(format!(
        "{}.json",
        Digest::of(isolation::SUBJECTS[0].as_bytes()).hex()
    ));
    let original_binding = fs::read(&binding).unwrap();
    fs::set_permissions(&binding, fs::Permissions::from_mode(0o644)).unwrap();
    fs::write(&binding, b"malformed root binding").unwrap();
    fs::set_permissions(&binding, fs::Permissions::from_mode(0o444)).unwrap();
    isolation::inspect_process(root.path(), "binding");
    fs::set_permissions(&binding, fs::Permissions::from_mode(0o644)).unwrap();
    fs::write(&binding, original_binding).unwrap();
    fs::set_permissions(&binding, fs::Permissions::from_mode(0o444)).unwrap();
    isolation::inspect_process(root.path(), "binding");
    revoke_key(&paths, old.active_key_id(), 4000).unwrap();
    isolation::inspect_process(root.path(), "revoked");
    isolation::inspect_process(root.path(), "revoked");
    for subject in isolation::SUBJECTS {
        assert_eq!(
            fs::read(root.path().join(format!("{subject}.receipt"))).unwrap(),
            fs::read(
                root.path()
                    .join("state/receipts/sessions")
                    .join(subject)
                    .join("00000000000000000000.receipt.json")
            )
            .unwrap()
        );
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "One installed authority transaction covers old snapshots, exact bytes, retry and current-key revocation."
)]
fn privileged_installed_key_revocation_preserves_bytes_and_unaffected_authority() {
    if std::env::var_os("LOUISELM_REQUIRE_BROKER_LAUNCH").is_none() {
        eprintln!("skipping: installed key revocation requires the disposable launcher VM");
        return;
    }
    assert!(rustix::process::geteuid().is_root());
    let _account = BrokerAccount::create();
    let root = tempfile::Builder::new()
        .prefix("louiselm-revocation-")
        .tempdir_in("/var/lib")
        .unwrap();
    let (paths, config, _) = install_fixture_with_slots(root.path(), 4);
    let old = LauncherSigner::open(&paths).unwrap();
    let verifier = Arc::new(LauncherVerifier::open(&paths, &root.path().join("state")).unwrap());
    let store =
        ReceiptStore::installed(&root.path().join("receipts"), Arc::clone(&verifier)).unwrap();
    let compromised = authorization(root.path(), &config, "compromised");
    let original = sign(&old, genesis(&old, &compromised));
    append(&store, &verifier, &compromised, &original);
    rotate(
        &paths,
        &SystemCommandRunner,
        &RotationRequest {
            rotation_id: "second-key".into(),
            expected_active_key_id: old.active_key_id().into(),
        },
        3000,
    )
    .unwrap();
    let healthy = LauncherSigner::open(&paths).unwrap();
    let unaffected = authorization(root.path(), &config, "unaffected");
    let good = sign(&healthy, genesis(&healthy, &unaffected));
    append(&store, &verifier, &unaffected, &good);

    // Signing and revocation serialize on the same installation lock.
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(paths.state_root.join("private/install.lock"))
        .unwrap();
    lock.lock().unwrap();
    assert!(matches!(
        revoke_key(&paths, old.active_key_id(), 4000),
        Err(LauncherError::InstallBusy)
    ));
    lock.unlock().unwrap();
    let race = std::sync::Barrier::new(2);
    thread::scope(|scope| {
        let signing = scope.spawn(|| {
            race.wait();
            old.sign_receipt(old.active_key_id(), &original.payload.canonical_bytes())
        });
        race.wait();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match revoke_key(&paths, old.active_key_id(), 4000) {
                Ok(_) => break,
                Err(LauncherError::InstallBusy) => {
                    assert!(Instant::now() < deadline);
                    thread::sleep(Duration::from_millis(1));
                }
                Err(error) => panic!("revocation race failed: {error}"),
            }
        }
        if let Ok(signature) = signing.join().unwrap() {
            // A signing transaction that won the race still loses all trust
            // once revocation commits; its earlier timestamp gives no exemption.
            assert!(
                verifier
                    .verify(
                        old.active_key_id(),
                        &original.payload.canonical_bytes(),
                        &signature
                    )
                    .is_err()
            );
        }
    });
    assert!(
        old.sign_receipt(old.active_key_id(), &original.payload.canonical_bytes())
            .is_err()
    );
    assert!(
        verifier
            .verify(
                old.active_key_id(),
                &original.payload.canonical_bytes(),
                &original.signature
            )
            .is_err()
    );
    assert!(verifier.receipt_anchor(&compromised.session_id).is_err());
    assert_eq!(
        store.stored_bytes("compromised").unwrap(),
        vec![original.canonical_bytes()]
    );
    assert!(
        verifier
            .verify(
                healthy.active_key_id(),
                &good.payload.canonical_bytes(),
                &good.signature
            )
            .is_ok()
    );
    assert!(
        healthy
            .sign_receipt(healthy.active_key_id(), &good.payload.canonical_bytes())
            .is_ok()
    );
    let inspection = verifier.key_revocation("compromised").unwrap().unwrap();
    assert_eq!(inspection.key_id, old.active_key_id());
    assert!(
        inspection.observation.is_none(),
        "revocation is not proof of freeze"
    );
    // Storage API test only: this injected observation does not claim an actual freeze.
    old.record_containment(old.active_key_id(), "compromised", KeyContainment::Failed)
        .unwrap();
    let reopened = LauncherVerifier::open(&paths, &root.path().join("state")).unwrap();
    assert_eq!(
        reopened
            .key_revocation("compromised")
            .unwrap()
            .unwrap()
            .observation
            .unwrap()
            .containment,
        KeyContainment::Failed
    );
    assert!(reopened.key_revocation("unaffected").unwrap().is_none());
    assert!(
        reopened
            .verify(
                old.active_key_id(),
                &original.payload.canonical_bytes(),
                &original.signature
            )
            .is_err()
    );
    revoke_key(&paths, old.active_key_id(), 5000).unwrap();
    assert_eq!(
        reopened
            .key_revocation("compromised")
            .unwrap()
            .unwrap()
            .revoked_at_ms,
        4000
    );
    // Current-key revocation refuses new chains without silently rotating.
    revoke_key(&paths, healthy.active_key_id(), 6000).unwrap();
    let pending = authorization(root.path(), &config, "pending");
    assert!(
        healthy
            .sign_receipt(
                healthy.active_key_id(),
                &genesis(&healthy, &pending).canonical_bytes()
            )
            .is_err()
    );
    assert_eq!(
        crate::launcher_install::public_keyring(&paths)
            .unwrap()
            .active_key_id,
        healthy.active_key_id()
    );
}
