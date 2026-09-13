//! Attenuation and non-refundable reservation; no process or transport mechanics.

use super::{
    AuditDecision, CommandAuthority, CommandOperation, CommandPrincipal, DelegationError, Instant,
};
use crate::{
    broker::delegation::{CommandScope, uses_within},
    launch_protocol::GrantRequest,
};
use std::time::Duration;

pub(super) struct Grant {
    pub(super) principal: CommandPrincipal,
    pub(super) scope: CommandScope,
    pub(super) expires_at: Instant,
    pub(super) remaining: Option<u32>,
    pub(super) sequence: u64,
    pub(super) revocation: Option<(String, bool)>,
}

impl CommandAuthority {
    pub(super) fn is_agent(&self, principal: &CommandPrincipal) -> bool {
        principal.channel_id == self.binding.channel_id
            && principal.pid == self.binding.agent_pid
            && principal.uid == self.binding.assigned_uid
            && principal.gid == self.binding.assigned_gid
    }

    pub(super) fn delegate(
        &mut self,
        principal: &CommandPrincipal,
        tool: &CommandPrincipal,
        request: &GrantRequest,
    ) -> Result<CommandOperation, DelegationError> {
        if !self.active {
            return self.deny(DelegationError::Revoked);
        }
        if !self.is_agent(principal) {
            return self.deny(DelegationError::IdentityMismatch);
        }
        if !self.policy.allow_delegation {
            return self.deny(DelegationError::DelegationDenied);
        }
        if self.grant_sequence.checked_add(1) != Some(request.sequence) {
            return self.deny(DelegationError::Replay);
        }
        let scope = CommandScope {
            command_digest: crate::Digest::parse(&request.command_digest)
                .map_err(|_| DelegationError::InvalidRequest)?,
            timeout_ms: request.timeout_ms,
            uses: request.uses,
        };
        let now = Instant::now();
        let expires_at = now
            .checked_add(Duration::from_millis(u64::from(request.valid_for_ms)))
            .ok_or(DelegationError::Expired)?;
        if now >= self.policy.expires_at {
            return self.deny(DelegationError::Expired);
        }
        if !scope.is_within(&self.policy.scope) || expires_at > self.policy.expires_at {
            return self.deny(DelegationError::ScopeMismatch);
        }
        if tool.uid != self.binding.assigned_uid
            || tool.gid != self.binding.assigned_gid
            || tool.pid == self.binding.agent_pid
            || tool.channel_id == self.binding.channel_id
            || self.grants.values().any(|grant| {
                grant.principal.pid == tool.pid || grant.principal.channel_id == tool.channel_id
            })
        {
            return self.deny(DelegationError::IdentityMismatch);
        }
        if !uses_within(scope.uses, self.remaining) {
            return self.deny(DelegationError::BudgetExhausted);
        }
        // Reserve before durability. Neither audit failure nor a lost reply can
        // make this authority available again; restart refuses this lifetime.
        if let (Some(remaining), Some(uses)) = (&mut self.remaining, scope.uses) {
            *remaining -= uses;
        }
        self.grant_sequence = request.sequence;
        self.grants.insert(
            request.sequence,
            Grant {
                principal: tool.clone(),
                remaining: scope.uses,
                scope,
                expires_at,
                sequence: 0,
                revocation: None,
            },
        );
        self.record(AuditDecision::ToolGranted {
            grant: request.sequence,
            pid: tool.pid,
            revision: self.binding.envelope_revision,
            uses: request.uses,
        })?;
        let valid_for_ms = u32::try_from(
            expires_at
                .saturating_duration_since(Instant::now())
                .as_millis(),
        )
        .map_err(|_| DelegationError::Expired)?;
        if valid_for_ms == 0 {
            return Err(DelegationError::Expired);
        }
        Ok(CommandOperation::Granted {
            tool: tool.clone(),
            grant: request.sequence,
            valid_for_ms,
        })
    }

    /// Stops approvals for one grant before requesting supervisor enforcement.
    /// Other grants and the Agent retain their independently reserved authority.
    ///
    /// # Errors
    /// Refuses unknown/already-revoked grants, malformed IDs or audit failure.
    /// Audit failure closes the whole owner; the caller must close its transport.
    pub fn revoke_grant(
        &mut self,
        request_id: &str,
        grant: u64,
    ) -> Result<crate::launch_protocol::CommandMessage, DelegationError> {
        if !super::is_record_identifier(request_id) {
            return Err(DelegationError::InvalidRequest);
        }
        let target = self
            .grants
            .get_mut(&grant)
            .ok_or(DelegationError::InvalidRequest)?;
        if target.revocation.is_some() {
            return Err(DelegationError::Replay);
        }
        target.revocation = Some((request_id.to_owned(), false));
        self.record(AuditDecision::ToolRevocationRequested { grant })?;
        Ok(self.message(request_id, CommandOperation::RevokeGrant { grant }))
    }

    /// True only after the named grant's authenticated enforcement was audited.
    #[must_use]
    pub fn grant_revocation_complete(&self, grant: u64) -> bool {
        self.grants
            .get(&grant)
            .is_some_and(|grant| grant.revocation.as_ref().is_some_and(|(_, done)| *done))
    }

    pub(super) fn record_grant_revocation(
        &mut self,
        request_id: &str,
        grant: u64,
        enforced: bool,
    ) -> Result<(), DelegationError> {
        if !self.grants.get(&grant).is_some_and(|grant| {
            grant
                .revocation
                .as_ref()
                .is_some_and(|(id, done)| id == request_id && !done)
        }) {
            return Err(DelegationError::Replay);
        }
        if !enforced {
            return Err(DelegationError::EffectFailed);
        }
        self.record(AuditDecision::ToolRevoked { grant })?;
        if let Some(target) = self.grants.get_mut(&grant) {
            target.revocation = Some((request_id.to_owned(), true));
        }
        Ok(())
    }
}
