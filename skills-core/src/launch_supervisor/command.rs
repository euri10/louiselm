//! Supervisor-local lifetime, replay and final-start enforcement; no policy selection.

use super::{CapabilityBinding, SupervisorError};
use crate::{
    Digest,
    launch_protocol::{CommandMessage, CommandOperation, CommandPrincipal, ToolExecutionRequest},
    launch_transport::KernelProcess,
};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[cfg(test)]
#[path = "command_tests.rs"]
mod tests;

struct State {
    active: bool,
    sequence: u64,
    grants: BTreeMap<u64, LocalGrant>,
}

#[derive(Clone)]
struct LocalGrant {
    principal: CommandPrincipal,
    process: Arc<KernelProcess>,
    deadline: Instant,
    active: bool,
}

/// Supervisor-owned enforcement for one already-authenticated Agent lifetime.
/// Policy remains broker-owned; this object verifies exact decisions and owns
/// the local lock ordering revocation against the irreversible start boundary.
pub struct CommandEnforcer {
    binding: CapabilityBinding,
    agent: Arc<KernelProcess>,
    state: Arc<Mutex<State>>,
}

/// Single-use supervisor permit. Preparation is not permission to start.
///
/// The executor must call [`Self::start`] directly around the actual startup
/// gate release, then keep checking [`Self::valid`] until cleanup completes.
pub struct CommandPermit {
    request: ToolExecutionRequest,
    dispatch_sequence: u64,
    state: Arc<Mutex<State>>,
    agent: Arc<KernelProcess>,
    deadline: Instant,
    started: Cell<bool>,
    grant: Option<u64>,
}

impl CommandEnforcer {
    pub(crate) fn publish_cache<T>(
        &self,
        deadline: Instant,
        publish: impl FnOnce() -> Result<T, SupervisorError>,
    ) -> Result<T, SupervisorError> {
        let state = self
            .state
            .lock()
            .map_err(|_| SupervisorError::AuthorizationRejected)?;
        if !state.active
            || Instant::now() >= deadline
            || !self
                .agent
                .valid()
                .map_err(|_| SupervisorError::AgentIdentityRejected)?
        {
            return Err(SupervisorError::AuthorizationRejected);
        }
        let result = publish();
        drop(state);
        result
    }
    /// Creates a fresh command generation for the same pinned Agent only after
    /// durable operator Resume. Old permits/grants keep their revoked state;
    /// the dispatch floor carries forward so consumed decisions cannot replay.
    pub(crate) fn resume_agent(&self) -> Result<Self, SupervisorError> {
        let previous = self
            .state
            .lock()
            .map_err(|_| SupervisorError::AuthorizationRejected)?;
        if previous.active
            || !self
                .agent
                .valid()
                .map_err(|_| SupervisorError::AgentIdentityRejected)?
        {
            return Err(SupervisorError::AuthorizationRejected);
        }
        let mut grants = previous.grants.clone();
        for grant in grants.values_mut() {
            grant.active = false;
        }
        Ok(Self {
            binding: self.binding.clone(),
            agent: Arc::clone(&self.agent),
            state: Arc::new(Mutex::new(State {
                active: true,
                sequence: previous.sequence,
                grants,
            })),
        })
    }

    pub(crate) fn agent_valid(&self) -> Result<bool, SupervisorError> {
        Ok(self
            .state
            .lock()
            .map_err(|_| SupervisorError::AuthorizationRejected)?
            .active
            && self
                .agent
                .valid()
                .map_err(|_| SupervisorError::AgentIdentityRejected)?)
    }
    /// Binds an actual supervisor-retained kernel pin, never a peer PID.
    ///
    /// # Errors
    /// Refuses mismatched credentials or an unavailable/dead process lifetime.
    pub fn new(
        binding: CapabilityBinding,
        agent: Arc<KernelProcess>,
    ) -> Result<Self, SupervisorError> {
        let credentials = agent.credentials();
        if binding.agent_pid != credentials.pid
            || binding.assigned_uid != credentials.uid
            || binding.assigned_gid != credentials.gid
            || !agent
                .valid()
                .map_err(|_| SupervisorError::AgentIdentityRejected)?
        {
            return Err(SupervisorError::AgentIdentityRejected);
        }
        Ok(Self {
            binding,
            agent,
            state: Arc::new(Mutex::new(State {
                active: true,
                sequence: 0,
                grants: BTreeMap::new(),
            })),
        })
    }

    /// Checks a correlated broker decision against the original authenticated request.
    ///
    /// `forwarded_at` is captured before requesting authorization. Applying the
    /// brokers remaining lifetime to that earlier instant is conservative: time
    /// in either transport direction cannot extend the approved lifetime.
    ///
    /// # Errors
    /// Refuses stale/replayed, contradictory, expired or revoked decisions.
    pub fn admit(
        &self,
        request: &ToolExecutionRequest,
        principal: &CommandPrincipal,
        decision: &CommandMessage,
        forwarded_at: Instant,
    ) -> Result<CommandPermit, SupervisorError> {
        request
            .validate()
            .map_err(|_| SupervisorError::AuthorizationRejected)?;
        decision
            .validate()
            .map_err(|_| SupervisorError::AuthorizationRejected)?;
        let CommandOperation::Authorize {
            principal: approved,
            principal_sequence,
            dispatch_sequence,
            command_digest,
            timeout_ms,
            valid_for_ms,
        } = &decision.operation
        else {
            return Err(SupervisorError::AuthorizationRejected);
        };
        if principal != approved
            || decision.request_id != request.request_id
            || decision.session_id != self.binding.session_id
            || decision.run_id != self.binding.run_id
            || decision.envelope_revision != self.binding.envelope_revision
            || request.session_id != decision.session_id
            || request.run_id != decision.run_id
            || request.envelope_revision != decision.envelope_revision
            || request.sequence != *principal_sequence
            || command_digest != &Digest::of(request.command.as_bytes()).to_string()
            || request.timeout_ms != *timeout_ms
        {
            return Err(SupervisorError::AuthorizationRejected);
        }
        let mut deadline = forwarded_at
            .checked_add(Duration::from_millis(u64::from(*valid_for_ms)))
            .ok_or(SupervisorError::AuthorizationRejected)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| SupervisorError::AuthorizationRejected)?;
        let grant = if principal.channel_id == self.binding.channel_id
            && principal.pid == self.binding.agent_pid
            && principal.uid == self.binding.assigned_uid
            && principal.gid == self.binding.assigned_gid
        {
            None
        } else {
            let (id, grant) = state
                .grants
                .iter()
                .find(|(_, grant)| grant.principal == *principal)
                .ok_or(SupervisorError::AuthorizationRejected)?;
            if !grant.active
                || !grant
                    .process
                    .valid()
                    .map_err(|_| SupervisorError::AgentIdentityRejected)?
            {
                return Err(SupervisorError::AuthorizationRejected);
            }
            deadline = deadline.min(grant.deadline);
            Some(*id)
        };
        if !state.active
            || *dispatch_sequence <= state.sequence
            || Instant::now() >= deadline
            || !self
                .agent
                .valid()
                .map_err(|_| SupervisorError::AgentIdentityRejected)?
        {
            return Err(SupervisorError::AuthorizationRejected);
        }
        // Allow gaps: earlier broker authorizations may have been spent without
        // reaching this supervisor. Never allow reuse or reversal of a sequence.
        state.sequence = *dispatch_sequence;
        Ok(CommandPermit {
            request: request.clone(),
            dispatch_sequence: *dispatch_sequence,
            state: Arc::clone(&self.state),
            agent: Arc::clone(&self.agent),
            deadline,
            started: Cell::new(false),
            grant,
        })
    }

    /// Prevents every queued start before the caller cancels and joins execution.
    /// This alone is NOT proof that running commands have terminated.
    ///
    /// # Errors
    /// Returns owner failure on poisoned enforcement state; callers must dispose.
    pub fn revoke(&self) -> Result<(), SupervisorError> {
        self.state
            .lock()
            .map_err(|_| SupervisorError::CleanupUnproven)?
            .active = false;
        Ok(())
    }

    /// Installs one broker-approved grant against an independently launched tool pin.
    /// The caller must supply its own correlated decision and isolated kernel proof.
    ///
    /// # Errors
    /// Refuses replay, inconsistent attribution, expiry or dead Agent/tool lifetimes.
    pub fn register_grant(
        &self,
        process: Arc<KernelProcess>,
        decision: &CommandMessage,
        forwarded_at: Instant,
    ) -> Result<Instant, SupervisorError> {
        decision
            .validate()
            .map_err(|_| SupervisorError::AuthorizationRejected)?;
        let CommandOperation::Granted {
            tool,
            grant,
            valid_for_ms,
        } = &decision.operation
        else {
            return Err(SupervisorError::AuthorizationRejected);
        };
        let credentials = process.credentials();
        if decision.session_id != self.binding.session_id
            || decision.run_id != self.binding.run_id
            || decision.envelope_revision != self.binding.envelope_revision
            || tool.pid != credentials.pid
            || tool.uid != credentials.uid
            || tool.gid != credentials.gid
            || tool.pid == self.binding.agent_pid
            || tool.channel_id == self.binding.channel_id
            || tool.uid != self.binding.assigned_uid
            || tool.gid != self.binding.assigned_gid
        {
            return Err(SupervisorError::AuthorizationRejected);
        }
        let deadline = forwarded_at
            .checked_add(Duration::from_millis(u64::from(*valid_for_ms)))
            .ok_or(SupervisorError::AuthorizationRejected)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| SupervisorError::AuthorizationRejected)?;
        if !state.active
            || Instant::now() >= deadline
            || state.grants.contains_key(grant)
            || state.grants.values().any(|existing| {
                existing.principal.pid == tool.pid
                    || existing.principal.channel_id == tool.channel_id
            })
            || !self
                .agent
                .valid()
                .map_err(|_| SupervisorError::AgentIdentityRejected)?
            || !process
                .valid()
                .map_err(|_| SupervisorError::AgentIdentityRejected)?
        {
            return Err(SupervisorError::AuthorizationRejected);
        }
        state.grants.insert(
            *grant,
            LocalGrant {
                principal: tool.clone(),
                process,
                deadline,
                active: true,
            },
        );
        Ok(deadline)
    }

    /// Blocks queued starts for one grant. Running tree cleanup is a separate obligation.
    ///
    /// # Errors
    /// Refuses unknown grants or poisoned enforcement state.
    pub fn revoke_grant(&self, grant: u64) -> Result<(), SupervisorError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| SupervisorError::CleanupUnproven)?;
        state
            .grants
            .get_mut(&grant)
            .ok_or(SupervisorError::AuthorizationRejected)?
            .active = false;
        Ok(())
    }
}

impl Drop for CommandEnforcer {
    fn drop(&mut self) {
        // A poisoned mutex remains fail-closed in every permit. No identity is released.
        let _ = self.revoke();
    }
}

impl CommandPermit {
    /// Delegated grant, or `None` for the Agent's own authority.
    #[must_use]
    pub const fn grant(&self) -> Option<u64> {
        self.grant
    }

    /// Exact immutable command approved by the broker.
    #[must_use]
    pub const fn request(&self) -> &ToolExecutionRequest {
        &self.request
    }

    /// Session-wide dispatch number, distinct from the principal request number.
    #[must_use]
    pub const fn dispatch_sequence(&self) -> u64 {
        self.dispatch_sequence
    }

    /// Revalidates this exact lifetime and deadline, including after start.
    ///
    /// # Errors
    /// Reports unavailable kernel or local-owner evidence rather than guessing.
    pub fn valid(&self) -> Result<bool, SupervisorError> {
        let state = self
            .state
            .lock()
            .map_err(|_| SupervisorError::AuthorizationRejected)?;
        self.valid_locked(&state)
    }

    fn valid_locked(&self, state: &State) -> Result<bool, SupervisorError> {
        let tool_valid = if let Some(id) = self.grant {
            let Some(grant) = state.grants.get(&id) else {
                return Ok(false);
            };
            grant.active
                && Instant::now() < grant.deadline
                && grant
                    .process
                    .valid()
                    .map_err(|_| SupervisorError::AgentIdentityRejected)?
        } else {
            true
        };
        Ok(state.active
            && tool_valid
            && Instant::now() < self.deadline
            && self
                .agent
                .valid()
                .map_err(|_| SupervisorError::AgentIdentityRejected)?)
    }

    /// Runs the short irreversible startup action once under the revocation lock.
    /// The closure must release the actual process gate, not enqueue work or wait
    /// for command completion. It must not call back into this enforcement owner.
    ///
    /// # Errors
    /// Refuses replay, revocation, expiry or lost identity; preserves startup errors.
    pub fn start<T>(
        &self,
        start: impl FnOnce() -> Result<T, SupervisorError>,
    ) -> Result<T, SupervisorError> {
        let state = self
            .state
            .lock()
            .map_err(|_| SupervisorError::AuthorizationRejected)?;
        if self.started.get() || !self.valid_locked(&state)? {
            return Err(SupervisorError::AuthorizationRejected);
        }
        self.started.set(true);
        let result = start();
        drop(state);
        result
    }
}
