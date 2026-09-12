//! Installed controller -> broker -> serialized supervisor -> protected storage.

use super::*;
use crate::{
    broker::{BrokerSession, lifecycle::LifecycleCaller, recovery::RecoveryReadiness},
    launch_protocol::{
        LIFECYCLE_REQUEST_SCHEMA, LifecycleAction, LifecycleRequest, RECOVERY_REQUEST_SCHEMA,
        RecoveryRequest, RetentionRequest,
    },
    launch_receipt::SessionState,
};

pub(super) fn assert_loss_settled(root: &Path) {
    let bytes = fs::read(root.join("state/authorizations/controller-loss/session.json")).unwrap();
    let record: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        record["acknowledgement"]["disposition"]["kind"],
        "recoverable"
    );
    let outbox =
        crate::broker::attention::Outbox::open(&root.join("state/authorizations/attention-outbox"))
            .unwrap();
    assert!(
        outbox.next().unwrap().is_some(),
        "capture-service outage cannot prevent local settlement"
    );
    assert_eq!(
        fs::metadata(root.join("sessions/session")).unwrap().mode() & 0o777,
        0o700
    );
}

pub(super) fn controller_registers(
    broker: &InstalledBroker,
    session: &mut BrokerSession,
    uid: u32,
) {
    assert!(broker.admit_recovery("session").is_err());
    // First controller prompt attempts a governed effect before recovery admission.
    assert!(!broker.step(session).unwrap());
    let mut input = String::new();
    std::io::stdin().read_line(&mut input).unwrap();
    assert_eq!(input, "checkpoint-initialized\n");
    let caller = LifecycleCaller::Operator { uid };
    let park = LifecycleRequest {
        schema: LIFECYCLE_REQUEST_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        request_id: "recovery-park".into(),
        session_id: "session".into(),
        run_id: "run".into(),
        authorization_id: "operator-recovery-park".into(),
        action: LifecycleAction::Park,
        expected_state: SessionState::Running,
        expected_receipt_sequence: Some(1),
        envelope_revision: 1,
    };
    broker.request_lifecycle(session, &caller, &park).unwrap();
    let now = u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    let request = RecoveryRequest {
        schema: RECOVERY_REQUEST_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        launch: super::request(),
        head: broker
            .inspect("session")
            .unwrap()
            .unwrap()
            .broker_head
            .unwrap(),
        retention: RetentionRequest {
            request_id: "retain-initial".into(),
            acp_session_id: "fixture-acp".into(),
            expires_at_ms: now + 60000,
        },
    };
    let evidence = broker
        .register_recovery(session, &caller, &request)
        .unwrap();
    assert_eq!(
        broker
            .register_recovery(session, &caller, &request)
            .unwrap(),
        evidence
    );
    assert_eq!(evidence.request.acp_session_id, "fixture-acp");
    assert!(matches!(
        broker.recovery_readiness("session").unwrap(),
        RecoveryReadiness::Ready { .. }
    ));
    broker.admit_recovery("session").unwrap();
    assert_eq!(
        broker.inspect("session").unwrap().unwrap().state,
        SessionState::Parked
    );
    let resume = LifecycleRequest {
        request_id: "operator-resume-after-recovery".into(),
        authorization_id: "operator-resume-after-recovery".into(),
        action: LifecycleAction::Resume,
        expected_state: SessionState::Parked,
        expected_receipt_sequence: Some(2),
        ..park
    };
    broker.request_lifecycle(session, &caller, &resume).unwrap();
    println!("BROKER_RECOVERY_READY");
}

pub(super) fn initialize_checkpoint(
    input: &mut impl Write,
    output: &mut impl BufRead,
    command: &ToolExecutionRequest,
    broker: &mut Child,
    lines: &mpsc::Receiver<String>,
) {
    let response = exchange(input, output, &command.canonical_bytes());
    assert!(matches!(
        response.operation,
        CommandOperation::Result {
            outcome: CommandOutcome::NotStarted { .. }
        }
    ));
    input.write_all(b"\x1dsave fixture-acp\n").unwrap();
    input.flush().unwrap();
    let mut saved = String::new();
    output.read_line(&mut saved).unwrap();
    assert!(saved.starts_with('\x1d'));
    broker
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"checkpoint-initialized\n")
        .unwrap();
    marker(lines, "BROKER_RECOVERY_READY");
    input.write_all(&[0x1f]).unwrap();
    input.flush().unwrap();
    let mut reconnected = String::new();
    output.read_line(&mut reconnected).unwrap();
    assert_eq!(reconnected, "\x1f\n");
}
