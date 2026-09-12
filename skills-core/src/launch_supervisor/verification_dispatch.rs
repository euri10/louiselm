//! Verification runs through the original serialized lifecycle owner and broker channel.

use super::{OwnerEvent, SessionOwner};
use crate::{
    launch_protocol::{
        BrokerConnection, ChannelState, ErrorCode, PROTOCOL_VERSION, ProtocolError,
        ProtocolResponse, RESPONSE_SCHEMA, ResponseResult, VerificationOperation,
        VerificationRequest,
    },
    launch_receipt::{ReceiptOutcome, SessionState},
    launch_supervisor::SupervisorError,
};
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub(super) struct VerificationDispatch {
    pending: Option<(VerificationRequest, u64, u64)>,
    result: Arc<Mutex<Option<Result<ResponseResult, SupervisorError>>>>,
    spent: bool,
}

impl SessionOwner {
    fn verification_matches(&self, request: &VerificationRequest) -> bool {
        let correct_state = match request.operation {
            VerificationOperation::Export { .. } => {
                self.state == SessionState::Parked && self.channel_state == ChannelState::Revoked
            }
            VerificationOperation::Run { .. } => {
                self.state == SessionState::Running && request.head.sequence == 1
            }
        };
        request.validate().is_ok() && correct_state
            && self.broker_connection == BrokerConnection::Connected && self.pending.is_none()
            && !self.has_receipt_backlog() && !self.controller_loss_unresolved && !self.quarantined && !self.cleanup_unproven
            && request.head == self.broker_head
            && request.launch.session_id == self.binding.session_id && request.launch.run_id == self.binding.run_id
            && request.launch.envelope_revision == self.binding.envelope_revision
            && self.receipts.first().is_some_and(|receipt| matches!(&receipt.payload.outcome,
                ReceiptOutcome::Launch { authorization, .. } if authorization.authorization_id == request.launch.authorization_id
                    && authorization.request_digest == request.launch.digest().to_string()))
    }

    pub(super) fn handle_verification(&mut self, request: VerificationRequest) {
        let run = matches!(request.operation, VerificationOperation::Run { .. });
        if self.verification.pending.is_some()
            || (run && self.verification.spent)
            || !self.verification_matches(&request)
        {
            self.send_error(
                request.request_id,
                ProtocolError::new(
                    ErrorCode::InvalidRequest,
                    Some(self.state),
                    Some(self.broker_head.sequence),
                ),
            );
            return;
        }
        self.verification.spent |= run;
        let mailbox = Arc::clone(&self.verification.result);
        let wake = self.sender.clone();
        self.verification.pending =
            Some((request.clone(), self.connection_epoch, self.process_epoch));
        let started = self
            .resources
            .process
            .as_mut()
            .ok_or(SupervisorError::ToolIsolationUnproven)
            .and_then(|process| {
                process.verification(
                    request.clone(),
                    Box::new(move |result| {
                        *mailbox
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(result);
                        let _ = wake.try_send(OwnerEvent::VerificationFinished);
                    }),
                )
            });
        if started.is_err() {
            self.verification.pending = None;
            self.send_error(
                request.request_id,
                ProtocolError::new(
                    ErrorCode::LifecycleMechanicUnavailable,
                    Some(self.state),
                    Some(self.broker_head.sequence),
                ),
            );
        }
    }

    pub(super) fn collect_verification(&mut self) {
        let result = self
            .verification
            .result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let Some(result) = result else {
            return;
        };
        let Some((request, connection, process)) = self.verification.pending.take() else {
            return;
        };
        if connection != self.connection_epoch || process != self.process_epoch {
            return;
        }
        let current = self.verification_matches(&request);
        let result = match result {
            Ok(ResponseResult::VerificationExport { evidence })
                if current && evidence.request == request && evidence.validate().is_ok() =>
            {
                ResponseResult::VerificationExport { evidence }
            }
            Ok(ResponseResult::VerificationExecution { mut evidence })
                if evidence.request == request && evidence.validate().is_ok() =>
            {
                evidence.interrupted |= !current;
                if !evidence.cleanup_proven {
                    self.cleanup_unproven = true;
                }
                ResponseResult::VerificationExecution { evidence }
            }
            _ => ResponseResult::Error {
                error: ProtocolError::new(
                    ErrorCode::LifecycleMechanicUnavailable,
                    Some(self.state),
                    Some(self.broker_head.sequence),
                ),
            },
        };
        self.send_response(ProtocolResponse {
            schema: RESPONSE_SCHEMA.into(),
            protocol_version: PROTOCOL_VERSION,
            request_id: request.request_id,
            result,
        });
    }
}
