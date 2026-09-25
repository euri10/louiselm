//! Broker-owned Agent and delegated command decisions. Kernel pins stay in the supervisor.

#[path = "command_grants.rs"]
mod grants;

use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use super::{
    AuditDecision, AuditEntry, AuditLog,
    delegation::{DelegationError, DelegationPolicy},
    is_record_identifier,
};
use crate::{
    launch_protocol::{
        COMMAND_SCHEMA, CommandMessage, CommandOperation, CommandOutcome, CommandPrincipal,
        PROTOCOL_VERSION,
    },
    launch_supervisor::CapabilityBinding,
};

struct SpentCommand {
    request_id: String,
    principal_sequence: u64,
    outcome: OutcomeState,
    grant: Option<u64>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OutcomeState {
    Pending,
    Unknown,
    Finished,
}

/// One exclusive broker policy owner for an authenticated Agent lifetime.
///
/// Construct only from trusted launch context and supervisor-authenticated
/// attribution. This type never accepts Agent sockets or reconstructs a process
/// from a PID. The supervisor independently checks the actual sender at request
/// and start, and stops running commands on revocation or the local deadline.
/// Methods perform durable audit I/O and belong on the broker worker.
pub struct CommandAuthority {
    binding: CapabilityBinding,
    policy: DelegationPolicy,
    audit: Arc<AuditLog>,
    remaining: Option<u32>,
    principal_sequence: u64,
    dispatch_sequence: u64,
    spent: BTreeMap<u64, SpentCommand>,
    active: bool,
    revocation: Option<(String, bool)>,
    grants: BTreeMap<u64, grants::Grant>,
    grant_sequence: u64,
}

impl CommandAuthority {
    /// Binds the existing approved command scope and budget, not peer policy.
    ///
    /// # Errors
    /// Refuses invalid context, expired policy or previously spent lifetime state.
    pub fn new(
        binding: CapabilityBinding,
        policy: DelegationPolicy,
        audit: Arc<AuditLog>,
    ) -> Result<Self, DelegationError> {
        if [
            &binding.session_id,
            &binding.run_id,
            &binding.channel_id,
            &policy.authorization_id,
        ]
        .into_iter()
        .any(|id| !is_record_identifier(id))
            || binding.agent_pid == 0
            || !policy.scope.valid()
        {
            return Err(DelegationError::InvalidRequest);
        }
        if Instant::now() >= policy.expires_at {
            return Err(DelegationError::Expired);
        }
        // A restart cannot restore spent budget from an old launch. Recovery is
        // a later lifecycle contract; require a fresh authorized lifetime here.
        if audit
            .find(|entry| {
                entry.session_id == binding.session_id
                    && matches!(
                        entry.decision,
                        AuditDecision::EffectCommitIntent { .. }
                            | AuditDecision::ToolGranted { .. }
                            | AuditDecision::CapabilitiesRevocationRequested
                            | AuditDecision::CapabilitiesRevoked
                    )
            })?
            .is_some()
        {
            return Err(DelegationError::Revoked);
        }
        let remaining = policy.scope.uses;
        Ok(Self {
            binding,
            policy,
            audit,
            remaining,
            principal_sequence: 0,
            dispatch_sequence: 0,
            spent: BTreeMap::new(),
            active: true,
            revocation: None,
            grants: BTreeMap::new(),
            grant_sequence: 0,
        })
    }

    /// Exact immutable context; it conveys no kernel authority.
    #[must_use]
    pub const fn binding(&self) -> &CapabilityBinding {
        &self.binding
    }

    /// Handles one record authenticated by the owning supervisor transport.
    ///
    /// Requests spend budget before a decision leaves this owner. Late outcomes
    /// remain recordable after revocation; uncertainty never refunds a use.
    ///
    /// # Errors
    /// Returns closed-protocol, identity, policy, replay, budget or durable-audit refusal.
    pub fn handle(&mut self, message: &CommandMessage) -> Result<CommandMessage, DelegationError> {
        message
            .validate()
            .map_err(|_| DelegationError::InvalidRequest)?;
        if message.session_id != self.binding.session_id
            || message.run_id != self.binding.run_id
            || message.envelope_revision != self.binding.envelope_revision
        {
            return self.deny(DelegationError::ScopeMismatch);
        }
        let operation = match &message.operation {
            CommandOperation::DelegationRequest {
                principal,
                tool,
                grant,
            } => self.delegate(principal, tool, grant)?,
            CommandOperation::GrantRevoked { grant, enforced } => {
                self.record_grant_revocation(&message.request_id, *grant, *enforced)?;
                CommandOperation::GrantRevoked {
                    grant: *grant,
                    enforced: true,
                }
            }
            CommandOperation::Request { principal, command } => {
                self.authorize(principal, command)?
            }
            CommandOperation::Outcome {
                dispatch_sequence,
                outcome,
            } => {
                self.record_outcome(&message.request_id, *dispatch_sequence, outcome)?;
                CommandOperation::OutcomeAcknowledged {
                    dispatch_sequence: *dispatch_sequence,
                }
            }
            CommandOperation::Revoked { enforced } => {
                if !self
                    .revocation
                    .as_ref()
                    .is_some_and(|(id, done)| id == &message.request_id && !done)
                {
                    return Err(DelegationError::Replay);
                }
                if !enforced {
                    return Err(DelegationError::EffectFailed);
                }
                self.record(AuditDecision::CapabilitiesRevoked)?;
                self.revocation = Some((message.request_id.clone(), true));
                CommandOperation::Revoked { enforced: true }
            }
            _ => return self.deny(DelegationError::InvalidRequest),
        };
        Ok(self.message(&message.request_id, operation))
    }

    /// Stops approvals before asking the supervisor to enforce cancellation.
    ///
    /// # Errors
    /// Refuses malformed/repeated IDs or audit failure. Authority remains closed
    /// even when audit fails; the caller must close the supervisor transport.
    pub fn revoke(&mut self, request_id: &str) -> Result<CommandMessage, DelegationError> {
        if !is_record_identifier(request_id) || self.revocation.is_some() {
            return Err(DelegationError::InvalidRequest);
        }
        self.active = false;
        self.revocation = Some((request_id.to_owned(), false));
        self.record(AuditDecision::CapabilitiesRevocationRequested)?;
        Ok(self.message(request_id, CommandOperation::Revoke))
    }

    /// True once revocation was requested, whether or not it is enforced yet.
    #[must_use]
    pub fn revocation_requested(&self) -> bool {
        self.revocation.is_some()
    }

    /// True only after authenticated supervisor enforcement and durable audit.
    #[must_use]
    pub fn revocation_complete(&self) -> bool {
        self.revocation.as_ref().is_some_and(|(_, done)| *done)
    }

    fn authorize(
        &mut self,
        principal: &CommandPrincipal,
        command: &crate::launch_protocol::ToolExecutionRequest,
    ) -> Result<CommandOperation, DelegationError> {
        if !self.active {
            return self.deny(DelegationError::Revoked);
        }
        if Instant::now() >= self.policy.expires_at {
            self.active = false;
            return self.deny(DelegationError::Expired);
        }
        let grant = if self.is_agent(principal) {
            None
        } else if let Some((id, _)) = self
            .grants
            .iter()
            .find(|(_, grant)| grant.principal == *principal)
        {
            Some(*id)
        } else {
            return self.deny(DelegationError::IdentityMismatch);
        };
        let (scope, expires_at, sequence, remaining) = if let Some(id) = grant {
            let granted = &self.grants[&id];
            if granted.revocation.is_some() {
                return self.deny(DelegationError::Revoked);
            }
            (
                &granted.scope,
                granted.expires_at,
                granted.sequence,
                granted.remaining,
            )
        } else {
            (
                &self.policy.scope,
                self.policy.expires_at,
                self.principal_sequence,
                self.remaining,
            )
        };
        if Instant::now() >= expires_at {
            return self.deny(DelegationError::Expired);
        }
        if !scope.permits(command) {
            return self.deny(DelegationError::ScopeMismatch);
        }
        if sequence.checked_add(1) != Some(command.sequence)
            || self
                .spent
                .values()
                .any(|spent| spent.request_id == command.request_id)
        {
            return self.deny(DelegationError::Replay);
        }
        if remaining == Some(0) {
            return self.deny(DelegationError::BudgetExhausted);
        }
        let dispatch_sequence = self
            .dispatch_sequence
            .checked_add(1)
            .ok_or(DelegationError::Replay)?;
        if let Some(id) = grant {
            let granted = self
                .grants
                .get_mut(&id)
                .ok_or(DelegationError::OwnerUnavailable)?;
            granted.remaining = granted.remaining.map(|uses| uses - 1);
            granted.sequence = command.sequence;
        } else {
            self.remaining = self.remaining.map(|uses| uses - 1);
            self.principal_sequence = command.sequence;
        }
        self.dispatch_sequence = dispatch_sequence;
        self.spent.insert(
            dispatch_sequence,
            SpentCommand {
                request_id: command.request_id.clone(),
                principal_sequence: command.sequence,
                outcome: OutcomeState::Pending,
                grant,
            },
        );
        self.record(AuditDecision::EffectCommitIntent {
            grant,
            sequence: command.sequence,
        })?;
        let remaining_ms = expires_at
            .saturating_duration_since(Instant::now())
            .as_millis();
        let valid_for_ms =
            u32::try_from(remaining_ms.min(30_000)).map_err(|_| DelegationError::Expired)?;
        if valid_for_ms == 0 {
            return Err(DelegationError::Expired);
        }
        Ok(CommandOperation::Authorize {
            principal: principal.clone(),
            principal_sequence: command.sequence,
            dispatch_sequence,
            command_digest: self.policy.scope.command_digest.to_string(),
            timeout_ms: command.timeout_ms,
            valid_for_ms,
        })
    }

    fn record_outcome(
        &mut self,
        request_id: &str,
        sequence: u64,
        outcome: &CommandOutcome,
    ) -> Result<(), DelegationError> {
        let spent = self.spent.get(&sequence).ok_or(DelegationError::Replay)?;
        let finished = !matches!(outcome, CommandOutcome::Unknown);
        if spent.request_id != request_id
            || spent.outcome == OutcomeState::Finished
            || (!finished && spent.outcome == OutcomeState::Unknown)
        {
            return Err(DelegationError::Replay);
        }
        let decision = if finished {
            AuditDecision::EffectFinished {
                grant: spent.grant,
                sequence: spent.principal_sequence,
                succeeded: matches!(outcome, CommandOutcome::Completed { .. }),
            }
        } else {
            AuditDecision::EffectOutcomeUnknown { sequence }
        };
        self.record(decision)?;
        if let Some(spent) = self.spent.get_mut(&sequence) {
            spent.outcome = if finished {
                OutcomeState::Finished
            } else {
                OutcomeState::Unknown
            };
        }
        Ok(())
    }

    fn message(&self, request_id: &str, operation: CommandOperation) -> CommandMessage {
        CommandMessage {
            schema: COMMAND_SCHEMA.to_owned(),
            protocol_version: PROTOCOL_VERSION,
            request_id: request_id.to_owned(),
            session_id: self.binding.session_id.clone(),
            run_id: self.binding.run_id.clone(),
            envelope_revision: self.binding.envelope_revision,
            operation,
        }
    }

    fn record(&mut self, decision: AuditDecision) -> Result<(), DelegationError> {
        let Some(at_ms) = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|time| u64::try_from(time.as_millis()).ok())
        else {
            self.active = false;
            return Err(DelegationError::OwnerUnavailable);
        };
        self.audit
            .record(&AuditEntry {
                at_ms,
                session_id: self.binding.session_id.clone(),
                run_id: self.binding.run_id.clone(),
                authorization_id: self.policy.authorization_id.clone(),
                identity_slot: self.binding.identity_slot,
                decision,
            })
            .map_err(|error| {
                self.active = false;
                DelegationError::Audit(error)
            })
    }

    fn deny<T>(&mut self, error: DelegationError) -> Result<T, DelegationError> {
        self.record(AuditDecision::CapabilityDenied)?;
        Err(error)
    }
}
