//! Cold Resume follows real registration and loss settlement, then allocates once.
use super::*;
use louiselm_skills::broker::{
    BrokerSession,
    cold_resume::{ColdLoadOutcome, WithheldCommand},
    lifecycle::LifecycleCaller,
};
use louiselm_skills::launch_protocol::{
    CONTROLLER_LOSS_SETTLEMENT_SCHEMA, ControllerLossSettlement, RecoveryRequest,
};

fn dispose_source(
    service: &BrokerService,
    session: &mut BrokerSession,
    recovery: &RecoveryRequest,
    channel: &SeqpacketChannel,
) {
    let loss = ControllerLossSettlement {
        schema: CONTROLLER_LOSS_SETTLEMENT_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        request_id: "loss".into(),
        session_id: recovery.launch.session_id.clone(),
        run_id: recovery.launch.run_id.clone(),
        envelope_revision: recovery.launch.envelope_revision,
        parked_head: recovery.head.clone(),
    };
    settle(|complete| channel.send(loss.canonical_bytes(), complete));
    assert!(
        !service
            .step(session, 4000, verify_fixture_signature)
            .unwrap()
    );
    let _ = settle(|complete| channel.receive(complete));
    let receipt = signed(payload(
        session.authorization(),
        "loss-disposed",
        3,
        Some(recovery.head.digest.clone()),
        ReceiptOutcome::Disposal {
            authority: ReceiptAuthority::Cause {
                cause: ReceiptCause::ControllerLost,
            },
        },
        SessionState::Terminal,
    ));
    settle(|complete| channel.send(receipt.canonical_bytes(), complete));
    assert!(
        service
            .step(session, 4000, verify_fixture_signature)
            .unwrap()
    );
    let _ = settle(|complete| channel.receive(complete));
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "One allocation is followed through authorization, restart, consumption and the interrupted-consumption crash boundary."
)]
fn cold_resume_requires_disposal_and_operator_then_survives_retry_without_new_authority() {
    super::recovery::registration(
        false,
        Some(3),
        |service, session, recovery, channel, root| {
            let caller = LifecycleCaller::Operator {
                uid: CONTROLLER_UID,
            };
            let target = request("replacement");
            assert!(
                service
                    .authorize_cold_resume(
                        "session-1",
                        &target,
                        &caller,
                        4000,
                        verify_fixture_signature
                    )
                    .is_err()
            );
            dispose_source(service, session, recovery, channel);
            assert!(
                service
                    .authorize_cold_resume(
                        "session-1",
                        &target,
                        &LifecycleCaller::Agent,
                        4000,
                        verify_fixture_signature
                    )
                    .is_err()
            );
            let first = service
                .authorize_cold_resume(
                    "session-1",
                    &target,
                    &caller,
                    4000,
                    verify_fixture_signature,
                )
                .unwrap();
            assert_eq!(first.commands.as_ref().unwrap().uses, Some(3));
            assert_eq!(first.expires_at_ms, 30000);
            assert_eq!(
                service
                    .authorize_cold_resume(
                        "session-1",
                        &target,
                        &caller,
                        5000,
                        verify_fixture_signature
                    )
                    .unwrap(),
                first
            );
            assert!(
                service
                    .authorize_cold_resume(
                        "session-1",
                        &request("competitor"),
                        &caller,
                        5000,
                        verify_fixture_signature
                    )
                    .is_err()
            );
            assert!(
                service
                    .authorize_cold_resume(
                        "session-1",
                        &target,
                        &caller,
                        30000,
                        verify_fixture_signature
                    )
                    .is_err()
            );
            let restarted = BrokerService::bind(
                &root.join("restart-cold.sock"),
                AuthorizationStore::open(&root.join("authorizations"), pool(4)).unwrap(),
                ReceiptStore::open(&root.join("receipts"), trusted_release()).unwrap(),
                AuditLog::open(&root.join("audit")).unwrap(),
                local_pin(),
            )
            .unwrap();
            assert_eq!(
                restarted
                    .authorize_cold_resume(
                        "session-1",
                        &target,
                        &caller,
                        5000,
                        verify_fixture_signature
                    )
                    .unwrap(),
                first
            );
            restarted
                .authorizations()
                .consume_for_launcher(&target, 5000)
                .unwrap();
            assert_eq!(
                restarted
                    .authorize_cold_resume(
                        "session-1",
                        &target,
                        &caller,
                        5000,
                        verify_fixture_signature
                    )
                    .unwrap(),
                first
            );
            assert!(
                restarted
                    .authorizations()
                    .consume_for_launcher(&target, 5000)
                    .is_err()
            );
            fs::remove_file(root.join("authorizations/consumed/authorization-replacement.json"))
                .unwrap();
            assert!(
                restarted
                    .authorize_cold_resume(
                        "session-1",
                        &target,
                        &caller,
                        5000,
                        verify_fixture_signature
                    )
                    .is_err(),
                "uncertain consumption must not recreate pending authority"
            );
        },
    );
}

#[test]
fn cold_resume_uncapped_and_unavailable_accounting_keep_exact_scope() {
    for uses in [None, Some(3)] {
        super::recovery::registration(false, uses, |service, session, recovery, channel, root| {
            dispose_source(service, session, recovery, channel);
            // Unavailable bounded balance withholds only that permission. Base
            // ACP reconstruction stays eligible; no process command is fabricated.
            fs::write(root.join("audit/decisions.jsonl"), b"invalid audit\n").unwrap();
            let caller = LifecycleCaller::Operator {
                uid: CONTROLLER_UID,
            };
            let allocation = service
                .authorize_cold_resume(
                    "session-1",
                    &request("replacement"),
                    &caller,
                    5000,
                    verify_fixture_signature,
                )
                .unwrap();
            if uses.is_some() {
                assert_eq!(
                    allocation.withheld_command,
                    Some(WithheldCommand::AccountingUnavailable)
                );
                assert!(allocation.commands.is_none());
            } else {
                assert_eq!(allocation.commands.unwrap().uses, None);
                assert_eq!(allocation.withheld_command, None);
            }
        });
    }
}

#[test]
fn cold_resume_failed_load_cannot_be_finalized_by_a_late_success() {
    super::recovery::registration(
        false,
        Some(3),
        |service, session, recovery, channel, root| {
            dispose_source(service, session, recovery, channel);
            let target = request("replacement");
            let caller = LifecycleCaller::Operator {
                uid: CONTROLLER_UID,
            };
            service
                .authorize_cold_resume(
                    "session-1",
                    &target,
                    &caller,
                    5000,
                    verify_fixture_signature,
                )
                .unwrap();
            let socket = root.join("broker.sock");
            let peer = thread::spawn(move || fake_supervisor(&socket, &target, 5000));
            let mut replacement = service
                .serve_launch(5000, verify_fixture_signature)
                .unwrap();
            let (_, _peer) = peer.join().unwrap();
            service
                .finish_cold_resume(
                    &mut replacement,
                    &caller,
                    &ColdLoadOutcome::Failed,
                    5000,
                    verify_fixture_signature,
                )
                .unwrap();
            assert!(
                !replacement.channel().is_closed(),
                "retain the authenticated channel for terminal cleanup proof"
            );
            assert!(
                service
                    .finish_cold_resume(
                        &mut replacement,
                        &caller,
                        &ColdLoadOutcome::Loaded {
                            acp_session_id: "acp-1".into()
                        },
                        5000,
                        verify_fixture_signature
                    )
                    .is_err()
            );
        },
    );
}

#[test]
fn concurrent_cold_resumes_allocate_only_one_target() {
    super::recovery::registration(false, Some(3), |service, session, recovery, channel, _| {
        dispose_source(service, session, recovery, channel);
        let barrier = std::sync::Barrier::new(2);
        thread::scope(|scope| {
            let attempts: Vec<_> = ["target-a", "target-b"]
                .into_iter()
                .map(|id| {
                    let barrier = &barrier;
                    scope.spawn(move || {
                        barrier.wait();
                        service.authorize_cold_resume(
                            "session-1",
                            &request(id),
                            &LifecycleCaller::Operator {
                                uid: CONTROLLER_UID,
                            },
                            5000,
                            verify_fixture_signature,
                        )
                    })
                })
                .collect();
            let results: Vec<_> = attempts.into_iter().map(|t| t.join().unwrap()).collect();
            assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        });
    });
}
