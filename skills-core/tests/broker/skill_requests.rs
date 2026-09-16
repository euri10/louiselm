//! Real broker channel, durable operator decisions and subject cleanup.
use super::*;
use louiselm_skills::{
    broker::{
        BrokerSession,
        attention::{Outbox, ProjectionChange},
    },
    launch_protocol::{COMMAND_SCHEMA, CommandMessage, CommandOperation, LifecycleAction},
    skill_request::{
        ApprovedSkillRequests, SkillRequest, SkillRequestOutcome, SkillRequestStatus, SkillSubject,
    },
};
use std::path::PathBuf;

fn query(auth: &LaunchAuthorization) -> CommandMessage {
    CommandMessage {
        schema: COMMAND_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        request_id: "relay-1".into(),
        session_id: auth.session_id.clone(),
        run_id: auth.run_id.clone(),
        envelope_revision: auth.envelope_revision,
        operation: CommandOperation::SkillRequest {
            request: SkillRequest {
                request_id: "stable-request".into(),
                subject: SkillSubject::Session,
                packages: vec![Digest::of(b"exact-package").to_string()],
                agents: vec!["codex".into()],
            },
        },
    }
}

fn fixture(root: &Path, permitted: bool) -> (BrokerService, BrokerSession, SeqpacketChannel) {
    fixture_with_run(root, permitted, false)
}

fn fixture_with_run(
    root: &Path,
    permitted: bool,
    allow_run: bool,
) -> (BrokerService, BrokerSession, SeqpacketChannel) {
    let socket = root.join("broker.sock");
    let mut request = request("skill-session");
    request.run_id = "12345678-1234-4234-8234-123456789abc".into();
    let mut approval = grant(&request);
    approval.skill_requests = permitted.then(|| ApprovedSkillRequests {
        agents: vec!["codex".into()],
        allow_run,
        expires_at_ms: 60000,
    });
    let authorizations = AuthorizationStore::open(&root.join("authorizations"), pool(4)).unwrap();
    authorizations.authorize(&approval, 1000).unwrap();
    let service = BrokerService::bind(
        &socket,
        authorizations,
        ReceiptStore::open(&root.join("receipts"), trusted_release()).unwrap(),
        AuditLog::open(&root.join("audit")).unwrap(),
        local_pin(),
    )
    .unwrap();
    let peer = thread::spawn(move || fake_supervisor(&socket, &request, 2000));
    let session = service
        .serve_launch(2000, verify_fixture_signature)
        .unwrap();
    (service, session, peer.join().unwrap().1)
}

#[test]
#[ignore = "invoked with a disposable capture-service by scripts/test-skill-requests"]
fn cross_service_requests() {
    use louiselm_skills::broker::attention::AttentionEndpoint;
    let root = PathBuf::from(std::env::var_os("LOUISELM_REQUEST_STATE").unwrap());
    let endpoint: AttentionEndpoint = serde_json::from_slice(
        &fs::read(std::env::var_os("LOUISELM_REQUEST_ENDPOINT").unwrap()).unwrap(),
    )
    .unwrap();
    let phase = std::env::var("LOUISELM_REQUEST_PHASE").unwrap();
    let service = if phase == "accept" {
        let (service, mut session, peer) = fixture_with_run(&root, true, true);
        let mut message = query(session.authorization());
        let session_request = accepted(exchange(&service, &mut session, &peer, &message, true));
        let CommandOperation::SkillRequest { request } = &mut message.operation else {
            unreachable!()
        };
        request.subject = SkillSubject::Run;
        request.request_id = "run-request".into();
        let current = lifecycle::status(session.authorization());
        thread::scope(|scope| {
            let worker = scope.spawn(|| {
                for _ in 0..2 {
                    settle(|done| peer.send(message.canonical_bytes(), done));
                    lifecycle::answer_one_status_query(&peer, &current);
                    let LauncherPacket::Request(ProtocolMessage::Command(reply)) =
                        settle(|done| peer.receive(done)).packet
                    else {
                        panic!("request result");
                    };
                    assert_eq!(
                        accepted(reply.operation).outcome,
                        SkillRequestOutcome::Pending
                    );
                }
            });
            service
                .step(
                    &mut session,
                    3000,
                    Some(&endpoint),
                    verify_fixture_signature,
                )
                .unwrap();
            // An accepted retry needs no receiver connection; no second request is created.
            service
                .step(&mut session, 3000, None, verify_fixture_signature)
                .unwrap();
            worker.join().unwrap();
        });
        assert_eq!(
            service
                .skill_request_control(CONTROLLER_UID, &session_request.operation_id, None)
                .unwrap(),
            session_request
        );
        service
    } else {
        BrokerService::bind(
            &root.join(format!("{phase}.sock")),
            AuthorizationStore::open(&root.join("authorizations"), pool(4)).unwrap(),
            ReceiptStore::open(&root.join("receipts"), trusted_release()).unwrap(),
            AuditLog::open(&root.join("audit")).unwrap(),
            local_pin(),
        )
        .unwrap()
    };
    service
        .reconcile_skill_requests(Some(&endpoint), verify_fixture_signature)
        .unwrap();
    let outbox = Outbox::open(&root.join("authorizations/attention-outbox")).unwrap();
    while outbox.deliver_next(&endpoint).unwrap() {}
}

fn exchange(
    service: &BrokerService,
    session: &mut BrokerSession,
    peer: &SeqpacketChannel,
    message: &CommandMessage,
    queries_status: bool,
) -> CommandOperation {
    let status = lifecycle::status(session.authorization());
    thread::scope(|scope| {
        let worker = scope.spawn(|| {
            settle(|done| peer.send(message.canonical_bytes(), done));
            if queries_status {
                lifecycle::answer_one_status_query(peer, &status);
            }
            let LauncherPacket::Request(ProtocolMessage::Command(reply)) =
                settle(|done| peer.receive(done)).packet
            else {
                panic!("request reply");
            };
            assert_eq!(reply.request_id, message.request_id);
            reply.operation
        });
        assert!(
            !service
                .step(session, 3000, None, verify_fixture_signature)
                .unwrap()
        );
        worker.join().unwrap()
    })
}

fn accepted(operation: CommandOperation) -> SkillRequestStatus {
    let CommandOperation::SkillRequestResult { status } = operation else {
        panic!("durable acceptance: {operation:?}");
    };
    status
}

#[test]
fn broker_permission_is_explicit_and_subject_revision_scope_are_not_client_authority() {
    for permitted in [false, true] {
        let root = TempDir::new().unwrap();
        let (service, mut session, peer) = fixture(root.path(), permitted);
        let original = query(session.authorization());
        let mut foreign = original.clone();
        foreign.session_id = "other-session".into();
        let mut stale = original.clone();
        stale.envelope_revision += 1;
        let mut scope = original.clone();
        let CommandOperation::SkillRequest { request } = &mut scope.operation else {
            unreachable!()
        };
        request.agents = vec!["other".into()];
        let mut run = original.clone();
        let CommandOperation::SkillRequest { request } = &mut run.operation else {
            unreachable!()
        };
        request.subject = SkillSubject::Run;
        for invalid in [&foreign, &stale, &scope, &run] {
            assert!(matches!(
                exchange(&service, &mut session, &peer, invalid, false),
                CommandOperation::SkillRequestRefused { .. }
            ));
        }
        let result = exchange(&service, &mut session, &peer, &original, permitted);
        if permitted {
            assert_eq!(accepted(result).outcome, SkillRequestOutcome::Pending);
        } else {
            assert!(matches!(
                result,
                CommandOperation::SkillRequestRefused { .. }
            ));
        }
        let outbox = Outbox::open(&root.path().join("authorizations/attention-outbox")).unwrap();
        assert_eq!(outbox.next().unwrap().is_some(), permitted);
    }
}

#[test]
fn durable_operator_result_replays_without_admission_or_receipt_changes() {
    let root = TempDir::new().unwrap();
    let (service, mut session, peer) = fixture(root.path(), true);
    let mut message = query(session.authorization());
    let before = service.receipts().stored_bytes("skill-session").unwrap();
    let first = accepted(exchange(&service, &mut session, &peer, &message, true));
    message.request_id = "relay-retry".into();
    assert_eq!(
        accepted(exchange(&service, &mut session, &peer, &message, true)),
        first
    );
    let mut conflict = message.clone();
    let CommandOperation::SkillRequest { request } = &mut conflict.operation else {
        unreachable!()
    };
    request.packages = vec![Digest::of(b"changed").to_string()];
    assert!(matches!(
        exchange(&service, &mut session, &peer, &conflict, true),
        CommandOperation::SkillRequestRefused { .. }
    ));
    assert!(
        service
            .skill_request_control(CONTROLLER_UID + 1, &first.operation_id, None)
            .is_err()
    );
    let rejected = service
        .skill_request_control(
            CONTROLLER_UID,
            &first.operation_id,
            Some(SkillRequestOutcome::Rejected),
        )
        .unwrap();
    assert_eq!(
        accepted(exchange(&service, &mut session, &peer, &message, true)),
        rejected
    );
    assert_eq!(
        service.receipts().stored_bytes("skill-session").unwrap(),
        before
    );
    drop(session);
    drop(peer);
    drop(service);
    let socket = root.path().join("restarted.sock");
    let service = BrokerService::bind(
        &socket,
        AuthorizationStore::open(&root.path().join("authorizations"), pool(4)).unwrap(),
        ReceiptStore::open(&root.path().join("receipts"), trusted_release()).unwrap(),
        AuditLog::open(&root.path().join("audit")).unwrap(),
        local_pin(),
    )
    .unwrap();
    assert_eq!(
        service
            .skill_request_control(CONTROLLER_UID, &first.operation_id, None)
            .unwrap(),
        rejected
    );
    let outbox = Outbox::open(&root.path().join("authorizations/attention-outbox")).unwrap();
    let pending = outbox.next().unwrap().unwrap();
    assert!(matches!(pending.change, ProjectionChange::Upsert(_)));
    let bytes = pending.wire().to_string();
    assert!(
        bytes.contains("admission_required")
            && !bytes.contains("codex")
            && !bytes.contains("packages")
    );
    outbox
        .acknowledge(pending.sequence, &pending.digest())
        .unwrap();
    assert!(matches!(
        outbox.next().unwrap().unwrap().change,
        ProjectionChange::Clear(_)
    ));
}

#[test]
fn signed_disposal_cancels_but_signed_park_retains_the_request() {
    for action in [LifecycleAction::Park, LifecycleAction::Disposal] {
        let root = TempDir::new().unwrap();
        let (service, mut session, peer) = fixture(root.path(), true);
        let message = query(session.authorization());
        let pending = accepted(exchange(&service, &mut session, &peer, &message, true));
        let current = lifecycle::status(session.authorization());
        let mut change = lifecycle::park(session.authorization());
        change.action = action;
        thread::scope(|scope| {
            let worker = scope.spawn(|| lifecycle::drive_lifecycle_peer(&peer, &current));
            service
                .request_lifecycle(
                    &mut session,
                    &louiselm_skills::broker::lifecycle::LifecycleCaller::Operator {
                        uid: CONTROLLER_UID,
                    },
                    &change,
                    4000,
                    verify_fixture_signature,
                )
                .unwrap();
            worker.join().unwrap();
        });
        let expected = if action == LifecycleAction::Disposal {
            SkillRequestOutcome::Cancelled
        } else {
            SkillRequestOutcome::Pending
        };
        assert_eq!(
            service
                .skill_request_control(CONTROLLER_UID, &pending.operation_id, None)
                .unwrap()
                .outcome,
            expected
        );
    }
}
