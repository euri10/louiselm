//! Agent status, Skill Admission and Beads requests on the authenticated relay.

use super::{
    BrokerConnection, ChannelState, CommandMessage, CommandOperation, Duration, ErrorCode,
    OwnerEvent, SessionOwner, SessionState,
};

pub(super) struct PendingStatus {
    query: CommandMessage,
    broker_request_id: String,
}

impl SessionOwner {
    pub(super) fn forward_agent_status(&mut self, query: CommandMessage) {
        let refused =
            if query.session_id != self.binding.session_id || query.run_id != self.binding.run_id {
                Some(ErrorCode::SubjectMismatch)
            } else if query.envelope_revision != self.binding.envelope_revision {
                Some(ErrorCode::InvalidRequest)
            } else if self.commands.closed
                || self.state != SessionState::Running
                || self.channel_state != ChannelState::Enabled
                || self.broker_connection != BrokerConnection::Connected
                || self.pending.is_some()
                || self.commands.pending.is_some()
                || self.commands.grants.pending.is_some()
                || self.commands.status.is_some()
            {
                Some(ErrorCode::OperationPending)
            } else {
                None
            };
        if let Some(error) = refused {
            let reply = self.command_message(&query.request_id, refusal(&query, error));
            self.send_agent_reply(reply);
            self.arm_agent_receive();
            return;
        }
        let Some(sequence) = self.commands.status_sequence.checked_add(1) else {
            self.commands.closed = true;
            return;
        };
        self.commands.status_sequence = sequence;
        let id = format!("agent-status-{sequence}");
        let sender = self.sender.clone();
        let deadline_id = id.clone();
        if self
            .timer
            .schedule(
                Duration::from_mins(1),
                Box::new(move || {
                    let _ = sender.send(OwnerEvent::AgentStatusDeadline {
                        request_id: deadline_id,
                    });
                }),
            )
            .is_err()
        {
            let reply = self.command_message(
                &query.request_id,
                refusal(&query, ErrorCode::BrokerUnavailable),
            );
            self.send_agent_reply(reply);
            self.arm_agent_receive();
            return;
        }
        let mut forwarded = query.clone();
        forwarded.request_id.clone_from(&id);
        self.commands.status = Some(PendingStatus {
            query,
            broker_request_id: id,
        });
        self.send_command_broker(forwarded);
    }

    pub(super) fn handle_status_reply(&mut self, message: &CommandMessage) -> bool {
        if !matches!(
            message.operation,
            CommandOperation::StatusResult { .. }
                | CommandOperation::StatusRefused { .. }
                | CommandOperation::SkillRequestResult { .. }
                | CommandOperation::SkillRequestRefused { .. }
                | CommandOperation::BeadsMutationResult { .. }
                | CommandOperation::BeadsMutationRefused { .. }
        ) {
            return false;
        }
        let Some(pending) = self.commands.status.as_ref() else {
            return true;
        };
        let query = &pending.query;
        if message.validate().is_err()
            || message.request_id != pending.broker_request_id
            || message.session_id != query.session_id
            || message.run_id != query.run_id
            || message.envelope_revision != query.envelope_revision
            || !matching_reply(query, message)
        {
            return true;
        }
        let mut reply = message.clone();
        reply.request_id.clone_from(&query.request_id);
        self.commands.status = None;
        // Park, loss and revocation can overtake this request. A late result must
        // never reach a revoked channel or a later Resume receive generation.
        if !self.commands.closed
            && self.channel_state == ChannelState::Enabled
            && self.broker_connection == BrokerConnection::Connected
        {
            self.send_agent_reply(reply);
            self.arm_agent_receive();
        }
        true
    }

    pub(in crate::launch_supervisor) fn expire_agent_status(&mut self, id: &str) {
        if self
            .commands
            .status
            .as_ref()
            .is_none_or(|pending| pending.broker_request_id != id)
        {
            return;
        }
        if let Some(pending) = self.commands.status.take() {
            let reply = self.command_message(
                &pending.query.request_id,
                refusal(&pending.query, ErrorCode::BrokerUnavailable),
            );
            if !self.commands.closed && self.channel_state == ChannelState::Enabled {
                self.send_agent_reply(reply);
                self.arm_agent_receive();
            }
        }
    }
}

fn refusal(query: &CommandMessage, error: ErrorCode) -> CommandOperation {
    if matches!(query.operation, CommandOperation::SkillRequest { .. }) {
        CommandOperation::SkillRequestRefused { error }
    } else if matches!(query.operation, CommandOperation::BeadsMutation { .. }) {
        CommandOperation::BeadsMutationRefused { error }
    } else {
        CommandOperation::StatusRefused { error }
    }
}

fn matching_reply(query: &CommandMessage, reply: &CommandMessage) -> bool {
    match (&query.operation, &reply.operation) {
        (
            CommandOperation::StatusRequest {},
            CommandOperation::StatusResult { .. } | CommandOperation::StatusRefused { .. },
        )
        | (CommandOperation::SkillRequest { .. }, CommandOperation::SkillRequestRefused { .. })
        | (CommandOperation::BeadsMutation { .. }, CommandOperation::BeadsMutationRefused { .. }) => {
            true
        }
        (
            CommandOperation::SkillRequest { request },
            CommandOperation::SkillRequestResult { status },
        ) => request.request_id == status.request_id,
        (
            CommandOperation::BeadsMutation { request },
            CommandOperation::BeadsMutationResult { status },
        ) => request.request_id == status.request_id,
        _ => false,
    }
}
