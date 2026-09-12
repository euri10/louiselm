//! Retention completion stays on the serialized lifecycle owner.

use std::sync::{Arc, Mutex};

use super::{OwnerEvent, SessionOwner};
use crate::{
    launch_protocol::{
        BrokerConnection, ChannelState, ErrorCode, PROTOCOL_VERSION, ProtocolError,
        ProtocolResponse, RESPONSE_SCHEMA, RecoveryRequest, ResponseResult,
    },
    launch_receipt::{ReceiptOutcome, SessionState},
    launch_supervisor::recovery::{RecoveryError, RetentionEvidence},
};

#[derive(Default)]
pub(super) struct RecoveryDispatch {
    pending: Option<(RecoveryRequest, u64, u64)>,
    result: Arc<Mutex<Option<Result<RetentionEvidence, RecoveryError>>>>,
}

impl SessionOwner {
    fn recovery_matches(&self, request: &RecoveryRequest) -> bool {
        request.validate().is_ok()
            && self.state == SessionState::Parked
            && self.channel_state == ChannelState::Revoked
            && self.broker_connection == BrokerConnection::Connected
            && self.pending.is_none()
            && !self.has_receipt_backlog()
            && !self.controller_loss_unresolved
            && !self.quarantined
            && !self.cleanup_unproven
            && request.head == self.broker_head
            && request.launch.session_id == self.binding.session_id
            && request.launch.run_id == self.binding.run_id
            && request.launch.envelope_revision == self.binding.envelope_revision
            && self.receipts.first().is_some_and(|receipt| {
                matches!(&receipt.payload.outcome, ReceiptOutcome::Launch { authorization, .. }
                    if authorization.authorization_id == request.launch.authorization_id
                    && authorization.request_digest == request.launch.digest().to_string())
            })
    }

    pub(super) fn handle_recovery(&mut self, request: RecoveryRequest) {
        if self.recovery.pending.is_some() || !self.recovery_matches(&request) {
            self.send_error(
                request.retention.request_id,
                ProtocolError::new(
                    ErrorCode::StateMismatch,
                    Some(self.state),
                    Some(self.broker_head.sequence),
                ),
            );
            return;
        }
        let mailbox = Arc::clone(&self.recovery.result);
        let wake = self.sender.clone();
        let Some(process) = self.resources.process.as_mut() else {
            self.send_error(
                request.retention.request_id,
                ProtocolError::new(
                    ErrorCode::LifecycleMechanicUnavailable,
                    Some(self.state),
                    Some(self.broker_head.sequence),
                ),
            );
            return;
        };
        self.recovery.pending = Some((request.clone(), self.connection_epoch, self.process_epoch));
        let started = process.retain_recovery(
            request.retention.clone(),
            Box::new(move |result| {
                *mailbox
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(result);
                // Disposal may join the worker: its callback must never wait on the owner.
                let _ = wake.try_send(OwnerEvent::RecoveryFinished);
            }),
        );
        if started.is_err() {
            self.recovery.pending = None;
            self.send_error(
                request.retention.request_id,
                ProtocolError::new(
                    ErrorCode::LifecycleMechanicUnavailable,
                    Some(self.state),
                    Some(self.broker_head.sequence),
                ),
            );
        }
    }

    pub(super) fn collect_recovery(&mut self) {
        let result = self
            .recovery
            .result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let Some(result) = result else {
            return;
        };
        let Some((request, connection, process)) = self.recovery.pending.take() else {
            return;
        };
        if connection != self.connection_epoch || process != self.process_epoch || self.quarantined
        {
            return;
        }
        let result = match result {
            Ok(evidence)
                if self.recovery_matches(&request)
                    && evidence.validate().is_ok()
                    && evidence.launch == request.launch
                    && evidence.request == request.retention =>
            {
                ResponseResult::RecoveryRetention { evidence }
            }
            result => {
                let code = match result {
                    Err(RecoveryError::Conflict) => ErrorCode::RequestIdConflict,
                    Err(RecoveryError::Invalid) => ErrorCode::InvalidRequest,
                    Err(RecoveryError::Io(_) | RecoveryError::Storage(_)) => {
                        ErrorCode::DurabilityUnavailable
                    }
                    Err(RecoveryError::Unsupported) => ErrorCode::LifecycleMechanicUnavailable,
                    Ok(_) | Err(RecoveryError::NotParked | RecoveryError::Expired) => {
                        ErrorCode::StateMismatch
                    }
                };
                ResponseResult::Error {
                    error: ProtocolError::new(
                        code,
                        Some(self.state),
                        Some(self.broker_head.sequence),
                    ),
                }
            }
        };
        self.send_response(ProtocolResponse {
            schema: RESPONSE_SCHEMA.into(),
            protocol_version: PROTOCOL_VERSION,
            request_id: request.retention.request_id,
            result,
        });
    }
}
