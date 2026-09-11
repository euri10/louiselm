//! Session-owned grant accounting and the final effect commitment boundary.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, MutexGuard},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use super::{
    BoundProcess, CommandScope, DelegationError, DelegationPolicy, EffectCompletion, ToolEffect,
    ToolGrantRequest,
};
use crate::{
    broker::{AuditDecision, AuditEntry, AuditLog, is_record_identifier},
    launch_protocol::{ProtocolMessage, ToolExecutionRequest, ToolExecutionResult},
    launch_supervisor::CapabilityBinding,
    launch_transport::{AuthenticatedPacket, KernelCredentials, LauncherPacket},
};

struct Grant {
    target: BoundProcess,
    scope: CommandScope,
    expires_at: Instant,
    remaining: u32,
    sequence: u64,
}

struct State {
    binding: CapabilityBinding,
    policy: DelegationPolicy,
    agent: BoundProcess,
    grants: BTreeMap<u64, Grant>,
    remaining: u32,
    agent_sequence: u64,
    grant_sequence: u64,
    active: bool,
    audit: Arc<AuditLog>,
}

/// Exclusive owner of one Agent lifetime's command authority and tool grants.
///
/// Drop and [`Self::revoke`] close every owned channel. The launch owner calls
/// `revoke` on the existing Agent-exit, revision-change or policy-revocation
/// event before cleanup. Each admission and commitment also checks the kernel
/// lifetime, so delayed event delivery cannot authorize another effect.
pub struct ToolDelegation {
    state: Arc<Mutex<State>>,
}

/// Opaque authority for exactly one authenticated isolated tool/channel pair.
///
/// Cloning a handle shares its budget and replay state. It cannot create another
/// grant, return reserved budget, or bypass per-message sender authentication.
#[derive(Clone)]
pub struct DelegatedTool {
    state: Arc<Mutex<State>>,
    grant: u64,
}

/// Single-use admitted effect, still subject to a final authority check.
///
/// Dropping it spends its reservation. It exposes neither a reusable token nor
/// an authorized command that an asynchronous adapter could retain unchecked.
pub struct PendingEffect {
    state: Arc<Mutex<State>>,
    grant: Option<u64>,
    request: ToolExecutionRequest,
    sender: KernelCredentials,
}

/// Attribution retained after an effect starts, without retaining authority.
///
/// The adapter owns completion and cleanup of the started operation and calls
/// [`Self::finish`] exactly once, including when authority was revoked meanwhile.
pub struct CommittedEffect {
    state: Arc<Mutex<State>>,
    grant: Option<u64>,
    sequence: u64,
}

impl ToolDelegation {
    /// Binds already approved command policy to the exact authenticated Agent.
    ///
    /// Runs on an I/O worker: kernel observation and audit storage can block.
    /// No peer-provided policy, ancestry-based pin or unproved tool boundary is
    /// accepted as a substitute for the launch owner's verified inputs.
    ///
    /// # Errors
    /// Refuses invalid context, exhausted/expired policy, or mismatched/lost identity.
    pub fn new(
        binding: CapabilityBinding,
        policy: DelegationPolicy,
        agent: BoundProcess,
        audit: Arc<AuditLog>,
    ) -> Result<Self, DelegationError> {
        let credentials = agent.process.credentials();
        if [
            &binding.session_id,
            &binding.run_id,
            &binding.channel_id,
            &policy.authorization_id,
        ]
        .into_iter()
        .any(|id| !is_record_identifier(id))
            || !policy.scope.valid()
            || binding.agent_pid != credentials.pid
            || binding.assigned_uid != credentials.uid
            || binding.assigned_gid != credentials.gid
        {
            return Err(DelegationError::InvalidRequest);
        }
        check_process(&agent, credentials)?;
        if Instant::now() >= policy.expires_at {
            return Err(DelegationError::Expired);
        }
        let remaining = policy.scope.uses;
        Ok(Self {
            state: Arc::new(Mutex::new(State {
                binding,
                policy,
                agent,
                grants: BTreeMap::new(),
                remaining,
                agent_sequence: 0,
                grant_sequence: 0,
                active: true,
                audit,
            })),
        })
    }

    /// Reserves a narrower grant requested by the authenticated Agent.
    ///
    /// `sender` must come from the Agent channel's kernel-authenticated packet,
    /// never a serialized PID. `target` comes from the supervisor's isolated
    /// tool launch proof. A failed request closes the offered target channel.
    /// Reservations are never refunded, preventing repeated delegation from
    /// amplifying the operator's total invocation budget.
    ///
    /// # Errors
    /// Refuses other senders, missing delegation approval, replay, widening,
    /// expiry, stale subjects/revisions, exhausted budget or unavailable audit.
    pub fn delegate(
        &self,
        sender: KernelCredentials,
        request: &ToolGrantRequest,
        target: BoundProcess,
    ) -> Result<DelegatedTool, DelegationError> {
        let mut state = lock(&self.state)?;
        let check = state.check_grant(sender, request, &target);
        if let Err(error) = check {
            target.channel.close();
            return state.deny(error);
        }
        let grant = request.sequence;
        let recorded = state.record(AuditDecision::ToolGranted {
            grant,
            pid: target.process.credentials().pid,
            revision: request.envelope_revision,
            uses: request.scope.uses,
        });
        if let Err(error) = recorded {
            target.channel.close();
            return Err(error);
        }
        // Recheck after blocking durability, before installing any authority.
        if let Err(error) = state.check_grant(sender, request, &target) {
            target.channel.close();
            return state.deny(error);
        }
        state.remaining -= request.scope.uses;
        state.grant_sequence = grant;
        state.grants.insert(
            grant,
            Grant {
                target,
                scope: request.scope.clone(),
                expires_at: request.expires_at,
                remaining: request.scope.uses,
                sequence: 0,
            },
        );
        Ok(DelegatedTool {
            state: Arc::clone(&self.state),
            grant,
        })
    }

    /// Dispatches an Agent-originated effect using its own remaining authority.
    ///
    /// Requests made on a tool's behalf remain Agent actions. Delegation grants
    /// neither exempt them from policy nor donate reserved tool budget back.
    /// The adapter's admission is asynchronous, including in deterministic doubles.
    ///
    /// # Errors
    /// Returns capability refusal or adapter admission failure. Admitted work
    /// delivers commit refusal or its actual result through `complete`.
    pub fn execute(
        &self,
        packet: &AuthenticatedPacket,
        effect: &dyn ToolEffect,
        complete: EffectCompletion,
    ) -> Result<(), DelegationError> {
        effect.execute(prepare(&self.state, None, packet)?, complete)
    }

    /// Permanently revokes this Agent channel and all delegated channels.
    ///
    /// Serialized with the irreversible commit boundary: an effect already
    /// committed is reported honestly; admitted but uncommitted work is denied.
    /// Call on the lifecycle I/O worker before tree cleanup. Never call this
    /// reentrantly from a commit closure.
    ///
    /// # Errors
    /// Returns audit/storage failure after channels are closed, or poisoned-owner failure.
    pub fn revoke(&self) -> Result<(), DelegationError> {
        lock(&self.state)?.revoke()
    }
}

impl Drop for ToolDelegation {
    fn drop(&mut self) {
        // Explicit revocation reports durability failures. Drop still closes
        // authority; it never releases the supervisor's containment/identity lease.
        let _ = self.revoke();
    }
}

impl DelegatedTool {
    /// Dispatches one tool request after checking both kernel credential observations.
    ///
    /// # Errors
    /// Refuses ungranted senders, replay, scope/context mismatch, expiry, lost
    /// parent/tool authority, exhausted budget or adapter admission failure.
    pub fn execute(
        &self,
        packet: &AuthenticatedPacket,
        effect: &dyn ToolEffect,
        complete: EffectCompletion,
    ) -> Result<(), DelegationError> {
        effect.execute(prepare(&self.state, Some(self.grant), packet)?, complete)
    }
}

impl PendingEffect {
    /// Rechecks authority immediately around the actual irreversible effect.
    ///
    /// This is a blocking commit primitive for the adapter's worker, not an
    /// asynchronous preparation callback. `apply` must start the actual effect
    /// here, not queue it or wait for its completion, and must not reenter this
    /// authority owner. Only this closure receives the command. The owner lock
    /// orders the short irreversible start against revocation. The returned `T`
    /// owns any started process/operation; collect it asynchronously and report
    /// the outcome with [`CommittedEffect::finish`], even after revocation.
    ///
    /// # Errors
    /// Returns current authority refusal or audit failure before calling `apply`,
    /// or its actual start failure afterward. A start failure is not retryable
    /// evidence: it may describe a partly performed external effect.
    pub fn commit<T>(
        self,
        apply: impl FnOnce(&ToolExecutionRequest) -> Result<T, DelegationError>,
    ) -> Result<(T, CommittedEffect), DelegationError> {
        let mut state = lock(&self.state)?;
        if let Err(error) = state.check_effect(self.grant, self.sender, &self.request) {
            return state.deny(error);
        }
        state.record(AuditDecision::EffectCommitIntent {
            grant: self.grant,
            sequence: self.request.sequence,
        })?;
        if let Err(error) = state.check_effect(self.grant, self.sender, &self.request) {
            return state.deny(error);
        }
        let result = apply(&self.request);
        drop(state);
        let committed = CommittedEffect {
            state: self.state,
            grant: self.grant,
            sequence: self.request.sequence,
        };
        match result {
            Ok(value) => Ok((value, committed)),
            Err(error) => match committed.finish(Err(error)) {
                Err(error) => Err(error),
                Ok(_) => Err(DelegationError::OwnerUnavailable),
            },
        }
    }
}

impl CommittedEffect {
    /// Records and returns the actual outcome without checking revoked authority.
    ///
    /// Runs on an I/O worker because the audit append is durable. This conveys
    /// no permission to start another action or to retry an uncertain effect.
    ///
    /// # Errors
    /// Preserves the adapter's failure. If audit storage fails, returns
    /// [`DelegationError::CompletionAudit`] containing the actual outcome.
    pub fn finish(
        self,
        outcome: Result<ToolExecutionResult, DelegationError>,
    ) -> Result<ToolExecutionResult, DelegationError> {
        let mut state = lock(&self.state)?;
        match state.record(AuditDecision::EffectFinished {
            grant: self.grant,
            sequence: self.sequence,
            succeeded: outcome.is_ok(),
        }) {
            Ok(()) => outcome,
            Err(DelegationError::Audit(source)) => Err(DelegationError::CompletionAudit {
                outcome: Box::new(outcome),
                source,
            }),
            Err(error) => Err(error),
        }
    }
}

fn lock(state: &Mutex<State>) -> Result<MutexGuard<'_, State>, DelegationError> {
    state.lock().map_err(|poisoned| {
        // A panic may have interrupted commitment; close instead of recovering
        // a possibly reusable reservation. No identity lease is released here.
        poisoned.into_inner().close_channels();
        DelegationError::OwnerUnavailable
    })
}

fn check_process(bound: &BoundProcess, sender: KernelCredentials) -> Result<(), DelegationError> {
    if bound.process.credentials() != sender || bound.channel.peer_credentials() != sender {
        return Err(DelegationError::IdentityMismatch);
    }
    if bound.channel.is_closed()
        || !bound
            .process
            .valid()
            .map_err(DelegationError::IdentityUnavailable)?
    {
        return Err(DelegationError::Revoked);
    }
    Ok(())
}

fn prepare(
    state: &Arc<Mutex<State>>,
    grant: Option<u64>,
    packet: &AuthenticatedPacket,
) -> Result<PendingEffect, DelegationError> {
    let mut owner = lock(state)?;
    let result = (|| {
        let LauncherPacket::Request(ProtocolMessage::ToolExecution(request)) = &packet.packet
        else {
            return Err(DelegationError::InvalidRequest);
        };
        if packet.peer_credentials != packet.message_credentials {
            return Err(DelegationError::IdentityMismatch);
        }
        owner.check_effect(grant, packet.message_credentials, request)?;
        let (remaining, sequence) = if let Some(id) = grant {
            let entry = owner.grants.get_mut(&id).ok_or(DelegationError::Revoked)?;
            (&mut entry.remaining, &mut entry.sequence)
        } else {
            let State {
                remaining,
                agent_sequence,
                ..
            } = &mut *owner;
            (remaining, agent_sequence)
        };
        if sequence.checked_add(1) != Some(request.sequence) {
            return Err(DelegationError::Replay);
        }
        if *remaining == 0 {
            return Err(DelegationError::BudgetExhausted);
        }
        *remaining -= 1;
        *sequence = request.sequence;
        Ok(PendingEffect {
            state: Arc::clone(state),
            grant,
            request: request.clone(),
            sender: packet.message_credentials,
        })
    })();
    match result {
        Ok(pending) => Ok(pending),
        Err(error) => owner.deny(error),
    }
}

impl State {
    fn parent(&mut self) -> Result<(), DelegationError> {
        if !self.active {
            return Err(DelegationError::Revoked);
        }
        if let Err(error) = check_process(&self.agent, self.agent.process.credentials()) {
            self.revoke()?;
            return Err(error);
        }
        if Instant::now() >= self.policy.expires_at {
            self.revoke()?;
            return Err(DelegationError::Expired);
        }
        Ok(())
    }

    fn check_grant(
        &mut self,
        sender: KernelCredentials,
        request: &ToolGrantRequest,
        target: &BoundProcess,
    ) -> Result<(), DelegationError> {
        self.parent()?;
        check_process(&self.agent, sender)?;
        if !self.policy.allow_delegation {
            return Err(DelegationError::DelegationDenied);
        }
        if self.grant_sequence.checked_add(1) != Some(request.sequence) {
            return Err(DelegationError::Replay);
        }
        if request.session_id != self.binding.session_id
            || request.run_id != self.binding.run_id
            || request.envelope_revision != self.binding.envelope_revision
            || !request.scope.is_within(&self.policy.scope)
            || request.expires_at > self.policy.expires_at
        {
            return Err(DelegationError::ScopeMismatch);
        }
        if Instant::now() >= request.expires_at {
            return Err(DelegationError::Expired);
        }
        if request.scope.uses > self.remaining {
            return Err(DelegationError::BudgetExhausted);
        }
        let credentials = target.process.credentials();
        if credentials == self.agent.process.credentials()
            || self
                .grants
                .values()
                .any(|grant| grant.target.process.credentials() == credentials)
        {
            return Err(DelegationError::IdentityMismatch);
        }
        check_process(target, credentials)
    }

    fn check_effect(
        &mut self,
        grant: Option<u64>,
        sender: KernelCredentials,
        request: &ToolExecutionRequest,
    ) -> Result<(), DelegationError> {
        self.parent()?;
        request
            .validate()
            .map_err(|_| DelegationError::InvalidRequest)?;
        if request.session_id != self.binding.session_id
            || request.run_id != self.binding.run_id
            || request.envelope_revision != self.binding.envelope_revision
        {
            return Err(DelegationError::ScopeMismatch);
        }
        let scope = if let Some(id) = grant {
            let entry = self.grants.get(&id).ok_or(DelegationError::Revoked)?;
            check_process(&entry.target, sender)?;
            if Instant::now() >= entry.expires_at {
                return Err(DelegationError::Expired);
            }
            &entry.scope
        } else {
            check_process(&self.agent, sender)?;
            &self.policy.scope
        };
        if !scope.permits(request) {
            return Err(DelegationError::ScopeMismatch);
        }
        Ok(())
    }

    fn close_channels(&mut self) {
        self.active = false;
        self.agent.channel.close();
        for grant in self.grants.values() {
            grant.target.channel.close();
        }
    }

    fn revoke(&mut self) -> Result<(), DelegationError> {
        if !self.active {
            return Ok(());
        }
        self.close_channels();
        self.record(AuditDecision::CapabilitiesRevoked)
    }

    fn record(&mut self, decision: AuditDecision) -> Result<(), DelegationError> {
        let at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|time| u64::try_from(time.as_millis()).ok())
            .ok_or(DelegationError::OwnerUnavailable)?;
        let result = self.audit.record(&AuditEntry {
            at_ms,
            session_id: self.binding.session_id.clone(),
            run_id: self.binding.run_id.clone(),
            authorization_id: self.policy.authorization_id.clone(),
            identity_slot: self.binding.identity_slot,
            decision,
        });
        if let Err(error) = result {
            self.close_channels();
            return Err(DelegationError::Audit(error));
        }
        Ok(())
    }

    fn deny<T>(&mut self, error: DelegationError) -> Result<T, DelegationError> {
        self.record(AuditDecision::CapabilityDenied)?;
        Err(error)
    }
}
