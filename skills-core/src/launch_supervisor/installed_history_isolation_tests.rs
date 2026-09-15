//! Real installed authority and dedicated broker identity isolate damaged history.

use super::*;
use crate::broker::BrokerError;
use crate::launch_protocol::{ErrorCode, RecoveryReadiness};

const INSPECTOR: &str = "launch_supervisor::system::installed_tests::receipt_history::isolation::installed_history_inspector";
const SUBJECTS: [&str; 4] = ["bad-signature", "unknown-identity", "unreadable", "healthy"];

#[test]
fn installed_history_inspector() {
    let Some(root) = std::env::var_os("LOUISELM_HISTORY_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let phase = std::env::var("LOUISELM_HISTORY_PHASE").unwrap();
    let opened = InstalledBroker::bind(&paths(&root), &root.join("state"));
    if phase == "shared" {
        assert!(matches!(opened, Err(BrokerError::InstallationAuthority(_))));
        return;
    }
    let broker = opened.unwrap();
    if phase == "seed" {
        let verifier =
            Arc::new(LauncherVerifier::open(&paths(&root), &root.join("state")).unwrap());
        let authorizations = AuthorizationStore::open(
            &root.join("state/authorizations"),
            verifier.config().pool.clone(),
        )
        .unwrap();
        let receipts =
            ReceiptStore::installed(&root.join("state/receipts"), Arc::clone(&verifier)).unwrap();
        for subject in SUBJECTS {
            let auth: LaunchAuthorization = serde_json::from_slice(
                &fs::read(root.join(format!("{subject}.authorization"))).unwrap(),
            )
            .unwrap();
            let request = LaunchRequest {
                session_id: subject.into(),
                request_id: format!("request-{subject}"),
                authorization_id: format!("authorization-{subject}"),
                ..request()
            };
            authorizations
                .authorize(
                    &GrantRequest {
                        request: request.clone(),
                        controller_uid: verifier.config().operator_uid,
                        expires_at_ms: 30000,
                        broker_loss_grace_ms: 5000,
                        commands: None,
                        require_cold_recovery: false,
                    },
                    1000,
                )
                .unwrap();
            assert_eq!(
                authorizations.consume_for_launcher(&request, 2000).unwrap(),
                auth
            );
            let receipt = SignedReceipt::parse_canonical(
                &fs::read(root.join(format!("{subject}.receipt"))).unwrap(),
            )
            .unwrap();
            append(&receipts, &verifier, &auth, &receipt);
            assert!(broker.inspect(subject).unwrap().is_some());
        }
    } else {
        for subject in &SUBJECTS[..3] {
            let error = broker.inspect(subject).unwrap_err();
            assert!(
                matches!(error, BrokerError::Policy(ref error) if error.code == ErrorCode::ReceiptChainInvalid),
                "{subject}: {error:?}"
            );
            assert_eq!(
                broker.recovery_readiness(subject).unwrap(),
                RecoveryReadiness::Quarantined {}
            );
        }
        assert_eq!(
            broker.inspect("healthy").unwrap().unwrap().state,
            SessionState::Starting
        );
    }
}

fn inspect_process(root: &Path, phase: &str) {
    let socket = paths(root).broker_socket;
    if socket.exists() {
        fs::remove_file(socket).unwrap();
    }
    assert!(
        Command::new("/usr/bin/setpriv")
            .args([
                "--reuid",
                &BROKER_UID.to_string(),
                "--regid",
                &BROKER_UID.to_string(),
                "--clear-groups"
            ])
            .arg(std::env::current_exe().unwrap())
            .args([INSPECTOR, "--exact", "--nocapture"])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LOUISELM_HISTORY_ROOT", root)
            .env("LOUISELM_HISTORY_PHASE", phase)
            .status()
            .unwrap()
            .success()
    );
}

#[test]
fn privileged_installed_history_failure_is_session_local() {
    if std::env::var_os("LOUISELM_REQUIRE_BROKER_LAUNCH").is_none() {
        eprintln!("skipping: installed history isolation requires the disposable launcher VM");
        return;
    }
    assert!(rustix::process::geteuid().is_root());
    let _account = BrokerAccount::create();
    let root = tempfile::Builder::new()
        .prefix("louiselm-history-isolation-")
        .tempdir_in("/var/lib")
        .unwrap();
    let (paths, config, _) = install_fixture_with_slots(root.path(), 4);
    let signer = LauncherSigner::open(&paths).unwrap();
    for subject in SUBJECTS {
        let auth = authorization(root.path(), &config, subject);
        fs::write(
            root.path().join(format!("{subject}.authorization")),
            serde_json::to_vec(&auth).unwrap(),
        )
        .unwrap();
        let receipt = sign(&signer, genesis(&signer, &auth));
        fs::write(
            root.path().join(format!("{subject}.receipt")),
            receipt.canonical_bytes(),
        )
        .unwrap();
    }
    inspect_process(root.path(), "seed");
    let receipt_path = |subject: &str| {
        root.path()
            .join("state/receipts/sessions")
            .join(subject)
            .join("00000000000000000000.receipt.json")
    };
    let original = fs::read(receipt_path("bad-signature")).unwrap();
    let mut damaged = SignedReceipt::parse_canonical(&original).unwrap();
    damaged.signature = "invalid-private-payload".into();
    fs::write(receipt_path("bad-signature"), damaged.canonical_bytes()).unwrap();
    let unknown = paths
        .state_root
        .join("receipt-bindings")
        .join(format!("{}.json", Digest::of(b"unknown-identity").hex()));
    fs::rename(&unknown, root.path().join("saved-binding")).unwrap();
    fs::set_permissions(receipt_path("unreadable"), fs::Permissions::from_mode(0o0)).unwrap();
    inspect_process(root.path(), "refuse");
    assert_eq!(
        fs::read(receipt_path("bad-signature")).unwrap(),
        damaged.canonical_bytes()
    );
    // Even restoration of exact formerly valid bytes does not clear quarantine.
    fs::write(receipt_path("bad-signature"), original).unwrap();
    fs::rename(root.path().join("saved-binding"), unknown).unwrap();
    fs::set_permissions(
        receipt_path("unreadable"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    inspect_process(root.path(), "refuse");
    let keyring = paths.state_root.join("keyring.json");
    fs::set_permissions(&keyring, fs::Permissions::from_mode(0o644)).unwrap();
    fs::write(&keyring, b"damaged shared registry").unwrap();
    fs::set_permissions(&keyring, fs::Permissions::from_mode(0o444)).unwrap();
    inspect_process(root.path(), "shared");
}
