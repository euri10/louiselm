//! Existing admitted chains retain their identity when current authority changes.

use super::*;

#[test]
fn history_refusal_withdraws_live_status_receipts_and_resume() {
    use louiselm_skills::broker::lifecycle::LifecycleCaller;
    for operation in ["status", "receipt", "resume"] {
        let root = TempDir::new().unwrap();
        let (service, authorization, chain) = reconnect::fixture(root.path());
        let channel = reconnect::peer(root.path(), &reconnect::checkpoint(&chain[1]));
        let mut session = service
            .serve_reconnect(3000, verify_fixture_signature)
            .unwrap();
        settle(|complete| channel.receive(complete));
        let path = root
            .path()
            .join("receipts/sessions/reconnect-session/00000000000000000000.receipt.json");
        fs::write(&path, b"damaged").unwrap();
        if operation != "status" {
            assert!(service.inspect(&authorization.session_id).is_err());
            fs::write(&path, chain[0].canonical_bytes()).unwrap();
        }
        let caller = LifecycleCaller::Operator {
            uid: CONTROLLER_UID,
        };
        let result = match operation {
            "status" => service
                .session_status(&mut session, &caller, 3000, verify_fixture_signature)
                .map(|_| ()),
            "receipt" => {
                settle(|complete| channel.send(chain[1].canonical_bytes(), complete));
                service
                    .step(&mut session, 3000, None, verify_fixture_signature)
                    .map(|_| ())
            }
            _ => {
                let mut request = lifecycle::park(&authorization);
                request.action = louiselm_skills::launch_protocol::LifecycleAction::Resume;
                request.expected_state = SessionState::Parked;
                service
                    .request_lifecycle(
                        &mut session,
                        &caller,
                        &request,
                        3000,
                        verify_fixture_signature,
                    )
                    .map(|_| ())
            }
        };
        assert!(
            matches!(result, Err(BrokerError::Policy(error)) if error.code == ErrorCode::ReceiptChainInvalid),
            "{operation}"
        );
        assert!(session.channel().is_closed(), "{operation}");
    }
}

#[test]
fn unverifiable_history_latches_refusal_and_retries_failed_reporting() {
    for kind in [
        "signature",
        "foreign",
        "gap",
        "missing",
        "directory",
        "fifo",
        "symlink",
        "reporting",
    ] {
        let root = TempDir::new().unwrap();
        let (service, authorization, chain) = reconnect::fixture(root.path());
        let directory = root.path().join("receipts/sessions/reconnect-session");
        let path = directory.join("00000000000000000000.receipt.json");
        let saved = root.path().join("saved-genesis");
        fs::rename(&path, &saved).unwrap();
        match kind {
            "signature" | "reporting" => {
                let mut changed = chain[0].clone();
                changed.signature = "invalid".into();
                fs::write(&path, changed.canonical_bytes()).unwrap();
            }
            "foreign" => {
                let mut changed = chain[0].payload.clone();
                changed.run_id = "foreign".into();
                fs::write(&path, signed(changed).canonical_bytes()).unwrap();
            }
            "missing" => {
                fs::rename(&directory, root.path().join("saved-chain")).unwrap();
            }
            "directory" => fs::create_dir(&path).unwrap(),
            "fifo" => rustix::fs::mkfifoat(rustix::fs::CWD, &path, rustix::fs::Mode::RUSR).unwrap(),
            "symlink" => std::os::unix::fs::symlink(&saved, &path).unwrap(),
            _ => {}
        }
        let entries = root.path().join("authorizations/attention-outbox/entries");
        if kind == "reporting" {
            fs::rename(&entries, root.path().join("saved-entries")).unwrap();
            fs::write(&entries, b"unavailable outbox").unwrap();
        }
        let channel = reconnect::peer(root.path(), &reconnect::checkpoint(&chain[1]));
        assert!(
            service
                .serve_reconnect(90000, verify_fixture_signature)
                .is_err(),
            "{kind}"
        );
        expect_disconnect(&channel);
        assert_eq!(
            service
                .recovery_readiness(&authorization.session_id, 3000)
                .unwrap(),
            louiselm_skills::launch_protocol::RecoveryReadiness::Quarantined {},
            "{kind}"
        );
        if kind == "reporting" {
            fs::remove_file(&entries).unwrap();
            fs::rename(root.path().join("saved-entries"), &entries).unwrap();
        }
        assert!(
            matches!(service.inspect(&authorization.session_id), Err(BrokerError::Policy(error)) if error.code == ErrorCode::ReceiptChainInvalid),
            "{kind}"
        );
        let outbox = louiselm_skills::broker::attention::Outbox::open(
            &root.path().join("authorizations/attention-outbox"),
        )
        .unwrap();
        let projection = outbox.next().unwrap().unwrap();
        let bytes = serde_json::to_vec(&projection).unwrap();
        assert!(!String::from_utf8(bytes).unwrap().contains("invalid"));
        assert_eq!(fs::read(saved).unwrap(), chain[0].canonical_bytes());
    }
}

#[test]
fn damaged_history_is_isolated_and_stays_refused_after_restart() {
    let root = TempDir::new().unwrap();
    let (service, authorization, chain) = reconnect::fixture(root.path());
    let healthy = consumed_authorization(root.path(), &request("healthy"));
    let launch = launch_receipt(&healthy);
    let start = start_receipt(&healthy, &launch);
    for receipt in [&launch, &start] {
        service
            .receipts()
            .append(
                &healthy,
                &receipt.canonical_bytes(),
                None,
                verify_fixture_signature,
            )
            .unwrap();
    }
    let damaged = root
        .path()
        .join("receipts/sessions/reconnect-session/00000000000000000000.receipt.json");
    fs::write(&damaged, b"malformed private payload").unwrap();
    let failure = service.inspect(&authorization.session_id).unwrap_err();
    assert!(
        matches!(failure, BrokerError::Policy(ref error) if error.code == ErrorCode::ReceiptChainInvalid),
        "{failure:?}"
    );
    assert_eq!(fs::read(&damaged).unwrap(), b"malformed private payload");
    assert_eq!(
        service.inspect("healthy").unwrap().unwrap().state,
        SessionState::Running
    );
    assert_eq!(
        service
            .recovery_readiness(&authorization.session_id, 3000)
            .unwrap(),
        louiselm_skills::launch_protocol::RecoveryReadiness::Quarantined {}
    );
    service.close();
    drop(service);
    fs::remove_file(root.path().join("broker.sock")).unwrap();
    // A stale copy cannot silently restore trust after a refusal was recorded.
    fs::write(&damaged, chain[0].canonical_bytes()).unwrap();
    let service = BrokerService::bind(
        &root.path().join("broker.sock"),
        AuthorizationStore::open(&root.path().join("authorizations"), pool(4)).unwrap(),
        ReceiptStore::open(&root.path().join("receipts"), trusted_release()).unwrap(),
        AuditLog::open(&root.path().join("audit")).unwrap(),
        local_pin(),
    )
    .unwrap();
    assert!(
        matches!(service.inspect(&authorization.session_id), Err(BrokerError::Policy(error)) if error.code == ErrorCode::ReceiptChainInvalid)
    );
    let channel = reconnect::peer(root.path(), &reconnect::checkpoint(&chain[1]));
    assert!(
        service
            .serve_reconnect(90000, verify_fixture_signature)
            .is_err()
    );
    expect_disconnect(&channel);
    let channel = reconnect::peer(root.path(), &reconnect::checkpoint(&start));
    let _session = service
        .serve_reconnect(90000, verify_fixture_signature)
        .unwrap();
    assert!(matches!(
        settle(|complete| channel.receive(complete)).packet,
        LauncherPacket::Response(_)
    ));
}

#[test]
fn admitted_chain_survives_rotation_and_release_upgrade() {
    for upgrade in [false, true] {
        let root = TempDir::new().unwrap();
        let authorization = consumed_authorization(root.path(), &request("original"));
        let path = root.path().join("receipts");
        let original = trusted_release();
        let receipts = ReceiptStore::open(&path, original.clone()).unwrap();
        let launch = launch_receipt(&authorization);
        receipts
            .append(
                &authorization,
                &launch.canonical_bytes(),
                None,
                verify_fixture_signature,
            )
            .unwrap();
        drop(receipts);

        let current = TrustedRelease {
            release_id: if upgrade {
                Digest::of(b"next release").to_string()
            } else {
                original.release_id
            },
            signing_key_id: Digest::of(b"next key").to_string(),
        };
        let receipts = ReceiptStore::open(&path, current).unwrap();
        let start = start_receipt(&authorization, &launch);
        receipts
            .append(
                &authorization,
                &start.canonical_bytes(),
                None,
                verify_fixture_signature,
            )
            .expect("admitted history continues under its original authority");
        assert_eq!(
            receipts.stored_bytes(&authorization.session_id).unwrap(),
            [launch.canonical_bytes(), start.canonical_bytes()]
        );

        let fresh = consumed_authorization(root.path(), &request("new-under-retired-key"));
        assert!(
            receipts
                .append(
                    &fresh,
                    &launch_receipt(&fresh).canonical_bytes(),
                    None,
                    verify_fixture_signature
                )
                .is_err(),
            "retained history does not authorize a new old-key chain"
        );
    }
}
