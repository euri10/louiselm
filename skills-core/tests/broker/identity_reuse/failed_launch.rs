//! A failed launch closes history without inventing successful preparation.
use super::*;
use louiselm_skills::launch_receipt::{AgentExecution, LaunchFailure};

fn failed(authorization: &LaunchAuthorization, prefix: &[Vec<u8>]) -> SignedReceipt {
    let previous = prefix
        .last()
        .map(|bytes| SignedReceipt::parse_canonical(bytes).unwrap());
    signed(payload(
        authorization,
        if prefix.is_empty() {
            &authorization.request_id
        } else {
            "failed-launch"
        },
        prefix.len() as u64,
        previous.map(|receipt| receipt.digest().to_string()),
        ReceiptOutcome::FailedLaunch {
            authorization: Box::new(Authorization {
                authorization_id: authorization.authorization_id.clone(),
                request_id: authorization.request_id.clone(),
                request_digest: authorization.request_digest.clone(),
            }),
            identity_slot: authorization.identity_slot,
            assigned_uid: authorization.assigned_uid,
            assigned_gid: authorization.assigned_gid,
            failure: LaunchFailure {
                phase: "workspace_prepare".into(),
                code: "unavailable".into(),
            },
            agent_execution: if prefix.len() == 2 {
                AgentExecution::Started
            } else {
                AgentExecution::NotStarted
            },
            retained_storage: louiselm_skills::launch_recovery::RetainedStorage::Sealed,
        },
        SessionState::Terminal,
    ))
}

#[test]
fn failed_launch_genesis_and_continuations_free_capacity_without_renewing_authority() {
    for count in 0..=2 {
        let root = TempDir::new().unwrap();
        let service = broker(root.path(), "first.sock");
        let authorization = consume(&service, "failed");
        let mut prefix = receipted::history(&service, &authorization, count);
        assert!(matches!(
            service.authorize(&grant(&request("next")), 3000, verify_fixture_signature),
            Err(BrokerError::IdentityExhausted(_))
        ));
        let terminal = failed(&authorization, &prefix);
        service
            .receipts()
            .append(
                &authorization,
                &terminal.canonical_bytes(),
                None,
                verify_fixture_signature,
            )
            .unwrap();
        prefix.push(terminal.canonical_bytes());
        let inspection = service.inspect("failed").unwrap().unwrap();
        assert_eq!(inspection.state, SessionState::Terminal);
        assert_eq!(
            inspection.last_failure.unwrap().code,
            ErrorCode::LaunchFailed
        );
        assert_eq!(inspection.launch_evidence.is_some(), count > 0);
        assert_eq!(inspection.start_evidence.is_some(), count > 1);
        let durable = service
            .inspect_consumed_recovery(&authorization.authorization_id, verify_fixture_signature)
            .unwrap();
        assert_eq!(
            serde_json::to_value(durable.outcome).unwrap()["status"],
            "failed_launch"
        );
        drop(service);
        let service = broker(root.path(), "second.sock");
        let successor = consume(&service, "successor");
        assert_eq!(successor.identity_slot, authorization.identity_slot);
        assert_eq!(service.receipts().stored_bytes("failed").unwrap(), prefix);
        assert_eq!(
            service
                .authorizations()
                .session_count_for_run("run-1")
                .unwrap(),
            2
        );
        assert!(matches!(
            service
                .authorizations()
                .consume(&request("failed"), CONTROLLER_UID, 3000),
            Err(BrokerError::UnknownAuthorization)
        ));
    }
}

fn deliver(service: &BrokerService, socket: &Path, bytes: &[Vec<u8>], lose_terminal_ack: bool) {
    thread::scope(|scope| {
        let server = scope.spawn(|| service.serve_connection(90000, verify_fixture_signature));
        let connector = SeqpacketConnector::new().unwrap();
        let channel = settle(|done| connector.connect(socket, local_pin(), done));
        for (index, saved) in bytes.iter().enumerate() {
            settle(|done| channel.send(saved.clone(), done));
            if lose_terminal_ack && index + 1 == bytes.len() {
                continue;
            }
            let reply = settle(|done| channel.receive(done));
            let LauncherPacket::Request(ProtocolMessage::ReceiptAcknowledgement(ack)) =
                reply.packet
            else {
                panic!("exact receipt acknowledgement")
            };
            let receipt = SignedReceipt::parse_canonical(saved).unwrap();
            assert_eq!(
                ack.exact_disposition(
                    &receipt.payload.session_id,
                    &receipt.payload.run_id,
                    &louiselm_skills::launch_receipt::ReceiptHead {
                        sequence: receipt.payload.sequence,
                        digest: receipt.digest().to_string()
                    }
                ),
                Some(ReceiptDisposition::DurablyStored)
            );
        }
        assert!(matches!(
            server.join().unwrap(),
            Err(BrokerError::LaunchFailed)
        ));
    });
}

#[test]
fn saved_cleanup_delivery_survives_restart_lost_ack_and_a_successor_without_new_authority() {
    for count in 0..=2 {
        let root = TempDir::new().unwrap();
        let service = broker(root.path(), "first.sock");
        let authorization = consume(&service, "failed");
        let launch = launch_receipt(&authorization);
        let start = start_receipt(&authorization, &launch);
        let mut saved: Vec<_> = [launch, start]
            .iter()
            .take(count)
            .map(SignedReceipt::canonical_bytes)
            .collect();
        saved.push(failed(&authorization, &saved).canonical_bytes());
        drop(service);
        let service = broker(root.path(), "second.sock");
        deliver(&service, &root.path().join("second.sock"), &saved, true);
        drop(service);
        let service = broker(root.path(), "third.sock");
        let successor = consume(&service, "successor");
        assert_eq!(successor.identity_slot, authorization.identity_slot);
        deliver(&service, &root.path().join("third.sock"), &saved, false);
        assert_eq!(service.receipts().stored_bytes("failed").unwrap(), saved);
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
    }
}

#[test]
fn conflicting_saved_outcome_cannot_replace_terminal_bytes_or_touch_its_successor() {
    let root = TempDir::new().unwrap();
    let service = broker(root.path(), "broker.sock");
    let authorization = consume(&service, "failed");
    let terminal = failed(&authorization, &[]);
    let bytes = terminal.canonical_bytes();
    deliver(
        &service,
        &root.path().join("broker.sock"),
        std::slice::from_ref(&bytes),
        false,
    );
    consume(&service, "successor");
    let successor = service
        .authorizations()
        .consumed_for_session("successor")
        .unwrap()
        .unwrap();
    let mut conflict = terminal.payload;
    let ReceiptOutcome::FailedLaunch { failure, .. } = &mut conflict.outcome else {
        panic!("failed launch")
    };
    failure.code = "substituted".into();
    let conflict = signed(conflict).canonical_bytes();
    thread::scope(|scope| {
        let server = scope.spawn(|| service.serve_connection(90000, verify_fixture_signature));
        let connector = SeqpacketConnector::new().unwrap();
        let peer =
            settle(|done| connector.connect(&root.path().join("broker.sock"), local_pin(), done));
        settle(|done| peer.send(conflict, done));
        expect_disconnect(&peer);
        assert!(matches!(
            server.join().unwrap(),
            Err(BrokerError::ReceiptUnauthorized)
        ));
    });
    assert_eq!(
        service.receipts().stored_bytes("failed").unwrap(),
        vec![bytes]
    );
    assert_eq!(
        service
            .authorizations()
            .consumed_for_session("successor")
            .unwrap()
            .unwrap(),
        successor
    );
    assert!(matches!(
        service.authorize(&grant(&request("third")), 3000, verify_fixture_signature),
        Err(BrokerError::IdentityExhausted(_))
    ));
}

#[test]
fn wrong_cleanup_bindings_or_signature_cannot_release_the_reserved_identity() {
    for mutation in 0..6 {
        let root = TempDir::new().unwrap();
        let service = broker(root.path(), "broker.sock");
        let authorization = consume(&service, "failed");
        let mut terminal = failed(&authorization, &[]);
        let ReceiptOutcome::FailedLaunch {
            authorization: claimed,
            identity_slot,
            assigned_uid,
            assigned_gid,
            ..
        } = &mut terminal.payload.outcome
        else {
            panic!("failed launch")
        };
        match mutation {
            0 => claimed.authorization_id = "foreign".into(),
            1 => claimed.request_digest = Digest::of(b"foreign").to_string(),
            2 => *identity_slot += 1,
            3 => *assigned_uid += 1,
            4 => *assigned_gid += 1,
            _ => (),
        }
        terminal = signed(terminal.payload);
        if mutation == 5 {
            terminal.signature = "forged".into();
        }
        let error = service
            .receipts()
            .append(
                &authorization,
                &terminal.canonical_bytes(),
                None,
                verify_fixture_signature,
            )
            .unwrap_err();
        if mutation == 5 {
            assert!(matches!(
                error,
                BrokerError::ReceiptRefused(
                    louiselm_skills::launch_receipt::ReceiptError::InvalidSignature { sequence: 0 }
                )
            ));
        } else {
            assert!(matches!(error, BrokerError::ReceiptUnauthorized));
        }
        assert!(matches!(
            service.authorize(&grant(&request("next")), 3000, verify_fixture_signature),
            Err(BrokerError::IdentityExhausted(_))
        ));
    }
}
