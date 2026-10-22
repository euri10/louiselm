//! Recovery preserves a real nonterminal chain rather than inventing Disposal.
use super::*;

pub(super) fn history(
    service: &BrokerService,
    authorization: &LaunchAuthorization,
    count: usize,
) -> Vec<Vec<u8>> {
    let launch = launch_receipt(authorization);
    let start = start_receipt(authorization, &launch);
    let park = signed(payload(
        authorization,
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
    let bytes: Vec<_> = [launch, start, park]
        .iter()
        .take(count)
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
fn receipted_recovery_preserves_history_and_reuses_only_the_original_reservation() {
    let root = TempDir::new().unwrap();
    let service = broker(root.path(), "first.sock");
    let authorization = consume(&service, "parked");
    let original = history(&service, &authorization, 3);
    let proof = recovery_proof(&service, &authorization);
    assert_eq!(
        proof.payload.binding.receipt_head,
        service.receipts().head("parked").unwrap()
    );
    assert!(matches!(
        service.authorize(&grant(&request("next")), 3000, verify_fixture_signature),
        Err(BrokerError::IdentityExhausted(_))
    ));
    let ack = service
        .accept_consumed_recovery(&proof, verify_fixture_signature)
        .unwrap();
    drop(service);
    let service = broker(root.path(), "second.sock");
    let successor = consume(&service, "successor");
    assert_eq!(successor.identity_slot, authorization.identity_slot);
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
        Some(proof)
    );
    assert_eq!(service.receipts().stored_bytes("parked").unwrap(), original);
    assert_eq!(
        service
            .authorizations()
            .session_count_for_run("run-1")
            .unwrap(),
        2
    );
    assert!(matches!(
        service.authorize(&grant(&request("third")), 3000, verify_fixture_signature),
        Err(BrokerError::IdentityExhausted(_))
    ));
    assert!(matches!(
        service
            .receipts()
            .append(&authorization, &original[0], None, verify_fixture_signature,),
        Err(BrokerError::ReceiptUnauthorized)
    ));
    assert!(matches!(
        service
            .authorizations()
            .consume(&request("parked"), CONTROLLER_UID, 3000),
        Err(BrokerError::UnknownAuthorization)
    ));
}

#[test]
fn recovery_binds_starting_and_running_heads_too() {
    for count in [1, 2] {
        let root = TempDir::new().unwrap();
        let service = broker(root.path(), "broker.sock");
        let authorization = consume(&service, "held");
        let original = history(&service, &authorization, count);
        let proof = recovery_proof(&service, &authorization);
        assert_eq!(
            proof.payload.binding.receipt_head,
            service.receipts().head("held").unwrap()
        );
        service
            .accept_consumed_recovery(&proof, verify_fixture_signature)
            .unwrap();
        consume(&service, "successor");
        assert_eq!(service.receipts().stored_bytes("held").unwrap(), original);
    }
}

#[test]
fn recovery_rejects_a_missing_foreign_or_advanced_head_at_commit() {
    let root = TempDir::new().unwrap();
    let service = broker(root.path(), "broker.sock");
    let authorization = consume(&service, "held");
    let original = history(&service, &authorization, 1);
    let proof = recovery_proof(&service, &authorization);
    for mutation in 0..3 {
        let mut changed = proof.clone();
        match mutation {
            0 => changed.payload.binding.receipt_head = None,
            1 => {
                changed
                    .payload
                    .binding
                    .receipt_head
                    .as_mut()
                    .unwrap()
                    .sequence += 1;
            }
            _ => {
                changed
                    .payload
                    .binding
                    .receipt_head
                    .as_mut()
                    .unwrap()
                    .digest = Digest::of(b"foreign-head").to_string();
            }
        }
        changed.signature = Digest::of(&changed.payload.canonical_bytes()).to_string();
        assert!(matches!(
            service.accept_consumed_recovery(&changed, verify_fixture_signature),
            Err(BrokerError::RequestMismatch)
        ));
    }
    let start = start_receipt(
        &authorization,
        &SignedReceipt::parse_canonical(&original[0]).unwrap(),
    );
    service
        .receipts()
        .append(
            &authorization,
            &start.canonical_bytes(),
            None,
            verify_fixture_signature,
        )
        .unwrap();
    assert!(matches!(
        service.accept_consumed_recovery(&proof, verify_fixture_signature),
        Err(BrokerError::RequestMismatch)
    ));
    assert!(matches!(
        service.authorize(
            &grant(&request("successor")),
            3000,
            verify_fixture_signature
        ),
        Err(BrokerError::IdentityExhausted(_))
    ));
    assert!(!root.path().join("receipts/consumed-recoveries").exists());
}

#[test]
fn recovery_refuses_terminal_or_invalid_history() {
    let root = TempDir::new().unwrap();
    let service = broker(root.path(), "broker.sock");
    let authorization = consume(&service, "held");
    let proof = recovery_proof(&service, &authorization);
    dispose(&service, &authorization);
    assert!(matches!(
        service
            .consumed_recovery_snapshot(&authorization.authorization_id, verify_fixture_signature),
        Err(BrokerError::InvalidGrant)
    ));
    assert!(matches!(
        service.accept_consumed_recovery(&proof, verify_fixture_signature),
        Err(BrokerError::InvalidGrant)
    ));
    let root = TempDir::new().unwrap();
    let service = broker(root.path(), "broker.sock");
    let authorization = consume(&service, "held");
    history(&service, &authorization, 3);
    let proof = recovery_proof(&service, &authorization);
    // Withdraw receipt signature trust after arming, not the recovery signature.
    let reject_history = |key: &str, payload: &[u8], signature: &str| {
        let value: serde_json::Value = serde_json::from_slice(payload).unwrap();
        value["schema"] != louiselm_skills::launch_receipt::RECEIPT_SCHEMA
            && verify_fixture_signature(key, payload, signature)
    };
    assert!(matches!(
        service.consumed_recovery_snapshot(&authorization.authorization_id, reject_history),
        Err(BrokerError::Storage(error)) if error.kind() == std::io::ErrorKind::InvalidData
    ));
    assert!(matches!(
        service.accept_consumed_recovery(&proof, reject_history),
        Err(BrokerError::Storage(error)) if error.kind() == std::io::ErrorKind::InvalidData
    ));
    assert!(!root.path().join("receipts/consumed-recoveries").exists());
}

#[test]
fn recovery_commit_serializes_with_receipt_append() {
    use std::sync::{Arc, mpsc};
    use std::time::Duration;
    let root = TempDir::new().unwrap();
    let service = Arc::new(broker(root.path(), "broker.sock"));
    let authorization = consume(&service, "held");
    let original = history(&service, &authorization, 1);
    let proof = recovery_proof(&service, &authorization);
    let start = start_receipt(
        &authorization,
        &SignedReceipt::parse_canonical(&original[0]).unwrap(),
    );
    let (locked_tx, locked_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let committing = Arc::clone(&service);
    let commit = std::thread::spawn(move || {
        let mut announced = false;
        committing.accept_consumed_recovery(&proof, |key, bytes, signature| {
            if !announced && bytes == proof.payload.canonical_bytes() {
                announced = true;
                locked_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            }
            verify_fixture_signature(key, bytes, signature)
        })
    });
    locked_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let (appended_tx, appended_rx) = mpsc::channel();
    let appending = Arc::clone(&service);
    let append = std::thread::spawn(move || {
        appended_tx
            .send(appending.receipts().append(
                &authorization,
                &start.canonical_bytes(),
                None,
                verify_fixture_signature,
            ))
            .unwrap();
    });
    let premature = appended_rx.recv_timeout(Duration::from_millis(100));
    release_tx.send(()).unwrap();
    commit.join().unwrap().unwrap();
    append.join().unwrap();
    assert!(matches!(premature, Err(mpsc::RecvTimeoutError::Timeout)));
    assert!(matches!(
        appended_rx.recv().unwrap(),
        Err(BrokerError::ReceiptUnauthorized)
    ));
    assert_eq!(service.receipts().stored_bytes("held").unwrap(), original);
}

#[test]
fn changed_recovered_history_keeps_its_identity_quarantined_after_restore() {
    let root = TempDir::new().unwrap();
    let service = broker_with_slots(root.path(), "broker.sock", 2);
    let authorization = consume(&service, "held");
    let original = history(&service, &authorization, 3);
    let proof = recovery_proof(&service, &authorization);
    service
        .accept_consumed_recovery(&proof, verify_fixture_signature)
        .unwrap();
    let head_path = root
        .path()
        .join("receipts/sessions/held/00000000000000000002.receipt.json");
    let mut changed = SignedReceipt::parse_canonical(&original[2])
        .unwrap()
        .payload;
    changed.request_id = "different-valid-park".into();
    fs::write(&head_path, signed(changed).canonical_bytes()).unwrap();
    assert!(matches!(
        service
            .consumed_recovery_snapshot(&authorization.authorization_id, verify_fixture_signature),
        Err(BrokerError::RequestMismatch)
    ));
    assert_eq!(
        service
            .authorize(
                &grant(&request("unrelated")),
                3000,
                verify_fixture_signature
            )
            .unwrap()
            .identity
            .slot,
        1
    );
    fs::write(head_path, &original[2]).unwrap();
    assert!(matches!(
        service.consumed_recovery_snapshot(&authorization.authorization_id, verify_fixture_signature),
        Err(BrokerError::Policy(error)) if error.code == louiselm_skills::launch_protocol::ErrorCode::ReceiptChainInvalid
    ));
    assert!(matches!(
        service.authorize(&grant(&request("third")), 3000, verify_fixture_signature),
        Err(BrokerError::IdentityExhausted(_))
    ));
}
