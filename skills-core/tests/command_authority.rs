//! Single-use broker decisions, independent of supervisor process mechanics.
#![allow(
    clippy::unwrap_used,
    reason = "Tests assert fixture construction and outcomes."
)]

use louiselm_skills::{
    Digest,
    broker::{
        AuditLog,
        commands::CommandAuthority,
        delegation::{CommandScope, DelegationPolicy},
    },
    launch_protocol::{
        COMMAND_SCHEMA, CommandMessage, CommandOperation, CommandOutcome, CommandPrincipal,
        TOOL_EXECUTION_SCHEMA, ToolExecutionRequest,
    },
    launch_supervisor::CapabilityBinding,
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

fn message() -> CommandMessage {
    CommandMessage {
        schema: COMMAND_SCHEMA.to_owned(),
        protocol_version: 1,
        request_id: "command-1".to_owned(),
        session_id: "session-1".to_owned(),
        run_id: "run-1".to_owned(),
        envelope_revision: 1,
        operation: CommandOperation::Request {
            principal: CommandPrincipal {
                channel_id: "agent-capability".to_owned(),
                pid: 123,
                uid: 1001,
                gid: 1001,
            },
            command: ToolExecutionRequest {
                schema: TOOL_EXECUTION_SCHEMA.to_owned(),
                protocol_version: 1,
                request_id: "command-1".to_owned(),
                session_id: "session-1".to_owned(),
                run_id: "run-1".to_owned(),
                envelope_revision: 1,
                sequence: 1,
                command: "printf private".to_owned(),
                timeout_ms: 1000,
            },
        },
    }
}

fn authority(root: &std::path::Path, uses: impl Into<Option<u32>>) -> CommandAuthority {
    CommandAuthority::new(
        CapabilityBinding {
            session_id: "session-1".to_owned(),
            run_id: "run-1".to_owned(),
            channel_id: "agent-capability".to_owned(),
            envelope_revision: 1,
            identity_slot: 1,
            assigned_uid: 1001,
            assigned_gid: 1001,
            agent_pid: 123,
        },
        DelegationPolicy {
            authorization_id: "authorization-1".to_owned(),
            scope: CommandScope {
                command_digest: Digest::of(b"printf private"),
                timeout_ms: 1000,
                uses: uses.into(),
            },
            allow_delegation: false,
            expires_at: Instant::now() + Duration::from_secs(30),
        },
        Arc::new(AuditLog::open(root).unwrap()),
    )
    .unwrap()
}

#[test]
fn uncapped_commands_keep_exact_scope_replay_and_revocation_checks() {
    let root = tempfile::tempdir().unwrap();
    let mut owner = authority(root.path(), None);
    for sequence in 1..=65 {
        let mut request = message();
        request.request_id = format!("command-{sequence}");
        if let CommandOperation::Request { command, .. } = &mut request.operation {
            command.request_id.clone_from(&request.request_id);
            command.sequence = sequence;
        }
        assert!(owner.handle(&request).is_ok());
        assert!(matches!(
            owner.handle(&request),
            Err(louiselm_skills::broker::delegation::DelegationError::Replay)
        ));
    }
    owner.revoke("revoke-uncapped").unwrap();
    assert!(matches!(
        owner.handle(&message()),
        Err(louiselm_skills::broker::delegation::DelegationError::Revoked)
    ));
}

#[test]
fn each_authorization_spends_one_budget_and_unknown_never_refunds_it() {
    let root = tempfile::tempdir().unwrap();
    let mut owner = authority(root.path(), 1);
    let request = message();
    let reply = owner.handle(&request).unwrap();
    assert!(matches!(
        reply.operation,
        CommandOperation::Authorize {
            dispatch_sequence: 1,
            ..
        }
    ));
    assert!(owner.handle(&request).is_err());
    let mut report = request.clone();
    report.operation = CommandOperation::Outcome {
        dispatch_sequence: 1,
        outcome: CommandOutcome::Unknown,
    };
    assert!(owner.handle(&report).is_ok());
    assert!(
        owner.handle(&report).is_err(),
        "unknown reports are not replayable"
    );
    let mut next = request;
    next.request_id = "command-2".to_owned();
    if let CommandOperation::Request { command, .. } = &mut next.operation {
        command.request_id.clone_from(&next.request_id);
        command.sequence = 2;
    }
    assert!(owner.handle(&next).is_err());
}

#[test]
fn only_exact_attributed_agent_subject_revision_and_command_are_allowed() {
    for uses in [Some(2), None] {
        for field in [
            "pid", "uid", "channel", "session", "run", "revision", "command", "timeout",
        ] {
            let root = tempfile::tempdir().unwrap();
            let mut owner = authority(root.path(), uses);
            let mut request = message();
            if let CommandOperation::Request { principal, command } = &mut request.operation {
                match field {
                    "pid" => principal.pid += 1,
                    "uid" => principal.uid += 1,
                    "channel" => principal.channel_id = "tool".to_owned(),
                    "session" => {
                        request.session_id = "other".to_owned();
                        command.session_id.clone_from(&request.session_id);
                    }
                    "run" => {
                        request.run_id = "other".to_owned();
                        command.run_id.clone_from(&request.run_id);
                    }
                    "revision" => {
                        request.envelope_revision = 2;
                        command.envelope_revision = 2;
                    }
                    "command" => command.command = "other".to_owned(),
                    _ => command.timeout_ms += 1,
                }
            }
            assert!(owner.handle(&request).is_err(), "{field}");
        }
    }
}

#[test]
fn revocation_stops_approvals_but_late_actual_outcomes_remain_recordable() {
    let root = tempfile::tempdir().unwrap();
    let mut owner = authority(root.path(), 2);
    owner.handle(&message()).unwrap();
    let revoke = owner.revoke("revoke-1").unwrap();
    assert!(matches!(revoke.operation, CommandOperation::Revoke));
    assert!(!owner.revocation_complete());
    assert!(owner.handle(&message()).is_err());
    let mut report = message();
    report.operation = CommandOperation::Outcome {
        dispatch_sequence: 1,
        outcome: CommandOutcome::NotStarted {
            error: louiselm_skills::launch_protocol::ErrorCode::StateMismatch,
        },
    };
    assert!(owner.handle(&report).is_ok());
    let mut ack = revoke;
    ack.operation = CommandOperation::Revoked { enforced: false };
    assert!(owner.handle(&ack).is_err());
    assert!(!owner.revocation_complete());
    ack.operation = CommandOperation::Revoked { enforced: true };
    assert!(owner.handle(&ack).is_ok());
    assert!(owner.revocation_complete());
}

#[test]
fn audit_failure_revokes_authority_even_if_storage_recovers() {
    let root = tempfile::tempdir().unwrap();
    let mut owner = authority(root.path(), 2);
    // The fixture replaces its absent audit file with a directory to fail append.
    std::fs::create_dir(root.path().join("decisions.jsonl")).unwrap();
    assert!(owner.handle(&message()).is_err());
    std::fs::remove_dir(root.path().join("decisions.jsonl")).unwrap();
    let mut next = message();
    next.request_id = "command-2".to_owned();
    if let CommandOperation::Request { command, .. } = &mut next.operation {
        command.request_id.clone_from(&next.request_id);
        command.sequence = 2;
    }
    assert!(owner.handle(&next).is_err());
}

#[test]
fn unknown_can_resolve_once_to_actual_after_revocation() {
    let root = tempfile::tempdir().unwrap();
    let mut owner = authority(root.path(), 1);
    owner.handle(&message()).unwrap();
    let mut report = message();
    report.operation = CommandOperation::Outcome {
        dispatch_sequence: 1,
        outcome: CommandOutcome::Unknown,
    };
    owner.handle(&report).unwrap();
    owner.revoke("revoke").unwrap();
    report.operation = CommandOperation::Outcome {
        dispatch_sequence: 1,
        outcome: CommandOutcome::Completed {
            output: louiselm_skills::launch_protocol::ToolExecutionResult {
                exit_code: 0,
                stdout: "private-output".to_owned(),
                stderr: String::new(),
                truncated: false,
                timed_out: false,
            },
        },
    };
    owner.handle(&report).unwrap();
    assert!(owner.handle(&report).is_err());
    let audit =
        serde_json::to_string(&AuditLog::open(root.path()).unwrap().entries().unwrap()).unwrap();
    assert!(!audit.contains("private-output") && !audit.contains("printf"));
}

#[test]
fn first_intent_is_not_authorized_when_its_directory_cannot_be_synced() {
    use std::os::unix::fs::PermissionsExt;
    if rustix::process::geteuid().is_root() {
        eprintln!("SKIP: directory-read denial requires the unprivileged broker identity");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let mut owner = authority(root.path(), 1);
    // Search/write allow creation and fsync of the intent file; no read denies
    // opening its directory for the durability barrier of that new name.
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o300)).unwrap();
    let authorization = owner.handle(&message());
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        authorization.is_err(),
        "file fsync alone must not authorize a newly named audit record"
    );
}
