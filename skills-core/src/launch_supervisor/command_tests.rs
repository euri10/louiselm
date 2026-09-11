//! Real kernel pins at the final irreversible boundary; no installed-launch claim.
#![allow(clippy::unwrap_used, reason = "Tests assert controlled fixture setup.")]

use super::*;
use crate::{
    launch_protocol::{COMMAND_SCHEMA, TOOL_EXECUTION_SCHEMA},
    launch_transport::KernelCredentials,
};

fn fixture() -> (
    CommandEnforcer,
    ToolExecutionRequest,
    CommandPrincipal,
    CommandMessage,
) {
    let pid = std::process::id();
    let process = Arc::new(
        KernelProcess::from_exec_stop(
            KernelCredentials {
                pid,
                uid: rustix::process::getuid().as_raw(),
                gid: rustix::process::getgid().as_raw(),
            },
            rustix::process::pidfd_open(
                rustix::process::Pid::from_raw(i32::try_from(pid).unwrap()).unwrap(),
                rustix::process::PidfdFlags::empty(),
            )
            .unwrap(),
            &std::fs::File::open(std::env::current_exe().unwrap()).unwrap(),
        )
        .unwrap(),
    );
    let credentials = process.credentials();
    let principal = CommandPrincipal {
        channel_id: "agent-capability".to_owned(),
        pid,
        uid: credentials.uid,
        gid: credentials.gid,
    };
    let binding = CapabilityBinding {
        session_id: "session-1".to_owned(),
        run_id: "run-1".to_owned(),
        channel_id: principal.channel_id.clone(),
        envelope_revision: 1,
        identity_slot: 1,
        assigned_uid: credentials.uid,
        assigned_gid: credentials.gid,
        agent_pid: pid,
    };
    let request = ToolExecutionRequest {
        schema: TOOL_EXECUTION_SCHEMA.to_owned(),
        protocol_version: 1,
        request_id: "command-1".to_owned(),
        session_id: binding.session_id.clone(),
        run_id: binding.run_id.clone(),
        envelope_revision: 1,
        sequence: 1,
        command: "printf exact".to_owned(),
        timeout_ms: 1000,
    };
    let message = CommandMessage {
        schema: COMMAND_SCHEMA.to_owned(),
        protocol_version: 1,
        request_id: request.request_id.clone(),
        session_id: request.session_id.clone(),
        run_id: request.run_id.clone(),
        envelope_revision: 1,
        operation: CommandOperation::Authorize {
            principal: principal.clone(),
            principal_sequence: 1,
            dispatch_sequence: 7,
            command_digest: Digest::of(request.command.as_bytes()).to_string(),
            timeout_ms: 1000,
            valid_for_ms: 1000,
        },
    };
    (
        CommandEnforcer::new(binding, process).unwrap(),
        request,
        principal,
        message,
    )
}

#[test]
fn final_start_is_single_use_and_revocation_blocks_an_already_queued_permit() {
    let (owner, request, principal, decision) = fixture();
    let permit = owner
        .admit(&request, &principal, &decision, Instant::now())
        .unwrap();
    assert_eq!(permit.dispatch_sequence(), 7);
    assert_eq!(permit.request(), &request);
    assert!(
        owner
            .admit(&request, &principal, &decision, Instant::now())
            .is_err()
    );
    assert_eq!(permit.start(|| Ok(42)), Ok(42));
    assert!(permit.start(|| Ok(43)).is_err());
    owner.revoke().unwrap();
    assert_eq!(permit.valid(), Ok(false));

    let (owner, request, principal, decision) = fixture();
    let queued = owner
        .admit(&request, &principal, &decision, Instant::now())
        .unwrap();
    owner.revoke().unwrap();
    let mut started = false;
    assert!(
        queued
            .start(|| {
                started = true;
                Ok(())
            })
            .is_err()
    );
    assert!(!started);
}

#[test]
fn delayed_reply_cannot_move_expiry_and_owner_drop_revokes_queued_work() {
    let (owner, request, principal, decision) = fixture();
    assert!(
        owner
            .admit(
                &request,
                &principal,
                &decision,
                Instant::now().checked_sub(Duration::from_secs(2)).unwrap()
            )
            .is_err()
    );
    let permit = owner
        .admit(&request, &principal, &decision, Instant::now())
        .unwrap();
    drop(owner);
    assert!(permit.start(|| Ok(())).is_err());
}

#[test]
fn changed_command_and_attribution_never_consume_a_valid_decision() {
    let (owner, mut request, mut principal, decision) = fixture();
    request.command.push('x');
    assert!(
        owner
            .admit(&request, &principal, &decision, Instant::now())
            .is_err()
    );
    request.command.pop();
    principal.pid += 1;
    assert!(
        owner
            .admit(&request, &principal, &decision, Instant::now())
            .is_err()
    );
    principal.pid -= 1;
    assert!(
        owner
            .admit(&request, &principal, &decision, Instant::now())
            .is_ok()
    );
}

#[test]
fn expiry_after_admission_still_denies_the_final_start() {
    let (owner, request, principal, mut decision) = fixture();
    if let CommandOperation::Authorize { valid_for_ms, .. } = &mut decision.operation {
        *valid_for_ms = 40;
    }
    let permit = owner
        .admit(&request, &principal, &decision, Instant::now())
        .unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let queued = Arc::clone(&barrier);
    let worker = std::thread::spawn(move || {
        queued.wait();
        permit.start(|| Ok(()))
    });
    std::thread::sleep(Duration::from_millis(80));
    assert!(
        owner.agent.valid().unwrap(),
        "identity stays live; only the deadline changed"
    );
    barrier.wait();
    assert!(worker.join().unwrap().is_err());
}

struct FixtureChild(std::process::Child);
impl Drop for FixtureChild {
    fn drop(&mut self) {
        // Test-owned child may already have been explicitly killed/reaped.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn tool_pin() -> (FixtureChild, Arc<KernelProcess>) {
    use std::io::Read;
    let mut child = FixtureChild(
        std::process::Command::new("/bin/sh")
            .args(["-c", "printf r; read -r ignored"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap(),
    );
    child
        .0
        .stdout
        .as_mut()
        .unwrap()
        .read_exact(&mut [0])
        .unwrap();
    let pid = child.0.id();
    let process = Arc::new(
        KernelProcess::from_exec_stop(
            KernelCredentials {
                pid,
                uid: rustix::process::getuid().as_raw(),
                gid: rustix::process::getgid().as_raw(),
            },
            rustix::process::pidfd_open(
                rustix::process::Pid::from_raw(i32::try_from(pid).unwrap()).unwrap(),
                rustix::process::PidfdFlags::empty(),
            )
            .unwrap(),
            &std::fs::File::open("/bin/sh").unwrap(),
        )
        .unwrap(),
    );
    (child, process)
}

#[test]
fn grant_revocation_is_scoped_and_a_queued_permit_cannot_outlive_either_principal() {
    for scenario in ["revoked", "expired", "tool_exit", "agent_revoked"] {
        let (owner, request, agent, agent_decision) = fixture();
        let (mut child, process) = tool_pin();
        let credentials = process.credentials();
        let tool = CommandPrincipal {
            channel_id: "tool-1".into(),
            pid: credentials.pid,
            uid: credentials.uid,
            gid: credentials.gid,
        };
        let mut granted = agent_decision.clone();
        granted.operation = CommandOperation::Granted {
            tool: tool.clone(),
            grant: 1,
            valid_for_ms: if scenario == "expired" { 40 } else { 1000 },
        };
        owner
            .register_grant(Arc::clone(&process), &granted, Instant::now())
            .unwrap();
        assert!(
            owner
                .register_grant(process, &granted, Instant::now())
                .is_err(),
            "cloning/reconnecting cannot reinstall a grant"
        );
        let mut decision = agent_decision.clone();
        if let CommandOperation::Authorize {
            principal,
            dispatch_sequence,
            ..
        } = &mut decision.operation
        {
            *principal = tool.clone();
            *dispatch_sequence = 1;
        }
        let permit = owner
            .admit(&request, &tool, &decision, Instant::now())
            .unwrap();
        assert!(permit.valid().unwrap());
        match scenario {
            "revoked" => owner.revoke_grant(1).unwrap(),
            "expired" => std::thread::sleep(Duration::from_millis(60)),
            "tool_exit" => {
                child.0.kill().unwrap();
                child.0.wait().unwrap();
            }
            _ => owner.revoke().unwrap(),
        }
        assert!(!permit.valid().unwrap());
        assert!(permit.start(|| Ok(())).is_err());
        if scenario != "agent_revoked" {
            let independent = owner
                .admit(&request, &agent, &agent_decision, Instant::now())
                .unwrap();
            assert_eq!(
                independent.start(|| Ok("Agent remains authorized")),
                Ok("Agent remains authorized")
            );
        }
    }
}

#[test]
fn death_or_executable_replacement_denies_an_already_admitted_command() {
    use std::io::{Read, Write};
    for replacement in [false, true] {
        let mut child = FixtureChild(
            std::process::Command::new("/bin/sh")
                .args(["-c", "printf r; read -r ignored; exec /bin/sleep 30"])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let mut ready = [0];
        child
            .0
            .stdout
            .as_mut()
            .unwrap()
            .read_exact(&mut ready)
            .unwrap();
        assert_eq!(ready, *b"r");
        let (base, request, mut principal, mut decision) = fixture();
        principal.pid = child.0.id();
        if let CommandOperation::Authorize {
            principal: approved,
            ..
        } = &mut decision.operation
        {
            *approved = principal.clone();
        }
        let process = Arc::new(
            KernelProcess::from_exec_stop(
                KernelCredentials {
                    pid: principal.pid,
                    uid: principal.uid,
                    gid: principal.gid,
                },
                rustix::process::pidfd_open(
                    rustix::process::Pid::from_raw(i32::try_from(principal.pid).unwrap()).unwrap(),
                    rustix::process::PidfdFlags::empty(),
                )
                .unwrap(),
                &std::fs::File::open("/bin/sh").unwrap(),
            )
            .unwrap(),
        );
        let mut binding = base.binding.clone();
        binding.agent_pid = principal.pid;
        let owner = CommandEnforcer::new(binding, process).unwrap();
        let permit = owner
            .admit(&request, &principal, &decision, Instant::now())
            .unwrap();
        if replacement {
            child.0.stdin.as_mut().unwrap().write_all(b"go\n").unwrap();
            let deadline = Instant::now() + Duration::from_secs(2);
            while owner.agent.valid().unwrap() {
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(5));
            }
        } else {
            child.0.kill().unwrap();
            child.0.wait().unwrap();
            assert!(!owner.agent.valid().unwrap());
        }
        assert!(permit.start(|| Ok(())).is_err());
    }
}
