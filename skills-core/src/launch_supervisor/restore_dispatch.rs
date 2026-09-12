//! Restore completions cannot outlive the exact frozen target and connection.
use super::{OwnerEvent, SessionOwner};
use crate::{
    launch_protocol::{
        ErrorCode, PROTOCOL_VERSION, ProtocolError, ProtocolResponse, RECOVERY_REQUEST_SCHEMA,
        RESPONSE_SCHEMA, RecoveryRequest, RecoveryRestoreRequest, ResponseResult,
    },
    launch_supervisor::recovery::RecoveryError,
};
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub(super) struct RestoreDispatch {
    pending: Option<(RecoveryRestoreRequest, u64, u64)>,
    result: Arc<Mutex<Option<Result<RecoveryRestoreRequest, RecoveryError>>>>,
}

impl SessionOwner {
    fn restore_matches(&self, request: &RecoveryRestoreRequest) -> bool {
        request.validate().is_ok()
            && self.recovery_matches(&RecoveryRequest {
                schema: RECOVERY_REQUEST_SCHEMA.into(),
                protocol_version: PROTOCOL_VERSION,
                launch: request.target.clone(),
                head: request.head.clone(),
                retention: request.source.request.clone(),
            })
    }

    pub(super) fn handle_restore(&mut self, request: RecoveryRestoreRequest) {
        if self.restore.pending.is_some() || !self.restore_matches(&request) {
            self.send_error(
                request.request_id,
                ProtocolError::new(
                    ErrorCode::StateMismatch,
                    Some(self.state),
                    Some(self.broker_head.sequence),
                ),
            );
            return;
        }
        let mailbox = Arc::clone(&self.restore.result);
        let wake = self.sender.clone();
        let Some(process) = self.resources.process.as_mut() else {
            self.send_error(
                request.request_id,
                ProtocolError::new(
                    ErrorCode::LifecycleMechanicUnavailable,
                    Some(self.state),
                    Some(self.broker_head.sequence),
                ),
            );
            return;
        };
        self.restore.pending = Some((request.clone(), self.connection_epoch, self.process_epoch));
        let started = process.restore_recovery(
            request.clone(),
            Box::new(move |result| {
                *mailbox
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(result);
                // The owner can join the worker during disposal; never block it here.
                let _ = wake.try_send(OwnerEvent::RestoreFinished);
            }),
        );
        if started.is_err() {
            self.restore.pending = None;
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

    pub(super) fn collect_restore(&mut self) {
        let result = self
            .restore
            .result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let Some(result) = result else {
            return;
        };
        let Some((request, connection, process)) = self.restore.pending.take() else {
            return;
        };
        if connection != self.connection_epoch || process != self.process_epoch || self.quarantined
        {
            return;
        }
        let result = match result {
            Ok(evidence) if evidence == request && self.restore_matches(&request) => {
                ResponseResult::RecoveryRestored {
                    request: Box::new(evidence),
                }
            }
            other => ResponseResult::Error {
                error: ProtocolError::new(
                    match other {
                        Err(RecoveryError::Expired | RecoveryError::NotParked) | Ok(_) => {
                            ErrorCode::StateMismatch
                        }
                        Err(RecoveryError::Invalid) => ErrorCode::InvalidRequest,
                        Err(RecoveryError::Conflict) => ErrorCode::RequestIdConflict,
                        Err(RecoveryError::Unsupported) => ErrorCode::LifecycleMechanicUnavailable,
                        Err(RecoveryError::Io(_) | RecoveryError::Storage(_)) => {
                            ErrorCode::DurabilityUnavailable
                        }
                    },
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
