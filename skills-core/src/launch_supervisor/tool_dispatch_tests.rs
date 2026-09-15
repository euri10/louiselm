//! Real socket -> broker policy -> Session owner -> isolated command path.
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "Tests assert deterministic fixture setup and outcomes."
)]

use super::super::SessionResources;
use super::*;
use crate::{
    Digest,
    broker::{
        AuditDecision, AuditLog,
        commands::CommandAuthority,
        delegation::{CommandScope, DelegationPolicy},
    },
    launch_protocol::{ProtocolMessage, TOOL_EXECUTION_SCHEMA},
    launch_receipt::{
        RECEIPT_SCHEMA, ReceiptAuthority, ReceiptCause, ReceiptOutcome, ReceiptPayload,
        SIGNED_RECEIPT_SCHEMA, SignedReceipt,
    },
    launch_supervisor::{
        AgentAuthentication, LaunchSigner, MechanicFailure, RelayStdio, RunningAgent,
        RunningAgentEvents, SupervisorCompletion, SupervisorTimer,
        command::CommandPermit,
        system::command_test_support::{fixture, settle},
        tool_execution::ToolExecutor,
    },
    launch_transport::{KernelProcess, LauncherPacket, SeqpacketChannel},
};
use std::sync::{
    atomic::{AtomicBool, AtomicU8, Ordering},
    mpsc,
};

#[path = "grant_dispatch_tests.rs"]
mod grant_tests;

#[path = "recovery_dispatch_tests.rs"]
mod recovery_tests;

type HeldRecovery = Arc<
    Mutex<
        Option<(
            crate::launch_protocol::RetentionRequest,
            crate::launch_supervisor::recovery::RecoveryCompletion,
        )>,
    >,
>;

type HeldRestore = Arc<
    Mutex<
        Option<(
            crate::launch_protocol::RecoveryRestoreRequest,
            crate::launch_supervisor::recovery::RestoreCompletion,
        )>,
    >,
>;

type HeldCompletion = Arc<
    Mutex<
        Option<(
            SupervisorCompletion<ToolExecutionResult>,
            Result<ToolExecutionResult, SupervisorError>,
        )>,
    >,
>;

struct TestProcess {
    restore: HeldRestore,
    recovery: HeldRecovery,
    tools: ToolExecutor,
    pin: Arc<KernelProcess>,
    held: Option<HeldCompletion>,
    fail_cleanup: Arc<AtomicBool>,
}
impl RunningAgent for TestProcess {
    fn restore_recovery(
        &mut self,
        request: crate::launch_protocol::RecoveryRestoreRequest,
        complete: crate::launch_supervisor::recovery::RestoreCompletion,
    ) -> Result<(), SupervisorError> {
        *self.restore.lock().unwrap() = Some((request, complete));
        Ok(())
    }
    fn retain_recovery(
        &mut self,
        request: crate::launch_protocol::RetentionRequest,
        complete: crate::launch_supervisor::recovery::RecoveryCompletion,
    ) -> Result<(), SupervisorError> {
        *self.recovery.lock().unwrap() = Some((request, complete));
        Ok(())
    }
    fn launch_helper(
        &mut self,
        request: CommandMessage,
        enforcer: Arc<crate::launch_supervisor::command::CommandEnforcer>,
        complete: SupervisorCompletion<crate::launch_supervisor::HelperPrincipal>,
    ) -> Result<(), SupervisorError> {
        self.tools.launch_helper(request, enforcer, complete)
    }
    fn cancel_helper(&mut self) -> Result<(), SupervisorError> {
        self.tools.cancel_helper()
    }
    fn execute_tool(
        &mut self,
        permit: CommandPermit,
        complete: SupervisorCompletion<ToolExecutionResult>,
    ) -> Result<(), SupervisorError> {
        if let Some(held) = self.held.clone() {
            self.tools.execute(
                permit,
                Box::new(move |result| {
                    *held.lock().unwrap() = Some((complete, result));
                }),
            )
        } else {
            self.tools.execute(permit, complete)
        }
    }
    fn cancel_tool(&mut self) -> Result<(), SupervisorError> {
        self.tools.cancel()?;
        if self.fail_cleanup.load(Ordering::Acquire) {
            Err(SupervisorError::CleanupUnproven)
        } else {
            Ok(())
        }
    }
    fn authentication(&self) -> Result<AgentAuthentication, SupervisorError> {
        Ok(AgentAuthentication {
            credentials: self.pin.credentials(),
            process: Some(Arc::clone(&self.pin)),
            tool_isolation: None,
        })
    }
    fn start_relay(
        &mut self,
        _controller: mpsc::Receiver<RelayStdio>,
        _events: RunningAgentEvents,
    ) -> Result<(), SupervisorError> {
        Err(SupervisorError::RelayFailed)
    }
    fn quiesce_relay(&mut self, complete: SupervisorCompletion<()>) -> Result<(), SupervisorError> {
        complete(Ok(()));
        Ok(())
    }
    fn park(&mut self) -> Result<(), MechanicFailure> {
        Err(MechanicFailure::Ambiguous)
    }
    fn resume(&mut self) -> Result<(), MechanicFailure> {
        Err(MechanicFailure::Ambiguous)
    }
    fn interrupt(&mut self) -> Result<(), MechanicFailure> {
        Err(MechanicFailure::Ambiguous)
    }
    fn dispose(&mut self) -> Result<(), SupervisorError> {
        self.tools.dispose()?;
        self.cancel_tool()
    }
}

#[derive(Default)]
struct Timer(Mutex<Vec<Box<dyn FnOnce() + Send>>>);
impl SupervisorTimer for Timer {
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn schedule(
        &self,
        _delay: Duration,
        complete: Box<dyn FnOnce() + Send>,
    ) -> Result<(), SupervisorError> {
        self.0.lock().unwrap().push(complete);
        Ok(())
    }
}
impl Timer {
    fn expire(&self) {
        for complete in std::mem::take(&mut *self.0.lock().unwrap()) {
            std::thread::spawn(complete).join().unwrap();
        }
    }
}
struct NoSigner;
impl LaunchSigner for NoSigner {
    fn check_authority(&self, complete: SupervisorCompletion<()>) -> Result<(), SupervisorError> {
        complete(Ok(()));
        Ok(())
    }
    fn record_containment(
        &self,
        _: String,
        _: crate::launcher_install::KeyContainment,
        complete: SupervisorCompletion<()>,
    ) -> Result<(), SupervisorError> {
        complete(Ok(()));
        Ok(())
    }
    fn release_id(&self) -> &'static str {
        "fixture-release"
    }
    fn signing_key_id(&self) -> &'static str {
        "fixture-key"
    }
    fn sign(
        &self,
        _bytes: Vec<u8>,
        _complete: SupervisorCompletion<String>,
    ) -> Result<(), SupervisorError> {
        Err(SupervisorError::SigningUnavailable)
    }
}

struct TestIdentity {
    identity: crate::launcher_install::Identity,
    disposition: Arc<AtomicU8>,
}
impl crate::launch_supervisor::IdentityGuard for TestIdentity {
    fn identity(&self) -> crate::launcher_install::Identity {
        self.identity
    }
    fn release(self: Box<Self>) -> Result<(), SupervisorError> {
        self.disposition.store(1, Ordering::Release);
        Ok(())
    }
    fn poison(self: Box<Self>) -> Result<(), SupervisorError> {
        self.disposition.store(2, Ordering::Release);
        Ok(())
    }
}

struct Harness {
    restore: HeldRestore,
    recovery: HeldRecovery,
    owner: SessionOwner,
    agent: SeqpacketChannel,
    broker: SeqpacketChannel,
    authority: CommandAuthority,
    audit: Arc<AuditLog>,
    timer: Arc<Timer>,
    held: HeldCompletion,
    fail_cleanup: Arc<AtomicBool>,
    identity_disposition: Arc<AtomicU8>,
    root: tempfile::TempDir,
}

impl Harness {
    fn new(command: &str, delay_delivery: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let parts = fixture(root.path());
        let binding = parts.binding;
        let held: HeldCompletion = Arc::default();
        let fail_cleanup = Arc::new(AtomicBool::new(false));
        let identity_disposition = Arc::new(AtomicU8::new(0));
        let process = TestProcess {
            restore: Arc::default(),
            recovery: Arc::default(),
            tools: parts.tools,
            pin: parts.process,
            held: delay_delivery.then(|| Arc::clone(&held)),
            fail_cleanup: Arc::clone(&fail_cleanup),
        };
        let recovery = Arc::clone(&process.recovery);
        let restore = Arc::clone(&process.restore);
        let audit = Arc::new(AuditLog::open(&root.path().join("audit")).unwrap());
        let authority = CommandAuthority::new(
            binding.clone(),
            DelegationPolicy {
                authorization_id: "approved".to_owned(),
                scope: CommandScope {
                    command_digest: Digest::of(command.as_bytes()),
                    timeout_ms: 5000,
                    uses: Some(1),
                },
                allow_delegation: false,
                expires_at: Instant::now() + Duration::from_secs(30),
            },
            Arc::clone(&audit),
        )
        .unwrap();
        let receipt = SignedReceipt {
            schema: SIGNED_RECEIPT_SCHEMA.to_owned(),
            signature: "fixture-signature".to_owned(),
            payload: ReceiptPayload {
                schema: RECEIPT_SCHEMA.to_owned(),
                session_id: binding.session_id.clone(),
                run_id: binding.run_id.clone(),
                request_id: "start".to_owned(),
                envelope_revision: 1,
                sequence: 1,
                previous_receipt_digest: Some(Digest::of(b"fixture-genesis").to_string()),
                release_id: Digest::of(b"release").to_string(),
                signing_key_id: Digest::of(b"key").to_string(),
                outcome: ReceiptOutcome::Start {
                    evidence: crate::launch_receipt::StartEvidence {
                        agent_pid: binding.agent_pid,
                        assigned_uid: binding.assigned_uid,
                        assigned_gid: binding.assigned_gid,
                        tool_isolation_digest: Digest::of(b"fixture-tool-isolation").to_string(),
                    },
                    authority: ReceiptAuthority::Cause {
                        cause: ReceiptCause::LaunchAcknowledged,
                    },
                },
                resulting_state: SessionState::Running,
            },
        };
        let timer = Arc::new(Timer::default());
        let mut owner = SessionOwner::new(
            SessionResources {
                process: Some(Box::new(process)),
                capability: Some(parts.gate),
                identity: Some(Box::new(TestIdentity {
                    identity: crate::launcher_install::Identity {
                        slot: binding.identity_slot,
                        uid: binding.assigned_uid,
                        gid: binding.assigned_gid,
                    },
                    disposition: Arc::clone(&identity_disposition),
                })),
                broker: Some(parts.broker),
            },
            Arc::new(NoSigner),
            vec![receipt],
            binding,
            Duration::from_secs(1),
            timer.clone(),
            Duration::ZERO,
        );
        owner.arm_broker_receive();
        owner.arm_agent_receive();
        Self {
            restore,
            recovery,
            owner,
            agent: parts.agent,
            broker: parts.broker_channel,
            authority,
            audit,
            timer,
            held,
            fail_cleanup,
            identity_disposition,
            root,
        }
    }
    fn tick(&mut self) {
        match self
            .owner
            .receiver
            .recv_timeout(Duration::from_secs(5))
            .unwrap_or_else(|error| panic!("command event {error:?}: closed={}, helper_pending={}, command_pending={}, Agent_valid={:?}",
                self.owner.commands.closed, self.owner.commands.grants.pending.is_some(), self.owner.commands.pending.is_some(),
                self.owner.resources.capability.as_ref().map(|gate| gate.command_enforcer().and_then(|owner| owner.agent_valid()))))
        {
            OwnerEvent::ToolFinished | OwnerEvent::RelayQuiesced | OwnerEvent::RecoveryFinished | OwnerEvent::RestoreFinished => {}
            OwnerEvent::BrokerRequest {
                connection_epoch,
                result,
            } => self.owner.handle_broker_event(connection_epoch, *result),
            OwnerEvent::ResponseSent {
                connection_epoch,
                finish,
                result,
            } => self
                .owner
                .handle_response_sent(connection_epoch, finish, result),
            OwnerEvent::CommandDeadline { request_id } => self.owner.command_deadline(&request_id),
            OwnerEvent::AgentStatusDeadline { request_id } => {
                self.owner.expire_agent_status(&request_id);
            }
            _ => panic!("unexpected lifecycle event in command-only fixture"),
        }
        self.owner.collect_tool_result();
        self.owner.collect_recovery();
        self.owner.collect_restore();
        self.owner.collect_relay_quiescence();
    }
    fn request(&mut self, command: &str) -> CommandMessage {
        let request = ToolExecutionRequest {
            schema: TOOL_EXECUTION_SCHEMA.to_owned(),
            protocol_version: 1,
            request_id: "command-1".to_owned(),
            session_id: "session-1".to_owned(),
            run_id: "run-1".to_owned(),
            envelope_revision: 1,
            sequence: 1,
            command: command.to_owned(),
            timeout_ms: 5000,
        };
        settle(|complete| self.agent.send(request.canonical_bytes(), complete));
        while self.owner.commands.pending.is_none() {
            self.tick();
        }
        let received = receive(&self.broker);
        assert!(
            matches!(&received.operation,CommandOperation::Request {command,..} if command==&request)
        );
        received
    }
    fn authorize(&mut self, request: &CommandMessage) {
        let decision = self.authority.handle(request).unwrap();
        settle(|complete| self.broker.send(decision.canonical_bytes(), complete));
        while self
            .owner
            .commands
            .pending
            .as_ref()
            .unwrap()
            .dispatch
            .is_none()
        {
            self.tick();
        }
    }
    fn finish(&mut self) -> CommandMessage {
        while self
            .owner
            .commands
            .pending
            .as_ref()
            .unwrap()
            .outcome
            .is_none()
        {
            self.tick();
        }
        let outcome = receive(&self.broker);
        let ack = self.authority.handle(&outcome).unwrap();
        settle(|complete| self.broker.send(ack.canonical_bytes(), complete));
        while self.owner.commands.pending.is_some() {
            self.tick();
        }
        outcome
    }
}

fn receive(channel: &SeqpacketChannel) -> CommandMessage {
    let packet = settle(|complete| channel.receive(complete));
    assert_eq!(packet.peer_credentials, packet.message_credentials);
    let LauncherPacket::Request(ProtocolMessage::Command(message)) = packet.packet else {
        panic!("expected command record")
    };
    message
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "One real socket scenario preserves status before command admission, its canonical reply, and another read after the only command budget is spent."
)]
fn agent_status_crosses_the_authenticated_capability_relay() {
    let mut harness = Harness::new("true", false);
    let query = serde_json::json!({
        "schema": COMMAND_SCHEMA,
        "protocol_version": PROTOCOL_VERSION,
        "request_id": "self-status",
        "session_id": "session-1",
        "run_id": "run-1",
        "envelope_revision": 1,
        "operation": {"kind": "status_request"}
    });
    let mut unknown = query.clone();
    unknown["operation"]["authority"] = serde_json::json!("operator");
    assert!(
        crate::launch_protocol::decode_message(&serde_json::to_vec(&unknown).unwrap()).is_err()
    );
    settle(|complete| {
        harness
            .agent
            .send(serde_json::to_vec(&query).unwrap(), complete)
    });
    harness.tick();
    assert!(
        !harness.owner.commands.closed,
        "read-only status must keep the Agent channel open"
    );
    let mut forwarded = receive(&harness.broker);
    let mut expected_query = query.clone();
    expected_query["request_id"] = serde_json::json!(forwarded.request_id);
    assert_eq!(serde_json::to_value(&forwarded).unwrap(), expected_query);
    assert!(harness.audit.entries().unwrap().is_empty());
    let posture = crate::posture::Posture::evaluate(
        "session-1",
        "run-1",
        crate::posture::DimensionName::ALL
            .into_iter()
            .map(|dimension| {
                crate::posture::DimensionInput::failed(
                    dimension,
                    crate::posture::FailureCode::EvidenceMissing,
                    vec![],
                )
            })
            .collect(),
    )
    .unwrap();
    let status = crate::launch_protocol::SessionStatus::compose(
        harness.owner.status(),
        crate::launch_protocol::PostureStatus::from_posture(
            &posture,
            [crate::launch_protocol::EvidenceFreshness {
                basis: crate::launch_protocol::FreshnessBasis::Missing,
                last_verified_at_ms: None,
            }; 6],
        ),
        crate::launch_protocol::RecoveryReadiness::Expired {},
        vec![],
    )
    .unwrap();
    forwarded.operation = CommandOperation::StatusResult {
        status: Box::new(status),
    };
    let mut forged = forwarded.clone();
    if let CommandOperation::StatusResult { status } = &mut forged.operation {
        status
            .allowed_actions
            .push(crate::launch_protocol::LifecycleAction::Park);
    }
    assert!(
        forged.validate().is_err(),
        "Agent replies cannot carry lifecycle authority"
    );
    let mut foreign = forwarded.clone();
    foreign.session_id = "other-session".into();
    assert!(
        foreign.validate().is_err(),
        "nested status must match its envelope"
    );
    settle(|complete| harness.broker.send(forwarded.canonical_bytes(), complete));
    while harness.owner.commands.status.is_some() {
        harness.tick();
    }
    let mut expected_reply = forwarded.clone();
    expected_reply.request_id = "self-status".into();
    assert_eq!(
        receive(&harness.agent).canonical_bytes(),
        expected_reply.canonical_bytes()
    );
    assert!(harness.audit.entries().unwrap().is_empty());
    // A status read leaves the one permitted command usable; after consumption
    // another read must not revive the spent policy or effect sequence.
    let command = harness.request("true");
    harness.authorize(&command);
    harness.finish();
    receive(&harness.agent);
    let audit = harness.audit.entries().unwrap();
    let sequence = harness.owner.commands.sequence;
    settle(|complete| {
        harness
            .agent
            .send(serde_json::to_vec(&query).unwrap(), complete)
    });
    while harness.owner.commands.status.is_none() {
        harness.tick();
    }
    let next = receive(&harness.broker);
    assert_ne!(next.request_id, forwarded.request_id);
    harness.owner.expire_agent_status(&forwarded.request_id);
    assert!(
        harness.owner.commands.status.is_some(),
        "old deadline cannot end a newer read"
    );
    forwarded.request_id = next.request_id;
    settle(|complete| harness.broker.send(forwarded.canonical_bytes(), complete));
    while harness.owner.commands.status.is_some() {
        harness.tick();
    }
    assert_eq!(
        receive(&harness.agent).canonical_bytes(),
        expected_reply.canonical_bytes()
    );
    assert_eq!(harness.audit.entries().unwrap(), audit);
    assert_eq!(harness.owner.commands.sequence, sequence);
    assert!(harness.authority.handle(&command).is_err());
}

#[test]
fn status_relay_refuses_foreign_subjects_and_ignores_replies_after_revocation() {
    let mut harness = Harness::new("true", false);
    let query = harness
        .owner
        .command_message("self-status", CommandOperation::StatusRequest {});
    for foreign in ["sibling", "absent"] {
        let mut request = query.clone();
        request.session_id = foreign.to_owned();
        settle(|complete| harness.agent.send(request.canonical_bytes(), complete));
        harness.tick();
        let reply = receive(&harness.agent);
        assert!(matches!(
            reply.operation,
            CommandOperation::StatusRefused {
                error: ErrorCode::SubjectMismatch
            }
        ));
        assert!(
            !String::from_utf8(reply.canonical_bytes())
                .unwrap()
                .contains(foreign)
        );
        assert!(harness.owner.commands.status.is_none());
    }
    settle(|complete| harness.agent.send(query.canonical_bytes(), complete));
    while harness.owner.commands.status.is_none() {
        harness.tick();
    }
    let mut reply = receive(&harness.broker);
    reply.operation = CommandOperation::StatusRefused {
        error: ErrorCode::OperationPending,
    };
    let mut uncorrelated = reply.clone();
    uncorrelated.request_id = "wrong-request".into();
    settle(|complete| {
        harness
            .broker
            .send(uncorrelated.canonical_bytes(), complete)
    });
    harness.tick();
    assert!(harness.owner.commands.status.is_some());
    harness.owner.commands.closed = true;
    harness.owner.channel_state = ChannelState::Revoked;
    settle(|complete| harness.broker.send(reply.canonical_bytes(), complete));
    while harness.owner.commands.status.is_some() {
        harness.tick();
    }
    assert!(harness.owner.commands.closed);
    assert!(!harness.owner.commands.receiving);
}

#[test]
fn status_deadline_allows_a_retry_without_restoring_command_authority() {
    let mut harness = Harness::new("true", false);
    let query = harness
        .owner
        .command_message("self-status", CommandOperation::StatusRequest {});
    settle(|complete| harness.agent.send(query.canonical_bytes(), complete));
    while harness.owner.commands.status.is_none() {
        harness.tick();
    }
    let old = receive(&harness.broker);
    harness.timer.expire();
    while harness.owner.commands.status.is_some() {
        harness.tick();
    }
    assert!(matches!(
        receive(&harness.agent).operation,
        CommandOperation::StatusRefused {
            error: ErrorCode::BrokerUnavailable
        }
    ));
    assert!(!harness.owner.commands.closed);
    settle(|complete| harness.agent.send(query.canonical_bytes(), complete));
    while harness.owner.commands.status.is_none() {
        harness.tick();
    }
    let next = receive(&harness.broker);
    assert_ne!(old.request_id, next.request_id);
    let mut late = old;
    late.operation = CommandOperation::StatusRefused {
        error: ErrorCode::OperationPending,
    };
    assert!(harness.owner.handle_status_reply(&late));
    assert!(harness.owner.commands.status.is_some());
    assert!(harness.audit.entries().unwrap().is_empty());
}

#[test]
fn agent_packet_round_trips_through_policy_and_isolated_mechanics() {
    let command = "printf private-output; printf done > result";
    let mut harness = Harness::new(command, false);
    let request = harness.request(command);
    harness.authorize(&request);
    let outcome = harness.finish();
    assert!(
        matches!(outcome.operation,CommandOperation::Outcome {outcome:CommandOutcome::Completed {ref output},..} if output.exit_code==0 && output.stdout=="private-output")
    );
    assert!(matches!(
        receive(&harness.agent).operation,
        CommandOperation::Result {
            outcome: CommandOutcome::Completed { .. }
        }
    ));
    assert_eq!(
        std::fs::read_to_string(harness.root.path().join("workspace/result")).unwrap(),
        "done"
    );
    let audit = serde_json::to_string(&harness.audit.entries().unwrap()).unwrap();
    assert!(!audit.contains("private-output") && !audit.contains("printf"));
    assert!(harness.authority.handle(&request).is_err());
}

#[test]
fn lost_authorization_reply_stays_spent_and_late_reply_cannot_start() {
    let mut harness = Harness::new("touch forbidden", false);
    let request = harness.request("touch forbidden");
    let lost = harness.authority.handle(&request).unwrap();
    harness.timer.expire();
    while !harness.owner.commands.closed {
        harness.tick();
    }
    assert!(matches!(
        receive(&harness.agent).operation,
        CommandOperation::Result {
            outcome: CommandOutcome::Unknown
        }
    ));
    assert!(harness.authority.handle(&request).is_err());
    settle(|complete| harness.broker.send(lost.canonical_bytes(), complete));
    let outcome = harness.finish();
    assert!(matches!(
        outcome.operation,
        CommandOperation::Outcome {
            outcome: CommandOutcome::NotStarted { .. },
            ..
        }
    ));
    assert!(!harness.root.path().join("workspace/forbidden").exists());
}

#[test]
fn revocation_ack_waits_for_cleanup_and_late_actual_outcome_is_audited() {
    let mut harness = Harness::new("printf late-actual", true);
    let request = harness.request("printf late-actual");
    harness.authorize(&request);
    let deadline = Instant::now() + Duration::from_secs(5);
    while harness.held.lock().unwrap().is_none() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    let revoke = harness.authority.revoke("revoke-1").unwrap();
    settle(|complete| harness.broker.send(revoke.canonical_bytes(), complete));
    while !harness.owner.commands.closed {
        harness.tick();
    }
    let ack = receive(&harness.broker);
    assert!(matches!(
        ack.operation,
        CommandOperation::Revoked { enforced: true }
    ));
    harness.authority.handle(&ack).unwrap();
    assert!(harness.authority.revocation_complete());
    let (complete, result) = harness.held.lock().unwrap().take().unwrap();
    std::thread::spawn(move || complete(result)).join().unwrap();
    let outcome = harness.finish();
    assert!(
        matches!(outcome.operation,CommandOperation::Outcome {outcome:CommandOutcome::Completed {ref output},..} if output.stdout=="late-actual")
    );
    assert!(
        harness
            .audit
            .entries()
            .unwrap()
            .iter()
            .any(|entry| matches!(
                entry.decision,
                AuditDecision::EffectFinished {
                    succeeded: true,
                    ..
                }
            ))
    );
}

#[test]
fn revocation_terminates_a_running_command_before_successful_ack() {
    let command = "touch ready; (sleep 2; touch escaped) & sleep 30";
    let mut harness = Harness::new(command, false);
    let request = harness.request(command);
    harness.authorize(&request);
    wait_started(&harness);
    let revoke = harness.authority.revoke("revoke-running").unwrap();
    settle(|complete| harness.broker.send(revoke.canonical_bytes(), complete));
    while !harness.owner.commands.closed {
        harness.tick();
    }
    let outcome = receive(&harness.broker);
    assert!(matches!(
        outcome.operation,
        CommandOperation::Outcome {
            outcome: CommandOutcome::Unknown,
            ..
        }
    ));
    harness.authority.handle(&outcome).unwrap();
    let ack = receive(&harness.broker);
    assert!(matches!(
        ack.operation,
        CommandOperation::Revoked { enforced: true }
    ));
    harness.authority.handle(&ack).unwrap();
    assert!(harness.authority.revocation_complete());
    std::thread::sleep(Duration::from_millis(2100));
    assert!(!harness.root.path().join("workspace/escaped").exists());
}

fn wait_started(harness: &Harness) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !harness.root.path().join("workspace/ready").exists() {
        assert!(
            Instant::now() < deadline,
            "command must start before the race is tested"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn local_authorization_expiry_stops_running_work_before_its_command_timeout() {
    let command = "touch ready; (sleep 2; touch escaped) & sleep 30";
    let mut harness = Harness::new(command, false);
    let request = harness.request(command);
    let mut decision = harness.authority.handle(&request).unwrap();
    // The authenticated broker may narrow its decision below its policy ceiling.
    // This local lifetime expires well before the requested 5-second timeout.
    if let CommandOperation::Authorize { valid_for_ms, .. } = &mut decision.operation {
        *valid_for_ms = 1500;
    }
    settle(|complete| harness.broker.send(decision.canonical_bytes(), complete));
    while harness
        .owner
        .commands
        .pending
        .as_ref()
        .unwrap()
        .dispatch
        .is_none()
    {
        harness.tick();
    }
    wait_started(&harness);
    let outcome = harness.finish();
    assert!(matches!(
        outcome.operation,
        CommandOperation::Outcome {
            outcome: CommandOutcome::Unknown,
            ..
        }
    ));
    std::thread::sleep(Duration::from_millis(700));
    assert!(!harness.root.path().join("workspace/escaped").exists());
}

#[test]
fn failed_revocation_cleanup_is_not_acknowledged_and_poisons_identity() {
    let mut harness = Harness::new("printf unused", false);
    harness.fail_cleanup.store(true, Ordering::Release);
    let revoke = harness.authority.revoke("revoke-unproven").unwrap();
    settle(|complete| harness.broker.send(revoke.canonical_bytes(), complete));
    while !harness.owner.quarantined {
        harness.tick();
    }
    let ack = receive(&harness.broker);
    assert!(matches!(
        ack.operation,
        CommandOperation::Revoked { enforced: false }
    ));
    assert!(harness.authority.handle(&ack).is_err());
    assert!(!harness.authority.revocation_complete());
    assert_eq!(
        harness.identity_disposition.load(Ordering::Acquire),
        2,
        "failed cleanup must poison, never release, the identity"
    );
}

#[test]
fn agent_cannot_supply_its_own_supervisor_attribution_record() {
    let mut harness = Harness::new("printf unused", false);
    let forged = harness.owner.command_message(
        "forged",
        CommandOperation::Request {
            principal: CommandPrincipal {
                channel_id: harness.owner.binding.channel_id.clone(),
                pid: harness.owner.binding.agent_pid,
                uid: harness.owner.binding.assigned_uid,
                gid: harness.owner.binding.assigned_gid,
            },
            command: ToolExecutionRequest {
                schema: TOOL_EXECUTION_SCHEMA.to_owned(),
                protocol_version: 1,
                request_id: "forged".to_owned(),
                session_id: "session-1".to_owned(),
                run_id: "run-1".to_owned(),
                envelope_revision: 1,
                sequence: 1,
                command: "printf unused".to_owned(),
                timeout_ms: 1000,
            },
        },
    );
    settle(|complete| harness.agent.send(forged.canonical_bytes(), complete));
    while !harness.owner.commands.closed {
        harness.tick();
    }
    assert!(harness.owner.commands.pending.is_none());
    assert!(harness.audit.entries().unwrap().is_empty());
}
