//! Administrative durable history never pretends to observe a live supervisor.
use super::*;
use louiselm_skills::launch_recovery::{Inspection, InspectionOutcome, ProofError};

#[test]
fn recovery_inspection_preserves_each_original_state_across_restart() {
    for count in 0..=3 {
        let root = TempDir::new().unwrap();
        let service = broker(root.path(), "first.sock");
        let auth = consume(&service, "held");
        let original = receipted::history(&service, &auth, count);
        let before = service
            .inspect_consumed_recovery(&auth.authorization_id, verify_fixture_signature)
            .unwrap();
        assert_eq!(before.outcome, InspectionOutcome::Unrecovered {});
        assert_eq!(
            before.receipt_state,
            [
                None,
                Some(SessionState::Starting),
                Some(SessionState::Running),
                Some(SessionState::Parked)
            ][count]
        );
        assert_eq!(before.binding.authorization, auth);
        assert_eq!(
            before.binding.receipt_head,
            service.receipts().head("held").unwrap()
        );
        assert!(!root.path().join("receipts/consumed-recoveries").exists());
        assert!(matches!(
            service.authorize(&grant(&request("next")), 3000, verify_fixture_signature),
            Err(BrokerError::IdentityExhausted(_))
        ));
        let proof = recovery_proof(&service, &auth);
        service
            .accept_consumed_recovery(&proof, verify_fixture_signature)
            .unwrap();
        drop(service);
        let service = broker(root.path(), "second.sock");
        let after = service
            .inspect_consumed_recovery(&auth.authorization_id, verify_fixture_signature)
            .unwrap();
        assert_eq!(after.binding, before.binding);
        assert_eq!(after.receipt_state, before.receipt_state);
        assert_eq!(
            after.outcome,
            InspectionOutcome::RecoveredAfterReboot {
                proof: Box::new(proof)
            }
        );
        assert_eq!(service.receipts().stored_bytes("held").unwrap(), original);
        consume(&service, "successor");
        assert_eq!(
            service
                .inspect_consumed_recovery(&auth.authorization_id, verify_fixture_signature)
                .unwrap(),
            after
        );
    }
}

#[test]
fn normal_disposal_is_not_reboot_recovery_and_unknown_history_is_not_unrecovered() {
    let root = TempDir::new().unwrap();
    let service = broker(root.path(), "first.sock");
    let auth = consume(&service, "disposed");
    let original = dispose(&service, &auth);
    drop(service);
    let service = broker(root.path(), "second.sock");
    let inspection = service
        .inspect_consumed_recovery(&auth.authorization_id, verify_fixture_signature)
        .unwrap();
    assert_eq!(inspection.outcome, InspectionOutcome::Disposed {});
    assert_eq!(inspection.receipt_state, Some(SessionState::Terminal));
    assert_eq!(
        inspection.binding.receipt_head,
        service.receipts().head("disposed").unwrap()
    );
    assert_eq!(
        service.receipts().stored_bytes("disposed").unwrap(),
        original
    );
    assert!(matches!(
        service.inspect_consumed_recovery("unknown", verify_fixture_signature),
        Err(BrokerError::UnknownAuthorization)
    ));
}

#[test]
fn inspection_refuses_malformed_forged_and_mismatched_stored_proofs() {
    let root = TempDir::new().unwrap();
    let service = broker(root.path(), "broker.sock");
    let auth = consume(&service, "held");
    let original = receipted::history(&service, &auth, 3);
    let proof = recovery_proof(&service, &auth);
    service
        .accept_consumed_recovery(&proof, verify_fixture_signature)
        .unwrap();
    let path = fs::read_dir(root.path().join("receipts/consumed-recoveries"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    fs::write(&path, b"malformed fixture").unwrap();
    assert!(matches!(
        service.inspect_consumed_recovery(&auth.authorization_id, verify_fixture_signature),
        Err(BrokerError::Recovery(ProofError::Malformed))
    ));
    let mut changed = proof.clone();
    changed.signature = Digest::of(b"forged").to_string();
    fs::write(&path, changed.canonical_bytes()).unwrap();
    assert!(matches!(
        service.inspect_consumed_recovery(&auth.authorization_id, verify_fixture_signature),
        Err(BrokerError::Recovery(ProofError::InvalidSignature))
    ));
    changed = proof;
    changed
        .payload
        .binding
        .receipt_head
        .as_mut()
        .unwrap()
        .digest = Digest::of(b"foreign head").to_string();
    changed.signature = Digest::of(&changed.payload.canonical_bytes()).to_string();
    fs::write(&path, changed.canonical_bytes()).unwrap();
    assert!(matches!(
        service.inspect_consumed_recovery(&auth.authorization_id, verify_fixture_signature),
        Err(BrokerError::RequestMismatch)
    ));
    assert_eq!(service.receipts().stored_bytes("held").unwrap(), original);
}

#[test]
fn inspection_is_a_closed_consistent_document() {
    let root = TempDir::new().unwrap();
    let service = broker(root.path(), "broker.sock");
    let auth = consume(&service, "held");
    receipted::history(&service, &auth, 3);
    let inspection = service
        .inspect_consumed_recovery(&auth.authorization_id, verify_fixture_signature)
        .unwrap();
    inspection.validate().unwrap();
    let bytes = serde_json::to_vec(&inspection).unwrap();
    assert_eq!(
        serde_json::from_slice::<Inspection>(&bytes).unwrap(),
        inspection
    );
    let mut unknown = serde_json::to_value(&inspection).unwrap();
    unknown["outcome"]["live"] = true.into();
    assert!(serde_json::from_value::<Inspection>(unknown).is_err());
    for mutation in 0..5 {
        let mut changed = inspection.clone();
        match mutation {
            0 => changed.binding.receipt_head = None,
            1 => changed.receipt_state = None,
            2 => changed.receipt_state = Some(SessionState::Terminal),
            3 => changed.outcome = InspectionOutcome::Disposed {},
            _ => {
                let mut proof = recovery_proof(&service, &auth);
                proof.payload.binding.authorization.session_id = "foreign".into();
                changed.outcome = InspectionOutcome::RecoveredAfterReboot {
                    proof: Box::new(proof),
                };
            }
        }
        assert_eq!(changed.validate(), Err(ProofError::InvalidBinding));
    }
}
