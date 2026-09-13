//! Measured Agent -> real capability gate -> broker policy -> isolated granted helper.

use super::*;
use crate::launch_protocol::GrantRequest;
use std::io::{BufRead, Write};

// Only callback delivery and cleanup reporting are injected. Startup, sockets,
// pinning, command execution and actual teardown remain production operations.
struct DelayedProcess {
    inner: Box<dyn RunningAgent>,
    held: Option<HeldCompletion>,
    fail_cleanup: Arc<AtomicBool>,
}

impl RunningAgent for DelayedProcess {
    fn launch_helper(
        &mut self,
        request: CommandMessage,
        enforcer: Arc<crate::launch_supervisor::command::CommandEnforcer>,
        complete: SupervisorCompletion<crate::launch_supervisor::HelperPrincipal>,
    ) -> Result<(), SupervisorError> {
        self.inner.launch_helper(request, enforcer, complete)
    }
    fn cancel_helper(&mut self) -> Result<(), SupervisorError> {
        self.inner.cancel_helper()?;
        if self.fail_cleanup.load(Ordering::Acquire) {
            Err(SupervisorError::CleanupUnproven)
        } else {
            Ok(())
        }
    }
    fn execute_tool(
        &mut self,
        permit: CommandPermit,
        complete: SupervisorCompletion<ToolExecutionResult>,
    ) -> Result<(), SupervisorError> {
        if let Some(held) = self.held.clone() {
            self.inner.execute_tool(
                permit,
                Box::new(move |result| {
                    *held.lock().unwrap() = Some((complete, result));
                }),
            )
        } else {
            self.inner.execute_tool(permit, complete)
        }
    }
    fn cancel_tool(&mut self) -> Result<(), SupervisorError> {
        self.inner.cancel_tool()
    }
    fn authentication(&self) -> Result<AgentAuthentication, SupervisorError> {
        self.inner.authentication()
    }
    fn start_relay(
        &mut self,
        controller: mpsc::Receiver<RelayStdio>,
        events: RunningAgentEvents,
    ) -> Result<(), SupervisorError> {
        self.inner.start_relay(controller, events)
    }
    fn quiesce_relay(&mut self, complete: SupervisorCompletion<()>) -> Result<(), SupervisorError> {
        self.inner.quiesce_relay(complete)
    }
    fn park(&mut self) -> Result<(), MechanicFailure> {
        self.inner.park()
    }
    fn resume(&mut self) -> Result<(), MechanicFailure> {
        self.inner.resume()
    }
    fn interrupt(&mut self) -> Result<(), MechanicFailure> {
        self.inner.interrupt()
    }
    fn dispose(&mut self) -> Result<(), SupervisorError> {
        self.inner.dispose()?;
        // Production ToolExecutor permanently latches unproven helper cleanup.
        // Keep the reporting fault sticky across the later quarantine cleanup.
        self.cancel_helper()
    }
}

fn send_agent(input: &mut std::process::ChildStdin, bytes: &[u8]) {
    input.write_all(&[0x1e]).unwrap();
    input.write_all(bytes).unwrap();
    input.write_all(b"\n").unwrap();
    input.flush().unwrap();
}

fn delegation(command: &str, lifetime: u32) -> CommandMessage {
    let command = ToolExecutionRequest {
        schema: TOOL_EXECUTION_SCHEMA.into(),
        protocol_version: 1,
        request_id: "initial-tool-work".into(),
        session_id: "session-1".into(),
        run_id: "run-1".into(),
        envelope_revision: 1,
        sequence: 1,
        command: command.into(),
        timeout_ms: 5000,
    };
    CommandMessage {
        schema: COMMAND_SCHEMA.into(),
        protocol_version: 1,
        request_id: "delegate-1".into(),
        session_id: "session-1".into(),
        run_id: "run-1".into(),
        envelope_revision: 1,
        operation: CommandOperation::Delegate {
            grant: GrantRequest {
                sequence: 1,
                command_digest: Digest::of(command.command.as_bytes()).to_string(),
                timeout_ms: 5000,
                uses: Some(1),
                valid_for_ms: lifetime,
            },
            command,
        },
    }
}

fn measured_harness(
    command: &str,
    delay_delivery: bool,
) -> (
    Harness,
    std::process::ChildStdin,
    mpsc::Receiver<CommandMessage>,
    std::path::PathBuf,
) {
    let mut harness = Harness::new(command, false);
    let parts = crate::launch_supervisor::system::grant_test_support::fixture(harness.root.path());
    harness.owner.resources.capability = Some(parts.gate);
    harness.owner.resources.process = Some(Box::new(DelayedProcess {
        inner: parts.process,
        held: delay_delivery.then(|| Arc::clone(&harness.held)),
        fail_cleanup: Arc::clone(&harness.fail_cleanup),
    }));
    harness.owner.binding.clone_from(&parts.binding);
    harness.owner.commands = CommandDispatch::default();
    harness.authority = CommandAuthority::new(
        parts.binding,
        DelegationPolicy {
            authorization_id: "approved".into(),
            scope: CommandScope {
                command_digest: Digest::of(command.as_bytes()),
                timeout_ms: 5000,
                uses: Some(3),
            },
            allow_delegation: true,
            expires_at: Instant::now() + Duration::from_secs(30),
        },
        Arc::clone(&harness.audit),
    )
    .unwrap();
    // The original fixture's broker transport stays exact; only the Agent side is
    // replaced by the measured, assigned-UID native process and production gate.
    harness.owner.arm_agent_receive();
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(parts.output).split(b'\n') {
            let line = line.unwrap();
            if line.first() == Some(&0x1e) {
                let reply = serde_json::from_slice(&line[1..]).unwrap();
                if sender.send(reply).is_err() {
                    break;
                }
            }
        }
    });
    assert_eq!(
        parts.workspace,
        harness.root.path().join("measured/workspace")
    );
    (harness, parts.input, receiver, parts.cgroup)
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "The same measured composition exercises positive execution and each distributed grant cancellation race."
)]
fn privileged_measured_helper_grant_execution_and_revocation() {
    if std::env::var_os("LOUISELM_REQUIRE_TOOL_GRANTS").is_none() {
        eprintln!("skipping: set LOUISELM_REQUIRE_TOOL_GRANTS=1 only in the disposable VM");
        return;
    }
    assert!(rustix::process::geteuid().is_root());
    for scenario in [
        "complete",
        "queued_revoke",
        "running_revoke",
        "expiry",
        "agent_exit",
        "lost_grant",
        "transferred_descriptor",
        "failed_cleanup",
        "late_actual",
    ] {
        eprintln!("grant scenario: {scenario}");
        let command = if matches!(
            scenario,
            "complete" | "queued_revoke" | "late_actual" | "transferred_descriptor"
        ) {
            "printf delegated > granted; printf output"
        } else {
            "touch started; (sleep 8; touch escaped) & sleep 20"
        };
        let (mut harness, input, replies, cgroup) =
            measured_harness(command, scenario == "late_actual");
        let mut input = Some(input);
        let mut request = delegation(command, if scenario == "expiry" { 4000 } else { 10000 });
        if scenario == "transferred_descriptor"
            && let CommandOperation::Delegate { command, .. } = &mut request.operation
        {
            command.request_id = "probe-child".into();
        }
        send_agent(input.as_mut().unwrap(), &request.canonical_bytes());
        while harness.owner.commands.grants.helper.is_none() {
            harness.tick();
        }
        let forwarded = receive(&harness.broker);
        let helper = harness.owner.commands.grants.helper.as_ref().unwrap();
        let helper_pin = Arc::clone(&helper.process);
        let helper_root =
            std::path::PathBuf::from(format!("/proc/{}/root", helper_pin.credentials().pid));
        assert!(helper_root.join("tmp/louiselm-tool.sock").exists());
        assert!(!helper_root.join("tmp/louiselm-capability.sock").exists());
        for private in ["measured/home", "measured/runtime"] {
            assert!(
                !helper_root
                    .join(harness.root.path().join(private).strip_prefix("/").unwrap())
                    .exists(),
                "helper cannot read Agent-private mounts"
            );
        }
        assert_ne!(
            helper_pin.credentials().pid,
            harness.owner.binding.agent_pid
        );
        assert_eq!(
            helper_pin.credentials().uid,
            harness.owner.binding.assigned_uid
        );
        assert!(
            matches!(&forwarded.operation, CommandOperation::DelegationRequest { principal, tool, .. }
            if principal.pid == harness.owner.binding.agent_pid && tool.pid == helper_pin.credentials().pid)
        );
        let decision = harness.authority.handle(&forwarded).unwrap();
        assert!(
            !harness
                .root
                .path()
                .join("measured/workspace/granted")
                .exists()
        );
        assert!(
            !harness
                .root
                .path()
                .join("measured/workspace/started")
                .exists()
        );
        if scenario == "lost_grant" {
            harness.timer.expire();
            while harness.owner.commands.grants.pending.is_some() {
                harness.tick();
            }
            assert!(!helper_pin.valid().unwrap());
            assert!(matches!(
                replies
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap()
                    .operation,
                CommandOperation::Result {
                    outcome: CommandOutcome::Unknown
                }
            ));
            settle(|complete| harness.broker.send(decision.canonical_bytes(), complete));
            harness.tick();
            assert!(!harness.owner.commands.grants.active);
        } else if scenario == "transferred_descriptor" {
            settle(|complete| harness.broker.send(decision.canonical_bytes(), complete));
            while harness.owner.commands.grants.pending.is_some() {
                harness.tick();
            }
            assert!(matches!(
                replies
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap()
                    .operation,
                CommandOperation::Granted { .. }
            ));
            while harness.owner.commands.grants.active {
                harness.tick();
            }
            assert!(!helper_pin.valid().unwrap());
            assert!(harness.owner.commands.pending.is_none());
            assert!(
                !harness
                    .audit
                    .entries()
                    .unwrap()
                    .iter()
                    .any(|entry| matches!(
                        entry.decision,
                        AuditDecision::EffectCommitIntent { .. }
                    ))
            );
            assert!(
                !harness
                    .root
                    .path()
                    .join("measured/workspace/granted")
                    .exists()
            );
        } else {
            settle(|complete| harness.broker.send(decision.canonical_bytes(), complete));
            while harness.owner.commands.pending.is_none() {
                harness.tick();
            }
            assert!(matches!(
                replies
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap()
                    .operation,
                CommandOperation::Granted { .. }
            ));
            let tool_request = receive(&harness.broker);
            assert!(
                matches!(&tool_request.operation, CommandOperation::Request { principal, .. } if principal.pid == helper_pin.credentials().pid)
            );
            let authorization = harness.authority.handle(&tool_request).unwrap();
            if scenario == "queued_revoke" {
                let revoke = harness.authority.revoke_grant("revoke", 1).unwrap();
                settle(|complete| harness.broker.send(revoke.canonical_bytes(), complete));
                while harness.owner.commands.grants.active {
                    harness.tick();
                }
                let ack = receive(&harness.broker);
                assert!(matches!(
                    ack.operation,
                    CommandOperation::GrantRevoked { enforced: true, .. }
                ));
                harness.authority.handle(&ack).unwrap();
                settle(|complete| {
                    harness
                        .broker
                        .send(authorization.canonical_bytes(), complete)
                });
                let outcome = harness.finish();
                assert!(matches!(
                    outcome.operation,
                    CommandOperation::Outcome {
                        outcome: CommandOutcome::NotStarted { .. },
                        ..
                    }
                ));
            } else {
                settle(|complete| {
                    harness
                        .broker
                        .send(authorization.canonical_bytes(), complete)
                });
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
                if scenario == "late_actual" {
                    let deadline = Instant::now() + Duration::from_secs(5);
                    while harness.held.lock().unwrap().is_none() {
                        assert!(
                            Instant::now() < deadline,
                            "real command must finish before delaying its callback"
                        );
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    let revoke = harness.authority.revoke_grant("revoke", 1).unwrap();
                    settle(|complete| harness.broker.send(revoke.canonical_bytes(), complete));
                    while harness.owner.commands.grants.active {
                        harness.tick();
                    }
                    let ack = receive(&harness.broker);
                    assert!(matches!(
                        ack.operation,
                        CommandOperation::GrantRevoked { enforced: true, .. }
                    ));
                    harness.authority.handle(&ack).unwrap();
                    let (complete, actual) = harness.held.lock().unwrap().take().unwrap();
                    complete(actual);
                }
                if matches!(scenario, "running_revoke" | "agent_exit" | "failed_cleanup") {
                    let deadline = Instant::now() + Duration::from_secs(3);
                    while !harness
                        .root
                        .path()
                        .join("measured/workspace/started")
                        .exists()
                    {
                        assert!(
                            Instant::now() < deadline,
                            "command must reach its running barrier"
                        );
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    if scenario == "agent_exit" {
                        drop(input.take());
                    } else {
                        harness
                            .fail_cleanup
                            .store(scenario == "failed_cleanup", Ordering::Release);
                        let revoke = harness.authority.revoke_grant("revoke", 1).unwrap();
                        settle(|complete| harness.broker.send(revoke.canonical_bytes(), complete));
                    }
                }
                let outcome = if scenario == "failed_cleanup" {
                    while !harness.owner.quarantined {
                        harness.tick();
                    }
                    let outcome = receive(&harness.broker);
                    harness.authority.handle(&outcome).unwrap();
                    outcome
                } else {
                    harness.finish()
                };
                if scenario == "complete" || scenario == "late_actual" {
                    assert!(
                        matches!(outcome.operation, CommandOperation::Outcome { outcome: CommandOutcome::Completed { ref output }, .. } if output.stdout == "output")
                    );
                    assert_eq!(
                        std::fs::read_to_string(
                            harness.root.path().join("measured/workspace/granted")
                        )
                        .unwrap(),
                        "delegated"
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
                                    grant: Some(1),
                                    succeeded: true,
                                    ..
                                }
                            ))
                    );
                } else {
                    assert!(matches!(
                        outcome.operation,
                        CommandOperation::Outcome {
                            outcome: CommandOutcome::Unknown,
                            ..
                        }
                    ));
                }
                if scenario == "running_revoke" {
                    let ack = receive(&harness.broker);
                    assert!(matches!(
                        ack.operation,
                        CommandOperation::GrantRevoked { enforced: true, .. }
                    ));
                    assert!(!helper_pin.valid().unwrap());
                    harness.authority.handle(&ack).unwrap();
                    assert!(harness.authority.grant_revocation_complete(1));
                }
                if scenario == "failed_cleanup" {
                    while !harness.owner.quarantined {
                        harness.tick();
                    }
                    let ack = receive(&harness.broker);
                    assert!(matches!(
                        ack.operation,
                        CommandOperation::GrantRevoked {
                            enforced: false,
                            ..
                        }
                    ));
                    assert!(harness.authority.handle(&ack).is_err());
                    assert!(!harness.authority.grant_revocation_complete(1));
                    assert_eq!(harness.identity_disposition.load(Ordering::Acquire), 2);
                }
            }
        }
        assert!(
            !harness
                .root
                .path()
                .join("measured/workspace/escaped")
                .exists()
        );
        if scenario != "agent_exit" && scenario != "failed_cleanup" {
            assert!(
                harness
                    .owner
                    .resources
                    .capability
                    .as_ref()
                    .unwrap()
                    .command_enforcer()
                    .unwrap()
                    .agent_valid()
                    .unwrap(),
                "grant revocation must preserve Agent authority"
            );
        }
        if scenario == "queued_revoke" {
            // The same actual Agent retains its unreserved budget after a tool-only revoke.
            let mut own = if let CommandOperation::Delegate { command, .. } = &request.operation {
                command.clone()
            } else {
                panic!("fixture request")
            };
            own.request_id = "agent-after-revoke".into();
            send_agent(input.as_mut().unwrap(), &own.canonical_bytes());
            while harness.owner.commands.pending.is_none() {
                harness.tick();
            }
            let forwarded = receive(&harness.broker);
            harness.authorize(&forwarded);
            let outcome = harness.finish();
            assert!(
                matches!(outcome.operation, CommandOperation::Outcome { outcome: CommandOutcome::Completed { ref output }, .. } if output.stdout == "output")
            );
            assert!(matches!(
                replies
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap()
                    .operation,
                CommandOperation::Result {
                    outcome: CommandOutcome::Completed { .. }
                }
            ));
        }
        if scenario == "failed_cleanup" {
            assert_eq!(
                harness.owner.resources.cleanup(),
                Err(SupervisorError::CleanupUnproven)
            );
        } else {
            harness.owner.resources.cleanup().unwrap();
        }
        assert!(!helper_pin.valid().unwrap());
        assert_eq!(
            std::fs::read_dir(&cgroup)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_type().unwrap().is_dir())
                .count(),
            0
        );
        std::fs::remove_dir(cgroup).unwrap();
    }
}
