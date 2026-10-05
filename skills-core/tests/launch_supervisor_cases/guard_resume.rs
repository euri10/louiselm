//! Regression coverage for the receipt-gated warm Resume transaction.
use super::*;

fn guarded(request: LifecycleRequest, head: ReceiptHead, revision: u64) -> ProtocolMessage {
    use louiselm_skills::launch_protocol::{GUARD_RESUME_SCHEMA, GuardResumeRequest, GuardScope};
    ProtocolMessage::GuardResume(Box::new(GuardResumeRequest {
        schema: GUARD_RESUME_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        scope: GuardScope {
            session_id: request.session_id.clone(),
            run_id: request.run_id.clone(),
            envelope_revision: request.envelope_revision,
            revision,
            deadline_ns: u64::MAX,
        },
        request,
        parked_head: head,
    }))
}

#[test]
fn activation_deadline_revokes_both_and_late_completion_cannot_restore_them() {
    for fault in ["none", "capability", "guard"] {
        let setup = setup(
            true,
            |_| {},
            AppendBehavior::Hold,
            PlatformBehavior::default(),
            SUPERVISOR_TIMEOUT,
        );
        let timer = Arc::new(FakeTimer::new(Arc::clone(&setup.events)));
        let supervisor = setup.fresh_supervisor_with_timer(timer.clone());
        let session = complete_launch_on(&setup, &supervisor);
        let (input, receiver, worker, _) = park_launched_session(&setup, session);
        {
            let mut state = lock(&setup.platform.agent);
            state.guard_enabled = true;
            state.hold_guard_activation = true;
        }
        let deadline = timer.scheduled_count();
        let resume = resume_request(&setup, "held-activation");
        setup.broker.wait_for_session_request();
        setup
            .broker
            .deliver_session_request(ProtocolMessage::Lifecycle(resume.clone()));
        setup.broker.wait_for_session_receipt(1);
        setup.broker.wait_for_session_request();
        setup
            .broker
            .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
                setup.broker.session_receipt_acknowledgement(1),
            ));
        let pending = request_supervisor_status(&setup, "paired-activation-pending");
        assert_eq!(
            pending.pending_operation.unwrap().phase,
            PendingPhase::Activating
        );
        assert_eq!(pending.channel_state, ChannelState::Revoked);
        if fault == "capability" {
            lock(&setup.platform.gate_state()).revoke_error =
                Some(SupervisorError::CleanupUnproven);
        } else if fault == "guard" {
            lock(&setup.platform.agent).guard_revoke_fails = true;
        }
        timer.fire(deadline);
        let failed = setup
            .broker
            .wait_for_session_response(&resume.request_id, 0);
        assert!(matches!(failed.result, ResponseResult::Error { .. }));
        let (result, late) = lock(&setup.platform.agent).guard_activation.take().unwrap();
        late(result);
        if fault == "none" {
            let status = request_supervisor_status(&setup, "late-activation-after-deadline");
            assert_eq!(status.state, SessionState::Parked);
            assert_eq!(status.channel_state, ChannelState::Revoked);
            finish_session_relay_after_reconciling_backlog(
                &setup,
                input,
                receiver,
                worker,
                status.pending_receipt_count,
            );
        } else {
            drop(input);
            assert_eq!(
                receiver.recv_timeout(CALLBACK_TIMEOUT).unwrap(),
                Err(if fault == "guard" {
                    SupervisorError::CleanupUnproven
                } else {
                    SupervisorError::LifecycleMechanicUnavailable
                })
            );
            worker.join().unwrap();
            assert_eq!(
                event_count(&setup.events, "identity.poison"),
                usize::from(fault == "guard")
            );
            assert_eq!(
                event_count(&setup.events, "identity.release"),
                usize::from(fault == "capability")
            );
        }
        assert!(!lock(&setup.platform.agent).guard_active);
        assert!(!lock(&setup.platform.gate_state()).enabled);
    }
}

#[test]
fn disposal_cancels_frozen_guard_preparation_and_late_ack_cannot_thaw() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let session = complete_launch(&setup);
    let (input, receiver, worker, _) = park_launched_session(&setup, session);
    lock(&setup.platform.agent).hold_guard_preparation = true;
    let resume = resume_request(&setup, "held-guard-preparation");
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(resume.clone()));
    let waiting = request_supervisor_status(&setup, "frozen-preparation");
    assert_eq!(waiting.state, SessionState::Parked);
    assert_eq!(
        waiting.pending_operation.unwrap().phase,
        PendingPhase::Applying
    );
    let mut disposal = disposal_request(&setup, "dispose-held-guard");
    disposal.expected_state = SessionState::Parked;
    disposal.expected_receipt_sequence = Some(2);
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(disposal));
    let after = request_supervisor_status(&setup, "after-preparation-disposal");
    let late = lock(&setup.platform.agent)
        .guard_preparation
        .take()
        .unwrap();
    late(Ok(()));
    if after.state == SessionState::Terminal {
        setup.broker.wait_for_session_receipt(1);
        setup.broker.wait_for_session_request();
        setup
            .broker
            .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
                setup.broker.session_receipt_acknowledgement(1),
            ));
        finish_terminal_session_relay(input, receiver, worker);
    } else {
        // Retain cleanup on the pre-fix path before reporting the red assertion.
        finish_session_relay(&setup, input, receiver, worker);
    }
    assert_eq!(after.state, SessionState::Terminal);
    assert_eq!(event_count(&setup.events, "agent.resume"), 0);
    assert!(!lock(&setup.platform.gate_state()).enabled);
}

#[test]
fn partial_capability_activation_never_reports_success_or_leaves_effects_enabled() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let session = complete_launch(&setup);
    let (input, receiver, worker, _) = park_launched_session(&setup, session);
    lock(&setup.platform.gate_state()).resume_enable_error =
        Some(SupervisorError::CapabilityUnavailable);
    let resume = resume_request(&setup, "partial-activation");
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(resume.clone()));
    setup.broker.wait_for_session_receipt(1);
    let running = SignedReceipt::parse_canonical(&setup.broker.session_receipt_bytes(1)).unwrap();
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(1),
        ));
    let response = setup
        .broker
        .wait_for_session_response(&resume.request_id, 0);
    let status = request_supervisor_status(&setup, "after-partial-activation");
    let enabled = lock(&setup.platform.gate_state()).enabled;
    // Clean both the old behavior and the expected truthful rollback before asserting.
    if status.pending_receipt_count == 0 {
        finish_session_relay(&setup, input, receiver, worker);
    } else {
        finish_session_relay_after_reconciling_backlog(
            &setup,
            input,
            receiver,
            worker,
            status.pending_receipt_count,
        );
    }
    assert_eq!(running.payload.resulting_state, SessionState::Running);
    assert!(matches!(response.result, ResponseResult::Error { .. }));
    assert_eq!(status.state, SessionState::Parked);
    assert_eq!(status.channel_state, ChannelState::Revoked);
    assert!(!enabled);
    assert_eq!(
        status.pending_receipt_count, 1,
        "retain the causal rollback Park"
    );
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "One ordered failure, replay, reconciliation and fresh Resume trace."
)]
fn networking_activation_failure_revokes_capability_and_a_fresh_resume_can_succeed() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    let session = complete_launch(&setup);
    let (input, receiver, worker, _) = park_launched_session(&setup, session);
    {
        let mut state = lock(&setup.platform.agent);
        state.guard_enabled = true;
        state.guard_activation_fails = true;
    }
    let resume = resume_request(&setup, "networking-activation-failed");
    let parked = request_supervisor_status(&setup, "before-guarded-activation");
    let authority = guarded(resume.clone(), parked.broker_head.unwrap(), 2);
    setup.broker.wait_for_session_request();
    setup.broker.deliver_session_request(authority.clone());
    setup.broker.wait_for_session_receipt(1);
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(1),
        ));
    let failed = setup
        .broker
        .wait_for_session_response(&resume.request_id, 0);
    assert!(matches!(failed.result, ResponseResult::Error { .. }));
    assert!(!lock(&setup.platform.agent).guard_active);
    assert!(!lock(&setup.platform.gate_state()).enabled);
    assert_eq!(event_count(&setup.events, "capability.enable"), 2);
    setup.broker.wait_for_session_request();
    setup.broker.deliver_session_request(authority);
    assert_eq!(
        setup
            .broker
            .wait_for_session_response(&resume.request_id, 1),
        failed
    );
    assert_eq!(event_count(&setup.events, "agent.resume"), 1);
    assert_eq!(event_count(&setup.events, "agent.guard_activate"), 1);

    let status = request_supervisor_status(&setup, "before-activation-reconciliation");
    assert_eq!(status.state, SessionState::Parked);
    assert_eq!(status.pending_receipt_count, 1);
    setup.broker.wait_for_session_request();
    setup.broker.disconnect_session();
    setup.broker.wait_for_reconnect(0);
    let mut checkpoint = setup.broker.reconnect(0);
    let head = status.broker_head.unwrap();
    checkpoint.sequence = head.sequence;
    checkpoint.receipt_digest = head.digest;
    setup.broker.complete_reconnect(checkpoint);
    setup.broker.wait_for_session_receipt(2);
    let park = SignedReceipt::parse_canonical(&setup.broker.session_receipt_bytes(2)).unwrap();
    assert_eq!(park.payload.resulting_state, SessionState::Parked);
    assert_eq!(
        park.payload.outcome,
        ReceiptOutcome::Park {
            authority: ReceiptAuthority::Cause {
                cause: ReceiptCause::ResumeActivationFailed
            },
        }
    );
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(2),
        ));
    let reconciled = request_supervisor_status(&setup, "after-activation-reconciliation");
    assert_eq!(reconciled.pending_receipt_count, 0);
    assert_eq!(reconciled.state, SessionState::Parked);
    lock(&setup.platform.agent).guard_activation_fails = false;
    let mut fresh = resume_request(&setup, "fresh-after-activation-failure");
    fresh.expected_receipt_sequence = Some(park.payload.sequence);
    setup.broker.wait_for_session_request();
    setup.broker.deliver_session_request(guarded(
        fresh.clone(),
        ReceiptHead {
            sequence: park.payload.sequence,
            digest: park.digest().to_string(),
        },
        3,
    ));
    setup.broker.wait_for_session_receipt(3);
    assert!(!lock(&setup.platform.agent).guard_active);
    assert!(!lock(&setup.platform.gate_state()).enabled);
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(3),
        ));
    assert!(matches!(
        setup
            .broker
            .wait_for_session_response(&fresh.request_id, 0)
            .result,
        ResponseResult::Receipt { .. }
    ));
    assert!(lock(&setup.platform.agent).guard_active);
    assert!(lock(&setup.platform.gate_state()).enabled);
    assert_eq!(event_count(&setup.events, "agent.resume"), 2);
    finish_session_relay(&setup, input, receiver, worker);
}
