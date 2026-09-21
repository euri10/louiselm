//! Authenticated checks reach canonical posture, expire and survive broker restart.

use super::*;
use louiselm_skills::launch_protocol::{
    CONFORMANCE_UPDATE_SCHEMA, ConformanceCheck, ConformanceFailure, ConformanceUpdate,
    FreshnessBasis,
};

pub(super) fn update(
    authorization: &LaunchAuthorization,
    evidence: ConformanceEvidence,
) -> ConformanceUpdate {
    ConformanceUpdate {
        waiver_revision: 0,
        schema: CONFORMANCE_UPDATE_SCHEMA.into(),
        session_id: authorization.session_id.clone(),
        run_id: authorization.run_id.clone(),
        authorization_id: authorization.authorization_id.clone(),
        request_digest: authorization.request_digest.clone(),
        envelope_revision: authorization.envelope_revision,
        sequence: 1,
        observed_at_ms: 90_000,
        last_success_at_ms: Some(90_000),
        suspended: false,
        check: ConformanceCheck::Current { evidence },
    }
}

fn isolation(status: &SessionStatus) -> &louiselm_skills::launch_protocol::DimensionStatus {
    status
        .posture
        .dimensions
        .iter()
        .find(|row| row.dimension == DimensionName::Isolation)
        .unwrap()
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "Ordered authenticated peer exchange proves the decision reaches current posture without rewriting admission."
)]
fn operator_approval_reaches_current_posture_without_rewriting_admission() {
    use louiselm_skills::broker::waiver::{Proposal, Request};
    let root = TempDir::new().unwrap();
    let admission = ConformanceEvidence::Waived {
        condition: Condition::Missing,
        report_digest: None,
    };
    let (service, authorization, launch, start) = retained_service(root.path(), &admission, &[]);
    let mut failed = update(&authorization, admission.clone());
    failed.observed_at_ms = 20_000;
    failed.last_success_at_ms = None;
    failed.suspended = true;
    failed.check = ConformanceCheck::Invalid {
        failure: ConformanceFailure::Condition(Condition::Stale),
    };
    let mut mechanical = lifecycle::status(&authorization);
    let head = ReceiptHead {
        sequence: 1,
        digest: start.digest().to_string(),
    };
    mechanical.broker_head = Some(head.clone());
    mechanical.launcher_head = Some(head);
    let offer = reconnect::checkpoint(&start);
    let peer_root = root.path().to_owned();
    let peer = thread::spawn(move || {
        let channel = reconnect::peer(&peer_root, &offer);
        settle(|done| channel.receive(done));
        settle(|done| channel.send(failed.canonical_bytes().unwrap(), done));
        for _ in 0..2 {
            lifecycle::answer_one_status_query(&channel, &mechanical);
        }
        let packet = settle(|done| channel.receive(done));
        let LauncherPacket::Request(ProtocolMessage::WaiverChange(change)) = packet.packet else {
            panic!("expected authoritative waiver change")
        };
        assert_eq!(change.waiver.as_ref().unwrap().condition, Condition::Stale);
        let reply = ProtocolResponse {
            schema: RESPONSE_SCHEMA.into(),
            protocol_version: PROTOCOL_VERSION,
            request_id: change.request_id.clone(),
            result: louiselm_skills::launch_protocol::ResponseResult::WaiverChanged {
                change: change.clone(),
            },
        };
        settle(|done| channel.send(reply.canonical_bytes(), done));
        failed.sequence = 2;
        failed.waiver_revision = change.revision;
        failed.observed_at_ms = 20_100;
        failed.last_success_at_ms = Some(20_100);
        failed.check = ConformanceCheck::Current {
            evidence: ConformanceEvidence::Waived {
                condition: Condition::Stale,
                report_digest: None,
            },
        };
        settle(|done| channel.send(failed.canonical_bytes().unwrap(), done));
        lifecycle::answer_one_status_query(&channel, &mechanical);
    });
    let mut session = service
        .serve_reconnect(20_000, verify_fixture_signature)
        .unwrap();
    service
        .step(&mut session, 20_000, None, verify_fixture_signature)
        .unwrap();
    let plan = service
        .waiver_control(
            &mut session,
            authorization.controller_uid,
            &Request::Plan {
                proposal: Proposal {
                    request_id: "review-current-failure".into(),
                    condition: Condition::Stale,
                    rationale: "Investigate this exact host".into(),
                    expires_at_ms: 40_000,
                },
            },
            20_001,
            verify_fixture_signature,
        )
        .unwrap()
        .plan
        .unwrap();
    let result = service
        .waiver_control(
            &mut session,
            authorization.controller_uid,
            &Request::Apply {
                plan_digest: plan.digest,
            },
            20_002,
            verify_fixture_signature,
        )
        .unwrap();
    assert!(result.active);
    service
        .step(&mut session, 20_100, None, verify_fixture_signature)
        .unwrap();
    let status = service
        .session_status(
            &mut session,
            &LifecycleCaller::Agent,
            20_101,
            verify_fixture_signature,
        )
        .unwrap();
    assert_eq!(
        isolation(&status).state,
        louiselm_skills::posture::DimensionState::Waived
    );
    assert_eq!(status.conformance_admission, admission);
    assert_eq!(
        service
            .receipts()
            .stored_bytes(&authorization.session_id)
            .unwrap()[0],
        launch.canonical_bytes()
    );
    peer.join().unwrap();
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "The ordered peer scenario verifies expiry, retained failure inspection and immutable admission in one Session lifetime."
)]
fn authenticated_checks_expire_without_reads_renewing_or_rewriting_admission() {
    let root = TempDir::new().unwrap();
    let bytes = observations().canonical_bytes().unwrap();
    let decision = certified(&bytes);
    let (service, authorization, launch, start) = retained_service(root.path(), &decision, &bytes);
    let current = update(&authorization, decision.clone());
    let mut failed = current.clone();
    failed.sequence = 2;
    failed.observed_at_ms = 96_000;
    failed.suspended = true;
    failed.check = ConformanceCheck::Invalid {
        failure: ConformanceFailure::Condition(Condition::ContainmentFailure),
    };
    let mut status = lifecycle::status(&authorization);
    let head = ReceiptHead {
        sequence: 1,
        digest: start.digest().to_string(),
    };
    status.broker_head = Some(head.clone());
    status.launcher_head = Some(head);
    let offer = reconnect::checkpoint(&start);
    let peer_root = root.path().to_owned();
    let peer = thread::spawn(move || {
        let channel = reconnect::peer(&peer_root, &offer);
        settle(|done| channel.receive(done));
        settle(|done| channel.send(current.canonical_bytes().unwrap(), done));
        for _ in 0..3 {
            lifecycle::answer_one_status_query(&channel, &status);
        }
        settle(|done| channel.send(failed.canonical_bytes().unwrap(), done));
        lifecycle::answer_one_status_query(&channel, &status);
    });
    let mut session = service
        .serve_reconnect(90_000, verify_fixture_signature)
        .unwrap();
    service
        .step(&mut session, 90_000, None, verify_fixture_signature)
        .unwrap();
    let mut results = Vec::new();
    for at in [90_001, 94_999, 95_000] {
        results.push(
            service
                .session_status(
                    &mut session,
                    &LifecycleCaller::Agent,
                    at,
                    verify_fixture_signature,
                )
                .unwrap(),
        );
    }
    service
        .step(&mut session, 96_000, None, verify_fixture_signature)
        .unwrap();
    let inspected = service
        .inspect_conformance(&authorization.session_id)
        .unwrap()
        .unwrap();
    let observed = inspected.last_check.as_ref().unwrap();
    assert_eq!(observed.sequence, 2);
    assert_eq!(observed.observed_at_ms, 96_000);
    assert_eq!(observed.last_success_at_ms, Some(90_000));
    assert!(observed.suspended);
    assert_eq!(
        observed.check,
        ConformanceCheck::Invalid {
            failure: ConformanceFailure::Condition(Condition::ContainmentFailure),
        }
    );
    assert_eq!(inspected.admission, decision);
    assert_eq!(
        inspected.report.as_deref().map(str::as_bytes),
        Some(bytes.as_slice())
    );
    let failed = service
        .session_status(
            &mut session,
            &LifecycleCaller::Agent,
            96_001,
            verify_fixture_signature,
        )
        .unwrap();
    peer.join().unwrap();
    assert_eq!(
        service
            .inspect_conformance(&authorization.session_id)
            .unwrap(),
        Some(inspected)
    );
    assert_eq!(isolation(&results[0]).state, DimensionState::Verified);
    assert_eq!(isolation(&results[1]).state, DimensionState::Verified);
    assert_eq!(
        isolation(&results[0]).freshness.basis,
        FreshnessBasis::Check
    );
    assert_eq!(
        isolation(&results[2]).failure_code,
        Some(FailureCode::EvidenceInvalidated)
    );
    assert_eq!(
        isolation(&failed).failure_code,
        Some(FailureCode::IsolationFailed)
    );
    assert_eq!(
        isolation(&failed).freshness.last_verified_at_ms,
        Some(90_000)
    );
    for result in results.iter().chain([&failed]) {
        assert_eq!(result.conformance_admission, decision);
        assert_eq!(
            SessionStatus::parse_canonical(&result.canonical_bytes()).unwrap(),
            *result
        );
        assert!(result.allowed_actions.is_empty());
    }
    assert_eq!(
        service
            .receipts()
            .stored_bytes(&authorization.session_id)
            .unwrap(),
        vec![launch.canonical_bytes(), start.canonical_bytes()]
    );
}

#[test]
fn foreign_and_out_of_order_updates_cannot_replace_retained_facts() {
    for foreign in [true, false] {
        let root = TempDir::new().unwrap();
        let bytes = observations().canonical_bytes().unwrap();
        let decision = certified(&bytes);
        let (service, authorization, _, start) = retained_service(root.path(), &decision, &bytes);
        let first = update(&authorization, decision);
        let mut invalid = first.clone();
        if foreign {
            invalid.authorization_id = "foreign-authorization".into();
        } else {
            invalid.check = ConformanceCheck::Invalid {
                failure: ConformanceFailure::Unavailable,
            };
            invalid.suspended = true;
        }
        let offer = reconnect::checkpoint(&start);
        let peer_root = root.path().to_owned();
        let peer = thread::spawn(move || {
            let channel = reconnect::peer(&peer_root, &offer);
            settle(|done| channel.receive(done));
            settle(|done| channel.send(first.canonical_bytes().unwrap(), done));
            settle(|done| channel.send(invalid.canonical_bytes().unwrap(), done));
        });
        let mut session = service
            .serve_reconnect(90_000, verify_fixture_signature)
            .unwrap();
        service
            .step(&mut session, 90_000, None, verify_fixture_signature)
            .unwrap();
        assert!(
            service
                .step(&mut session, 90_001, None, verify_fixture_signature)
                .is_err()
        );
        peer.join().unwrap();
    }
}

#[test]
fn reconnect_preserves_source_time_and_missing_or_corrupt_records_fail_closed() {
    for damage in [None, Some("missing"), Some("corrupt")] {
        let root = TempDir::new().unwrap();
        let bytes = observations().canonical_bytes().unwrap();
        let decision = certified(&bytes);
        let (service, authorization, _, start) = retained_service(root.path(), &decision, &bytes);
        let first = update(&authorization, decision);
        let offer = reconnect::checkpoint(&start);
        let peer_root = root.path().to_owned();
        let peer = thread::spawn(move || {
            let channel = reconnect::peer(&peer_root, &offer);
            settle(|done| channel.receive(done));
            settle(|done| channel.send(first.canonical_bytes().unwrap(), done));
            // Exact replay is accepted without changing its source timestamp.
            settle(|done| channel.send(first.canonical_bytes().unwrap(), done));
        });
        let mut session = service
            .serve_reconnect(90_000, verify_fixture_signature)
            .unwrap();
        for at in [90_000, 90_001] {
            service
                .step(&mut session, at, None, verify_fixture_signature)
                .unwrap();
        }
        peer.join().unwrap();
        drop(session);
        drop(service);
        // The fixture has joined its sole peer and dropped its listener. Remove
        // only this now-unowned disposable rendezvous before rebinding it.
        fs::remove_file(root.path().join("broker.sock")).unwrap();
        let record = fs::read_dir(root.path().join("receipts/current-conformance"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        match damage {
            Some("missing") => fs::remove_file(&record).unwrap(),
            Some(_) => fs::write(&record, b"not a retained source record").unwrap(),
            None => {}
        }
        let service = BrokerService::bind(
            &root.path().join("broker.sock"),
            AuthorizationStore::open(&root.path().join("authorizations"), pool(4)).unwrap(),
            ReceiptStore::open(&root.path().join("receipts"), trusted_release()).unwrap(),
            AuditLog::open(&root.path().join("audit")).unwrap(),
            local_pin(),
        )
        .unwrap();
        let offer = reconnect::checkpoint(&start);
        let mut mechanical = lifecycle::status(&authorization);
        let head = ReceiptHead {
            sequence: 1,
            digest: start.digest().to_string(),
        };
        mechanical.broker_head = Some(head.clone());
        mechanical.launcher_head = Some(head);
        let peer_root = root.path().to_owned();
        let peer = thread::spawn(move || {
            let channel = reconnect::peer(&peer_root, &offer);
            settle(|done| channel.receive(done));
            lifecycle::answer_one_status_query(&channel, &mechanical);
        });
        let mut session = service
            .serve_reconnect(96_000, verify_fixture_signature)
            .unwrap();
        let status = service
            .session_status(
                &mut session,
                &LifecycleCaller::Agent,
                96_000,
                verify_fixture_signature,
            )
            .unwrap();
        peer.join().unwrap();
        assert_eq!(isolation(&status).state, DimensionState::Failed);
        assert_eq!(
            isolation(&status).freshness.last_verified_at_ms,
            damage.is_none().then_some(90_000)
        );
        assert_eq!(
            isolation(&status).failure_code,
            Some(if damage.is_none() {
                FailureCode::EvidenceInvalidated
            } else {
                FailureCode::EvidenceMissing
            })
        );
    }
}

#[test]
fn a_current_waiver_expires_and_cannot_hide_a_new_containment_failure() {
    let root = TempDir::new().unwrap();
    let decision = ConformanceEvidence::Waived {
        condition: Condition::Missing,
        report_digest: None,
    };
    let (service, authorization, _, start) = retained_service(root.path(), &decision, &[]);
    let mut current = update(&authorization, decision);
    current.observed_at_ms = 29_000;
    current.last_success_at_ms = Some(29_000);
    let mut failed = current.clone();
    failed.sequence = 2;
    failed.observed_at_ms = 30_001;
    failed.suspended = true;
    failed.check = ConformanceCheck::Invalid {
        failure: ConformanceFailure::Condition(Condition::ContainmentFailure),
    };
    let offer = reconnect::checkpoint(&start);
    let mut mechanical = lifecycle::status(&authorization);
    let head = ReceiptHead {
        sequence: 1,
        digest: start.digest().to_string(),
    };
    mechanical.broker_head = Some(head.clone());
    mechanical.launcher_head = Some(head);
    let peer_root = root.path().to_owned();
    let peer = thread::spawn(move || {
        let channel = reconnect::peer(&peer_root, &offer);
        settle(|done| channel.receive(done));
        settle(|done| channel.send(current.canonical_bytes().unwrap(), done));
        for _ in 0..2 {
            lifecycle::answer_one_status_query(&channel, &mechanical);
        }
        settle(|done| channel.send(failed.canonical_bytes().unwrap(), done));
        lifecycle::answer_one_status_query(&channel, &mechanical);
    });
    let mut session = service
        .serve_reconnect(29_000, verify_fixture_signature)
        .unwrap();
    service
        .step(&mut session, 29_000, None, verify_fixture_signature)
        .unwrap();
    let before = service
        .session_status(
            &mut session,
            &LifecycleCaller::Agent,
            29_999,
            verify_fixture_signature,
        )
        .unwrap();
    let expired = service
        .session_status(
            &mut session,
            &LifecycleCaller::Agent,
            30_000,
            verify_fixture_signature,
        )
        .unwrap();
    service
        .step(&mut session, 30_001, None, verify_fixture_signature)
        .unwrap();
    let failed = service
        .session_status(
            &mut session,
            &LifecycleCaller::Agent,
            30_002,
            verify_fixture_signature,
        )
        .unwrap();
    peer.join().unwrap();
    assert_eq!(isolation(&before).state, DimensionState::Waived);
    assert_eq!(
        isolation(&expired).failure_code,
        Some(FailureCode::EvidenceInvalidated)
    );
    assert_eq!(
        isolation(&failed).failure_code,
        Some(FailureCode::IsolationFailed)
    );
}

#[test]
fn command_admission_requires_fresh_unsuspended_source_evidence() {
    use louiselm_skills::launch_protocol::{
        CommandPrincipal, TOOL_EXECUTION_SCHEMA, ToolExecutionRequest,
    };
    for mode in 0..4 {
        let root = TempDir::new().unwrap();
        let bytes = observations().canonical_bytes().unwrap();
        let decision = certified(&bytes);
        let (service, authorization, _, start) = retained_service(root.path(), &decision, &bytes);
        let now = u64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis(),
        )
        .unwrap();
        let mut current = update(&authorization, decision);
        current.observed_at_ms = if mode == 1 { now - 5_000 } else { now };
        current.last_success_at_ms = Some(current.observed_at_ms);
        current.suspended = mode == 2;
        let query = CommandMessage {
            schema: COMMAND_SCHEMA.into(),
            protocol_version: PROTOCOL_VERSION,
            request_id: "check-command".into(),
            session_id: authorization.session_id.clone(),
            run_id: authorization.run_id.clone(),
            envelope_revision: authorization.envelope_revision,
            operation: CommandOperation::Request {
                principal: CommandPrincipal {
                    channel_id: "agent-capability".into(),
                    pid: 123,
                    uid: authorization.assigned_uid,
                    gid: authorization.assigned_gid,
                },
                command: ToolExecutionRequest {
                    schema: TOOL_EXECUTION_SCHEMA.into(),
                    protocol_version: PROTOCOL_VERSION,
                    request_id: "check-command".into(),
                    session_id: authorization.session_id.clone(),
                    run_id: authorization.run_id.clone(),
                    envelope_revision: authorization.envelope_revision,
                    sequence: 1,
                    command: "true".into(),
                    timeout_ms: 1_000,
                },
            },
        };
        let offer = reconnect::checkpoint(&start);
        let peer_root = root.path().to_owned();
        let peer = thread::spawn(move || {
            let channel = reconnect::peer(&peer_root, &offer);
            settle(|done| channel.receive(done));
            if mode != 0 {
                settle(|done| channel.send(current.canonical_bytes().unwrap(), done));
            }
            settle(|done| channel.send(query.canonical_bytes(), done));
            settle(|done| channel.receive(done))
        });
        let mut session = service
            .serve_reconnect(now, verify_fixture_signature)
            .unwrap();
        if mode != 0 {
            service
                .step(&mut session, now, None, verify_fixture_signature)
                .unwrap();
        }
        session.serve_command().unwrap();
        let reply = peer.join().unwrap();
        let LauncherPacket::Request(ProtocolMessage::Command(reply)) = reply.packet else {
            panic!("typed command reply");
        };
        // Fresh evidence reaches the ordinary policy boundary, where this
        // fixture has deliberately supplied no command authority.
        assert_eq!(
            reply.operation,
            CommandOperation::Reject {
                error: if mode == 3 {
                    ErrorCode::InvalidRequest
                } else {
                    ErrorCode::ConformanceUnavailable
                }
            }
        );
    }
}
