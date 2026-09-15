//! Real installed signer/storage checks; the daemon fixture proves actual disposal.

use super::*;
use crate::{
    launch_receipt::{ReceiptAuthority, ReceiptCause, StartEvidence},
    launcher_install::{cleanup_key, public_keyring},
};

fn advance(
    signer: &LauncherSigner,
    auth: &LaunchAuthorization,
    previous: &SignedReceipt,
    state: SessionState,
) -> SignedReceipt {
    let request_id = format!("lifecycle-{}", previous.payload.sequence + 1);
    let authorization = Authorization {
        authorization_id: request_id.clone(),
        request_id: request_id.clone(),
        request_digest: Digest::of(request_id.as_bytes()).to_string(),
    };
    let outcome = match state {
        SessionState::Running if previous.payload.sequence == 0 => ReceiptOutcome::Start {
            authority: ReceiptAuthority::Cause {
                cause: ReceiptCause::LaunchAcknowledged,
            },
            evidence: StartEvidence {
                agent_pid: 123,
                assigned_uid: auth.assigned_uid,
                assigned_gid: auth.assigned_gid,
                tool_isolation_digest: Digest::of(b"isolation").to_string(),
            },
        },
        SessionState::Running => ReceiptOutcome::Resume { authorization },
        SessionState::Parked => ReceiptOutcome::Park {
            authority: ReceiptAuthority::Authorized(authorization),
        },
        SessionState::Terminal => ReceiptOutcome::Disposal {
            authority: ReceiptAuthority::Authorized(authorization),
        },
        SessionState::Starting => panic!("genesis uses the admission helper"),
    };
    sign(
        signer,
        ReceiptPayload {
            sequence: previous.payload.sequence + 1,
            request_id,
            previous_receipt_digest: Some(previous.digest().to_string()),
            resulting_state: state,
            outcome,
            ..previous.payload.clone()
        },
    )
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "One installed transaction follows two key references through Park/resume, completion races, removal and upgrade."
)]
fn privileged_installed_key_cleanup_preserves_live_references_and_history() {
    if std::env::var_os("LOUISELM_REQUIRE_BROKER_LAUNCH").is_none() {
        eprintln!("skipping: installed key cleanup requires the disposable launcher VM");
        return;
    }
    assert!(rustix::process::geteuid().is_root());
    let _account = BrokerAccount::create();
    let root = tempfile::Builder::new()
        .prefix("louiselm-key-cleanup-")
        .tempdir_in("/var/lib")
        .unwrap();
    let (paths, config, _) = install_fixture_with_slots(root.path(), 2);
    let signer = LauncherSigner::open(&paths).unwrap();
    let key_id = signer.active_key_id();
    let private = paths
        .state_root
        .join("private/keys")
        .join(Digest::parse(key_id).unwrap().directory_name())
        .join("key");
    let verifier = Arc::new(LauncherVerifier::open(&paths, &root.path().join("state")).unwrap());
    let store =
        ReceiptStore::installed(&root.path().join("receipts"), Arc::clone(&verifier)).unwrap();
    let first = authorization(root.path(), &config, "first");
    let second = authorization(root.path(), &config, "second");
    let mut histories = Vec::new();
    for auth in [&first, &second] {
        let launch = sign(&signer, genesis(&signer, auth));
        append(&store, &verifier, auth, &launch);
        let running = advance(&signer, auth, &launch, SessionState::Running);
        append(&store, &verifier, auth, &running);
        histories.push(vec![launch, running]);
    }
    let parked = advance(
        &signer,
        &second,
        histories[1].last().unwrap(),
        SessionState::Parked,
    );
    append(&store, &verifier, &second, &parked);
    histories[1].push(parked.clone());
    assert!(
        signer
            .complete_session(&parked.payload, Instant::now() + Duration::from_secs(5))
            .is_err()
    );
    rotate(
        &paths,
        &SystemCommandRunner,
        &RotationRequest {
            rotation_id: "parked-session".into(),
            expected_active_key_id: key_id.into(),
        },
        4000,
    )
    .unwrap();
    assert!(private.is_file());
    assert!(cleanup_key(&paths, key_id).is_err());
    let terminal = advance(
        &signer,
        &first,
        histories[0].last().unwrap(),
        SessionState::Terminal,
    );
    append(&store, &verifier, &first, &terminal);
    histories[0].push(terminal.clone());
    signer
        .complete_session(&terminal.payload, Instant::now() + Duration::from_secs(5))
        .unwrap();
    assert!(
        private.is_file(),
        "the Parked sibling retains signing authority"
    );
    assert!(
        signer
            .sign_receipt(key_id, &histories[0][0].payload.canonical_bytes())
            .is_err(),
        "completed Session cannot sign again even while the key remains"
    );
    let resumed = advance(&signer, &second, &parked, SessionState::Running);
    append(&store, &verifier, &second, &resumed);
    histories[1].push(resumed.clone());
    let terminal = advance(&signer, &second, &resumed, SessionState::Terminal);
    append(&store, &verifier, &second, &terminal);
    histories[1].push(terminal.clone());
    assert!(
        private.is_file(),
        "a signed terminal receipt is not supervisor completion"
    );
    let binding = paths.state_root.join("receipt-bindings").join(format!(
        "{}.json",
        Digest::of(second.session_id.as_bytes()).hex()
    ));
    let saved = root.path().join("saved-binding");
    fs::rename(&binding, &saved).unwrap();
    assert!(
        signer
            .complete_session(&terminal.payload, Instant::now() + Duration::from_secs(5))
            .is_err()
    );
    assert!(cleanup_key(&paths, key_id).is_err());
    assert!(private.is_file());
    fs::rename(saved, binding).unwrap();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(paths.state_root.join("private/install.lock"))
        .unwrap();
    lock.lock().unwrap();
    assert!(
        signer
            .complete_session(&terminal.payload, Instant::now())
            .is_err()
    );
    assert!(matches!(
        cleanup_key(&paths, key_id),
        Err(LauncherError::InstallBusy)
    ));
    assert!(private.is_file());
    lock.unlock().unwrap();
    let race = std::sync::Barrier::new(2);
    thread::scope(|scope| {
        let signing = scope.spawn(|| {
            race.wait();
            signer.sign_receipt(key_id, &terminal.payload.canonical_bytes())
        });
        race.wait();
        signer
            .complete_session(&terminal.payload, Instant::now() + Duration::from_secs(5))
            .unwrap();
        if let Ok(signature) = signing.join().unwrap() {
            verifier
                .verify(key_id, &terminal.payload.canonical_bytes(), &signature)
                .unwrap();
        }
    });
    assert!(!private.exists());
    assert!(
        signer
            .sign_receipt(key_id, &terminal.payload.canonical_bytes())
            .is_err()
    );
    signer
        .complete_session(&terminal.payload, Instant::now() + Duration::from_secs(5))
        .unwrap();
    cleanup_key(&paths, key_id).unwrap();
    LauncherSigner::open(&paths).unwrap();
    upgrade(&paths, &config);
    LauncherSigner::open(&paths).unwrap();
    let reopened = Arc::new(LauncherVerifier::open(&paths, &root.path().join("state")).unwrap());
    let store =
        ReceiptStore::installed(&root.path().join("receipts"), Arc::clone(&reopened)).unwrap();
    for (auth, history) in [&first, &second].into_iter().zip(histories) {
        let original: Vec<_> = history.iter().map(SignedReceipt::canonical_bytes).collect();
        assert_eq!(store.stored_bytes(&auth.session_id).unwrap(), original);
        for receipt in history {
            reopened
                .verify(
                    key_id,
                    &receipt.payload.canonical_bytes(),
                    &receipt.signature,
                )
                .unwrap();
        }
    }
    assert!(public_keyring(&paths).unwrap().key(key_id).is_some());
}
