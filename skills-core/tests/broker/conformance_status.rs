//! Admission history survives restart without becoming current posture.

use super::*;
#[path = "current_conformance.rs"]
mod current;
use louiselm_skills::{
    broker::lifecycle::LifecycleCaller,
    launch_protocol::{COMMAND_SCHEMA, CommandMessage, CommandOperation, SessionStatus},
    launch_receipt::ReceiptHead,
    posture::{DimensionName, DimensionState, FailureCode},
};

#[test]
fn admission_history_is_default_read_only_and_identical_for_operator_and_agent() {
    let bytes = observations().canonical_bytes().unwrap();
    for decision in [
        ConformanceEvidence::Unevaluated,
        certified(&bytes),
        ConformanceEvidence::Waived {
            condition: Condition::Missing,
            report_digest: None,
        },
        ConformanceEvidence::Waived {
            condition: Condition::Stale,
            report_digest: Some(Digest::of(&bytes).to_string()),
        },
        ConformanceEvidence::Waived {
            condition: Condition::Incomplete,
            report_digest: None,
        },
    ] {
        assert_admission_status(&decision, &bytes);
    }
}

fn retained_service(
    root: &Path,
    decision: &ConformanceEvidence,
    bytes: &[u8],
) -> (
    BrokerService,
    LaunchAuthorization,
    SignedReceipt,
    SignedReceipt,
) {
    let authorization = authorized_admission(root, &request("admission-status"), decision);
    let launch = admission_receipt(&authorization, decision.clone());
    let start = start_receipt(&authorization, &launch);
    let path = root.join("receipts");
    let receipts = ReceiptStore::open(&path, trusted_release()).unwrap();
    let report = match decision {
        ConformanceEvidence::Certified { .. }
        | ConformanceEvidence::Waived {
            report_digest: Some(_),
            ..
        } => Some(bytes),
        _ => None,
    };
    receipts
        .append(
            &authorization,
            &launch.canonical_bytes(),
            report,
            verify_fixture_signature,
        )
        .unwrap();
    receipts
        .append(
            &authorization,
            &start.canonical_bytes(),
            None,
            verify_fixture_signature,
        )
        .unwrap();
    drop(receipts);
    let service = BrokerService::bind(
        &root.join("broker.sock"),
        AuthorizationStore::open(&root.join("authorizations"), pool(4)).unwrap(),
        ReceiptStore::open(&path, trusted_release()).unwrap(),
        AuditLog::open(&root.join("audit")).unwrap(),
        local_pin(),
    )
    .unwrap();
    (service, authorization, launch, start)
}

fn assert_admission_status(decision: &ConformanceEvidence, bytes: &[u8]) {
    let root = TempDir::new().unwrap();
    let (service, authorization, launch, start) = retained_service(root.path(), decision, bytes);
    let offer = reconnect::checkpoint(&start);
    let mut mechanical = lifecycle::status(&authorization);
    let head = ReceiptHead {
        sequence: 1,
        digest: start.digest().to_string(),
    };
    mechanical.broker_head = Some(head.clone());
    mechanical.launcher_head = Some(head);
    let query = CommandMessage {
        schema: COMMAND_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        request_id: "admission-self-status".into(),
        session_id: authorization.session_id.clone(),
        run_id: authorization.run_id.clone(),
        envelope_revision: authorization.envelope_revision,
        operation: CommandOperation::StatusRequest {},
    };
    let peer_root = root.path().to_owned();
    let peer = thread::spawn(move || {
        let channel = reconnect::peer(&peer_root, &offer);
        settle(|done| channel.receive(done));
        lifecycle::answer_one_status_query(&channel, &mechanical);
        settle(|done| channel.send(query.canonical_bytes(), done));
        lifecycle::answer_one_status_query(&channel, &mechanical);
        settle(|done| channel.receive(done))
    });
    // Deliberately after the historical waiver's expiry. Reads do not renew it.
    let mut session = service
        .serve_reconnect(90_000, verify_fixture_signature)
        .unwrap();
    let audit = service.audit().unwrap();
    let admission = service.inspect(&authorization.session_id).unwrap();
    let operator = service
        .session_status(
            &mut session,
            &LifecycleCaller::Operator {
                uid: CONTROLLER_UID,
            },
            90_000,
            verify_fixture_signature,
        )
        .unwrap();
    let agent = service
        .serve_agent_status(&mut session, 90_001, verify_fixture_signature)
        .unwrap();
    let reply = peer.join().unwrap();
    let LauncherPacket::Request(ProtocolMessage::Command(reply)) = reply.packet else {
        panic!("Agent reply")
    };
    let CommandOperation::StatusResult { status } = reply.operation else {
        panic!("Agent status")
    };
    assert_eq!(*status, agent);
    assert_eq!(agent.posture, operator.posture);
    assert!(agent.allowed_actions.is_empty());
    for status in [&operator, &agent] {
        assert_projection(status, decision);
    }
    assert_eq!(service.audit().unwrap(), audit);
    assert_eq!(
        service.inspect(&authorization.session_id).unwrap(),
        admission
    );
    assert_eq!(session.authorization(), &authorization);
    assert_eq!(
        service
            .receipts()
            .stored_bytes(&authorization.session_id)
            .unwrap(),
        vec![launch.canonical_bytes(), start.canonical_bytes()]
    );
}

fn assert_projection(status: &SessionStatus, decision: &ConformanceEvidence) {
    assert_eq!(&status.conformance_admission, decision);
    let isolation = status
        .posture
        .dimensions
        .iter()
        .find(|row| row.dimension == DimensionName::Isolation)
        .unwrap();
    assert_eq!(isolation.state, DimensionState::Failed);
    assert_eq!(isolation.failure_code, Some(FailureCode::EvidenceMissing));
    assert_eq!(isolation.next_action.id, "collect_trusted_evidence");
    assert_eq!(isolation.freshness.last_verified_at_ms, None);
    let wire = String::from_utf8(status.canonical_bytes()).unwrap();
    for forbidden in [
        "private probe observation",
        "path",
        "assigned_uid",
        "assigned_gid",
        "pid",
        "environment",
    ] {
        assert!(!wire.contains(forbidden), "{forbidden}");
    }
    assert_eq!(
        SessionStatus::parse_canonical(wire.as_bytes()).unwrap(),
        *status
    );
}
