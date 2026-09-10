//! Broker-only admission and completion of zero-authority tools.

use std::sync::Arc;

use crate::launch_protocol::{
    BrokerConnection, ChannelState, ErrorCode, PROTOCOL_VERSION, ProtocolError, ProtocolResponse,
    RESPONSE_SCHEMA, ResponseResult, ToolExecutionRequest,
};
use crate::launch_receipt::SessionState;

use super::{OwnerEvent, SessionOwner};

impl SessionOwner {
    pub(super) fn handle_tool(&mut self, request: ToolExecutionRequest) {
        let refusal = request
            .validate()
            .err()
            .map(|error| error.code)
            .or_else(|| {
                if request.session_id != self.binding.session_id
                    || request.run_id != self.binding.run_id
                {
                    Some(ErrorCode::SubjectMismatch)
                } else if request.envelope_revision != self.binding.envelope_revision {
                    Some(ErrorCode::EnvelopeRevisionMismatch)
                } else if self.tool_sequence.checked_add(1) != Some(request.sequence) {
                    Some(ErrorCode::RequestIdConflict)
                } else if self.state != SessionState::Running
                    || self.channel_state != ChannelState::Enabled
                    || self.broker_connection != BrokerConnection::Connected
                    || self.widening_blocked
                {
                    Some(ErrorCode::StateMismatch)
                } else if self.pending_tool.is_some() || self.pending.is_some() {
                    Some(ErrorCode::OperationPending)
                } else {
                    None
                }
            });
        if let Some(code) = refusal {
            self.send_error(
                request.request_id,
                ProtocolError::new(code, Some(self.state), Some(self.broker_head.sequence)),
            );
            return;
        }
        // Consume before calling mechanics. Failed, lost and completed executions
        // never become replayable after a broker reconnect.
        self.tool_sequence = request.sequence;
        self.pending_tool = Some((
            request.request_id.clone(),
            self.connection_epoch,
            self.process_epoch,
        ));
        let mailbox = Arc::clone(&self.tool_mailbox);
        let wake = self.sender.clone();
        let result = self
            .resources
            .process
            .as_mut()
            .ok_or(crate::launch_supervisor::SupervisorError::ToolIsolationUnproven)
            .and_then(|process| {
                process.execute_tool(
                    request,
                    Box::new(move |result| {
                        *mailbox
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(result);
                        // The mailbox owns completion. A full queue already guarantees a
                        // wakeup; never block a worker that disposal must join.
                        let _ = wake.try_send(OwnerEvent::ToolFinished);
                    }),
                )
            });
        if let Err(error) = result {
            *self
                .tool_mailbox
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Err(error));
        }
        self.collect_tool_result();
    }

    pub(super) fn collect_tool_result(&mut self) {
        let result = self
            .tool_mailbox
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let Some(result) = result else { return };
        let Some((request_id, connection_epoch, process_epoch)) = self.pending_tool.take() else {
            return;
        };
        if connection_epoch != self.connection_epoch
            || process_epoch != self.process_epoch
            || self.quarantined
            || self.finished.is_some()
        {
            return;
        }
        match result {
            Ok(output) => self.send_response(ProtocolResponse {
                schema: RESPONSE_SCHEMA.to_owned(),
                protocol_version: PROTOCOL_VERSION,
                request_id,
                result: ResponseResult::ToolExecution { output },
            }),
            Err(_) => self.send_error(
                request_id,
                ProtocolError::new(
                    ErrorCode::LifecycleMechanicUnavailable,
                    Some(self.state),
                    Some(self.broker_head.sequence),
                ),
            ),
        }
    }
}
