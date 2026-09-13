//! Delegation policy and delayed-effect regressions.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "Fixtures assert setup and observed outcomes."
)]

#[path = "delegation_test_support.rs"]
mod support;

use super::{
    AuditDecision,
    delegation::{CommandScope, DelegationError},
};
use crate::Digest;
use crate::launch_protocol::ProtocolMessage;
use crate::launch_transport::LauncherPacket;
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use support::{Fixture, Peer, QueuedEffect, completion, outcome};

#[test]
fn delegated_scope_cannot_expand_command_timeout_or_budget() {
    let approved = CommandScope {
        command_digest: Digest::of(b"printf permitted"),
        timeout_ms: 1000,
        uses: Some(2),
    };
    let mut requested = approved.clone();
    requested.uses = Some(1);
    requested.timeout_ms = 500;
    assert!(requested.is_within(&approved));
    requested.command_digest = Digest::of(b"printf other");
    assert!(!requested.is_within(&approved));
    requested = approved.clone();
    requested.timeout_ms += 1;
    assert!(!requested.is_within(&approved));
    requested = approved.clone();
    requested.uses = Some(3);
    assert!(!requested.is_within(&approved));
}

#[test]
fn local_uncapped_grants_preserve_finite_attenuation_and_admission_checks() {
    for parent in [Some(4), None] {
        let mut fixture = Fixture::with_limit(true, parent);
        fixture.grant.scope.uses = None;
        let delegated = fixture.delegate();
        if parent.is_some() {
            assert!(matches!(delegated, Err(DelegationError::ScopeMismatch)));
            continue;
        }
        let tool = delegated.unwrap();
        for sequence in 1..=65 {
            let mut packet = fixture.tool.packet();
            if let LauncherPacket::Request(ProtocolMessage::ToolExecution(request)) =
                &mut packet.packet
            {
                request.sequence = sequence;
                request.request_id = format!("uncapped-{sequence}");
            }
            // Abandon preparation: an admission is never refunded, but uncapped
            // authority must still admit the next exactly sequenced request.
            tool.execute(&packet, &QueuedEffect::default(), completion().0)
                .unwrap();
            assert!(matches!(
                tool.execute(&packet, &QueuedEffect::default(), completion().0),
                Err(DelegationError::Replay)
            ));
        }
        fixture.owner.revoke().unwrap();
        assert!(
            tool.execute(
                &fixture.tool.packet(),
                &QueuedEffect::default(),
                completion().0
            )
            .is_err()
        );
    }
}

#[test]
fn approved_tool_effect_uses_real_packet_credentials_and_audits_only_attribution() {
    let mut fixture = Fixture::new(true);
    let tool = fixture.delegate().unwrap();
    let packet = fixture.tool.receive_real_packet();
    let effect = QueuedEffect::default();
    let effects = Arc::new(AtomicUsize::new(0));
    let (done, result) = completion();
    tool.execute(&packet, &effect, done).unwrap();
    assert_eq!(effects.load(Ordering::SeqCst), 0);
    effect.finish(Arc::clone(&effects));
    assert!(outcome(&result).is_ok());
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    let audit = fixture.audit.entries().unwrap();
    assert!(audit.iter().any(|entry| matches!(
        entry.decision,
        AuditDecision::EffectFinished {
            grant: Some(1),
            succeeded: true,
            ..
        }
    )));
    let bytes = serde_json::to_string(&audit).unwrap();
    for forbidden in [
        "delegated-sensitive",
        "command",
        "stdout",
        "environment",
        "token",
    ] {
        assert!(!bytes.contains(forbidden));
    }
}

#[test]
fn subset_without_operator_delegation_permission_is_denied() {
    let fixture = Fixture::new(false);
    assert!(matches!(
        fixture.delegate(),
        Err(DelegationError::DelegationDenied)
    ));
    assert!(fixture.tool.channel.is_closed());
}

#[test]
fn only_agent_may_delegate_and_other_processes_cannot_use_tool_handle() {
    let fixture = Fixture::new(true);
    let stranger = Peer::new(&fixture.root.path().join("stranger.sock"));
    assert!(matches!(
        fixture
            .owner
            .delegate(fixture.tool.credentials(), &fixture.grant, stranger.bound()),
        Err(DelegationError::IdentityMismatch)
    ));
    let tool = fixture.delegate().unwrap();
    let effect = QueuedEffect::default();
    for packet in [fixture.agent.packet(), stranger.packet()] {
        assert!(matches!(
            tool.execute(&packet, &effect, completion().0),
            Err(DelegationError::IdentityMismatch)
        ));
    }
    let mut inherited = fixture.tool.packet();
    inherited.message_credentials = fixture.agent.credentials();
    assert!(matches!(
        tool.execute(&inherited, &effect, completion().0),
        Err(DelegationError::IdentityMismatch)
    ));
}

#[test]
fn grant_subject_revision_expiry_scope_and_replay_cannot_expand_authority() {
    for case in 0..8 {
        let mut fixture = Fixture::new(true);
        match case {
            0 => fixture.grant.session_id = "different".to_owned(),
            1 => fixture.grant.run_id = "different".to_owned(),
            2 => fixture.grant.envelope_revision = 1,
            3 => fixture.grant.expires_at += Duration::from_secs(1),
            4 => fixture.grant.scope.command_digest = Digest::of(b"different"),
            5 => fixture.grant.scope.timeout_ms += 1,
            6 => fixture.grant.sequence = 2,
            _ => {
                fixture.grant.expires_at =
                    Instant::now().checked_sub(Duration::from_secs(1)).unwrap();
            }
        }
        assert!(fixture.delegate().is_err(), "case {case}");
        assert!(fixture.tool.channel.is_closed());
    }
}

#[test]
fn aggregate_budget_is_reserved_and_cannot_be_amplified_by_multiple_grants() {
    let mut fixture = Fixture::new(true);
    let _first = fixture.delegate().unwrap();
    let second = Peer::new(&fixture.root.path().join("second.sock"));
    fixture.grant.sequence = 2;
    fixture.grant.scope.uses = Some(3);
    assert!(matches!(
        fixture
            .owner
            .delegate(fixture.agent.credentials(), &fixture.grant, second.bound()),
        Err(DelegationError::BudgetExhausted)
    ));
}

#[test]
fn effect_replay_and_scope_mismatch_do_not_reexecute() {
    let fixture = Fixture::new(true);
    let tool = fixture.delegate().unwrap();
    let effect = QueuedEffect::default();
    let packet = fixture.tool.packet();
    let (done, result) = completion();
    tool.execute(&packet, &effect, done).unwrap();
    assert!(matches!(
        tool.execute(&packet, &effect, completion().0),
        Err(DelegationError::Replay)
    ));
    let effects = Arc::new(AtomicUsize::new(0));
    effect.finish(Arc::clone(&effects));
    assert!(outcome(&result).is_ok());
    for case in 0..5 {
        let mut packet = fixture.tool.packet();
        if let LauncherPacket::Request(ProtocolMessage::ToolExecution(request)) = &mut packet.packet
        {
            request.sequence = 2;
            match case {
                0 => request.envelope_revision = 1,
                1 => request.command = "other".to_owned(),
                2 => request.timeout_ms = 1001,
                3 => request.session_id = "other".to_owned(),
                _ => request.run_id = "other".to_owned(),
            }
        }
        assert!(matches!(
            tool.execute(&packet, &effect, completion().0),
            Err(DelegationError::ScopeMismatch)
        ));
    }
    assert_eq!(effects.load(Ordering::SeqCst), 1);
}

#[test]
fn agent_exit_cascades_to_every_grant_and_denies_delayed_commit() {
    let mut fixture = Fixture::new(true);
    let first = fixture.delegate().unwrap();
    let second_peer = Peer::new(&fixture.root.path().join("second.sock"));
    fixture.grant.sequence = 2;
    let second = fixture
        .owner
        .delegate(
            fixture.agent.credentials(),
            &fixture.grant,
            second_peer.bound(),
        )
        .unwrap();
    let effect = QueuedEffect::default();
    let effects = Arc::new(AtomicUsize::new(0));
    let (done, result) = completion();
    first
        .execute(&fixture.tool.packet(), &effect, done)
        .unwrap();
    fixture.agent.kill();
    effect.finish(Arc::clone(&effects));
    assert!(matches!(outcome(&result), Err(DelegationError::Revoked)));
    assert_eq!(effects.load(Ordering::SeqCst), 0);
    for channel in [
        &fixture.agent.channel,
        &fixture.tool.channel,
        &second_peer.channel,
    ] {
        assert!(channel.is_closed());
    }
    assert!(matches!(
        second.execute(&second_peer.packet(), &effect, completion().0),
        Err(DelegationError::Revoked)
    ));
}

#[test]
fn revoked_owner_or_dead_tool_cannot_commit_prepared_work() {
    for dead_tool in [false, true] {
        let mut fixture = Fixture::new(true);
        let tool = fixture.delegate().unwrap();
        let effect = QueuedEffect::default();
        let effects = Arc::new(AtomicUsize::new(0));
        let (done, result) = completion();
        tool.execute(&fixture.tool.packet(), &effect, done).unwrap();
        if dead_tool {
            fixture.tool.kill();
        } else {
            fixture.owner.revoke().unwrap();
        }
        effect.finish(Arc::clone(&effects));
        assert!(matches!(outcome(&result), Err(DelegationError::Revoked)));
        assert_eq!(effects.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn agent_proxy_requests_use_agent_policy_budget_and_attribution() {
    let fixture = Fixture::new(false);
    let effect = QueuedEffect::default();
    let (done, result) = completion();
    fixture
        .owner
        .execute(&fixture.agent.packet(), &effect, done)
        .unwrap();
    let effects = Arc::new(AtomicUsize::new(0));
    effect.finish(Arc::clone(&effects));
    assert!(outcome(&result).is_ok());
    assert!(
        fixture
            .audit
            .entries()
            .unwrap()
            .iter()
            .any(|entry| matches!(
                entry.decision,
                AuditDecision::EffectFinished {
                    grant: None,
                    succeeded: true,
                    ..
                }
            ))
    );
    assert!(matches!(
        fixture
            .owner
            .execute(&fixture.tool.packet(), &effect, completion().0),
        Err(DelegationError::IdentityMismatch)
    ));
}

#[test]
fn completed_effect_is_reported_after_revocation_before_completion_delivery() {
    let fixture = Fixture::new(true);
    let tool = fixture.delegate().unwrap();
    let queue = QueuedEffect::default();
    let (done, result) = completion();
    tool.execute(&fixture.tool.packet(), &queue, done).unwrap();
    let (pending, complete) = queue.take();
    let (output, committed) = std::thread::spawn(move || pending.commit(|_| Ok(17_u32)).unwrap())
        .join()
        .unwrap();
    assert_eq!(output, 17);
    fixture.owner.revoke().unwrap();
    complete(
        committed.finish(Ok(crate::launch_protocol::ToolExecutionResult {
            exit_code: 0,
            stdout: "already completed".to_owned(),
            stderr: String::new(),
            truncated: false,
            timed_out: false,
        })),
    );
    assert_eq!(outcome(&result).unwrap().stdout, "already completed");
}

#[test]
fn expired_grant_cannot_commit_already_admitted_effect() {
    for uses in [Some(4), None] {
        let mut fixture = Fixture::with_limit(true, uses);
        fixture.grant.scope.uses = uses;
        fixture.grant.expires_at = Instant::now() + Duration::from_secs(1);
        let tool = fixture.delegate().unwrap();
        let queue = QueuedEffect::default();
        let (done, result) = completion();
        tool.execute(&fixture.tool.packet(), &queue, done).unwrap();
        std::thread::sleep(
            fixture
                .grant
                .expires_at
                .saturating_duration_since(Instant::now()),
        );
        let effects = Arc::new(AtomicUsize::new(0));
        queue.finish(Arc::clone(&effects));
        assert!(matches!(outcome(&result), Err(DelegationError::Expired)));
        assert_eq!(effects.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn cloned_tool_handles_share_a_nonrefundable_budget() {
    let fixture = Fixture::new(true);
    let tool = fixture.delegate().unwrap();
    let clone = tool.clone();
    let queue = QueuedEffect::default();
    let effects = Arc::new(AtomicUsize::new(0));
    for sequence in 1..=3 {
        let mut packet = fixture.tool.packet();
        if let LauncherPacket::Request(ProtocolMessage::ToolExecution(request)) = &mut packet.packet
        {
            request.sequence = sequence;
        }
        let (done, result) = completion();
        let admitted = clone.execute(&packet, &queue, done);
        if sequence <= 2 {
            admitted.unwrap();
            queue.finish(Arc::clone(&effects));
            assert!(outcome(&result).is_ok());
        } else {
            assert!(matches!(admitted, Err(DelegationError::BudgetExhausted)));
        }
    }
    assert_eq!(effects.load(Ordering::SeqCst), 2);
}

#[test]
fn audit_failure_denies_commit_and_closes_all_authority() {
    let fixture = Fixture::new(true);
    let tool = fixture.delegate().unwrap();
    let queue = QueuedEffect::default();
    let (done, result) = completion();
    tool.execute(&fixture.tool.packet(), &queue, done).unwrap();
    obstruct_audit(&fixture);
    let effects = Arc::new(AtomicUsize::new(0));
    queue.finish(Arc::clone(&effects));
    assert!(matches!(outcome(&result), Err(DelegationError::Audit(_))));
    assert_eq!(effects.load(Ordering::SeqCst), 0);
    assert!(fixture.agent.channel.is_closed() && fixture.tool.channel.is_closed());
}

#[test]
fn audit_failure_after_effect_preserves_its_actual_outcome() {
    let fixture = Fixture::new(true);
    let tool = fixture.delegate().unwrap();
    let queue = QueuedEffect::default();
    tool.execute(&fixture.tool.packet(), &queue, completion().0)
        .unwrap();
    let (pending, _complete) = queue.take();
    let ((), committed) = pending.commit(|_| Ok(())).unwrap();
    obstruct_audit(&fixture);
    let result = committed.finish(Ok(crate::launch_protocol::ToolExecutionResult {
        exit_code: 0,
        stdout: "completed".to_owned(),
        stderr: String::new(),
        truncated: false,
        timed_out: false,
    }));
    assert!(
        matches!(result, Err(DelegationError::CompletionAudit { outcome, .. }) if outcome.as_ref().as_ref().unwrap().stdout == "completed")
    );
}

fn obstruct_audit(fixture: &Fixture) {
    std::fs::rename(
        fixture.root.path().join("decisions.jsonl"),
        fixture.root.path().join("saved-audit"),
    )
    .unwrap();
    std::fs::create_dir(fixture.root.path().join("decisions.jsonl")).unwrap();
}

#[test]
fn owner_drop_revokes_queued_work_and_all_cloned_handles() {
    let fixture = Fixture::new(true);
    let tool = fixture.delegate().unwrap();
    let queue = QueuedEffect::default();
    let (done, result) = completion();
    tool.execute(&fixture.tool.packet(), &queue, done).unwrap();
    drop(fixture.owner);
    let effects = Arc::new(AtomicUsize::new(0));
    queue.finish(Arc::clone(&effects));
    assert!(matches!(outcome(&result), Err(DelegationError::Revoked)));
    assert_eq!(effects.load(Ordering::SeqCst), 0);
    assert!(fixture.agent.channel.is_closed() && fixture.tool.channel.is_closed());
}
