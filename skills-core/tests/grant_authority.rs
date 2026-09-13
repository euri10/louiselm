//! Explicit grants share the Agent budget and retain spent outcomes.
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
    launch_protocol::{CommandMessage, CommandOperation},
    launch_supervisor::CapabilityBinding,
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

fn authority(root: &std::path::Path, allow_delegation: bool) -> CommandAuthority {
    authority_with_limit(root, allow_delegation, Some(3))
}

fn authority_with_limit(
    root: &std::path::Path,
    allow_delegation: bool,
    uses: Option<u32>,
) -> CommandAuthority {
    CommandAuthority::new(
        CapabilityBinding {
            session_id: "session".into(),
            run_id: "run".into(),
            channel_id: "agent-capability".into(),
            envelope_revision: 1,
            identity_slot: 1,
            assigned_uid: 1001,
            assigned_gid: 1001,
            agent_pid: 123,
        },
        DelegationPolicy {
            authorization_id: "approval".into(),
            scope: CommandScope {
                command_digest: Digest::of(b"printf delegated"),
                timeout_ms: 1000,
                uses,
            },
            allow_delegation,
            expires_at: Instant::now() + Duration::from_secs(30),
        },
        Arc::new(AuditLog::open(root).unwrap()),
    )
    .unwrap()
}

fn grant() -> CommandMessage {
    serde_json::from_value(serde_json::json!({
        "schema":"louiselm.launch.command/1", "protocol_version":1,
        "request_id":"grant-request", "session_id":"session", "run_id":"run", "envelope_revision":1,
        "operation": {"kind":"delegation_request",
            "principal":{"channel_id":"agent-capability","pid":123,"uid":1001,"gid":1001},
            "tool":{"channel_id":"tool-capability-1","pid":456,"uid":1001,"gid":1001},
            "grant":{"sequence":1,"command_digest":Digest::of(b"printf delegated").to_string(),"timeout_ms":1000,"uses":2,"valid_for_ms":1000}
        }
    })).unwrap()
}

#[test]
fn uncapped_grants_require_uncapped_parents_and_explicit_delegation() {
    for parent in [Some(3), None] {
        for child in [Some(2), None] {
            for allow in [false, true] {
                let root = tempfile::tempdir().unwrap();
                let mut owner = authority_with_limit(root.path(), allow, parent);
                let mut wire = serde_json::to_value(grant()).unwrap();
                if let Some(uses) = child {
                    wire["operation"]["grant"]["uses"] = uses.into();
                } else {
                    wire["operation"]["grant"]
                        .as_object_mut()
                        .unwrap()
                        .remove("uses");
                }
                let request: CommandMessage = serde_json::from_value(wire).unwrap();
                request.validate().unwrap();
                let granted = owner.handle(&request);
                if !allow || (parent.is_some() && child.is_none()) {
                    assert!(granted.is_err());
                    continue;
                }
                granted.unwrap();
                for sequence in 1..=65 {
                    let result = owner.handle(&command(true, sequence));
                    assert_eq!(
                        result.is_ok(),
                        child.is_none_or(|uses| sequence <= u64::from(uses))
                    );
                    if result.is_err() {
                        break;
                    }
                }
                assert!(owner.handle(&command(false, 1)).is_ok());
                let audit = AuditLog::open(root.path()).unwrap().entries().unwrap();
                assert!(audit.iter().any(|entry| matches!(entry.decision,
                    louiselm_skills::broker::AuditDecision::ToolGranted { uses, .. } if uses == child)));
                owner.revoke_grant("revoke-count-test", 1).unwrap();
                assert!(owner.handle(&command(true, 66)).is_err());
            }
        }
    }
}

#[test]
fn grants_reject_unbounded_fields_and_peer_selected_execution_authority() {
    use louiselm_skills::launch_protocol::decode_message;
    let wire = serde_json::to_value(grant()).unwrap();
    for (field, value) in [
        ("sequence", serde_json::json!(0)),
        ("uses", serde_json::json!(0)),
        ("uses", serde_json::json!(65)),
        ("uses", serde_json::json!(-1)),
        ("uses", serde_json::json!(1.5)),
        ("uses", serde_json::json!("unlimited")),
        ("timeout_ms", serde_json::json!(30_001)),
        ("valid_for_ms", serde_json::json!(0)),
        ("valid_for_ms", serde_json::json!(30_001)),
        ("command_digest", serde_json::json!("not-a-digest")),
        ("executable", serde_json::json!("/bin/sh")),
        ("mounts", serde_json::json!([])),
        ("environment", serde_json::json!({})),
        ("network", serde_json::json!(true)),
    ] {
        let mut changed = wire.clone();
        changed["operation"]["grant"][field] = value;
        assert!(
            decode_message(&serde_json::to_vec(&changed).unwrap()).is_err(),
            "{field}"
        );
    }
    let mut delegate = grant();
    let CommandOperation::DelegationRequest { grant, .. } = delegate.operation else {
        unreachable!()
    };
    let CommandOperation::Request { command, .. } = command(true, 1).operation else {
        unreachable!()
    };
    delegate.operation = CommandOperation::Delegate { grant, command };
    assert!(decode_message(&delegate.canonical_bytes()).is_ok());
    let wire = serde_json::to_value(delegate).unwrap();
    for (field, value) in [
        ("sequence", serde_json::json!(2)),
        ("envelope_revision", serde_json::json!(2)),
        ("session_id", serde_json::json!("other")),
        ("run_id", serde_json::json!("other")),
    ] {
        let mut changed = wire.clone();
        changed["operation"]["command"][field] = value;
        assert!(
            decode_message(&serde_json::to_vec(&changed).unwrap()).is_err(),
            "{field}"
        );
    }
}

#[test]
fn only_existing_delegation_approval_can_reserve_a_tool_grant() {
    let root = tempfile::tempdir().unwrap();
    let mut denied = authority(&root.path().join("denied"), false);
    assert!(denied.handle(&grant()).is_err());
    let mut allowed = authority(&root.path().join("allowed"), true);
    let reply = allowed.handle(&grant()).unwrap();
    assert_eq!(
        serde_json::to_value(reply).unwrap()["operation"]["kind"],
        "granted"
    );
    assert!(
        allowed.handle(&grant()).is_err(),
        "grant replay must not reserve fresh authority"
    );
}

fn command(tool: bool, sequence: u64) -> CommandMessage {
    let mut message = grant();
    message.request_id = format!("{}-{sequence}", if tool { "tool" } else { "agent" });
    let principal = if let CommandOperation::DelegationRequest {
        principal,
        tool: target,
        ..
    } = &message.operation
    {
        if tool {
            target.clone()
        } else {
            principal.clone()
        }
    } else {
        unreachable!()
    };
    message.operation = CommandOperation::Request {
        principal,
        command: louiselm_skills::launch_protocol::ToolExecutionRequest {
            schema: louiselm_skills::launch_protocol::TOOL_EXECUTION_SCHEMA.into(),
            protocol_version: 1,
            request_id: message.request_id.clone(),
            session_id: message.session_id.clone(),
            run_id: message.run_id.clone(),
            envelope_revision: 1,
            sequence,
            command: "printf delegated".into(),
            timeout_ms: 1000,
        },
    };
    message
}

#[test]
fn reserved_budget_and_sequences_are_separate_and_never_refunded() {
    let root = tempfile::tempdir().unwrap();
    let mut owner = authority(root.path(), true);
    assert!(
        owner.handle(&command(true, 1)).is_err(),
        "ungranted tool has no authority"
    );
    owner.handle(&grant()).unwrap();
    let agent = owner.handle(&command(false, 1)).unwrap();
    assert!(matches!(
        agent.operation,
        CommandOperation::Authorize {
            dispatch_sequence: 1,
            ..
        }
    ));
    assert!(
        owner.handle(&command(false, 2)).is_err(),
        "reserved uses are unavailable to the Agent"
    );
    let tool = owner.handle(&command(true, 1)).unwrap();
    assert!(matches!(
        tool.operation,
        CommandOperation::Authorize {
            dispatch_sequence: 2,
            ..
        }
    ));
    assert!(owner.handle(&command(true, 1)).is_err());
    let mut unknown = command(true, 1);
    unknown.operation = CommandOperation::Outcome {
        dispatch_sequence: 2,
        outcome: louiselm_skills::launch_protocol::CommandOutcome::Unknown,
    };
    owner.handle(&unknown).unwrap();
    owner.handle(&command(true, 2)).unwrap();
    assert!(owner.handle(&command(true, 3)).is_err());
    assert!(
        authority_from_audit(root.path()).is_err(),
        "restart cannot restore grant reservations"
    );
}

fn authority_from_audit(
    root: &std::path::Path,
) -> Result<CommandAuthority, louiselm_skills::broker::delegation::DelegationError> {
    let fresh = tempfile::tempdir().unwrap();
    let base = authority(fresh.path(), true);
    CommandAuthority::new(
        base.binding().clone(),
        DelegationPolicy {
            authorization_id: "approval".into(),
            scope: CommandScope {
                command_digest: Digest::of(b"printf delegated"),
                timeout_ms: 1000,
                uses: Some(3),
            },
            allow_delegation: true,
            expires_at: Instant::now() + Duration::from_secs(30),
        },
        Arc::new(AuditLog::open(root).unwrap()),
    )
}

#[test]
fn helper_cannot_redelegate_widen_rebind_or_use_stale_subjects() {
    for field in [
        "parent",
        "digest",
        "timeout",
        "uses",
        "expiry",
        "revision",
        "sequence",
        "target_pid",
        "target_uid",
    ] {
        let root = tempfile::tempdir().unwrap();
        let mut owner = authority(root.path(), true);
        let mut request = grant();
        if let CommandOperation::DelegationRequest {
            principal,
            tool,
            grant,
        } = &mut request.operation
        {
            match field {
                "parent" => *principal = tool.clone(),
                "digest" => grant.command_digest = Digest::of(b"other").to_string(),
                "timeout" => grant.timeout_ms += 1,
                "uses" => grant.uses = Some(4),
                "expiry" => grant.valid_for_ms = 30_000,
                "revision" => request.envelope_revision += 1,
                "sequence" => grant.sequence += 1,
                "target_pid" => tool.pid = principal.pid,
                _ => tool.uid += 1,
            }
        }
        assert!(owner.handle(&request).is_err(), "{field}");
    }
}

#[test]
fn grant_revocation_preserves_agent_authority_and_late_actual_outcomes() {
    use louiselm_skills::launch_protocol::CommandOutcome;
    let root = tempfile::tempdir().unwrap();
    let mut owner = authority(root.path(), true);
    owner.handle(&grant()).unwrap();
    owner.handle(&command(true, 1)).unwrap();
    let mut revoke = owner.revoke_grant("revoke", 1).unwrap();
    assert!(!owner.grant_revocation_complete(1));
    assert!(owner.handle(&command(true, 2)).is_err());
    assert!(owner.handle(&command(false, 1)).is_ok());
    revoke.operation = CommandOperation::GrantRevoked {
        grant: 1,
        enforced: false,
    };
    assert!(owner.handle(&revoke).is_err());
    assert!(!owner.grant_revocation_complete(1));
    let mut outcome = command(true, 1);
    outcome.operation = CommandOperation::Outcome {
        dispatch_sequence: 1,
        outcome: CommandOutcome::Unknown,
    };
    owner.handle(&outcome).unwrap();
    revoke.operation = CommandOperation::GrantRevoked {
        grant: 1,
        enforced: true,
    };
    owner.handle(&revoke).unwrap();
    assert!(owner.grant_revocation_complete(1));
    outcome.operation = CommandOperation::Outcome {
        dispatch_sequence: 1,
        outcome: CommandOutcome::NotStarted {
            error: louiselm_skills::launch_protocol::ErrorCode::StateMismatch,
        },
    };
    owner.handle(&outcome).unwrap();
    assert!(owner.handle(&outcome).is_err());
    let audit = AuditLog::open(root.path()).unwrap().entries().unwrap();
    assert!(audit.iter().any(|entry| matches!(
        entry.decision,
        louiselm_skills::broker::AuditDecision::EffectFinished { grant: Some(1), .. }
    )));
}

#[test]
fn expiry_and_audit_failure_never_reopen_reserved_authority() {
    for uses in [Some(3), None] {
        let root = tempfile::tempdir().unwrap();
        let mut owner = authority_with_limit(root.path(), true, uses);
        let mut request = grant();
        if let CommandOperation::DelegationRequest { grant, .. } = &mut request.operation {
            grant.valid_for_ms = 40;
            if uses.is_none() {
                grant.uses = None;
            }
        }
        owner.handle(&request).unwrap();
        std::thread::sleep(Duration::from_millis(60));
        assert!(owner.handle(&command(true, 1)).is_err());
        assert!(owner.handle(&command(false, 1)).is_ok());
        assert_eq!(owner.handle(&command(false, 2)).is_ok(), uses.is_none());
        let broken = tempfile::tempdir().unwrap();
        let mut owner = authority_with_limit(broken.path(), true, uses);
        std::fs::create_dir(broken.path().join("decisions.jsonl")).unwrap();
        assert!(owner.handle(&grant()).is_err());
        std::fs::remove_dir(broken.path().join("decisions.jsonl")).unwrap();
        assert!(owner.handle(&command(false, 1)).is_err());
    }
}
