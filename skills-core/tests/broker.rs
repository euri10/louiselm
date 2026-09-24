//! Control broker launch-transaction tests.
//!
//! The broker owns policy and canonical bytes: it decides one launch, persists
//! the pending authorization, and consumes it exactly once. These tests drive
//! the durable transaction directly; transport-level coverage lives alongside
//! the fake supervisor.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Test fixtures abort on setup failure and assert failures directly."
)]

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    sync::{Arc, mpsc},
    thread,
    time::Duration,
};

use louiselm_skills::{
    broker::{
        AuditDecision, AuditLog, AuthorizationStore, BrokerError, BrokerService, GrantRequest,
        ReceiptStore, TrustedRelease,
    },
    canonical::Digest,
    launch::{LaunchRequest, PROTOCOL_VERSION, REQUEST_SCHEMA},
    launch_protocol::{
        ErrorCode, LaunchAuthorization, ProtocolError, ProtocolMessage, ProtocolResponse,
        RESPONSE_SCHEMA, ReceiptAcknowledgement, ReceiptDisposition, ResponseResult,
        STATUS_REQUEST_SCHEMA, StatusRequest,
    },
    launch_receipt::{
        Authorization, ConformanceEvidence, LaunchEvidence, RECEIPT_SCHEMA, ReceiptAuthority,
        ReceiptCause, ReceiptOutcome, ReceiptPayload, SIGNED_RECEIPT_SCHEMA, SessionState,
        SignedReceipt,
    },
    launch_transport::{
        CredentialPin, LauncherPacket, SeqpacketChannel, SeqpacketConnector, TransportCompletion,
        TransportError,
    },
    launcher_install::IdentityPool,
};
use rustix::process::{getgid, getuid};
use tempfile::TempDir;

#[path = "support/discovery.rs"]
mod discovery_support;
#[path = "broker/supply_posture.rs"]
mod supply_posture;
#[path = "support/supply.rs"]
mod supply_support;
mod support;

#[path = "broker/launch_gates.rs"]
mod launch_gates;

#[path = "broker/workspace_retention.rs"]
mod workspace_retention;

#[path = "broker/beads_mutations.rs"]
mod beads_mutations;
#[path = "broker/buffered_requests.rs"]
mod buffered_requests;
#[path = "broker/lifecycle.rs"]
mod lifecycle;
#[path = "broker/skill_requests.rs"]
mod skill_requests;

#[path = "broker/dependencies.rs"]
mod dependencies;

#[path = "broker/posture.rs"]
mod posture;

#[path = "broker/conformance.rs"]
mod conformance;

#[path = "broker/conformance_transport.rs"]
mod conformance_transport;

#[path = "broker/pre_admission.rs"]
mod pre_admission;

#[path = "broker/attention.rs"]
mod attention;

#[path = "broker/recovery.rs"]
mod recovery;

#[path = "broker/recovery_status.rs"]
mod recovery_status;

#[path = "broker/reconnect.rs"]
mod reconnect;

#[path = "broker/receipt_history.rs"]
mod receipt_history;

#[path = "broker/serving.rs"]
mod serving;

#[path = "broker/controller_loss.rs"]
mod controller_loss;

#[path = "broker/cold_resume.rs"]
mod cold_resume;

#[path = "broker/promotion.rs"]
mod promotion;
#[path = "broker/verification.rs"]
mod verification;

/// Installed pool wide enough that slot assignment is not the subject.
fn pool(slots: u32) -> IdentityPool {
    IdentityPool {
        uid_start: 2_000_000,
        gid_start: 3_000_000,
        slots,
    }
}

/// One well-formed request; only the identifiers vary between fixtures.
fn request(session: &str) -> LaunchRequest {
    LaunchRequest {
        schema: REQUEST_SCHEMA.to_owned(),
        protocol_version: PROTOCOL_VERSION,
        request_id: format!("request-{session}"),
        authorization_id: format!("authorization-{session}"),
        session_id: session.to_owned(),
        run_id: "run-1".to_owned(),
        agent_id: "demo".to_owned(),
        envelope_id: "envelope-1".to_owned(),
        envelope_revision: 7,
        skill_generation_id: Digest::of(b"generation").to_string(),
        session_input_manifest_id: Digest::of(b"input").to_string(),
    }
}

/// The unprivileged controller the broker authorized.
const CONTROLLER_UID: u32 = 1501;

fn grant(request: &LaunchRequest) -> GrantRequest {
    GrantRequest {
        conformance: louiselm_skills::launch_protocol::ConformanceAuthorization::default(),
        dependencies: None,
        skill_requests: None,
        beads_mutations: None,
        require_cold_recovery: false,
        request: request.clone(),
        controller_uid: CONTROLLER_UID,
        expires_at_ms: 30_000,
        broker_loss_grace_ms: 5_000,
        commands: Some(louiselm_skills::broker::ApprovedCommands {
            command_digest: Digest::of(b"printf sensitive").to_string(),
            timeout_ms: 1000,
            uses: Some(3),
            allow_delegation: true,
            expires_at_ms: 60_000,
        }),
    }
}

#[test]
fn command_count_is_opt_in_and_durable_for_ordinary_and_unattended_runs() {
    for require_cold_recovery in [false, true] {
        let root = TempDir::new().unwrap();
        let request = request("optional-count");
        let mut approval = grant(&request);
        approval.require_cold_recovery = require_cold_recovery;
        let mut commands = serde_json::to_value(approval.commands.as_ref().unwrap()).unwrap();
        commands.as_object_mut().unwrap().remove("uses");
        approval.commands = Some(serde_json::from_value(commands.clone()).unwrap());
        let store = AuthorizationStore::open(root.path(), pool(1)).unwrap();
        let pending = store.authorize(&approval, 1000).unwrap();
        let encoded = serde_json::to_value(&pending).unwrap();
        assert!(encoded["commands"].get("uses").is_none());
        drop(store);
        let reopened = AuthorizationStore::open(root.path(), pool(1)).unwrap();
        reopened.consume(&request, CONTROLLER_UID, 2000).unwrap();
        assert_eq!(
            reopened
                .consumed_for_session(&request.session_id)
                .unwrap()
                .unwrap(),
            pending
        );
    }
}

#[test]
fn finite_command_counts_round_trip_and_invalid_counts_never_become_uncapped() {
    for value in [
        serde_json::json!(1),
        serde_json::json!(64),
        serde_json::json!(0),
        serde_json::json!(65),
        serde_json::json!(-1),
        serde_json::json!(1.5),
        serde_json::json!("unlimited"),
    ] {
        let root = TempDir::new().unwrap();
        let request = request("count-validation");
        let mut approval = grant(&request);
        let mut commands = serde_json::to_value(approval.commands.as_ref().unwrap()).unwrap();
        commands["uses"] = value.clone();
        let decoded = serde_json::from_value(commands);
        let valid = matches!(value.as_u64(), Some(1 | 64));
        match decoded {
            Ok(commands) => {
                approval.commands = Some(commands);
                let store = AuthorizationStore::open(root.path(), pool(1)).unwrap();
                let result = store.authorize(&approval, 1000);
                assert_eq!(result.is_ok(), valid, "{value}");
                if let Ok(pending) = result {
                    assert_eq!(
                        serde_json::to_value(pending).unwrap()["commands"]["uses"],
                        value
                    );
                    store.consume(&request, CONTROLLER_UID, 2000).unwrap();
                    assert_eq!(
                        store
                            .consumed_for_session(&request.session_id)
                            .unwrap()
                            .unwrap()
                            .commands,
                        approval.commands
                    );
                }
            }
            Err(_) => assert!(!valid),
        }
    }
}

#[test]
fn absent_command_approval_denies_effects_without_ending_the_session() {
    use louiselm_skills::launch_protocol::{
        COMMAND_SCHEMA, CommandMessage, CommandOperation, CommandPrincipal, TOOL_EXECUTION_SCHEMA,
        ToolExecutionRequest,
    };
    let root = TempDir::new().unwrap();
    let socket = root.path().join("control.sock");
    let request = request("session-1");
    let authorizations =
        AuthorizationStore::open(&root.path().join("authorizations"), pool(4)).unwrap();
    let mut approval = grant(&request);
    approval.commands = None;
    authorizations.authorize(&approval, 1000).unwrap();
    let service = BrokerService::bind(
        &socket,
        authorizations,
        ReceiptStore::open(&root.path().join("receipts"), trusted_release()).unwrap(),
        AuditLog::open(&root.path().join("audit")).unwrap(),
        local_pin(),
    )
    .unwrap();
    let peer = thread::spawn(move || fake_supervisor(&socket, &request, 2000));
    let mut session = service
        .serve_launch(2000, verify_fixture_signature)
        .unwrap();
    let (authorization, channel) = peer.join().unwrap();
    let command = CommandMessage {
        schema: COMMAND_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        request_id: "unapproved".into(),
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
                request_id: "unapproved".into(),
                session_id: authorization.session_id,
                run_id: authorization.run_id,
                envelope_revision: authorization.envelope_revision,
                sequence: 1,
                command: "printf sensitive".into(),
                timeout_ms: 1000,
            },
        },
    };
    settle(|complete| channel.send(command.canonical_bytes(), complete));
    session.serve_command().unwrap();
    let packet = settle(|complete| channel.receive(complete));
    assert!(matches!(
        packet.packet,
        LauncherPacket::Request(ProtocolMessage::Command(CommandMessage {
            operation: CommandOperation::Reject { .. },
            ..
        }))
    ));
    assert!(!session.channel().is_closed());
}

#[test]
fn a_signed_start_cannot_change_the_installed_identity() {
    let root = TempDir::new().unwrap();
    let authorization = consumed_authorization(root.path(), &request("session-1"));
    let receipts = ReceiptStore::open(&root.path().join("receipts"), trusted_release()).unwrap();
    let launch = launch_receipt(&authorization);
    receipts
        .append(
            &authorization,
            &launch.canonical_bytes(),
            None,
            verify_fixture_signature,
        )
        .unwrap();
    let mut start = start_receipt(&authorization, &launch);
    let ReceiptOutcome::Start { evidence, .. } = &mut start.payload.outcome else {
        panic!("start");
    };
    evidence.assigned_uid += 1;
    let changed = signed(start.payload);
    assert!(matches!(
        receipts.append(
            &authorization,
            &changed.canonical_bytes(),
            None,
            verify_fixture_signature
        ),
        Err(BrokerError::ReceiptUnauthorized)
    ));
    assert_eq!(
        receipts.state("session-1").unwrap(),
        Some(SessionState::Starting)
    );
    assert_eq!(receipts.head("session-1").unwrap().unwrap().sequence, 0);
}

#[test]
fn pending_approval_is_exact_durable_and_expiring() {
    let root = TempDir::new().unwrap();
    let store = AuthorizationStore::open(root.path(), pool(4)).unwrap();
    let mut approval = grant(&request("session-1"));
    let expected = approval.commands.clone();
    store.authorize(&approval, 1000).unwrap();
    store.consume_for_launcher(&approval.request, 2000).unwrap();
    drop(store);
    let reopened = AuthorizationStore::open(root.path(), pool(4)).unwrap();
    assert_eq!(
        reopened
            .consumed_for_session("session-1")
            .unwrap()
            .unwrap()
            .commands,
        expected
    );
    approval.request = request("session-2");
    approval.commands.as_mut().unwrap().expires_at_ms = 1000;
    assert!(matches!(
        reopened.authorize(&approval, 1000),
        Err(BrokerError::Expired)
    ));
    approval.commands.as_mut().unwrap().expires_at_ms = 30_000;
    approval.commands.as_mut().unwrap().command_digest = "invalid".into();
    assert!(matches!(
        reopened.authorize(&approval, 1000),
        Err(BrokerError::InvalidGrant)
    ));
    approval.commands = None;
    assert!(
        reopened
            .authorize(&approval, 1000)
            .unwrap()
            .commands
            .is_none()
    );
}

#[test]
fn one_authorization_is_consumed_once_and_stays_consumed_across_restart() {
    let root = TempDir::new().expect("broker state directory");
    let store = AuthorizationStore::open(root.path(), pool(4)).expect("open store");
    let request = request("session-1");

    let pending = store.authorize(&grant(&request), 1_000).expect("authorize");
    assert_eq!(pending.authorization_id, request.authorization_id);
    assert_eq!(pending.expires_at_ms, 30_000);

    let authorization = store
        .consume(&request, CONTROLLER_UID, 2_000)
        .expect("first consume");
    authorization
        .validate()
        .expect("authorization is well formed");
    assert_eq!(authorization.authorization_id, request.authorization_id);
    assert_eq!(authorization.identity_slot, pending.identity.slot);
    assert_eq!(authorization.assigned_uid, pending.identity.uid);
    assert_eq!(authorization.assigned_gid, pending.identity.gid);
    assert_eq!(authorization.request_digest, pending.request_digest);
    assert_eq!(authorization.envelope_revision, request.envelope_revision);

    assert!(matches!(
        store.consume(&request, CONTROLLER_UID, 2_100),
        Err(BrokerError::UnknownAuthorization)
    ));

    drop(store);
    let restarted = AuthorizationStore::open(root.path(), pool(4)).expect("reopen store");
    assert!(matches!(
        restarted.consume(&request, CONTROLLER_UID, 2_200),
        Err(BrokerError::UnknownAuthorization)
    ));
}

#[test]
fn an_expired_authorization_is_refused_at_its_exclusive_boundary() {
    let root = TempDir::new().expect("broker state directory");
    let store = AuthorizationStore::open(root.path(), pool(4)).expect("open store");
    let request = request("session-1");
    store.authorize(&grant(&request), 1_000).expect("authorize");

    assert!(matches!(
        store.consume(&request, CONTROLLER_UID, 30_000),
        Err(BrokerError::Expired)
    ));
}

#[test]
fn consumption_refuses_a_mismatched_request_or_controller() {
    let root = TempDir::new().expect("broker state directory");
    let store = AuthorizationStore::open(root.path(), pool(4)).expect("open store");
    let authorized = request("session-1");
    store
        .authorize(&grant(&authorized), 1_000)
        .expect("authorize");

    let mut tampered = authorized.clone();
    tampered.envelope_revision = 8;
    assert!(matches!(
        store.consume(&tampered, CONTROLLER_UID, 2_000),
        Err(BrokerError::RequestMismatch)
    ));

    assert!(matches!(
        store.consume(&authorized, CONTROLLER_UID + 1, 2_000),
        Err(BrokerError::ControllerMismatch)
    ));

    store
        .consume(&authorized, CONTROLLER_UID, 2_000)
        .expect("the exact request still consumes its authorization");
}

#[test]
fn an_exhausted_identity_pool_fails_without_evicting_a_live_holder() {
    let root = TempDir::new().expect("broker state directory");
    let store = AuthorizationStore::open(root.path(), pool(1)).expect("open store");
    let first = request("session-1");
    let second = request("session-2");
    let held = store.authorize(&grant(&first), 1_000).expect("authorize");

    let exhaustion = match store.authorize(&grant(&second), 1_000) {
        Err(BrokerError::IdentityExhausted(exhaustion)) => exhaustion,
        other => panic!("expected identity exhaustion, got {other:?}"),
    };
    assert_eq!(exhaustion.occupied_sessions.len(), 1);
    assert_eq!(exhaustion.occupied_sessions[0].session_id, first.session_id);

    let authorization = store
        .consume(&first, CONTROLLER_UID, 2_000)
        .expect("the live holder keeps its slot");
    assert_eq!(authorization.identity_slot, held.identity.slot);
}

#[test]
fn concurrent_consumption_has_exactly_one_winner() {
    let root = TempDir::new().expect("broker state directory");
    let store = Arc::new(AuthorizationStore::open(root.path(), pool(4)).expect("open store"));
    let request = Arc::new(request("session-1"));
    store.authorize(&grant(&request), 1_000).expect("authorize");

    let racers: Vec<_> = (0..8)
        .map(|_| {
            let store = Arc::clone(&store);
            let request = Arc::clone(&request);
            thread::spawn(move || store.consume(&request, CONTROLLER_UID, 2_000).is_ok())
        })
        .collect();
    let winners = racers
        .into_iter()
        .filter_map(|racer| racer.join().ok())
        .filter(|won| *won)
        .count();

    assert_eq!(winners, 1, "single-use consumption admits one winner");
}

/// Release the broker trusts for every chain in these tests.
fn trusted_release() -> TrustedRelease {
    TrustedRelease {
        release_id: Digest::of(b"release").to_string(),
        signing_key_id: Digest::of(b"launcher-key").to_string(),
    }
}

/// Accepts only the fixture signature made by the trusted key.
fn verify_fixture_signature(key_id: &str, payload_bytes: &[u8], signature: &str) -> bool {
    key_id == trusted_release().signing_key_id && signature == Digest::of(payload_bytes).to_string()
}

fn signed(payload: ReceiptPayload) -> SignedReceipt {
    let signature = Digest::of(&payload.canonical_bytes()).to_string();
    SignedReceipt {
        schema: SIGNED_RECEIPT_SCHEMA.to_owned(),
        payload,
        signature,
    }
}

fn payload(
    authorization: &LaunchAuthorization,
    request_id: &str,
    sequence: u64,
    previous_receipt_digest: Option<String>,
    outcome: ReceiptOutcome,
    resulting_state: SessionState,
) -> ReceiptPayload {
    ReceiptPayload {
        schema: RECEIPT_SCHEMA.to_owned(),
        session_id: authorization.session_id.clone(),
        run_id: authorization.run_id.clone(),
        request_id: request_id.to_owned(),
        envelope_revision: authorization.envelope_revision,
        sequence,
        previous_receipt_digest,
        release_id: trusted_release().release_id,
        signing_key_id: trusted_release().signing_key_id,
        outcome,
        resulting_state,
    }
}

/// The sequence-zero Launch receipt for a consumed authorization.
fn launch_receipt(authorization: &LaunchAuthorization) -> SignedReceipt {
    signed(payload(
        authorization,
        &authorization.request_id,
        0,
        None,
        ReceiptOutcome::Launch {
            authorization: Authorization {
                authorization_id: authorization.authorization_id.clone(),
                request_id: authorization.request_id.clone(),
                request_digest: authorization.request_digest.clone(),
            },
            evidence: Box::new(LaunchEvidence {
                conformance: ConformanceEvidence::Unevaluated,
                launch_request_digest: authorization.request_digest.clone(),
                runtime_measurement_digest: Digest::of(b"runtime").to_string(),
                skill_generation_id: Digest::of(b"generation").to_string(),
                session_input_manifest_id: Digest::of(b"input").to_string(),
                isolation_contract: "louiselm.isolation/1".to_owned(),
                isolation_backend_id: "bubblewrap-0_12".to_owned(),
                kernel_identity: "linux-6_12".to_owned(),
                isolation_evidence_digest: Digest::of(b"isolation").to_string(),
                broker_loss_grace_ms: authorization.broker_loss_grace_ms,
                capability_channel_ids: vec!["acp".to_owned(), "broker".to_owned()],
            }),
        },
        SessionState::Starting,
    ))
}

/// The sequence-one Start receipt that follows a durable launch acknowledgement.
fn start_receipt(authorization: &LaunchAuthorization, launch: &SignedReceipt) -> SignedReceipt {
    signed(payload(
        authorization,
        "start-after-launch",
        1,
        Some(launch.digest().to_string()),
        ReceiptOutcome::Start {
            evidence: louiselm_skills::launch_receipt::StartEvidence {
                agent_pid: 123,
                assigned_uid: authorization.assigned_uid,
                assigned_gid: authorization.assigned_gid,
                tool_isolation_digest: Digest::of(b"fixture-tool-isolation").to_string(),
            },
            authority: ReceiptAuthority::Cause {
                cause: ReceiptCause::LaunchAcknowledged,
            },
        },
        SessionState::Running,
    ))
}

/// Authorizes and consumes one launch, returning what the supervisor holds.
fn consumed_authorization(root: &Path, request: &LaunchRequest) -> LaunchAuthorization {
    let store =
        AuthorizationStore::open(&root.join("authorizations"), pool(4)).expect("open store");
    store.authorize(&grant(request), 1_000).expect("authorize");
    store
        .consume(request, CONTROLLER_UID, 2_000)
        .expect("consume")
}

#[test]
fn both_launch_receipts_are_durably_stored_with_their_exact_bytes() {
    let root = TempDir::new().expect("broker state directory");
    let request = request("session-1");
    let authorization = consumed_authorization(root.path(), &request);
    let receipts = ReceiptStore::open(&root.path().join("receipts"), trusted_release())
        .expect("open receipt store");

    let launch = launch_receipt(&authorization);
    let launch_bytes = launch.canonical_bytes();
    let acknowledgement = receipts
        .append(
            &authorization,
            &launch_bytes,
            None,
            verify_fixture_signature,
        )
        .expect("sequence zero is durable");
    assert_eq!(
        acknowledgement.disposition,
        ReceiptDisposition::DurablyStored
    );
    assert_eq!(acknowledgement.sequence, 0);
    assert_eq!(acknowledgement.receipt_digest, launch.digest().to_string());
    assert_eq!(acknowledgement.session_id, authorization.session_id);

    let start = start_receipt(&authorization, &launch);
    let start_bytes = start.canonical_bytes();
    let acknowledgement = receipts
        .append(&authorization, &start_bytes, None, verify_fixture_signature)
        .expect("sequence one is durable");
    assert_eq!(
        acknowledgement.disposition,
        ReceiptDisposition::DurablyStored
    );
    assert_eq!(acknowledgement.sequence, 1);

    let stored = receipts
        .stored_bytes(&authorization.session_id)
        .expect("stored chain");
    assert_eq!(
        stored,
        vec![launch_bytes, start_bytes],
        "the broker stores the supervisor's exact signed bytes, in order"
    );
    let head = receipts
        .head(&authorization.session_id)
        .expect("head")
        .expect("a stored chain has a head");
    assert_eq!(head.sequence, 1);
    assert_eq!(head.digest, start.digest().to_string());
}

#[test]
fn a_receipt_whose_signature_fails_verification_is_not_stored() {
    let root = TempDir::new().expect("broker state directory");
    let request = request("session-1");
    let authorization = consumed_authorization(root.path(), &request);
    let receipts = ReceiptStore::open(&root.path().join("receipts"), trusted_release())
        .expect("open receipt store");
    let launch = launch_receipt(&authorization);

    let refusal = receipts.append(
        &authorization,
        &launch.canonical_bytes(),
        None,
        |_, _, _| false,
    );
    assert!(matches!(refusal, Err(BrokerError::ReceiptRefused(_))));
    assert!(
        receipts
            .stored_bytes(&authorization.session_id)
            .expect("stored chain")
            .is_empty(),
        "a refused receipt leaves no bytes behind"
    );
}

#[test]
fn a_receipt_that_skips_its_predecessor_is_not_stored() {
    let root = TempDir::new().expect("broker state directory");
    let request = request("session-1");
    let authorization = consumed_authorization(root.path(), &request);
    let receipts = ReceiptStore::open(&root.path().join("receipts"), trusted_release())
        .expect("open receipt store");
    let launch = launch_receipt(&authorization);
    let start = start_receipt(&authorization, &launch);

    let refusal = receipts.append(
        &authorization,
        &start.canonical_bytes(),
        None,
        verify_fixture_signature,
    );
    assert!(matches!(refusal, Err(BrokerError::ReceiptRefused(_))));
    assert!(
        receipts
            .stored_bytes(&authorization.session_id)
            .expect("stored chain")
            .is_empty()
    );
}

#[test]
fn a_receipt_answering_another_authorization_is_not_stored() {
    let root = TempDir::new().expect("broker state directory");
    let request = request("session-1");
    let authorization = consumed_authorization(root.path(), &request);
    let receipts = ReceiptStore::open(&root.path().join("receipts"), trusted_release())
        .expect("open receipt store");

    // A receipt that is internally consistent, correctly signed, and answers a
    // launch request this broker never authorized.
    let mut launch = launch_receipt(&authorization);
    if let ReceiptOutcome::Launch {
        authorization: claimed,
        evidence,
    } = &mut launch.payload.outcome
    {
        claimed.request_digest = Digest::of(b"another-request").to_string();
        evidence.launch_request_digest = claimed.request_digest.clone();
    }
    let launch = signed(launch.payload);

    let refusal = receipts.append(
        &authorization,
        &launch.canonical_bytes(),
        None,
        verify_fixture_signature,
    );
    assert!(
        matches!(refusal, Err(BrokerError::ReceiptUnauthorized)),
        "unexpected refusal: {refusal:?}"
    );
    assert!(
        receipts
            .stored_bytes(&authorization.session_id)
            .expect("stored chain")
            .is_empty()
    );
}

#[test]
fn a_restarted_broker_continues_the_stored_chain() {
    let root = TempDir::new().expect("broker state directory");
    let request = request("session-1");
    let authorization = consumed_authorization(root.path(), &request);
    let launch = {
        let receipts = ReceiptStore::open(&root.path().join("receipts"), trusted_release())
            .expect("open receipt store");
        let launch = launch_receipt(&authorization);
        receipts
            .append(
                &authorization,
                &launch.canonical_bytes(),
                None,
                verify_fixture_signature,
            )
            .expect("sequence zero is durable");
        launch
    };

    let restarted = ReceiptStore::open(&root.path().join("receipts"), trusted_release())
        .expect("reopen receipt store");
    let start = start_receipt(&authorization, &launch);
    let acknowledgement = restarted
        .append(
            &authorization,
            &start.canonical_bytes(),
            None,
            verify_fixture_signature,
        )
        .expect("sequence one continues the recovered chain");
    assert_eq!(acknowledgement.sequence, 1);

    let replayed = restarted.append(
        &authorization,
        &start.canonical_bytes(),
        None,
        verify_fixture_signature,
    );
    assert!(matches!(replayed, Err(BrokerError::ReceiptRefused(_))));
}

/// Kernel identity of this test process, which plays both peers.
fn local_pin() -> CredentialPin {
    CredentialPin::Identity {
        uid: getuid().as_raw(),
        gid: getgid().as_raw(),
    }
}

/// Blocks on one transport completion, as a fake supervisor would.
fn settle<T>(start: impl FnOnce(TransportCompletion<T>) -> Result<(), TransportError>) -> T
where
    T: Send + 'static,
{
    let (sender, receiver) = mpsc::sync_channel(1);
    start(Box::new(move |result| {
        let _delivered = sender.send(result);
    }))
    .expect("transport admits the operation");
    receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("transport completes")
        .expect("transport operation succeeds")
}

fn expect_disconnect(channel: &SeqpacketChannel) {
    let (sender, receiver) = mpsc::sync_channel(1);
    channel
        .receive(Box::new(move |result| {
            let _delivered = sender.send(result);
        }))
        .expect("receive admitted on the peer");
    assert!(matches!(
        receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("peer observes shutdown"),
        Err(TransportError::Disconnected)
    ));
}

/// Drives the supervisor half of one launch over the production transport.
///
/// This is the deterministic fake supervisor: it signs with the fixture key and
/// never spawns a process, but it speaks the exact wire protocol the installed
/// launcher speaks.
fn fake_supervisor(
    socket: &Path,
    request: &LaunchRequest,
    now_ms: u64,
) -> (LaunchAuthorization, SeqpacketChannel) {
    fake_supervisor_for(socket, request, now_ms, CONTROLLER_UID)
}

fn fake_supervisor_for(
    socket: &Path,
    request: &LaunchRequest,
    now_ms: u64,
    uid: u32,
) -> (LaunchAuthorization, SeqpacketChannel) {
    let (authorization, channel) = supervisor_authorization_for(socket, request, now_ms, uid);

    let launch = launch_receipt(&authorization);
    settle(|complete| channel.send(launch.canonical_bytes(), complete));
    let acknowledgement = expect_acknowledgement(&channel);
    assert_eq!(acknowledgement.sequence, 0);
    assert_eq!(
        acknowledgement.disposition,
        ReceiptDisposition::DurablyStored
    );

    let start = start_receipt(&authorization, &launch);
    settle(|complete| channel.send(start.canonical_bytes(), complete));
    let acknowledgement = expect_acknowledgement(&channel);
    assert_eq!(acknowledgement.sequence, 1);
    assert_eq!(acknowledgement.receipt_digest, start.digest().to_string());

    (authorization, channel)
}

fn supervisor_authorization(
    socket: &Path,
    request: &LaunchRequest,
    now_ms: u64,
) -> (LaunchAuthorization, SeqpacketChannel) {
    supervisor_authorization_for(socket, request, now_ms, CONTROLLER_UID)
}

fn supervisor_authorization_for(
    socket: &Path,
    request: &LaunchRequest,
    now_ms: u64,
    uid: u32,
) -> (LaunchAuthorization, SeqpacketChannel) {
    let connector = SeqpacketConnector::new().expect("connector");
    let channel = settle(|complete| connector.connect(socket, local_pin(), complete));

    settle(|complete| channel.send(request.canonical_bytes(), complete));
    let packet = settle(|complete| channel.receive(complete));
    let LauncherPacket::Response(response) = packet.packet else {
        panic!("expected a correlated response, got {:?}", packet.packet);
    };
    let ResponseResult::LaunchAuthorization { authorization } = response.result else {
        panic!("expected an authorization, got {:?}", response.result);
    };
    authorization
        .validate_for(request, uid, now_ms)
        .expect("the broker's authorization binds this exact launch");

    (authorization, channel)
}

fn expect_acknowledgement(channel: &SeqpacketChannel) -> ReceiptAcknowledgement {
    let packet = settle(|complete| channel.receive(complete));
    match packet.packet {
        LauncherPacket::Request(ProtocolMessage::ReceiptAcknowledgement(acknowledgement)) => {
            acknowledgement
        }
        other => panic!("expected a durable acknowledgement, got {other:?}"),
    }
}

fn assert_post_launch_exchange(
    broker_channel: &SeqpacketChannel,
    supervisor_channel: &SeqpacketChannel,
    authorization: &LaunchAuthorization,
) {
    let status_request = StatusRequest {
        schema: STATUS_REQUEST_SCHEMA.to_owned(),
        protocol_version: PROTOCOL_VERSION,
        request_id: "post-launch-status".to_owned(),
        session_id: authorization.session_id.clone(),
        run_id: authorization.run_id.clone(),
    };
    settle(|complete| broker_channel.send(status_request.canonical_bytes(), complete));
    let received = settle(|complete| supervisor_channel.receive(complete));
    assert_eq!(
        received.packet,
        LauncherPacket::Request(ProtocolMessage::Status(status_request.clone()))
    );
    // This fake supervisor has no live process to inspect. Its typed refusal
    // still proves correlation and bidirectional use of the original channel.
    let response = ProtocolResponse {
        schema: RESPONSE_SCHEMA.to_owned(),
        protocol_version: PROTOCOL_VERSION,
        request_id: status_request.request_id,
        result: ResponseResult::Error {
            error: ProtocolError::new(
                ErrorCode::StateMismatch,
                Some(SessionState::Running),
                Some(1),
            ),
        },
    };
    settle(|complete| supervisor_channel.send(response.canonical_bytes(), complete));
    let received = settle(|complete| broker_channel.receive(complete));
    assert_eq!(
        received.packet,
        LauncherPacket::Response(Box::new(response))
    );
    assert_eq!(received.peer_credentials, received.message_credentials);
}

fn assert_command_exchange(
    session: &mut louiselm_skills::broker::BrokerSession,
    supervisor: &SeqpacketChannel,
) {
    use louiselm_skills::launch_protocol::{
        COMMAND_SCHEMA, CommandMessage, CommandOperation, CommandOutcome, CommandPrincipal,
        TOOL_EXECUTION_SCHEMA, ToolExecutionRequest,
    };
    let authorization = session.authorization().clone();
    let principal = CommandPrincipal {
        channel_id: "agent-capability".to_owned(),
        pid: 123,
        uid: authorization.assigned_uid,
        gid: authorization.assigned_gid,
    };
    let mut message = CommandMessage {
        schema: COMMAND_SCHEMA.to_owned(),
        protocol_version: 1,
        request_id: "command-1".to_owned(),
        session_id: authorization.session_id.clone(),
        run_id: authorization.run_id.clone(),
        envelope_revision: authorization.envelope_revision,
        operation: CommandOperation::Request {
            principal,
            command: ToolExecutionRequest {
                schema: TOOL_EXECUTION_SCHEMA.to_owned(),
                protocol_version: 1,
                request_id: "command-1".to_owned(),
                session_id: authorization.session_id,
                run_id: authorization.run_id,
                envelope_revision: authorization.envelope_revision,
                sequence: 1,
                command: "printf sensitive".to_owned(),
                timeout_ms: 1000,
            },
        },
    };
    settle(|complete| supervisor.send(message.canonical_bytes(), complete));
    session.serve_command().unwrap();
    assert!(matches!(
        settle(|complete| supervisor.receive(complete)).packet,
        LauncherPacket::Request(ProtocolMessage::Command(CommandMessage {
            operation: CommandOperation::Authorize {
                dispatch_sequence: 1,
                ..
            },
            ..
        }))
    ));
    assert_grant_exchange(session, supervisor, &message);
    session.revoke_commands("revoke-command").unwrap();
    let revoke = settle(|complete| supervisor.receive(complete));
    let LauncherPacket::Request(ProtocolMessage::Command(mut revoke)) = revoke.packet else {
        panic!("expected revocation")
    };
    assert!(!session.command_revocation_complete());
    message.operation = CommandOperation::Outcome {
        dispatch_sequence: 1,
        outcome: CommandOutcome::Unknown,
    };
    settle(|complete| supervisor.send(message.canonical_bytes(), complete));
    session.serve_command().unwrap();
    assert!(matches!(
        settle(|complete| supervisor.receive(complete)).packet,
        LauncherPacket::Request(ProtocolMessage::Command(CommandMessage {
            operation: CommandOperation::OutcomeAcknowledged {
                dispatch_sequence: 1
            },
            ..
        }))
    ));
    revoke.operation = CommandOperation::Revoked { enforced: true };
    settle(|complete| supervisor.send(revoke.canonical_bytes(), complete));
    session.serve_command().unwrap();
    assert!(session.command_revocation_complete());
}

fn assert_grant_exchange(
    session: &mut louiselm_skills::broker::BrokerSession,
    supervisor: &SeqpacketChannel,
    agent: &louiselm_skills::launch_protocol::CommandMessage,
) {
    use louiselm_skills::launch_protocol::{
        CommandMessage, CommandOperation, CommandOutcome, GrantRequest,
    };
    let CommandOperation::Request { principal, command } = &agent.operation else {
        panic!("Agent request")
    };
    let mut tool = principal.clone();
    tool.pid += 1;
    tool.channel_id = "tool-capability-1".into();
    let mut message = agent.clone();
    message.request_id = "grant-request".into();
    message.operation = CommandOperation::DelegationRequest {
        principal: principal.clone(),
        tool: tool.clone(),
        grant: GrantRequest {
            sequence: 1,
            command_digest: Digest::of(command.command.as_bytes()).to_string(),
            timeout_ms: 1000,
            uses: Some(1),
            valid_for_ms: 5000,
        },
    };
    settle(|complete| supervisor.send(message.canonical_bytes(), complete));
    session.serve_command().unwrap();
    assert!(matches!(
        settle(|complete| supervisor.receive(complete)).packet,
        LauncherPacket::Request(ProtocolMessage::Command(CommandMessage {
            operation: CommandOperation::Granted { grant: 1, .. },
            ..
        }))
    ));
    message.request_id = "tool-command".into();
    let mut command = command.clone();
    command.request_id.clone_from(&message.request_id);
    message.operation = CommandOperation::Request {
        principal: tool,
        command,
    };
    settle(|complete| supervisor.send(message.canonical_bytes(), complete));
    session.serve_command().unwrap();
    assert!(matches!(
        settle(|complete| supervisor.receive(complete)).packet,
        LauncherPacket::Request(ProtocolMessage::Command(CommandMessage {
            operation: CommandOperation::Authorize {
                dispatch_sequence: 2,
                ..
            },
            ..
        }))
    ));
    session.revoke_tool_grant("revoke-grant", 1).unwrap();
    let LauncherPacket::Request(ProtocolMessage::Command(mut revoke)) =
        settle(|complete| supervisor.receive(complete)).packet
    else {
        panic!("grant revocation")
    };
    assert_eq!(revoke.operation, CommandOperation::RevokeGrant { grant: 1 });
    assert!(!session.tool_grant_revocation_complete(1));
    revoke.operation = CommandOperation::GrantRevoked {
        grant: 1,
        enforced: true,
    };
    settle(|complete| supervisor.send(revoke.canonical_bytes(), complete));
    session.serve_command().unwrap();
    assert!(session.tool_grant_revocation_complete(1));
    assert!(!session.command_revocation_complete());
    message.operation = CommandOperation::Outcome {
        dispatch_sequence: 2,
        outcome: CommandOutcome::Unknown,
    };
    settle(|complete| supervisor.send(message.canonical_bytes(), complete));
    session.serve_command().unwrap();
    // The previous revocation ACK must not have produced an echo packet.
    assert!(matches!(
        settle(|complete| supervisor.receive(complete)).packet,
        LauncherPacket::Request(ProtocolMessage::Command(CommandMessage {
            operation: CommandOperation::OutcomeAcknowledged {
                dispatch_sequence: 2
            },
            ..
        }))
    ));
}

#[test]
fn the_production_rendezvous_carries_one_complete_launch_transaction() {
    let root = TempDir::new().expect("broker state directory");
    let socket = root.path().join("control.sock");
    let request = request("session-1");

    let authorizations =
        AuthorizationStore::open(&root.path().join("authorizations"), pool(4)).expect("open store");
    let pending = authorizations
        .authorize(&grant(&request), 1_000)
        .expect("authorize");
    let receipts = ReceiptStore::open(&root.path().join("receipts"), trusted_release())
        .expect("open receipt store");
    let audit = AuditLog::open(&root.path().join("audit")).expect("open the operator record");
    let service = BrokerService::bind(&socket, authorizations, receipts, audit, local_pin())
        .expect("bind the rendezvous");

    let supervisor = {
        let socket = socket.clone();
        let request = request.clone();
        thread::spawn(move || fake_supervisor(&socket, &request, 2_000))
    };
    let mut session = service
        .serve_launch(2_000, verify_fixture_signature)
        .expect("the broker completes the transaction");
    let (authorization, supervisor_channel) = supervisor.join().expect("supervisor thread");

    assert_eq!(session.authorization(), &authorization);
    assert_eq!(session.authorization().session_id, request.session_id);
    assert_eq!(
        session.authorization().authorization_id,
        request.authorization_id
    );
    assert_eq!(session.authorization().identity_slot, pending.identity.slot);
    assert_eq!(session.launch_head().sequence, 1);
    assert_eq!(authorization.assigned_uid, pending.identity.uid);

    let stored = service
        .receipts()
        .stored_bytes(&request.session_id)
        .expect("stored chain");
    let launch = launch_receipt(&authorization);
    let start = start_receipt(&authorization, &launch);
    assert_eq!(
        stored,
        vec![launch.canonical_bytes(), start.canonical_bytes()]
    );
    assert_eq!(session.launch_head().digest, start.digest().to_string());

    // The authorization is spent: a second supervisor cannot replay it.
    let replay = service
        .authorizations()
        .consume_for_launcher(&request, 2_500);
    assert!(matches!(replay, Err(BrokerError::UnknownAuthorization)));
    service.close();
    drop(service);

    // Dropping the listener/store owner must not end this Session's channel.
    assert_post_launch_exchange(
        session.channel(),
        &supervisor_channel,
        session.authorization(),
    );
    assert_command_exchange(&mut session, &supervisor_channel);

    let (sender, disconnected) = mpsc::sync_channel(1);
    supervisor_channel
        .receive(Box::new(move |result| {
            let _delivered = sender.send(result);
        }))
        .expect("receive is pending before owner drop");
    let channel_clone = session.channel().clone();
    drop(session);
    assert!(
        channel_clone.is_closed(),
        "a clone cannot outlive its Session authority"
    );
    assert!(matches!(
        disconnected
            .recv_timeout(Duration::from_secs(5))
            .expect("pending receive completes"),
        Err(TransportError::Disconnected)
    ));
}

#[test]
fn an_incomplete_launch_never_hands_off_a_live_session() {
    for rejected_sequence in [0, 1] {
        let root = TempDir::new().expect("broker state directory");
        let socket = root.path().join("control.sock");
        let request = request("session-1");
        let authorizations = AuthorizationStore::open(&root.path().join("authorizations"), pool(4))
            .expect("open store");
        authorizations
            .authorize(&grant(&request), 1_000)
            .expect("authorize");
        let receipts = ReceiptStore::open(&root.path().join("receipts"), trusted_release())
            .expect("open receipt store");
        let audit = AuditLog::open(&root.path().join("audit")).expect("open audit");
        let service = BrokerService::bind(&socket, authorizations, receipts, audit, local_pin())
            .expect("bind rendezvous");
        let supervisor = {
            let request = request.clone();
            thread::spawn(move || {
                let (authorization, channel) = supervisor_authorization(&socket, &request, 2_000);
                let launch = launch_receipt(&authorization);
                let wrong_receipt = if rejected_sequence == 0 {
                    start_receipt(&authorization, &launch)
                } else {
                    settle(|complete| channel.send(launch.canonical_bytes(), complete));
                    assert_eq!(expect_acknowledgement(&channel).sequence, 0);
                    launch
                };
                settle(|complete| channel.send(wrong_receipt.canonical_bytes(), complete));
                expect_disconnect(&channel);
            })
        };

        assert!(matches!(
            service.serve_launch(2_000, verify_fixture_signature),
            Err(BrokerError::ReceiptUnauthorized)
        ));
        supervisor
            .join()
            .expect("supervisor observes closure without an ACK");
        let inspection = service
            .inspect(&request.session_id)
            .expect("inspect")
            .expect("consumed");
        assert_eq!(inspection.state, SessionState::Starting);
        assert_eq!(
            inspection.launch,
            louiselm_skills::broker::LaunchObservation::DurableOnly
        );
        assert_eq!(inspection.launch_evidence.is_some(), rejected_sequence == 1);
        assert!(inspection.start_evidence.is_none());
        assert_eq!(
            inspection.broker_head.map(|head| head.sequence),
            if rejected_sequence == 0 {
                None
            } else {
                Some(0)
            }
        );
    }
}

#[test]
fn a_receipt_that_cannot_be_stored_is_never_acknowledged() {
    if getuid().is_root() {
        // A read-only directory does not stop root, so this boundary cannot be
        // observed here. Reported rather than silently passing.
        eprintln!("skipped: durable-failure coverage needs an unprivileged user");
        return;
    }
    let root = TempDir::new().expect("broker state directory");
    let request = request("session-1");
    let authorization = consumed_authorization(root.path(), &request);
    let sessions = root.path().join("receipts").join("sessions");
    let receipts = ReceiptStore::open(&root.path().join("receipts"), trusted_release())
        .expect("open receipt store");
    let launch = launch_receipt(&authorization);

    fs::set_permissions(&sessions, fs::Permissions::from_mode(0o500))
        .expect("make durable storage unwritable");
    let failure = receipts.append(
        &authorization,
        &launch.canonical_bytes(),
        None,
        verify_fixture_signature,
    );
    fs::set_permissions(&sessions, fs::Permissions::from_mode(0o700))
        .expect("restore durable storage");

    assert!(
        matches!(failure, Err(BrokerError::Storage(_))),
        "a receipt that never reached the disk is not acknowledged: {failure:?}"
    );
    assert!(
        receipts
            .stored_bytes(&authorization.session_id)
            .expect("stored chain")
            .is_empty()
    );
}

#[test]
fn the_operator_record_stays_normalized_after_a_launch() {
    let root = TempDir::new().expect("broker state directory");
    let socket = root.path().join("control.sock");
    let request = request("session-1");

    let authorizations =
        AuthorizationStore::open(&root.path().join("authorizations"), pool(4)).expect("open store");
    authorizations
        .authorize(&grant(&request), 1_000)
        .expect("authorize");
    let receipts = ReceiptStore::open(&root.path().join("receipts"), trusted_release())
        .expect("open receipt store");
    let audit = AuditLog::open(&root.path().join("audit")).expect("open the operator record");
    let service = BrokerService::bind(&socket, authorizations, receipts, audit, local_pin())
        .expect("bind the rendezvous");

    let supervisor = {
        let socket = socket.clone();
        let request = request.clone();
        thread::spawn(move || fake_supervisor(&socket, &request, 2_000))
    };
    let session = service
        .serve_launch(2_000, verify_fixture_signature)
        .expect("the broker completes the transaction");
    let (_, supervisor_channel) = supervisor.join().expect("supervisor thread");

    let inspection = service
        .inspect(&request.session_id)
        .expect("inspect")
        .expect("a launched Session has broker-owned state");
    assert_eq!(inspection.session_id, request.session_id);
    assert_eq!(inspection.run_id, request.run_id);
    assert_eq!(inspection.envelope_revision, request.envelope_revision);
    assert_eq!(inspection.state, SessionState::Running);
    assert_eq!(inspection.broker_head.expect("head").sequence, 1);
    assert!(inspection.last_failure.is_none());
    assert_eq!(
        inspection.launch,
        louiselm_skills::broker::LaunchObservation::DurableOnly
    );
    assert!(inspection.launch_evidence.is_some());
    assert!(inspection.start_evidence.is_some());
    assert_eq!(
        service.inspect_active(&session).unwrap().launch,
        louiselm_skills::broker::LaunchObservation::Acknowledged { channel_open: true }
    );
    session.close();
    assert_eq!(
        service.inspect_active(&session).unwrap().launch,
        louiselm_skills::broker::LaunchObservation::Acknowledged {
            channel_open: false
        }
    );

    let decisions: Vec<AuditDecision> = service
        .audit()
        .expect("audit")
        .into_iter()
        .map(|entry| {
            assert_eq!(entry.session_id, request.session_id);
            assert_eq!(entry.identity_slot, inspection.identity_slot);
            entry.decision
        })
        .collect();
    assert_eq!(
        decisions,
        vec![
            AuditDecision::AuthorizationConsumed,
            AuditDecision::ReceiptStored { sequence: 0 },
            AuditDecision::ReceiptStored { sequence: 1 },
        ],
        "the operator sees one normalized entry per broker decision"
    );

    // Nothing but normalized identifiers reaches the operator record: no
    // signature material, no receipt payloads, no request bytes.
    let recorded = fs::read(root.path().join("audit").join("decisions.jsonl"))
        .expect("durable operator record");
    let recorded = String::from_utf8(recorded).expect("audit is text");
    assert!(!recorded.contains(SIGNED_RECEIPT_SCHEMA));
    assert!(!recorded.contains("signature"));
    assert!(!recorded.contains("evidence"));

    // Credential custody extends the same closed-record boundary: no secret
    // field is admitted, even when attached to an otherwise valid decision.
    let entry = &service.audit().unwrap()[0];
    for field in ["credential", "provider_credentials", "authorization_header"] {
        let mut value = serde_json::to_value(entry).unwrap();
        value[field] = serde_json::json!("fixture-provider-secret");
        assert!(serde_json::from_value::<louiselm_skills::broker::AuditEntry>(value).is_err());
    }

    session.close();
    session.close();
    assert!(session.channel().is_closed());
    expect_disconnect(&supervisor_channel);
}

#[test]
fn a_refused_launch_is_recorded_as_a_stable_error() {
    let root = TempDir::new().expect("broker state directory");
    let socket = root.path().join("control.sock");
    let request = request("session-1");

    let authorizations =
        AuthorizationStore::open(&root.path().join("authorizations"), pool(4)).expect("open store");
    authorizations
        .authorize(&grant(&request), 1_000)
        .expect("authorize");
    let receipts = ReceiptStore::open(&root.path().join("receipts"), trusted_release())
        .expect("open receipt store");
    let audit = AuditLog::open(&root.path().join("audit")).expect("open the operator record");
    let service = BrokerService::bind(&socket, authorizations, receipts, audit, local_pin())
        .expect("bind the rendezvous");

    // The supervisor arrives after the authorization has expired.
    let refused = {
        let socket = socket.clone();
        let request = request.clone();
        thread::spawn(move || {
            let connector = SeqpacketConnector::new().expect("connector");
            let channel = settle(|complete| connector.connect(&socket, local_pin(), complete));
            settle(|complete| channel.send(request.canonical_bytes(), complete));
            let packet = settle(|complete| channel.receive(complete));
            expect_disconnect(&channel);
            packet.packet
        })
    };
    let outcome = service.serve_launch(30_000, verify_fixture_signature);
    let answer = refused.join().expect("supervisor thread");

    assert!(matches!(outcome, Err(BrokerError::Expired)));
    let LauncherPacket::Response(response) = answer else {
        panic!("expected a correlated refusal, got {answer:?}");
    };
    assert!(matches!(response.result, ResponseResult::Error { .. }));

    let decisions: Vec<AuditDecision> = service
        .audit()
        .expect("audit")
        .into_iter()
        .map(|entry| entry.decision)
        .collect();
    assert_eq!(
        decisions,
        vec![AuditDecision::AuthorizationRefused {
            error: ErrorCode::InvalidRequest
        }],
        "a refusal is recorded as one stable code, not as prose"
    );
}
