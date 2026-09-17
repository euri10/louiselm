//! One Agent request -> broker decision -> local start -> durable outcome.

#[cfg(test)]
#[path = "tool_dispatch_tests.rs"]
mod tests;

#[path = "grant_dispatch.rs"]
mod grants;

#[path = "status_dispatch.rs"]
mod status;

#[path = "dependency_dispatch.rs"]
mod dependency;

use super::{OwnerEvent, SessionOwner};
use crate::launch_protocol::{
    BrokerConnection, COMMAND_SCHEMA, ChannelState, CommandMessage, CommandOperation,
    CommandOutcome, CommandPrincipal, ErrorCode, PROTOCOL_VERSION, ProtocolError, ProtocolMessage,
    ToolExecutionRequest, ToolExecutionResult,
};
use crate::{launch_receipt::SessionState, launch_supervisor::SupervisorError};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

struct PendingCommand {
    request: ToolExecutionRequest,
    forwarded_at: Instant,
    dispatch: Option<u64>,
    outcome: Option<CommandOutcome>,
    abandoned: bool,
    grant: Option<u64>,
}

#[derive(Default)]
pub(super) struct CommandDispatch {
    pub(super) closed: bool,
    sequence: u64,
    pending: Option<PendingCommand>,
    status: Option<status::PendingStatus>,
    status_sequence: u64,
    cache_result: Arc<Mutex<dependency::Completion>>,
    request: Arc<Mutex<Option<Result<ProtocolMessage, SupervisorError>>>>,
    result: Arc<Mutex<Option<Result<ToolExecutionResult, SupervisorError>>>>,
    receiving: bool,
    grants: grants::GrantDispatch,
}

impl SessionOwner {
    pub(super) fn resume_command_reception(&mut self) {
        // Callbacks from the revoked channel retain the old mailbox and cannot
        // close or inject a command into this new receive generation.
        self.commands.request = Arc::default();
        self.commands.status = None;
        self.commands.cache_result = Arc::default();
        if let Some(pending) = self.commands.pending.as_mut() {
            pending.abandoned = true;
        }
        self.commands.grants.active = false;
        self.commands.grants.pending = None;
        self.commands.receiving = false;
        self.commands.closed = false;
        self.arm_agent_receive();
    }

    pub(super) fn handle_tool(&mut self, request: ToolExecutionRequest) {
        // A raw broker command has no original authenticated Agent request and
        // no single-use authorization. It must never reach process mechanics.
        self.send_error(
            request.request_id,
            ProtocolError::new(
                ErrorCode::InvalidRequest,
                Some(self.state),
                Some(self.broker_head.sequence),
            ),
        );
    }

    pub(super) fn arm_agent_receive(&mut self) {
        if self.commands.closed
            || self.commands.receiving
            || self.commands.pending.is_some()
            || self.commands.status.is_some()
            || self.commands.grants.pending.is_some()
        {
            return;
        }
        let mailbox = Arc::clone(&self.commands.request);
        let wake = self.sender.clone();
        let result = self
            .resources
            .capability
            .as_mut()
            .ok_or(SupervisorError::CapabilityUnavailable)
            .and_then(|gate| {
                gate.receive_command(Box::new(move |result| {
                    *mailbox
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(result);
                    let _ = wake.try_send(OwnerEvent::ToolFinished);
                }))
            });
        if result.is_err() {
            self.commands.closed = true;
        } else {
            self.commands.receiving = true;
        }
    }

    fn command_message(&self, id: &str, operation: CommandOperation) -> CommandMessage {
        CommandMessage {
            schema: COMMAND_SCHEMA.to_owned(),
            protocol_version: PROTOCOL_VERSION,
            request_id: id.to_owned(),
            session_id: self.binding.session_id.clone(),
            run_id: self.binding.run_id.clone(),
            envelope_revision: self.binding.envelope_revision,
            operation,
        }
    }

    fn send_command_broker(&mut self, message: CommandMessage) {
        let sender = self.sender.clone();
        let connection_epoch = self.connection_epoch;
        let result = self
            .resources
            .broker
            .as_ref()
            .ok_or(SupervisorError::BrokerUnavailable)
            .and_then(|broker| {
                broker.send_command(
                    message,
                    Box::new(move |result| {
                        let _ = sender.send(OwnerEvent::ResponseSent {
                            connection_epoch,
                            finish: None,
                            result,
                        });
                    }),
                )
            });
        if let Err(error) = result {
            self.lose_broker(error);
        }
    }

    fn send_command_agent(&mut self, id: &str, outcome: CommandOutcome) {
        let message = self.command_message(id, CommandOperation::Result { outcome });
        self.send_agent_reply(message);
    }

    fn send_agent_reply(&mut self, message: CommandMessage) {
        let mailbox = Arc::clone(&self.commands.request);
        let wake = self.sender.clone();
        let result = self
            .resources
            .capability
            .as_ref()
            .ok_or(SupervisorError::CapabilityUnavailable)
            .and_then(|gate| {
                gate.send_command(
                    message,
                    Box::new(move |result| {
                        if let Err(error) = result {
                            *mailbox
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                                Some(Err(error));
                            let _ = wake.try_send(OwnerEvent::ToolFinished);
                        }
                    }),
                )
            });
        if result.is_err() {
            self.commands.closed = true;
        }
    }

    fn forward_command(&mut self, request: ToolExecutionRequest, grant: Option<u64>) {
        let sequence = if grant.is_some() {
            self.commands.grants.sequence
        } else {
            self.commands.sequence
        };
        let valid = request.validate().is_ok()
            && request.session_id == self.binding.session_id
            && request.run_id == self.binding.run_id
            && request.envelope_revision == self.binding.envelope_revision
            && sequence.checked_add(1) == Some(request.sequence)
            && self.state == SessionState::Running
            && self.channel_state == ChannelState::Enabled
            && self.broker_connection == BrokerConnection::Connected
            && !self.widening_blocked
            && self.pending.is_none()
            && self.commands.pending.is_none()
            && self.commands.grants.pending.is_none()
            && (grant.is_none() || self.commands.grants.active)
            && !self.commands.closed;
        if !valid {
            self.send_command_principal(
                grant,
                &request.request_id,
                CommandOutcome::NotStarted {
                    error: ErrorCode::InvalidRequest,
                },
            );
            self.arm_agent_receive();
            self.arm_tool_receive();
            return;
        }
        let message = self.command_message(
            &request.request_id,
            CommandOperation::Request {
                principal: if grant.is_some() {
                    let Some(helper) = self.commands.grants.helper.as_ref() else {
                        return;
                    };
                    helper.principal.clone()
                } else {
                    CommandPrincipal {
                        channel_id: self.binding.channel_id.clone(),
                        pid: self.binding.agent_pid,
                        uid: self.binding.assigned_uid,
                        gid: self.binding.assigned_gid,
                    }
                },
                command: request.clone(),
            },
        );
        let id = request.request_id.clone();
        self.commands.pending = Some(PendingCommand {
            request,
            forwarded_at: Instant::now(),
            dispatch: None,
            outcome: None,
            abandoned: false,
            grant,
        });
        let sender = self.sender.clone();
        if self
            .timeout
            .checked_add(Duration::from_secs(30))
            .ok_or(SupervisorError::WorkerUnavailable)
            .and_then(|delay| {
                self.timer.schedule(
                    delay,
                    Box::new(move || {
                        let _ = sender.send(OwnerEvent::CommandDeadline { request_id: id });
                    }),
                )
            })
            .is_err()
        {
            self.commands.pending = None;
            self.commands.closed = true;
            self.send_command_principal(
                grant,
                &message.request_id,
                CommandOutcome::NotStarted {
                    error: ErrorCode::BrokerUnavailable,
                },
            );
            return;
        }
        self.send_command_broker(message);
    }

    pub(super) fn handle_command(&mut self, message: CommandMessage) {
        if self.handle_dependency_chunk(&message) {
            return;
        }
        if self.handle_status_reply(&message) {
            return;
        }
        if message.validate().is_err()
            || message.session_id != self.binding.session_id
            || message.run_id != self.binding.run_id
            || message.envelope_revision != self.binding.envelope_revision
        {
            self.send_error(
                message.request_id,
                ProtocolError::new(
                    ErrorCode::SubjectMismatch,
                    Some(self.state),
                    Some(self.broker_head.sequence),
                ),
            );
            return;
        }
        if matches!(message.operation, CommandOperation::Revoke) {
            self.revoke_commands(message);
            return;
        }
        if self.handle_grant_decision(&message) {
            return;
        }
        let Some(pending) = self.commands.pending.as_ref() else {
            return;
        };
        if pending.request.request_id != message.request_id {
            return;
        }
        match &message.operation {
            CommandOperation::Authorize {
                dispatch_sequence, ..
            } if pending.dispatch.is_none() => {
                self.start_command(&message, *dispatch_sequence);
            }
            CommandOperation::Reject { error } if pending.dispatch.is_none() => {
                let error = *error;
                let grant = pending.grant;
                self.commands.pending = None;
                self.send_command_principal(
                    grant,
                    &message.request_id,
                    CommandOutcome::NotStarted { error },
                );
                self.arm_agent_receive();
                self.arm_tool_receive();
            }
            CommandOperation::OutcomeAcknowledged { dispatch_sequence }
                if pending.dispatch == Some(*dispatch_sequence) && pending.outcome.is_some() =>
            {
                self.commands.pending = None;
                self.arm_agent_receive();
                self.arm_tool_receive();
            }
            _ => {}
        }
    }

    fn start_command(&mut self, message: &CommandMessage, dispatch: u64) {
        let Some(pending) = self.commands.pending.as_mut() else {
            return;
        };
        pending.dispatch = Some(dispatch);
        if pending.grant.is_some() {
            self.commands.grants.sequence = pending.request.sequence;
        } else {
            self.commands.sequence = pending.request.sequence;
        }
        let permit = self
            .resources
            .capability
            .as_ref()
            .ok_or(SupervisorError::CapabilityUnavailable)
            .and_then(|gate| {
                if pending.grant.is_some() {
                    let helper = self
                        .commands
                        .grants
                        .helper
                        .as_ref()
                        .ok_or(SupervisorError::CapabilityUnavailable)?;
                    gate.command_enforcer()?.admit(
                        &pending.request,
                        &helper.principal,
                        message,
                        pending.forwarded_at,
                    )
                } else {
                    gate.authorize_command(&pending.request, message, pending.forwarded_at)
                }
            });
        let ready = !pending.abandoned
            && !self.commands.closed
            && !self.widening_blocked
            && self.state == SessionState::Running
            && self.channel_state == ChannelState::Enabled
            && self.pending.is_none();
        let ready = ready && (pending.grant.is_none() || self.commands.grants.active);
        let permit = match permit {
            Ok(permit) if ready => permit,
            _ => {
                self.report_command(CommandOutcome::NotStarted {
                    error: ErrorCode::StateMismatch,
                });
                return;
            }
        };
        let mailbox = Arc::clone(&self.commands.result);
        let wake = self.sender.clone();
        let result = self
            .resources
            .process
            .as_mut()
            .ok_or(SupervisorError::ToolIsolationUnproven)
            .and_then(|process| {
                process.execute_tool(
                    permit,
                    Box::new(move |result| {
                        *mailbox
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(result);
                        // Never block the executor that revocation/disposal must join.
                        let _ = wake.try_send(OwnerEvent::ToolFinished);
                    }),
                )
            });
        if result.is_err() {
            self.report_command(CommandOutcome::NotStarted {
                error: ErrorCode::LifecycleMechanicUnavailable,
            });
        }
    }

    fn report_command(&mut self, outcome: CommandOutcome) {
        let Some(pending) = self.commands.pending.as_mut() else {
            return;
        };
        let Some(dispatch_sequence) = pending.dispatch else {
            return;
        };
        pending.outcome = Some(outcome.clone());
        let id = pending.request.request_id.clone();
        let grant = pending.grant;
        // Deliver known actual output even if its durable audit subsequently fails.
        // Nothing here authorizes a retry or refunds the broker's spent budget.
        self.send_command_principal(grant, &id, outcome.clone());
        let message = self.command_message(
            &id,
            CommandOperation::Outcome {
                dispatch_sequence,
                outcome,
            },
        );
        self.send_command_broker(message);
    }

    pub(super) fn collect_tool_result(&mut self) {
        self.collect_dependency_result();
        let request = self
            .commands
            .request
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if request.is_some() {
            self.commands.receiving = false;
        }
        match request {
            Some(Ok(ProtocolMessage::ToolExecution(request))) => {
                self.forward_command(request, None);
            }
            Some(Ok(ProtocolMessage::Command(request))) => {
                if matches!(
                    request.operation,
                    CommandOperation::StatusRequest {}
                        | CommandOperation::SkillRequest { .. }
                        | CommandOperation::BeadsMutation { .. }
                        | CommandOperation::DependencyFetch { .. }
                ) {
                    self.forward_agent_status(request);
                } else {
                    self.request_grant(&request);
                }
            }
            Some(Ok(_) | Err(_)) => {
                self.commands.closed = true;
            }
            None => {}
        }
        self.collect_grant_events();
        let result = self
            .commands
            .result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(result) = result {
            // Revocation and process/connection epochs do not erase actual results.
            // Reconnect replay is out of scope; a failed send remains uncertain.
            let cleanup_failed = matches!(result, Err(SupervisorError::CleanupUnproven));
            let outcome = match result {
                Ok(output) => CommandOutcome::Completed { output },
                Err(SupervisorError::CleanupUnproven) => {
                    self.cleanup_unproven = true;
                    self.commands.closed = true;
                    CommandOutcome::Unknown
                }
                Err(_) => CommandOutcome::Unknown,
            };
            self.report_command(outcome);
            if cleanup_failed {
                let pending = self.pending.take();
                self.quarantine_mechanic(pending);
            }
        }
    }

    fn revoke_commands(&mut self, mut message: CommandMessage) {
        self.commands.closed = true;
        let revoked = self
            .resources
            .capability
            .as_mut()
            .ok_or(SupervisorError::CapabilityUnavailable)
            .and_then(|gate| gate.revoke());
        self.channel_state = ChannelState::Revoked;
        let cancelled = self
            .resources
            .process
            .as_mut()
            .ok_or(SupervisorError::CleanupUnproven)
            .and_then(|process| process.cancel_tool());
        let helper = self.stop_helper();
        self.collect_tool_result();
        let enforced = revoked.is_ok() && cancelled.is_ok() && helper.is_ok();
        message.operation = CommandOperation::Revoked { enforced };
        self.send_command_broker(message);
        if !enforced {
            self.cleanup_unproven = true;
            let pending = self.pending.take();
            self.quarantine_mechanic(pending);
        }
    }

    pub(super) fn command_deadline(&mut self, id: &str) {
        if self.expire_pending_grant(id) {
            return;
        }
        let Some(pending) = self.commands.pending.as_mut() else {
            return;
        };
        if pending.request.request_id != id || pending.outcome.is_some() {
            return;
        }
        pending.abandoned = true;
        self.commands.closed = true;
        let grant = pending.grant;
        self.send_command_principal(grant, id, CommandOutcome::Unknown);
        // A running permit already has its earlier local deadline (<=30 s).
        // Join it before considering the command settled. No automatic retry.
        if self
            .resources
            .process
            .as_mut()
            .is_some_and(|process| process.cancel_tool().is_err())
        {
            self.cleanup_unproven = true;
            let pending = self.pending.take();
            self.quarantine_mechanic(pending);
        }
        self.collect_tool_result();
    }
}
