//! Pure conservation of optional command counts across a cold reconstruction.
use crate::broker::{ApprovedCommands, AuditEntry, PendingAuthorization};

pub(super) fn remaining_commands(
    source: &PendingAuthorization,
    audit: Option<&[AuditEntry]>,
    now_ms: u64,
) -> Option<ApprovedCommands> {
    let commands = source.commands.as_ref()?;
    if commands.expires_at_ms <= now_ms {
        return None;
    }
    if commands.uses.is_none() {
        return Some(commands.clone());
    }
    // Cold Resume sizes a new Session; it keeps its historical tolerance of
    // revocation entries, which only a warm restore must refuse.
    crate::broker::command_replay::replay(source, audit?, now_ms)
        .ok()
        .map(|replay| replay.commands)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{broker::AuditDecision, launcher_install::Identity};

    fn source() -> PendingAuthorization {
        PendingAuthorization {
            role: crate::launch_protocol::LaunchRole::Agent,
            conformance: crate::launch_protocol::ConformanceAuthorization::default(),
            dependencies: None,
            skill_requests: None,
            beads_mutations: None,
            provider_requests: None,
            require_cold_recovery: true,
            authorization_id: "authorization".into(),
            request_id: "request".into(),
            request_digest: crate::Digest::of(b"request").to_string(),
            controller_uid: 1000,
            session_id: "source".into(),
            run_id: "run".into(),
            agent_id: "agent".into(),
            envelope_revision: 1,
            identity: Identity {
                slot: 0,
                uid: 2000,
                gid: 2000,
            },
            expires_at_ms: 10000,
            broker_loss_grace_ms: 500,
            commands: Some(ApprovedCommands {
                command_digest: crate::Digest::of(b"command").to_string(),
                timeout_ms: 1000,
                uses: Some(6),
                allow_delegation: true,
                expires_at_ms: 9000,
            }),
        }
    }

    fn entry(decision: AuditDecision) -> AuditEntry {
        AuditEntry {
            at_ms: 1000,
            session_id: "source".into(),
            run_id: "run".into(),
            authorization_id: "authorization".into(),
            identity_slot: 0,
            decision,
        }
    }

    #[test]
    fn cold_resume_spends_intents_and_delegated_reservations_exactly_once() {
        let audit = [
            entry(AuditDecision::AuthorizationConsumed),
            entry(AuditDecision::EffectCommitIntent {
                grant: None,
                sequence: 1,
            }),
            entry(AuditDecision::EffectOutcomeUnknown { sequence: 1 }),
            entry(AuditDecision::ToolGranted {
                grant: 1,
                pid: 42,
                revision: 1,
                uses: Some(3),
            }),
            entry(AuditDecision::EffectCommitIntent {
                grant: Some(1),
                sequence: 1,
            }),
        ];
        assert_eq!(
            remaining_commands(&source(), Some(&audit), 2000).map(|c| c.uses),
            Some(Some(2))
        );
    }

    #[test]
    fn cold_resume_zero_missing_or_contradictory_balance_never_becomes_uncapped() {
        let consumed = entry(AuditDecision::AuthorizationConsumed);
        assert!(remaining_commands(&source(), None, 2000).is_none());
        assert!(remaining_commands(&source(), Some(&[]), 2000).is_none());
        assert!(
            remaining_commands(&source(), Some(std::slice::from_ref(&consumed)), 9000).is_none()
        );
        let spent = entry(AuditDecision::ToolGranted {
            grant: 1,
            pid: 42,
            revision: 1,
            uses: Some(6),
        });
        assert_eq!(
            remaining_commands(&source(), Some(&[consumed.clone(), spent]), 2000).map(|c| c.uses),
            Some(Some(0))
        );
        for decision in [
            AuditDecision::EffectCommitIntent {
                grant: None,
                sequence: 2,
            },
            AuditDecision::ToolGranted {
                grant: 1,
                pid: 42,
                revision: 2,
                uses: Some(2),
            },
            AuditDecision::ToolGranted {
                grant: 1,
                pid: 42,
                revision: 1,
                uses: None,
            },
            AuditDecision::EffectCommitIntent {
                grant: Some(1),
                sequence: 1,
            },
        ] {
            assert!(
                remaining_commands(&source(), Some(&[consumed.clone(), entry(decision)]), 2000)
                    .is_none()
            );
        }
    }
}
