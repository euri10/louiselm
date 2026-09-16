//! Controller-loss settlement is durable local policy, not a projection ACK.
use super::*;
use louiselm_skills::launch_protocol::{
    CONTROLLER_LOSS_SETTLEMENT_SCHEMA, ControllerLossAcknowledgement, ControllerLossDisposition,
    ControllerLossSettlement,
};
use louiselm_skills::launch_receipt::{ReceiptAuthority, ReceiptCause, ReceiptHead};

fn settlement(authorization: &LaunchAuthorization, head: ReceiptHead) -> ControllerLossSettlement {
    ControllerLossSettlement {
        schema: CONTROLLER_LOSS_SETTLEMENT_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        request_id: format!("settle-{}", head.sequence),
        session_id: authorization.session_id.clone(),
        run_id: authorization.run_id.clone(),
        envelope_revision: authorization.envelope_revision,
        parked_head: head,
    }
}

fn acknowledgement(channel: &SeqpacketChannel) -> ControllerLossAcknowledgement {
    let reply = settle(|complete| channel.receive(complete));
    let LauncherPacket::Response(response) = reply.packet else {
        panic!("settlement response");
    };
    let ResponseResult::ControllerLossAcknowledgement { acknowledgement } = response.result else {
        panic!("settlement acknowledgement");
    };
    acknowledgement
}

#[test]
fn registered_point_settles_identically_but_expired_retry_cannot_renew_it() {
    super::recovery::with_registered(|service, session, recovery, channel| {
        let request = settlement(session.authorization(), recovery.head.clone());
        settle(|complete| channel.send(request.canonical_bytes(), complete));
        assert!(
            !service
                .step(session, 4000, None, verify_fixture_signature)
                .unwrap()
        );
        let original = acknowledgement(channel);
        original.validate_for(&request).unwrap();
        assert!(
            matches!(&original.disposition, ControllerLossDisposition::Recoverable { acp_recovery_reference, .. }
            if acp_recovery_reference == &recovery.retention.request_id)
        );
        settle(|complete| channel.send(request.canonical_bytes(), complete));
        service
            .step(session, 5000, None, verify_fixture_signature)
            .unwrap();
        assert_eq!(acknowledgement(channel), original);
        settle(|complete| channel.send(request.canonical_bytes(), complete));
        assert!(
            service
                .step(session, 60000, None, verify_fixture_signature)
                .is_err()
        );
        expect_disconnect(channel);
    });
}

#[test]
fn expired_point_before_loss_records_no_recovery() {
    super::recovery::with_registered(|service, session, recovery, channel| {
        let request = settlement(session.authorization(), recovery.head.clone());
        settle(|complete| channel.send(request.canonical_bytes(), complete));
        service
            .step(session, 60000, None, verify_fixture_signature)
            .unwrap();
        assert!(matches!(
            acknowledgement(channel).disposition,
            ControllerLossDisposition::NoRecovery { .. }
        ));
    });
}

#[test]
fn foreign_or_stale_settlement_never_acknowledges_disposal() {
    for kind in ["session", "run", "revision", "head"] {
        super::recovery::with_registered(|service, session, recovery, channel| {
            let mut request = settlement(session.authorization(), recovery.head.clone());
            match kind {
                "session" => request.session_id = "another-session".into(),
                "run" => request.run_id = "another-run".into(),
                "revision" => request.envelope_revision += 1,
                _ => request.parked_head.digest = Digest::of(b"stale").to_string(),
            }
            settle(|complete| channel.send(request.canonical_bytes(), complete));
            assert!(
                service
                    .step(session, 4000, None, verify_fixture_signature)
                    .is_err()
            );
            expect_disconnect(channel);
        });
    }
}

#[test]
fn absent_recovery_settles_abnormal_loss_only_after_local_attention_is_durable() {
    check_absent("none");
}

#[test]
fn storage_or_outbox_failure_cannot_acknowledge_disposal_and_retry_is_durable() {
    for fault in ["decision", "outbox"] {
        check_absent(fault);
    }
}

fn check_absent(fault: &str) {
    let root = TempDir::new().unwrap();
    let (service, authorization, chain) = super::reconnect::fixture(root.path());
    let park = signed(payload(
        &authorization,
        "lost",
        2,
        Some(chain[1].digest().to_string()),
        ReceiptOutcome::Park {
            authority: ReceiptAuthority::Cause {
                cause: ReceiptCause::ControllerLost,
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
    let mut channel = super::reconnect::peer(root.path(), &super::reconnect::checkpoint(&park));
    let mut session = service
        .serve_reconnect(3000, verify_fixture_signature)
        .unwrap();
    let _ = settle(|complete| channel.receive(complete));
    let request = settlement(
        &authorization,
        ReceiptHead {
            sequence: 2,
            digest: park.digest().to_string(),
        },
    );
    let fault_path = root.path().join(if fault == "decision" {
        "authorizations/controller-loss"
    } else {
        "authorizations/attention-outbox/entries"
    });
    if fault == "outbox" {
        fs::rename(&fault_path, fault_path.with_extension("backup")).unwrap();
    }
    if fault != "none" {
        fs::write(&fault_path, b"storage unavailable").unwrap();
    }
    settle(|complete| channel.send(request.canonical_bytes(), complete));
    if fault != "none" {
        assert!(
            service
                .step(&mut session, 4000, None, verify_fixture_signature)
                .is_err()
        );
        expect_disconnect(&channel);
        fs::remove_file(&fault_path).unwrap();
        if fault == "outbox" {
            fs::rename(fault_path.with_extension("backup"), &fault_path).unwrap();
        }
        channel = super::reconnect::peer(root.path(), &super::reconnect::checkpoint(&park));
        session = service
            .serve_reconnect(5000, verify_fixture_signature)
            .unwrap();
        let _ = settle(|complete| channel.receive(complete));
        settle(|complete| channel.send(request.canonical_bytes(), complete));
    }
    assert!(
        !service
            .step(&mut session, 4000, None, verify_fixture_signature)
            .unwrap()
    );
    let outbox = louiselm_skills::broker::attention::Outbox::open(
        &root.path().join("authorizations/attention-outbox"),
    )
    .unwrap();
    let queued = outbox.next().unwrap().expect("local enqueue precedes ACK");
    assert!(matches!(
        acknowledgement(&channel).disposition,
        ControllerLossDisposition::NoRecovery { .. }
    ));
    settle(|complete| channel.send(request.canonical_bytes(), complete));
    service
        .step(&mut session, 5000, None, verify_fixture_signature)
        .unwrap();
    let _ = acknowledgement(&channel);
    assert_eq!(outbox.next().unwrap(), Some(queued));
}
