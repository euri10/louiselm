//! Pure replay of one Session's durable command audit.
//!
//! The broker spends a command use in its audit before any approval leaves it,
//! so the audit alone fixes how much of a Session's approved budget remains.
//! Cold Resume uses this to size a new Session. An authorized warm Resume after
//! a broker restart uses it to rebuild the same Session's authority without
//! refunding anything (louiselm-d6fv.12.5.5.2).

use crate::broker::{ApprovedCommands, AuditDecision, AuditEntry, PendingAuthorization};
use std::collections::BTreeMap;

/// Command state of one Session as of its last durable audit entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CommandReplay {
    /// Approved commands with `uses` reduced to the remaining budget.
    pub(crate) commands: ApprovedCommands,
    /// Last direct (non-delegated) principal sequence the broker authorized.
    pub(crate) principal_sequence: u64,
    /// Count of every authorized dispatch, direct or delegated.
    pub(crate) dispatch_sequence: u64,
    /// Count of delegated grants issued.
    pub(crate) grant_sequence: u64,
    /// Whether capability revocation was ever requested or enforced.
    pub(crate) revoked: bool,
}

/// Why the audit cannot yield a trustworthy command state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReplayRefusal {
    /// The authorization approved no commands.
    NotApproved,
    /// The original command approval has expired.
    Expired,
    /// The authorization was never consumed by a launch.
    NotConsumed,
    /// Entries are foreign, out of order, over budget or otherwise inconsistent.
    Contradictory,
}

/// Replays `audit` for `source`'s Session. Uncertain outcomes stay spent.
pub(crate) fn replay(
    source: &PendingAuthorization,
    audit: &[AuditEntry],
    now_ms: u64,
) -> Result<CommandReplay, ReplayRefusal> {
    let mut commands = source.commands.clone().ok_or(ReplayRefusal::NotApproved)?;
    if commands.expires_at_ms <= now_ms {
        return Err(ReplayRefusal::Expired);
    }
    let limit = commands.uses;
    let mut consumed = false;
    let mut revoked = false;
    let mut principal_sequence = 0_u64;
    let mut dispatch_sequence = 0_u64;
    let mut grants: BTreeMap<u64, (u32, u64)> = BTreeMap::new();
    let mut spent = 0_u32;
    let contradictory = || ReplayRefusal::Contradictory;
    for entry in audit
        .iter()
        .filter(|entry| entry.session_id == source.session_id)
    {
        if entry.authorization_id != source.authorization_id
            || entry.run_id != source.run_id
            || entry.identity_slot != source.identity.slot
        {
            return Err(contradictory());
        }
        match entry.decision {
            AuditDecision::AuthorizationConsumed if !consumed => consumed = true,
            AuditDecision::AuthorizationConsumed => return Err(contradictory()),
            AuditDecision::EffectCommitIntent {
                grant: None,
                sequence,
            } => {
                if !consumed || principal_sequence.checked_add(1) != Some(sequence) {
                    return Err(contradictory());
                }
                principal_sequence = sequence;
                dispatch_sequence = dispatch_sequence.checked_add(1).ok_or_else(contradictory)?;
                spent = spent.checked_add(1).ok_or_else(contradictory)?;
            }
            AuditDecision::ToolGranted {
                grant,
                revision,
                uses,
                ..
            } => {
                let next = u64::try_from(grants.len())
                    .ok()
                    .and_then(|count| count.checked_add(1));
                let reserved = match (limit, uses) {
                    (Some(limit), Some(uses)) if uses != 0 && uses <= limit => uses,
                    (None, Some(uses)) if uses != 0 => uses,
                    (None, None) => u32::MAX,
                    _ => return Err(contradictory()),
                };
                if !consumed
                    || revision != source.envelope_revision
                    || Some(grant) != next
                    || grants.insert(grant, (reserved, 0)).is_some()
                {
                    return Err(contradictory());
                }
                if limit.is_some() {
                    spent = spent.checked_add(reserved).ok_or_else(contradictory)?;
                }
            }
            AuditDecision::EffectCommitIntent {
                grant: Some(grant),
                sequence,
            } => {
                let (reserved, previous) = grants.get_mut(&grant).ok_or_else(contradictory)?;
                if previous.checked_add(1) != Some(sequence) || sequence > u64::from(*reserved) {
                    return Err(contradictory());
                }
                *previous = sequence;
                dispatch_sequence = dispatch_sequence.checked_add(1).ok_or_else(contradictory)?;
            }
            AuditDecision::CapabilitiesRevocationRequested | AuditDecision::CapabilitiesRevoked => {
                revoked = true;
            }
            _ => {}
        }
    }
    if !consumed {
        return Err(ReplayRefusal::NotConsumed);
    }
    commands.uses = match limit {
        Some(limit) => Some(limit.checked_sub(spent).ok_or_else(contradictory)?),
        None => None,
    };
    Ok(CommandReplay {
        commands,
        principal_sequence,
        dispatch_sequence,
        grant_sequence: u64::try_from(grants.len()).map_err(|_| contradictory())?,
        revoked,
    })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "Fixtures assert replayed command state directly."
)]
mod tests {
    use super::*;
    use crate::launcher_install::Identity;

    fn source(uses: Option<u32>) -> PendingAuthorization {
        PendingAuthorization {
            role: crate::launch_protocol::LaunchRole::Agent,
            conformance: crate::launch_protocol::ConformanceAuthorization::default(),
            dependencies: None,
            skill_requests: None,
            beads_mutations: None,
            provider_requests: None,
            require_cold_recovery: false,
            authorization_id: "authorization".into(),
            request_id: "request".into(),
            request_digest: crate::Digest::of(b"request").to_string(),
            controller_uid: 1000,
            session_id: "session".into(),
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
                uses,
                allow_delegation: true,
                expires_at_ms: 9000,
            }),
        }
    }

    fn entry(decision: AuditDecision) -> AuditEntry {
        AuditEntry {
            at_ms: 1000,
            session_id: "session".into(),
            run_id: "run".into(),
            authorization_id: "authorization".into(),
            identity_slot: 0,
            decision,
        }
    }

    fn direct(sequence: u64) -> AuditEntry {
        entry(AuditDecision::EffectCommitIntent {
            grant: None,
            sequence,
        })
    }

    #[test]
    fn uncertain_and_finished_uses_stay_spent_and_sequences_continue() {
        let audit = [
            entry(AuditDecision::AuthorizationConsumed),
            direct(1),
            entry(AuditDecision::EffectFinished {
                grant: None,
                sequence: 1,
                succeeded: true,
            }),
            direct(2),
            entry(AuditDecision::EffectOutcomeUnknown { sequence: 2 }),
            entry(AuditDecision::ToolGranted {
                grant: 1,
                pid: 42,
                revision: 1,
                uses: Some(2),
            }),
            entry(AuditDecision::EffectCommitIntent {
                grant: Some(1),
                sequence: 1,
            }),
        ];
        let replay = replay(&source(Some(6)), &audit, 2000).unwrap();
        assert_eq!(replay.commands.uses, Some(2), "6 - 2 direct - 2 reserved");
        assert_eq!(replay.commands.expires_at_ms, 9000, "original expiry");
        assert_eq!(
            (
                replay.principal_sequence,
                replay.dispatch_sequence,
                replay.grant_sequence,
                replay.revoked
            ),
            (2, 3, 1, false)
        );
    }

    #[test]
    fn uncapped_policy_still_yields_sequences() {
        let audit = [
            entry(AuditDecision::AuthorizationConsumed),
            direct(1),
            direct(2),
        ];
        let replay = replay(&source(None), &audit, 2000).unwrap();
        assert_eq!(replay.commands.uses, None);
        assert_eq!(
            (replay.principal_sequence, replay.dispatch_sequence),
            (2, 2)
        );
    }

    #[test]
    fn revocation_is_reported_not_hidden() {
        for decision in [
            AuditDecision::CapabilitiesRevocationRequested,
            AuditDecision::CapabilitiesRevoked,
        ] {
            let audit = [entry(AuditDecision::AuthorizationConsumed), entry(decision)];
            assert!(replay(&source(Some(2)), &audit, 2000).unwrap().revoked);
        }
    }

    #[test]
    fn refusals_are_typed_and_never_refund() {
        let consumed = entry(AuditDecision::AuthorizationConsumed);
        let mut none = source(Some(2));
        none.commands = None;
        assert_eq!(
            replay(&none, std::slice::from_ref(&consumed), 2000),
            Err(ReplayRefusal::NotApproved)
        );
        assert_eq!(
            replay(&source(Some(2)), std::slice::from_ref(&consumed), 9000),
            Err(ReplayRefusal::Expired)
        );
        assert_eq!(
            replay(&source(Some(2)), &[], 2000),
            Err(ReplayRefusal::NotConsumed)
        );
        let mut foreign = direct(1);
        foreign.authorization_id = "other".into();
        for audit in [
            vec![consumed.clone(), consumed.clone()],
            vec![consumed.clone(), direct(2)],
            vec![consumed.clone(), foreign],
            vec![direct(1)],
            vec![consumed.clone(), direct(1), direct(2), direct(3)],
            vec![
                consumed.clone(),
                entry(AuditDecision::ToolGranted {
                    grant: 2,
                    pid: 42,
                    revision: 1,
                    uses: Some(1),
                }),
            ],
        ] {
            assert_eq!(
                replay(&source(Some(2)), &audit, 2000),
                Err(ReplayRefusal::Contradictory),
                "{audit:?}"
            );
        }
        let spent = [consumed, direct(1), direct(2)];
        assert_eq!(
            replay(&source(Some(2)), &spent, 2000)
                .unwrap()
                .commands
                .uses,
            Some(0)
        );
    }

    #[test]
    fn other_sessions_do_not_affect_the_replay() {
        let mut other = direct(1);
        other.session_id = "other-session".into();
        let audit = [entry(AuditDecision::AuthorizationConsumed), other];
        let replay = replay(&source(Some(2)), &audit, 2000).unwrap();
        assert_eq!(
            (replay.commands.uses, replay.dispatch_sequence),
            (Some(2), 0)
        );
    }
}
