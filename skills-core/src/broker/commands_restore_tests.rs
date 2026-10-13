#![allow(
    clippy::unwrap_used,
    reason = "Fixtures assert restored command authority directly."
)]
use super::*;
use crate::{
    Digest,
    broker::{ApprovedCommands, command_replay::CommandReplay, delegation::CommandScope},
    launch_protocol::{TOOL_EXECUTION_SCHEMA, ToolExecutionRequest},
};
use std::time::Duration;

fn binding() -> CapabilityBinding {
    CapabilityBinding {
        session_id: "session-1".to_owned(),
        run_id: "run-1".to_owned(),
        channel_id: "agent-capability".to_owned(),
        envelope_revision: 1,
        identity_slot: 1,
        assigned_uid: 1001,
        assigned_gid: 1001,
        agent_pid: 123,
    }
}

fn policy(uses: Option<u32>) -> DelegationPolicy {
    DelegationPolicy {
        authorization_id: "authorization-1".to_owned(),
        scope: CommandScope {
            command_digest: Digest::of(b"printf private"),
            timeout_ms: 1000,
            uses,
        },
        allow_delegation: false,
        expires_at: Instant::now() + Duration::from_secs(30),
    }
}

fn replay(uses: Option<u32>, revoked: bool) -> CommandReplay {
    CommandReplay {
        commands: ApprovedCommands {
            command_digest: Digest::of(b"printf private").to_string(),
            timeout_ms: 1000,
            uses,
            allow_delegation: false,
            expires_at_ms: u64::MAX,
        },
        principal_sequence: 1,
        dispatch_sequence: 1,
        grant_sequence: 0,
        revoked,
    }
}

fn request(sequence: u64) -> CommandMessage {
    let id = format!("command-{sequence}");
    CommandMessage {
        schema: COMMAND_SCHEMA.to_owned(),
        protocol_version: 1,
        request_id: id.clone(),
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
                request_id: id,
                session_id: "session-1".to_owned(),
                run_id: "run-1".to_owned(),
                envelope_revision: 1,
                sequence,
                command: "printf private".to_owned(),
                timeout_ms: 1000,
            },
        },
    }
}

#[test]
fn restore_continues_sequences_and_spends_only_the_remaining_uses() {
    let root = tempfile::tempdir().unwrap();
    let audit = Arc::new(AuditLog::open(root.path()).unwrap());
    let mut owner =
        CommandAuthority::restore(binding(), policy(Some(1)), audit, &replay(Some(1), false))
            .unwrap();
    assert!(matches!(
        owner.handle(&request(1)),
        Err(DelegationError::Replay)
    ));
    let reply = owner.handle(&request(2)).unwrap();
    assert!(matches!(
        reply.operation,
        CommandOperation::Authorize {
            principal_sequence: 2,
            dispatch_sequence: 2,
            ..
        }
    ));
    assert!(matches!(
        owner.handle(&request(3)),
        Err(DelegationError::BudgetExhausted)
    ));
}

#[test]
fn restore_refuses_revoked_or_inconsistent_authority() {
    let root = tempfile::tempdir().unwrap();
    let audit = Arc::new(AuditLog::open(root.path()).unwrap());
    assert!(matches!(
        CommandAuthority::restore(
            binding(),
            policy(Some(1)),
            Arc::clone(&audit),
            &replay(Some(1), true)
        ),
        Err(DelegationError::Revoked)
    ));
    assert!(matches!(
        CommandAuthority::restore(
            binding(),
            policy(Some(2)),
            Arc::clone(&audit),
            &replay(Some(1), false)
        ),
        Err(DelegationError::InvalidRequest)
    ));
    let mut expired = policy(Some(1));
    expired.expires_at = Instant::now();
    assert!(matches!(
        CommandAuthority::restore(binding(), expired, audit, &replay(Some(1), false)),
        Err(DelegationError::Expired)
    ));
}
