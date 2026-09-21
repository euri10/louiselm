//! Current conformance is enforced without relying on completed checks.

use super::*;

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "One Session proves approval, late-check expiry, revocation and replay ordering without automatic Resume."
)]
fn live_waiver_changes_reach_checks_without_resuming_or_accepting_old_revisions() {
    use louiselm_skills::{
        conformance::admission::{Attendance, Condition},
        launch_protocol::{ConformanceWaiver, WaiverChange},
    };
    let setup = setup(
        true,
        |authorization| authorization.conformance.attendance = Attendance::Interactive,
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    lock(&setup.platform.state).conformance_report = Some(b"admission observations".to_vec());
    let timer = Arc::new(FakeTimer::new(Arc::clone(&setup.events)));
    let supervisor = setup.fresh_supervisor_with_timer(timer.clone());
    let session = complete_launch_on(&setup, &supervisor);
    let (input, receiver, worker) = begin_session_relay(session);
    complete_check(
        &setup,
        0,
        Err(SupervisorError::ConformanceRefused(Condition::Missing)),
    );
    acknowledge_park(&setup);
    let change = WaiverChange {
        schema: "louiselm.launch.waiver-change/1".into(),
        protocol_version: PROTOCOL_VERSION,
        request_id: "approve-waiver".into(),
        session_id: setup.request.session_id.clone(),
        request_digest: setup.request.digest().to_string(),
        revision: 1,
        waiver: Some(ConformanceWaiver {
            session_id: setup.request.session_id.clone(),
            request_digest: setup.request.digest().to_string(),
            operator_uid: CONTROLLER_UID,
            condition: Condition::Missing,
            expires_at_ms: NOW_MS + 100,
            receipt_digest: Digest::of(b"durable operator decision").to_string(),
        }),
    };
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::WaiverChange(Box::new(change.clone())));
    let response = setup
        .broker
        .wait_for_session_response(&change.request_id, 0);
    assert!(matches!(
        response.result,
        ResponseResult::WaiverChanged { .. }
    ));
    wait_for_check(&setup, 1);
    assert_eq!(
        lock(&setup.platform.state).conformance_authorizations[1]
            .conformance
            .waiver,
        change.waiver
    );
    complete_check(
        &setup,
        1,
        Ok(ConformanceEvidence::Waived {
            condition: Condition::Missing,
            report_digest: None,
        }),
    );
    wait_for_update(&setup, 3);
    assert_eq!(
        request_supervisor_status(&setup, "waiver-does-not-resume").state,
        SessionState::Parked
    );
    setup.broker.wait_for_session_request();
    let resume = resume_request(&setup, "waiver-expiry-during-check");
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(resume.clone()));
    wait_for_check(&setup, 2);
    timer.advance(Duration::from_millis(100));
    complete_check(
        &setup,
        2,
        Ok(ConformanceEvidence::Waived {
            condition: Condition::Missing,
            report_digest: None,
        }),
    );
    assert!(matches!(
        setup
            .broker
            .wait_for_session_response(&resume.request_id, 0)
            .result,
        ResponseResult::Error { .. }
    ));
    assert_eq!(event_count(&setup.events, "agent.resume"), 0);
    let mut revoke = change.clone();
    revoke.request_id = "revoke-waiver".into();
    revoke.revision = 2;
    revoke.waiver = None;
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::WaiverChange(Box::new(revoke.clone())));
    assert!(matches!(
        setup
            .broker
            .wait_for_session_response(&revoke.request_id, 0)
            .result,
        ResponseResult::WaiverChanged { .. }
    ));
    wait_for_check(&setup, 3);
    assert!(
        lock(&setup.platform.state).conformance_authorizations[3]
            .conformance
            .waiver
            .is_none()
    );
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::WaiverChange(Box::new(change.clone())));
    assert!(matches!(
        setup
            .broker
            .wait_for_session_response(&change.request_id, 1)
            .result,
        ResponseResult::Error { .. }
    ));
    complete_check(
        &setup,
        3,
        Err(SupervisorError::ConformanceRefused(Condition::Missing)),
    );
    assert_eq!(event_count(&setup.events, "agent.resume"), 0);
    finish_session_relay(&setup, input, receiver, worker);
}

pub(super) fn real_tree_recovery(
    setup: &Setup,
    agent_pid: u32,
    invalid: &AtomicBool,
    receipts: &mut Vec<SignedReceipt>,
) {
    invalid.store(true, Ordering::SeqCst);
    acknowledge_park(setup);
    let park = SignedReceipt::parse_canonical(&setup.broker.session_receipt_bytes(0)).unwrap();
    assert!(matches!(
        park.payload.outcome,
        ReceiptOutcome::Park {
            authority: ReceiptAuthority::Cause {
                cause: ReceiptCause::ConformanceInvalid
            }
        }
    ));
    let membership = fs::read_to_string(format!("/proc/{agent_pid}/cgroup")).unwrap();
    let group = membership
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .unwrap();
    let events = Path::new("/sys/fs/cgroup")
        .join(group.trim_start_matches('/'))
        .join("cgroup.events");
    assert!(
        fs::read_to_string(events)
            .unwrap()
            .lines()
            .any(|line| line == "frozen 1")
    );
    receipts.push(park);
    invalid.store(false, Ordering::SeqCst);
    setup.broker.wait_for_session_request();
    let resume = resume_request(setup, "real-conformance-resume");
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
    let response = setup
        .broker
        .wait_for_session_response(&resume.request_id, 0);
    assert!(matches!(response.result, ResponseResult::Receipt { .. }));
    let status = request_supervisor_status(setup, "real-conformance-resumed");
    assert_eq!(status.state, SessionState::Running);
    assert_eq!(status.channel_state, ChannelState::Enabled);
    receipts.push(SignedReceipt::parse_canonical(&setup.broker.session_receipt_bytes(1)).unwrap());
    setup.broker.wait_for_session_request();
    let mut disposal = disposal_request(setup, "real-conformance-disposal");
    disposal.expected_receipt_sequence = Some(4);
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(disposal));
}

fn wait_for_check(setup: &Setup, index: usize) {
    let state = lock(&setup.platform.state);
    let (_state, timeout) = setup
        .platform
        .conformance_changed
        .wait_timeout_while(state, CALLBACK_TIMEOUT, |state| {
            state.conformance_checks.len() <= index
        })
        .unwrap();
    assert!(
        !timeout.timed_out(),
        "current check {index} was not started"
    );
}

fn complete_check(
    setup: &Setup,
    index: usize,
    result: Result<ConformanceEvidence, SupervisorError>,
) {
    wait_for_check(setup, index);
    let callback = lock(&setup.platform.state).conformance_checks[index]
        .take()
        .unwrap();
    thread::spawn(move || callback(result)).join().unwrap();
}

fn passing_check() -> ConformanceEvidence {
    ConformanceEvidence::Certified {
        report_digest: Digest::of(b"admission observations").to_string(),
    }
}

fn acknowledge_park(setup: &Setup) {
    setup.broker.wait_for_session_receipt(0);
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(0),
        ));
    request_supervisor_status(setup, "conformance-park-acknowledged");
}

fn wait_for_update(setup: &Setup, count: usize) {
    let state = lock(&setup.broker.state);
    let (_state, timeout) = setup
        .broker
        .changed
        .wait_timeout_while(state, CALLBACK_TIMEOUT, |state| {
            state.conformance_updates.len() < count
        })
        .unwrap();
    assert!(
        !timeout.timed_out(),
        "supervisor did not publish current conformance"
    );
}

#[test]
fn missing_current_checks_suspend_a_certified_session_after_five_seconds() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    // This platform double owns admission; these opaque bytes are never passed
    // off as installed observations. Broker report validation has separate tests.
    lock(&setup.platform.state).conformance_report = Some(b"admission observations".to_vec());
    let timer = Arc::new(FakeTimer::new(Arc::clone(&setup.events)));
    let supervisor = setup.fresh_supervisor_with_timer(timer.clone());
    let session = complete_launch_on(&setup, &supervisor);
    let (input, receiver, worker) = begin_session_relay(session);
    wait_for_check(&setup, 0);

    timer.advance(Duration::from_secs(5));
    let status = request_supervisor_status(&setup, "conformance-deadline");
    // The unsolicited Park must be durable before exit.
    if status.state == SessionState::Parked {
        acknowledge_park(&setup);
    }
    complete_check(&setup, 0, Ok(passing_check()));
    finish_session_relay(&setup, input, receiver, worker);
    assert_eq!(status.state, SessionState::Parked);
    assert_eq!(status.channel_state, ChannelState::Revoked);
}

#[test]
fn blocked_checker_under_request_load_cannot_delay_suspension_or_disposal() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    {
        let mut state = lock(&setup.platform.state);
        state.conformance_report = Some(b"admission observations".to_vec());
        state.conformance_blocked = true;
    }
    let timer = Arc::new(FakeTimer::new(Arc::clone(&setup.events)));
    let supervisor = setup.fresh_supervisor_with_timer(timer.clone());
    let session = complete_launch_on(&setup, &supervisor);
    let (input, receiver, worker) = begin_session_relay(session);
    wait_for_check(&setup, 0);
    // The producer method itself is stuck, not merely its callback. Lifecycle
    // and expiry still progress independently under repeated request traffic.
    for index in 0..50 {
        timer.advance(Duration::from_millis(100));
        request_supervisor_status(&setup, &format!("loaded-check-{index}"));
    }
    acknowledge_park(&setup);
    setup.broker.wait_for_session_request();
    let resume = resume_request(&setup, "blocked-check-resume");
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(resume.clone()));
    let waiting = request_supervisor_status(&setup, "blocked-resume-pending");
    setup.broker.wait_for_session_request();
    let mut disposal = disposal_request(&setup, "dispose-during-blocked-check");
    disposal.expected_state = SessionState::Parked;
    disposal.expected_receipt_sequence = Some(2);
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(disposal));
    let cancelled = setup
        .broker
        .wait_for_session_response(&resume.request_id, 0);
    setup.broker.wait_for_session_receipt(1);
    let terminal = request_supervisor_status(&setup, "disposed-with-check-still-blocked");
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(1),
        ));
    // Join ownership is retained until the read-only worker returns. Agent
    // termination and capability withdrawal never wait for it.
    complete_check(&setup, 0, Ok(passing_check()));
    lock(&setup.platform.state).conformance_blocked = false;
    setup.platform.conformance_changed.notify_all();
    finish_terminal_session_relay(input, receiver, worker);
    assert_eq!(waiting.state, SessionState::Parked);
    assert!(waiting.pending_operation.is_some());
    assert!(
        matches!(cancelled.result, ResponseResult::Error { error } if error.code == ErrorCode::ConformanceUnavailable)
    );
    assert_eq!(terminal.state, SessionState::Terminal);
    assert_eq!(event_count(&setup.events, "agent.resume"), 0);
    assert_eq!(event_count(&setup.events, "identity.release"), 1);
}

#[test]
fn fresh_resume_check_timeout_rejects_late_success() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    lock(&setup.platform.state).conformance_report = Some(b"admission observations".to_vec());
    let timer = Arc::new(FakeTimer::new(Arc::clone(&setup.events)));
    let supervisor = setup.fresh_supervisor_with_timer(timer.clone());
    let session = complete_launch_on(&setup, &supervisor);
    let (input, receiver, worker) = begin_session_relay(session);
    complete_check(&setup, 0, Err(SupervisorError::ConformanceUnavailable));
    acknowledge_park(&setup);
    setup.broker.wait_for_session_request();
    let resume = resume_request(&setup, "timed-out-current-check");
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(resume.clone()));
    wait_for_check(&setup, 1);
    timer.advance(SUPERVISOR_TIMEOUT.min(Duration::from_secs(5)));
    let response = setup
        .broker
        .wait_for_session_response(&resume.request_id, 0);
    complete_check(&setup, 1, Ok(passing_check()));
    wait_for_update(&setup, 2);
    let status = request_supervisor_status(&setup, "late-check-after-resume-timeout");
    finish_session_relay(&setup, input, receiver, worker);
    assert!(
        matches!(response.result, ResponseResult::Error { error } if error.code == ErrorCode::ConformanceUnavailable)
    );
    assert_eq!(status.state, SessionState::Parked);
    assert_eq!(status.channel_state, ChannelState::Revoked);
    assert_eq!(event_count(&setup.events, "agent.resume"), 0);
}

#[test]
fn invalidation_during_resume_rejects_late_signature_or_ack_without_duplicate_parks() {
    for hold_signature in [true, false] {
        let setup = setup(
            true,
            |_| {},
            AppendBehavior::Hold,
            PlatformBehavior::default(),
            SUPERVISOR_TIMEOUT,
        );
        lock(&setup.platform.state).conformance_report = Some(b"admission observations".to_vec());
        let timer = Arc::new(FakeTimer::new(Arc::clone(&setup.events)));
        let supervisor = setup.fresh_supervisor_with_timer(timer.clone());
        let session = complete_launch_on(&setup, &supervisor);
        let (input, receiver, worker) = begin_session_relay(session);
        complete_check(&setup, 0, Err(SupervisorError::ConformanceUnavailable));
        acknowledge_park(&setup);
        if hold_signature {
            setup.signer.hold_on_call(setup.signer.payloads().len());
        }
        setup.broker.wait_for_session_request();
        let resume = resume_request(&setup, "invalidated-pending-resume");
        setup
            .broker
            .deliver_session_request(ProtocolMessage::Lifecycle(resume.clone()));
        complete_check(&setup, 1, Ok(passing_check()));
        if hold_signature {
            setup.signer.wait_for_held_call();
        } else {
            setup.broker.wait_for_session_receipt(1);
        }
        timer.advance(Duration::from_secs(5));
        let refused = setup
            .broker
            .wait_for_session_response(&resume.request_id, 0);
        if hold_signature {
            setup.signer.release_held_call();
        } else {
            setup.broker.wait_for_session_request();
            setup
                .broker
                .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
                    setup.broker.session_receipt_acknowledgement(1),
                ));
        }
        let status = request_supervisor_status(&setup, "late-resume-completion");
        complete_check(&setup, 2, Ok(passing_check()));
        finish_session_relay_after_reconciling_backlog(&setup, input, receiver, worker, 2);
        assert!(
            matches!(refused.result, ResponseResult::Error { error } if error.code == ErrorCode::ConformanceUnavailable)
        );
        assert_eq!(status.state, SessionState::Parked);
        assert_eq!(status.channel_state, ChannelState::Revoked);
        assert_eq!(event_count(&setup.events, "capability.enable"), 1);
    }
}

#[test]
fn failed_freeze_attempts_termination_and_never_releases_unproven_identity() {
    for cleanup_fails in [false, true] {
        let setup = setup(
            true,
            |_| {},
            AppendBehavior::Hold,
            PlatformBehavior {
                park_fails: true,
                dispose_fails: cleanup_fails,
                ..PlatformBehavior::default()
            },
            SUPERVISOR_TIMEOUT,
        );
        lock(&setup.platform.state).conformance_report = Some(b"admission observations".to_vec());
        let session = complete_launch(&setup);
        let (input, receiver, worker) = begin_session_relay(session);
        complete_check(&setup, 0, Err(SupervisorError::ConformanceUnavailable));
        let failure = receiver.recv_timeout(CALLBACK_TIMEOUT).unwrap();
        drop(input);
        worker.join().unwrap();
        assert_eq!(
            failure,
            Err(if cleanup_fails {
                SupervisorError::CleanupUnproven
            } else {
                SupervisorError::LifecycleMechanicUnavailable
            })
        );
        assert!(event_count(&setup.events, "agent.dispose") >= 1);
        assert_eq!(
            event_count(&setup.events, "identity.release"),
            usize::from(!cleanup_fails)
        );
        assert_eq!(
            event_count(&setup.events, "identity.poison"),
            usize::from(cleanup_fails)
        );
        assert_eq!(setup.broker.session_receipt_count(), 0);
        let events = event_snapshot(&setup.events);
        let revoked = events
            .iter()
            .position(|event| event == "capability.revoke")
            .unwrap();
        let frozen = events
            .iter()
            .position(|event| event == "agent.park")
            .unwrap();
        let terminated = events
            .iter()
            .position(|event| event == "agent.dispose")
            .unwrap();
        assert!(revoked < frozen && frozen < terminated);
    }
}

#[test]
fn pre_cutover_sessions_do_not_start_checks_or_acquire_a_freshness_deadline() {
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
    let (input, receiver, worker) = begin_session_relay(session);
    timer.advance(Duration::from_secs(20));
    let status = request_supervisor_status(&setup, "ordinary-session");
    let checks = lock(&setup.platform.state).conformance_checks.len();
    finish_session_relay(&setup, input, receiver, worker);
    assert_eq!(status.state, SessionState::Running);
    assert_eq!(status.channel_state, ChannelState::Enabled);
    assert_eq!(checks, 0);
}

#[test]
fn checks_run_every_second_and_late_success_cannot_resume_after_a_stall() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    lock(&setup.platform.state).conformance_report = Some(b"admission observations".to_vec());
    let timer = Arc::new(FakeTimer::new(Arc::clone(&setup.events)));
    let supervisor = setup.fresh_supervisor_with_timer(timer.clone());
    let session = complete_launch_on(&setup, &supervisor);
    let (input, receiver, worker) = begin_session_relay(session);
    complete_check(&setup, 0, Ok(passing_check()));
    // Wait for the owner to consume the result, without using a status read as
    // either a producer or the deadline trigger.
    wait_for_update(&setup, 1);
    timer.advance(Duration::from_secs(1));
    wait_for_check(&setup, 1);
    timer.advance(Duration::from_millis(3_999));
    let before = request_supervisor_status(&setup, "before-freshness-deadline");
    timer.advance(Duration::from_millis(1));
    setup.broker.wait_for_session_receipt(0);
    let after = request_supervisor_status(&setup, "after-freshness-deadline");
    acknowledge_park(&setup);
    complete_check(&setup, 1, Ok(passing_check()));
    wait_for_update(&setup, 3);
    let late = request_supervisor_status(&setup, "after-late-check");
    finish_session_relay(&setup, input, receiver, worker);
    assert_eq!(before.state, SessionState::Running);
    assert_eq!(after.state, SessionState::Parked);
    assert_eq!(late.state, SessionState::Parked);
    assert_eq!(late.channel_state, ChannelState::Revoked);
}

#[test]
fn evidence_only_suspension_requires_a_new_check_after_operator_resume() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    lock(&setup.platform.state).conformance_report = Some(b"admission observations".to_vec());
    let timer = Arc::new(FakeTimer::new(Arc::clone(&setup.events)));
    let supervisor = setup.fresh_supervisor_with_timer(timer.clone());
    let session = complete_launch_on(&setup, &supervisor);
    let (input, receiver, worker) = begin_session_relay(session);
    complete_check(
        &setup,
        0,
        Err(SupervisorError::ConformanceRefused(
            louiselm_skills::conformance::admission::Condition::Stale,
        )),
    );
    acknowledge_park(&setup);
    timer.advance(Duration::from_secs(1));
    complete_check(&setup, 1, Ok(passing_check()));
    wait_for_update(&setup, 2);
    let recertified = request_supervisor_status(&setup, "recertification-does-not-resume");
    setup.broker.wait_for_session_request();
    let resume = resume_request(&setup, "manual-conformance-resume");
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(resume.clone()));
    wait_for_check(&setup, 2);
    let waiting = request_supervisor_status(&setup, "resume-waits-for-new-check");
    complete_check(&setup, 2, Ok(passing_check()));
    setup.broker.wait_for_session_receipt(1);
    setup.broker.wait_for_session_request();
    setup
        .broker
        .deliver_session_request(ProtocolMessage::ReceiptAcknowledgement(
            setup.broker.session_receipt_acknowledgement(1),
        ));
    let response = setup
        .broker
        .wait_for_session_response(&resume.request_id, 0);
    let resumed = request_supervisor_status(&setup, "authorized-resume-complete");
    finish_session_relay(&setup, input, receiver, worker);
    assert_eq!(recertified.state, SessionState::Parked);
    assert_eq!(recertified.channel_state, ChannelState::Revoked);
    assert_eq!(waiting.state, SessionState::Parked);
    assert!(waiting.pending_operation.is_some());
    assert!(matches!(response.result, ResponseResult::Receipt { .. }));
    assert_eq!(resumed.state, SessionState::Running);
    assert_eq!(resumed.channel_state, ChannelState::Enabled);
}

#[test]
fn containment_failure_cannot_resume_even_after_successful_recertification() {
    let setup = setup(
        true,
        |_| {},
        AppendBehavior::Hold,
        PlatformBehavior::default(),
        SUPERVISOR_TIMEOUT,
    );
    lock(&setup.platform.state).conformance_report = Some(b"admission observations".to_vec());
    let timer = Arc::new(FakeTimer::new(Arc::clone(&setup.events)));
    let supervisor = setup.fresh_supervisor_with_timer(timer.clone());
    let session = complete_launch_on(&setup, &supervisor);
    let (input, receiver, worker) = begin_session_relay(session);
    complete_check(
        &setup,
        0,
        Err(SupervisorError::ConformanceRefused(
            louiselm_skills::conformance::admission::Condition::ContainmentFailure,
        )),
    );
    acknowledge_park(&setup);
    timer.advance(Duration::from_secs(1));
    complete_check(&setup, 1, Ok(passing_check()));
    wait_for_update(&setup, 2);
    setup.broker.wait_for_session_request();
    let resume = resume_request(&setup, "cannot-reuse-failed-containment");
    setup
        .broker
        .deliver_session_request(ProtocolMessage::Lifecycle(resume.clone()));
    let response = setup
        .broker
        .wait_for_session_response(&resume.request_id, 0);
    let status = request_supervisor_status(&setup, "containment-still-failed");
    finish_session_relay(&setup, input, receiver, worker);
    assert!(
        matches!(response.result, ResponseResult::Error { error } if error.code == ErrorCode::ConformanceUnavailable)
    );
    assert_eq!(status.state, SessionState::Parked);
    assert_eq!(status.channel_state, ChannelState::Revoked);
}
