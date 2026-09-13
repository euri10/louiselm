//! Pure conservation of optional command counts across a cold reconstruction.
use crate::broker::{ApprovedCommands, AuditDecision, AuditEntry, PendingAuthorization};

pub(super) fn remaining_commands(
    source: &PendingAuthorization,
    audit: Option<&[AuditEntry]>,
    now_ms: u64,
) -> Option<ApprovedCommands> {
    let mut commands = source.commands.clone()?;
    if commands.expires_at_ms <= now_ms {
        return None;
    }
    let Some(limit) = commands.uses else {
        return Some(commands);
    };
    let entries = audit?
        .iter()
        .filter(|entry| entry.session_id == source.session_id);
    let mut consumed = false;
    let mut direct_sequence = 0_u64;
    let mut grants = std::collections::BTreeMap::new();
    let mut spent = 0_u32;
    for entry in entries {
        if entry.authorization_id != source.authorization_id
            || entry.run_id != source.run_id
            || entry.identity_slot != source.identity.slot
        {
            return None;
        }
        match entry.decision {
            AuditDecision::AuthorizationConsumed if !consumed => consumed = true,
            AuditDecision::AuthorizationConsumed => return None,
            AuditDecision::EffectCommitIntent {
                grant: None,
                sequence,
            } => {
                if !consumed || direct_sequence.checked_add(1) != Some(sequence) {
                    return None;
                }
                direct_sequence = sequence;
                spent = spent.checked_add(1)?;
            }
            AuditDecision::ToolGranted {
                grant,
                revision,
                uses,
                ..
            } => {
                let uses = uses?;
                if !consumed
                    || revision != source.envelope_revision
                    || uses == 0
                    || uses > limit
                    || grant != u64::try_from(grants.len()).ok()?.checked_add(1)?
                    || grants.insert(grant, (uses, 0_u64)).is_some()
                {
                    return None;
                }
                spent = spent.checked_add(uses)?;
            }
            AuditDecision::EffectCommitIntent {
                grant: Some(grant),
                sequence,
            } => {
                let (reserved, previous) = grants.get_mut(&grant)?;
                if previous.checked_add(1) != Some(sequence) || sequence > u64::from(*reserved) {
                    return None;
                }
                *previous = sequence;
            }
            _ => {}
        }
    }
    let remaining = limit.checked_sub(spent)?;
    if !consumed {
        return None;
    }
    commands.uses = Some(remaining);
    Some(commands)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{broker::AuditDecision, launcher_install::Identity};

    fn source() -> PendingAuthorization {
        PendingAuthorization {
            require_cold_recovery: true,
            authorization_id: "authorization".into(),
            request_id: "request".into(),
            request_digest: crate::Digest::of(b"request").to_string(),
            controller_uid: 1000,
            session_id: "source".into(),
            run_id: "run".into(),
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
