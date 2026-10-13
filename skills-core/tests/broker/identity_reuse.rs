//! A consumed authorization is history, not permanent live identity occupancy.
use super::*;

fn broker(root: &Path, socket: &str) -> BrokerService {
    broker_with_slots(root, socket, 1)
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
