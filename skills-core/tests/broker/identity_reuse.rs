//! A consumed authorization is history, not permanent live identity occupancy.
use super::*;

fn broker(root: &Path, socket: &str) -> BrokerService {
    broker_with_slots(root, socket, 1)
}

fn recovery_proof(
    service: &BrokerService,
    authorization: &LaunchAuthorization,
) -> louiselm_skills::launch_recovery::Proof {
    use louiselm_skills::launch_recovery::{Payload, Proof, SCHEMA, SIGNED_SCHEMA};
    let snapshot = service
        .consumed_recovery_snapshot(&authorization.authorization_id, verify_fixture_signature)
        .unwrap();
    assert!(snapshot.recovered.is_none());
    let payload = Payload {
        schema: SCHEMA.into(),
        binding: snapshot.binding,
        arm_digest: Digest::of(b"actual-root-arm-fixture").to_string(),
        receipt_binding_digest: None,
        armed_boot_id: "00000000-0000-0000-0000-000000000001".into(),
        recovered_boot_id: "00000000-0000-0000-0000-000000000002".into(),
        retained_storage: louiselm_skills::launch_recovery::RetainedStorage::Sealed,
        release_id: trusted_release().release_id,
        signing_key_id: trusted_release().signing_key_id,
    };
    let signature = Digest::of(&payload.canonical_bytes()).to_string();
    Proof {
        schema: SIGNED_SCHEMA.into(),
        payload,
        signature,
    }
}

#[test]
fn missing_receipt_recovery_reuses_the_slot_without_replaying_consumption_or_refilling_run() {
    let root = TempDir::new().unwrap();
    let service = broker(root.path(), "first.sock");
    let authorization = consume(&service, "refused-before-admission");
    let consumption = fs::read_dir(root.path().join("authorizations/consumed"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let original = fs::read(&consumption).unwrap();
    let proof = recovery_proof(&service, &authorization);
    assert!(matches!(
        service.authorize(&grant(&request("later")), 3000, verify_fixture_signature),
        Err(BrokerError::IdentityExhausted(_))
    ));
    let ack = service
        .accept_consumed_recovery(&proof, verify_fixture_signature)
        .unwrap();
    assert_eq!(ack.proof_digest, proof.digest());
    assert!(matches!(
        service.receipts().append(
            &authorization,
            &launch_receipt(&authorization).canonical_bytes(),
            None,
            verify_fixture_signature,
        ),
        Err(BrokerError::ReceiptUnauthorized)
    ));
    drop(service);
    let service = broker(root.path(), "second.sock");
    assert_eq!(
        service
            .accept_consumed_recovery(&proof, verify_fixture_signature)
            .unwrap(),
        ack
    );
    assert_eq!(
        service
            .consumed_recovery_snapshot(&authorization.authorization_id, verify_fixture_signature)
            .unwrap()
            .recovered,
        Some(proof.clone())
    );
    assert_eq!(
        service
            .receipts()
            .stored_bytes(&authorization.session_id)
            .unwrap(),
        [] as [Vec<u8>; 0]
    );
    let inspection = service.inspect(&authorization.session_id).unwrap().unwrap();
    assert_eq!(inspection.state, SessionState::Terminal);
    assert!(inspection.broker_head.is_none());
    assert!(inspection.launch_evidence.is_none());
    assert_eq!(fs::read(consumption).unwrap(), original);
    assert!(matches!(
        service.authorizations().consume(
            &request("refused-before-admission"),
            CONTROLLER_UID,
            3000
        ),
        Err(BrokerError::UnknownAuthorization)
    ));
    consume(&service, "new-session");
    assert_eq!(
        service
            .authorizations()
            .session_count_for_run("run-1")
            .unwrap(),
        2
    );
    // Historical retry must not clear or reassign the new live occupant.
    assert_eq!(
        service
            .accept_consumed_recovery(&proof, verify_fixture_signature)
            .unwrap(),
        ack
    );
    assert!(matches!(
        service.authorize(&grant(&request("third")), 3000, verify_fixture_signature),
        Err(BrokerError::IdentityExhausted(_))
    ));
}

#[test]
fn recovery_rejects_forged_same_boot_foreign_or_conflicting_proofs() {
    let root = TempDir::new().unwrap();
    let service = broker(root.path(), "broker.sock");
    let authorization = consume(&service, "held");
    let proof = recovery_proof(&service, &authorization);
    let mut forged = proof.clone();
    forged.signature = Digest::of(b"forged").to_string();
    assert!(matches!(
        service.accept_consumed_recovery(&forged, verify_fixture_signature),
        Err(BrokerError::Recovery(
            louiselm_skills::launch_recovery::ProofError::InvalidSignature
        ))
    ));
    for mutation in 0..4 {
        use louiselm_skills::launch_recovery::{Proof, ProofError};
        let mut changed = proof.payload.clone();
        match mutation {
            0 => changed.recovered_boot_id.clone_from(&changed.armed_boot_id),
            1 => changed.binding.consumption_digest = Digest::of(b"other-consumption").to_string(),
            2 => changed.binding.authorization.assigned_uid += 1,
            _ => changed.binding.authorization.run_id = "foreign-run".into(),
        }
        let signature = Digest::of(&changed.canonical_bytes()).to_string();
        let result = service.accept_consumed_recovery(
            &Proof {
                payload: changed,
                signature,
                ..proof.clone()
            },
            verify_fixture_signature,
        );
        match mutation {
            0 => assert!(matches!(
                result,
                Err(BrokerError::Recovery(ProofError::InvalidBoot))
            )),
            _ => assert!(matches!(result, Err(BrokerError::RequestMismatch))),
        }
    }
    assert_eq!(
        service
            .receipts()
            .stored_bytes(&authorization.session_id)
            .unwrap(),
        [] as [Vec<u8>; 0]
    );
    let launch = launch_receipt(&authorization);
    service
        .receipts()
        .append(
            &authorization,
            &launch.canonical_bytes(),
            None,
            verify_fixture_signature,
        )
        .unwrap();
    assert!(matches!(
        service
            .consumed_recovery_snapshot(&authorization.authorization_id, verify_fixture_signature),
        Err(BrokerError::InvalidGrant)
    ));
    assert!(matches!(
        service.accept_consumed_recovery(&proof, verify_fixture_signature),
        Err(BrokerError::InvalidGrant)
    ));
}

#[test]
fn recovery_proof_is_canonical_closed_and_separate_from_launcher_receipts() {
    use louiselm_skills::launch_recovery::{Proof, ProofError};
    let root = TempDir::new().unwrap();
    let service = broker(root.path(), "broker.sock");
    let authorization = consume(&service, "held");
    let proof = recovery_proof(&service, &authorization);
    assert_eq!(Proof::parse(&proof.canonical_bytes()).unwrap(), proof);
    assert_eq!(
        SignedReceipt::parse_canonical(&proof.canonical_bytes()),
        Err(
            louiselm_skills::launch_receipt::ReceiptError::UnsupportedSchema(
                louiselm_skills::launch_recovery::SIGNED_SCHEMA.into()
            )
        )
    );
    let mut noncanonical = proof.canonical_bytes();
    noncanonical.push(b' ');
    assert_eq!(Proof::parse(&noncanonical), Err(ProofError::NonCanonical));
    let mut value = serde_json::to_value(&proof).unwrap();
    value["payload"]["sequence"] = serde_json::json!(0);
    assert_eq!(
        Proof::parse(&serde_json::to_vec(&value).unwrap()),
        Err(ProofError::Malformed)
    );
}

#[test]
fn corrupt_recovery_holds_its_slot_after_restore_without_blocking_another_identity() {
    let root = TempDir::new().unwrap();
    let service = broker_with_slots(root.path(), "broker.sock", 2);
    let authorization = consume(&service, "held");
    let proof = recovery_proof(&service, &authorization);
    service
        .accept_consumed_recovery(&proof, verify_fixture_signature)
        .unwrap();
    let path = fs::read_dir(root.path().join("receipts/consumed-recoveries"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let mut corrupt = proof.clone();
    corrupt.signature = Digest::of(b"forged").to_string();
    fs::write(&path, corrupt.canonical_bytes()).unwrap();
    assert_eq!(
        service
            .authorize(&grant(&request("next")), 3000, verify_fixture_signature)
            .unwrap()
            .identity
            .slot,
        1
    );
    fs::write(path, proof.canonical_bytes()).unwrap();
    assert!(matches!(
        service.authorize(&grant(&request("third")), 3000, verify_fixture_signature),
        Err(BrokerError::IdentityExhausted(_))
    ));
}

fn broker_with_slots(root: &Path, socket: &str, slots: u32) -> BrokerService {
    BrokerService::bind(
        &root.join(socket),
        AuthorizationStore::open(&root.join("authorizations"), pool(slots)).unwrap(),
        ReceiptStore::open(&root.join("receipts"), trusted_release()).unwrap(),
        AuditLog::open(&root.join("audit")).unwrap(),
        local_pin(),
    )
    .unwrap()
}

#[test]
fn corrupt_history_holds_only_its_slot_even_after_original_bytes_are_restored() {
    let root = TempDir::new().unwrap();
    let service = broker_with_slots(root.path(), "broker.sock", 2);
    let authorization = consume(&service, "first");
    let bytes = dispose(&service, &authorization);
    let terminal_path = root
        .path()
        .join("receipts/sessions/first/00000000000000000002.receipt.json");
    let mut terminal = SignedReceipt::parse_canonical(bytes.last().unwrap()).unwrap();
    terminal.signature = Digest::of(b"forged").to_string();
    fs::write(&terminal_path, terminal.canonical_bytes()).unwrap();
    let next = service
        .authorize(&grant(&request("next")), 3000, verify_fixture_signature)
        .unwrap();
    assert_eq!(next.identity.slot, 1, "unrelated capacity remains usable");
    fs::write(&terminal_path, bytes.last().unwrap()).unwrap();
    assert!(matches!(
        service.authorize(&grant(&request("third")), 3000, verify_fixture_signature),
        Err(BrokerError::IdentityExhausted(_))
    ));
}

fn consume(service: &BrokerService, name: &str) -> LaunchAuthorization {
    let request = request(name);
    let pending = service
        .authorize(&grant(&request), 1000, verify_fixture_signature)
        .unwrap();
    assert_eq!(pending.identity.slot, 0);
    service
        .authorizations()
        .consume(&request, CONTROLLER_UID, 2000)
        .unwrap()
}

fn dispose(service: &BrokerService, authorization: &LaunchAuthorization) -> Vec<Vec<u8>> {
    let launch = launch_receipt(authorization);
    let start = start_receipt(authorization, &launch);
    let terminal = signed(payload(
        authorization,
        "disposed",
        2,
        Some(start.digest().to_string()),
        ReceiptOutcome::Disposal {
            authority: ReceiptAuthority::Cause {
                cause: ReceiptCause::ControllerLost,
            },
        },
        SessionState::Terminal,
    ));
    let bytes: Vec<_> = [launch, start, terminal]
        .iter()
        .map(SignedReceipt::canonical_bytes)
        .collect();
    for receipt in &bytes {
        service
            .receipts()
            .append(authorization, receipt, None, verify_fixture_signature)
            .unwrap();
    }
    bytes
}

#[test]
fn disposal_reuses_one_slot_across_restart_without_erasing_consumption_or_run_count() {
    let root = TempDir::new().unwrap();
    let service = broker(root.path(), "first.sock");
    let first = consume(&service, "first");
    let history = dispose(&service, &first);
    drop(service);
    let service = broker(root.path(), "second.sock");
    for name in ["second", "third"] {
        let authorization = consume(&service, name);
        dispose(&service, &authorization);
    }
    assert_eq!(service.receipts().stored_bytes("first").unwrap(), history);
    assert_eq!(
        service
            .authorizations()
            .session_count_for_run("run-1")
            .unwrap(),
        3
    );
    assert!(
        service
            .authorizations()
            .consumed_for_session("first")
            .unwrap()
            .is_some()
    );
    assert!(matches!(
        service.authorize(&grant(&request("first")), 3000, verify_fixture_signature),
        Err(BrokerError::DuplicateAuthorization)
    ));
    assert!(matches!(
        service
            .authorizations()
            .consume(&request("first"), CONTROLLER_UID, 3000),
        Err(BrokerError::UnknownAuthorization)
    ));
}

#[test]
fn missing_receipts_and_live_or_parked_sessions_keep_their_slot_after_expiry() {
    for state in [
        None,
        Some(SessionState::Running),
        Some(SessionState::Parked),
    ] {
        let root = TempDir::new().unwrap();
        let service = broker(root.path(), "broker.sock");
        let authorization = consume(&service, "held");
        if let Some(state) = state {
            let launch = launch_receipt(&authorization);
            let start = start_receipt(&authorization, &launch);
            for receipt in [&launch, &start] {
                service
                    .receipts()
                    .append(
                        &authorization,
                        &receipt.canonical_bytes(),
                        None,
                        verify_fixture_signature,
                    )
                    .unwrap();
            }
            if state == SessionState::Parked {
                let park = signed(payload(
                    &authorization,
                    "parked",
                    2,
                    Some(start.digest().to_string()),
                    ReceiptOutcome::Park {
                        authority: ReceiptAuthority::Cause {
                            cause: ReceiptCause::BrokerLost,
                        },
                    },
                    SessionState::Parked,
                ));
                service
                    .receipts()
                    .append(
                        &authorization,
                        &park.canonical_bytes(),
                        None,
                        verify_fixture_signature,
                    )
                    .unwrap();
            }
        }
        let mut later = grant(&request("later"));
        later.expires_at_ms = 50_000;
        assert!(
            matches!(
                service.authorize(&later, 40_000, verify_fixture_signature),
                Err(BrokerError::IdentityExhausted(_))
            ),
            "state={state:?}"
        );
    }
}

#[test]
fn pending_grant_still_reserves_a_recycled_slot() {
    let root = TempDir::new().unwrap();
    let service = broker(root.path(), "broker.sock");
    dispose(&service, &consume(&service, "first"));
    service
        .authorize(&grant(&request("pending")), 3000, verify_fixture_signature)
        .unwrap();
    assert!(matches!(
        service.authorize(
            &grant(&request("competitor")),
            3000,
            verify_fixture_signature
        ),
        Err(BrokerError::IdentityExhausted(_))
    ));
}

#[test]
fn invalid_terminal_signature_cannot_release_a_slot() {
    let root = TempDir::new().unwrap();
    let service = broker(root.path(), "broker.sock");
    let authorization = consume(&service, "first");
    let bytes = dispose(&service, &authorization);
    let mut terminal = SignedReceipt::parse_canonical(bytes.last().unwrap()).unwrap();
    terminal.signature = Digest::of(b"forged").to_string();
    fs::write(
        root.path()
            .join("receipts/sessions/first/00000000000000000002.receipt.json"),
        terminal.canonical_bytes(),
    )
    .unwrap();
    assert!(matches!(
        service.authorize(&grant(&request("next")), 3000, verify_fixture_signature),
        Err(BrokerError::IdentityExhausted(_))
    ));
    assert_eq!(
        service
            .authorizations()
            .session_count_for_run("run-1")
            .unwrap(),
        1
    );
}

#[test]
fn terminal_receipt_for_another_run_cannot_release_a_slot() {
    let root = TempDir::new().unwrap();
    let service = broker(root.path(), "broker.sock");
    let authorization = consume(&service, "first");
    let bytes = dispose(&service, &authorization);
    let mut terminal = SignedReceipt::parse_canonical(bytes.last().unwrap()).unwrap();
    terminal.payload.run_id = "other-run".into();
    let terminal = signed(terminal.payload);
    fs::write(
        root.path()
            .join("receipts/sessions/first/00000000000000000002.receipt.json"),
        terminal.canonical_bytes(),
    )
    .unwrap();
    assert!(matches!(
        service.authorize(&grant(&request("next")), 3000, verify_fixture_signature),
        Err(BrokerError::IdentityExhausted(_))
    ));
}

#[test]
fn concurrent_authorizations_cannot_share_a_recycled_slot() {
    let root = TempDir::new().unwrap();
    let service = Arc::new(broker(root.path(), "broker.sock"));
    dispose(&service, &consume(&service, "first"));
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let threads: Vec<_> = ["one", "two"]
        .into_iter()
        .map(|name| {
            let service = Arc::clone(&service);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                match service.authorize(&grant(&request(name)), 3000, verify_fixture_signature) {
                    Ok(pending) => {
                        assert_eq!(pending.identity.slot, 0);
                        true
                    }
                    Err(BrokerError::IdentityExhausted(_)) => false,
                    other => panic!("unexpected allocation result: {other:?}"),
                }
            })
        })
        .collect();
    assert_eq!(
        threads
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .filter(|won| *won)
            .count(),
        1
    );
}
