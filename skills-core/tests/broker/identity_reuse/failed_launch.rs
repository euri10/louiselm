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
