//! Status after a real broker Resume and durable recovery registration.

use super::*;

#[test]
fn agent_status_after_resume_preserves_registered_recovery_and_admission_history() {
    super::super::recovery::with_registered(|service, session, recovery, channel| {
        let authorization = session.authorization().clone();
        let mut parked = status(&authorization);
        parked.state = SessionState::Parked;
        parked.channel_state = ChannelState::Disabled;
        parked.broker_head = Some(recovery.head.clone());
        parked.launcher_head = Some(recovery.head.clone());
        let mut resume = park(&authorization);
        resume.request_id = "resume-for-status".into();
        resume.action = LifecycleAction::Resume;
        resume.expected_state = SessionState::Parked;
        resume.expected_receipt_sequence = Some(recovery.head.sequence);
        let admission = service
            .receipts()
            .stored_bytes(&authorization.session_id)
            .unwrap();
        thread::scope(|scope| {
            let peer = scope.spawn(|| {
                let receipt = drive_lifecycle_peer(channel, &parked);
                let mut running = status(&authorization);
                let head = ReceiptHead {
                    sequence: receipt.payload.sequence,
                    digest: receipt.digest().to_string(),
                };
                running.broker_head = Some(head.clone());
                running.launcher_head = Some(head);
                let mut replies = Vec::new();
                for _ in 0..2 {
                    settle(|done| {
                        channel.send(
                            agent_status_query(&authorization, &authorization.session_id),
                            done,
                        )
                    });
                    answer_one_status_query(channel, &running);
                    replies.push(expect_response(channel));
                }
                answer_one_status_query(channel, &running);
                replies
            });
            let resumed = service
                .request_lifecycle(
                    session,
                    &operator(),
                    &resume,
                    4000,
                    verify_fixture_signature,
                )
                .unwrap();
            assert_eq!(resumed.payload.resulting_state, SessionState::Running);
            let audit = service.audit().unwrap();
            let head = service.receipts().head(&authorization.session_id).unwrap();
            for _ in 0..2 {
                assert!(
                    !service
                        .step(session, 4000, verify_fixture_signature)
                        .unwrap()
                );
            }
            let mut operator = service
                .session_status(session, &operator(), 4000, verify_fixture_signature)
                .unwrap();
            operator.allowed_actions.clear();
            for reply in peer.join().unwrap() {
                let CommandOperation::StatusResult { status } = reply.operation else {
                    panic!("self status after Resume")
                };
                assert_eq!(status.state, SessionState::Running);
                assert_eq!(status.channel_state, ChannelState::Enabled);
                assert!(status.allowed_actions.is_empty());
                assert_eq!(status.canonical_bytes(), operator.canonical_bytes());
                assert!(
                    matches!(&status.recovery, louiselm_skills::launch_protocol::RecoveryReadiness::Ready { operation_id, expires_at_ms }
                    if operation_id == &recovery.retention.request_id && *expires_at_ms == recovery.retention.expires_at_ms)
                );
            }
            assert_eq!(service.audit().unwrap(), audit);
            assert_eq!(
                service.receipts().head(&authorization.session_id).unwrap(),
                head
            );
            let after = service
                .receipts()
                .stored_bytes(&authorization.session_id)
                .unwrap();
            assert_eq!(&after[..admission.len()], admission);
            assert_eq!(
                after.len(),
                admission.len() + 1,
                "only Resume appends a receipt"
            );
        });
    });
}
