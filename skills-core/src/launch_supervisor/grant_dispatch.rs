//! Explicit Agent delegation through the existing broker connection.

use super::{
    Arc, BrokerConnection, ChannelState, CommandMessage, CommandOperation, CommandOutcome,
    CommandPrincipal, Duration, ErrorCode, Instant, Mutex, OwnerEvent, ProtocolMessage,
    SessionOwner, SessionState, SupervisorError, ToolExecutionRequest,
};
use crate::{launch_supervisor::HelperPrincipal, launch_transport::LauncherPacket};

pub(super) struct PendingGrant {
    request: CommandMessage,
    forwarded_at: Instant,
}

#[derive(Default)]
pub(super) struct GrantDispatch {
    pub(super) pending: Option<PendingGrant>,
    pub(super) helper: Option<HelperPrincipal>,
    pub(super) sequence: u64,
    pub(super) active: bool,
    grant: Option<u64>,
    registered: bool,
    receiving: bool,
    launched: Arc<Mutex<Option<Result<HelperPrincipal, SupervisorError>>>>,
    request: Arc<Mutex<Option<Result<ToolExecutionRequest, SupervisorError>>>>,
}

impl SessionOwner {
    pub(super) fn request_grant(&mut self, request: &CommandMessage) {
        let CommandOperation::Delegate { grant, .. } = &request.operation else {
            return;
        };
        let valid = request.validate().is_ok()
            && grant.sequence == 1
            && request.session_id == self.binding.session_id
            && request.run_id == self.binding.run_id
            && request.envelope_revision == self.binding.envelope_revision
            && self.state == SessionState::Running
            && self.channel_state == ChannelState::Enabled
            && self.broker_connection == BrokerConnection::Connected
            && !self.widening_blocked
            && self.pending.is_none()
            && !self.commands.closed
            && self.commands.pending.is_none()
            && self.commands.grants.pending.is_none()
            && self.commands.grants.grant.is_none();
        if !valid {
            self.send_command_agent(
                &request.request_id,
                CommandOutcome::NotStarted {
                    error: ErrorCode::InvalidRequest,
                },
            );
            self.arm_agent_receive();
            return;
        }
        self.commands.grants.grant = Some(grant.sequence);
        self.commands.grants.pending = Some(PendingGrant {
            request: request.clone(),
            forwarded_at: Instant::now(),
        });
        let id = request.request_id.clone();
        let wake = self.sender.clone();
        if self
            .timer
            .schedule(
                Duration::from_millis(u64::from(grant.valid_for_ms)),
                Box::new(move || {
                    let _ = wake.send(OwnerEvent::CommandDeadline { request_id: id });
                }),
            )
            .is_err()
        {
            self.commands.grants.pending = None;
            self.send_command_agent(
                &request.request_id,
                CommandOutcome::NotStarted {
                    error: ErrorCode::LifecycleMechanicUnavailable,
                },
            );
            self.arm_agent_receive();
            return;
        }
        let mailbox = Arc::clone(&self.commands.grants.launched);
        let wake = self.sender.clone();
        let result = self
            .resources
            .capability
            .as_ref()
            .ok_or(SupervisorError::CapabilityUnavailable)
            .and_then(|gate| gate.command_enforcer())
            .and_then(|enforcer| {
                self.resources
                    .process
                    .as_mut()
                    .ok_or(SupervisorError::ToolIsolationUnproven)?
                    .launch_helper(
                        request.clone(),
                        enforcer,
                        Box::new(move |result| {
                            *mailbox
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(result);
                            let _ = wake.try_send(OwnerEvent::ToolFinished);
                        }),
                    )
            });
        if result.is_err() {
            self.commands.grants.pending = None;
            self.send_command_agent(
                &request.request_id,
                CommandOutcome::NotStarted {
                    error: ErrorCode::LifecycleMechanicUnavailable,
                },
            );
            self.arm_agent_receive();
        }
    }

    pub(super) fn collect_grant_events(&mut self) {
        let launched = self
            .commands
            .grants
            .launched
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(result) = launched {
            self.forward_grant(result);
        }
        let request = self
            .commands
            .grants
            .request
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if request.is_some() {
            self.commands.grants.receiving = false;
        }
        match request {
            Some(Ok(request)) => self.forward_command(request, self.commands.grants.grant),
            Some(Err(_)) if self.stop_helper().is_err() => {
                self.cleanup_unproven = true;
                let pending = self.pending.take();
                self.quarantine_mechanic(pending);
            }
            Some(Err(_)) | None => {}
        }
    }

    fn forward_grant(&mut self, result: Result<HelperPrincipal, SupervisorError>) {
        let Some(pending) = self.commands.grants.pending.as_ref() else {
            if let Ok(helper) = result {
                helper.close();
            }
            return;
        };
        let id = pending.request.request_id.clone();
        let helper = match result {
            Ok(helper)
                if !self.commands.closed
                    && self.state == SessionState::Running
                    && !self.widening_blocked
                    && self.pending.is_none() =>
            {
                helper
            }
            result => {
                if let Ok(helper) = result {
                    helper.close();
                }
                self.commands.grants.pending = None;
                if self.stop_helper().is_err() {
                    self.cleanup_unproven = true;
                    let pending = self.pending.take();
                    self.quarantine_mechanic(pending);
                }
                self.send_command_agent(
                    &id,
                    CommandOutcome::NotStarted {
                        error: ErrorCode::StateMismatch,
                    },
                );
                self.arm_agent_receive();
                return;
            }
        };
        let CommandOperation::Delegate { grant, .. } = &pending.request.operation else {
            return;
        };
        let message = self.command_message(
            &id,
            CommandOperation::DelegationRequest {
                principal: CommandPrincipal {
                    channel_id: self.binding.channel_id.clone(),
                    pid: self.binding.agent_pid,
                    uid: self.binding.assigned_uid,
                    gid: self.binding.assigned_gid,
                },
                tool: helper.principal.clone(),
                grant: grant.clone(),
            },
        );
        self.commands.grants.helper = Some(helper);
        self.send_command_broker(message);
    }

    pub(super) fn handle_grant_decision(&mut self, message: &CommandMessage) -> bool {
        if let CommandOperation::RevokeGrant { grant } = message.operation {
            self.enforce_grant_revocation(&message.request_id, grant);
            return true;
        }
        let Some(pending) = self.commands.grants.pending.as_ref() else {
            return false;
        };
        if pending.request.request_id != message.request_id {
            return false;
        }
        let accepted = match &message.operation {
            CommandOperation::Granted {
                tool,
                grant,
                valid_for_ms,
            } => {
                let result = (|| {
                    let CommandOperation::Delegate {
                        grant: requested, ..
                    } = &pending.request.operation
                    else {
                        return Err(SupervisorError::AuthorizationRejected);
                    };
                    let helper = self
                        .commands
                        .grants
                        .helper
                        .as_ref()
                        .ok_or(SupervisorError::CapabilityUnavailable)?;
                    if *grant != requested.sequence
                        || *tool != helper.principal
                        || *valid_for_ms > requested.valid_for_ms
                        || self.commands.closed
                        || self.widening_blocked
                        || self.pending.is_some()
                        || self.state != SessionState::Running
                        || helper.channel.is_closed()
                    {
                        return Err(SupervisorError::AuthorizationRejected);
                    }
                    let enforcer = self
                        .resources
                        .capability
                        .as_ref()
                        .ok_or(SupervisorError::CapabilityUnavailable)?
                        .command_enforcer()?;
                    let deadline = enforcer.register_grant(
                        Arc::clone(&helper.process),
                        message,
                        pending.forwarded_at,
                    )?;
                    self.commands.grants.registered = true;
                    helper.narrow_deadline(deadline)
                })();
                result.is_ok()
            }
            CommandOperation::Reject { .. } => false,
            _ => return true,
        };
        self.commands.grants.pending = None;
        if accepted {
            self.commands.grants.active = true;
            if let Some(gate) = self.resources.capability.as_ref() {
                // Losing the grant reply cannot refund or reinstall authority.
                // The independent Agent receive/lifetime checks close its grants.
                let _ = gate.send_command(message.clone(), Box::new(|_| {}));
            }
            self.arm_tool_receive();
        } else {
            if self.stop_helper().is_err() {
                self.cleanup_unproven = true;
                let pending = self.pending.take();
                self.quarantine_mechanic(pending);
            }
            self.send_command_agent(
                &message.request_id,
                CommandOutcome::NotStarted {
                    error: ErrorCode::InvalidRequest,
                },
            );
        }
        self.arm_agent_receive();
        true
    }

    fn enforce_grant_revocation(&mut self, request_id: &str, grant: u64) {
        if self.commands.grants.grant != Some(grant) {
            return;
        }
        let stopped = self.stop_helper();
        let cancelled = if self
            .commands
            .pending
            .as_ref()
            .is_some_and(|pending| pending.grant == Some(grant))
        {
            self.resources
                .process
                .as_mut()
                .ok_or(SupervisorError::CleanupUnproven)
                .and_then(|process| process.cancel_tool())
        } else {
            Ok(())
        };
        self.collect_tool_result();
        let enforced = stopped.is_ok() && cancelled.is_ok();
        self.send_command_broker(self.command_message(
            request_id,
            CommandOperation::GrantRevoked { grant, enforced },
        ));
        if !enforced {
            self.cleanup_unproven = true;
            let pending = self.pending.take();
            self.quarantine_mechanic(pending);
        }
    }

    pub(super) fn arm_tool_receive(&mut self) {
        if !self.commands.grants.active
            || self.commands.grants.receiving
            || self.commands.pending.is_some()
            || self.commands.closed
        {
            return;
        }
        let Some(helper) = self.commands.grants.helper.as_ref() else {
            return;
        };
        let process = Arc::clone(&helper.process);
        let channel = helper.channel.clone();
        let mailbox = Arc::clone(&self.commands.grants.request);
        let wake = self.sender.clone();
        let Some(grant) = self.commands.grants.grant else {
            return;
        };
        let Some(enforcer) = self
            .resources
            .capability
            .as_ref()
            .and_then(|gate| gate.command_enforcer().ok())
        else {
            return;
        };
        let result = helper.channel.receive(Box::new(move |result| {
            let result = result
                .map_err(|_| SupervisorError::CapabilityUnavailable)
                .and_then(|packet| {
                    if enforcer.agent_valid() != Ok(true)
                        || process.valid().ok() != Some(true)
                        || packet.peer_credentials != process.credentials()
                        || packet.message_credentials != process.credentials()
                    {
                        return Err(SupervisorError::AgentIdentityRejected);
                    }
                    let LauncherPacket::Request(ProtocolMessage::ToolExecution(request)) =
                        packet.packet
                    else {
                        return Err(SupervisorError::AuthorizationRejected);
                    };
                    Ok(request)
                });
            if result.is_err() {
                // Close local authority before delivery; late owner events cannot admit a queued start.
                let _ = enforcer.revoke_grant(grant);
                channel.close();
            }
            *mailbox
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(result);
            let _ = wake.try_send(OwnerEvent::ToolFinished);
        }));
        if result.is_ok() {
            self.commands.grants.receiving = true;
        } else {
            self.commands.grants.active = false;
        }
    }

    pub(super) fn expire_pending_grant(&mut self, id: &str) -> bool {
        if self
            .commands
            .grants
            .pending
            .as_ref()
            .is_none_or(|pending| pending.request.request_id != id)
        {
            return false;
        }
        self.commands.grants.pending = None;
        if self.stop_helper().is_err() {
            self.cleanup_unproven = true;
            let pending = self.pending.take();
            self.quarantine_mechanic(pending);
        }
        self.send_command_agent(id, CommandOutcome::Unknown);
        self.arm_agent_receive();
        true
    }

    pub(super) fn stop_helper(&mut self) -> Result<(), SupervisorError> {
        self.commands.grants.active = false;
        let revoked = if self.commands.grants.registered {
            self.resources
                .capability
                .as_ref()
                .ok_or(SupervisorError::CapabilityUnavailable)
                .and_then(|gate| gate.command_enforcer())
                .and_then(|enforcer| {
                    enforcer.revoke_grant(
                        self.commands
                            .grants
                            .grant
                            .ok_or(SupervisorError::AuthorizationRejected)?,
                    )
                })
        } else {
            Ok(())
        };
        if let Some(helper) = &self.commands.grants.helper {
            helper.close();
        }
        let stopped = self
            .resources
            .process
            .as_mut()
            .ok_or(SupervisorError::CleanupUnproven)
            .and_then(|process| process.cancel_helper());
        revoked.and(stopped)
    }

    pub(super) fn send_command_principal(
        &mut self,
        grant: Option<u64>,
        id: &str,
        outcome: CommandOutcome,
    ) {
        if grant.is_none() {
            self.send_command_agent(id, outcome);
            return;
        }
        if let Some(helper) = &self.commands.grants.helper {
            let message = self.command_message(id, CommandOperation::Result { outcome });
            // Lost output conveys uncertainty to the tool, never fresh authority.
            // The broker's actual outcome is sent independently by the caller.
            let _ = helper
                .channel
                .send(message.canonical_bytes(), Box::new(|_| {}));
        }
    }
}
