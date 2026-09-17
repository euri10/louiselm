//! Exact broker chunks become opaque cache bytes under the existing local authority gate.

use super::{
    Arc, BrokerConnection, ChannelState, CommandMessage, CommandOperation, ErrorCode, OwnerEvent,
    SessionOwner, SessionState, SupervisorError,
};
use crate::Digest;
use crate::dependency_fetch::{Artifact, CacheChunk};

pub(super) type Completion = Option<(String, Result<Artifact, SupervisorError>)>;

pub(super) struct Transfer {
    header: CacheChunk,
    bytes: Vec<u8>,
    writing: bool,
    pub(super) artifact: Option<Artifact>,
}

impl Transfer {
    fn append(&mut self, chunk: &CacheChunk) -> Result<u64, SupervisorError> {
        if self.writing
            || chunk.offset != self.bytes.len() as u64
            || chunk.artifact_digest != self.header.artifact_digest
            || chunk.total_size != self.header.total_size
            || chunk.head != self.header.head
            || chunk.expires_at_ms != self.header.expires_at_ms
        {
            return Err(SupervisorError::AuthorizationRejected);
        }
        self.bytes.extend_from_slice(&chunk.bytes);
        Ok(self.bytes.len() as u64)
    }
}

impl SessionOwner {
    pub(super) fn handle_dependency_chunk(&mut self, message: &CommandMessage) -> bool {
        let CommandOperation::DependencyChunk { chunk } = &message.operation else {
            return false;
        };
        if self.accept_dependency_chunk(message, chunk).is_err() {
            self.send_command_broker(self.command_message(
                &message.request_id,
                CommandOperation::DependencyRefused {
                    error: ErrorCode::InvalidRequest,
                },
            ));
        }
        true
    }

    fn accept_dependency_chunk(
        &mut self,
        message: &CommandMessage,
        chunk: &CacheChunk,
    ) -> Result<(), SupervisorError> {
        message
            .validate()
            .map_err(|_| SupervisorError::AuthorizationRejected)?;
        if self.commands.closed
            || self.state != SessionState::Running
            || self.channel_state != ChannelState::Enabled
            || self.broker_connection != BrokerConnection::Connected
            || self.pending.is_some()
            || chunk.head != self.broker_head
        {
            return Err(SupervisorError::AuthorizationRejected);
        }
        let pending = self
            .commands
            .status
            .as_mut()
            .ok_or(SupervisorError::AuthorizationRejected)?;
        let query = &pending.query;
        if message.request_id != pending.broker_request_id
            || message.session_id != query.session_id
            || message.run_id != query.run_id
            || message.envelope_revision != query.envelope_revision
        {
            return Err(SupervisorError::AuthorizationRejected);
        }
        let CommandOperation::DependencyFetch { request } = &query.operation else {
            return Err(SupervisorError::AuthorizationRejected);
        };
        if request
            .candidate
            .id()
            .map_err(|_| SupervisorError::AuthorizationRejected)?
            != chunk.candidate_id
            || chunk.total_size > request.max_bytes
            || request
                .candidate
                .integrity
                .as_ref()
                .is_some_and(|digest| digest != &chunk.artifact_digest)
        {
            return Err(SupervisorError::AuthorizationRejected);
        }
        let transfer = pending.cache.get_or_insert_with(|| {
            let mut header = chunk.clone();
            header.bytes.clear();
            Transfer {
                header,
                bytes: Vec::new(),
                writing: false,
                artifact: None,
            }
        });
        let received = transfer.append(chunk)?;
        if received < chunk.total_size {
            self.send_command_broker(self.command_message(
                &message.request_id,
                CommandOperation::DependencyChunkResult {
                    received,
                    artifact: None,
                },
            ));
            return Ok(());
        }
        let digest = Digest::parse(&chunk.artifact_digest)
            .map_err(|_| SupervisorError::AuthorizationRejected)?;
        let expected = Artifact {
            name: format!("artifact-{}", digest.hex()),
            digest: digest.to_string(),
            size: received,
            integrity_verified: request.candidate.integrity.is_some(),
        };
        transfer.writing = true;
        let bytes = std::mem::take(&mut transfer.bytes);
        self.start_cache_write(message, chunk, digest, bytes, expected)
    }

    fn start_cache_write(
        &mut self,
        message: &CommandMessage,
        chunk: &CacheChunk,
        digest: Digest,
        bytes: Vec<u8>,
        expected: Artifact,
    ) -> Result<(), SupervisorError> {
        let enforcer = self
            .resources
            .capability
            .as_ref()
            .ok_or(SupervisorError::CapabilityUnavailable)?
            .command_enforcer()?;
        let mailbox = Arc::clone(&self.commands.cache_result);
        let wake = self.sender.clone();
        let id = message.request_id.clone();
        self.resources
            .process
            .as_mut()
            .ok_or(SupervisorError::CapabilityUnavailable)?
            .store_dependency(
                digest,
                bytes,
                enforcer,
                chunk.expires_at_ms,
                Box::new(move |result| {
                    let result = result.and_then(|name| {
                        if name == expected.name {
                            Ok(expected)
                        } else {
                            Err(SupervisorError::AuthorizationRejected)
                        }
                    });
                    *mailbox
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((id, result));
                    let _ = wake.try_send(OwnerEvent::ToolFinished);
                }),
            )
    }

    pub(super) fn collect_dependency_result(&mut self) {
        let completed = self
            .commands
            .cache_result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let Some((id, result)) = completed else {
            return;
        };
        let Some(pending) = self
            .commands
            .status
            .as_mut()
            .filter(|pending| pending.broker_request_id == id)
        else {
            return;
        };
        let Some(cache) = pending.cache.as_mut() else {
            return;
        };
        let operation = match result {
            Ok(artifact) => {
                cache.artifact = Some(artifact.clone());
                CommandOperation::DependencyChunkResult {
                    received: artifact.size,
                    artifact: Some(artifact),
                }
            }
            Err(_) => CommandOperation::DependencyRefused {
                error: ErrorCode::InvalidRequest,
            },
        };
        self.send_command_broker(self.command_message(&id, operation));
    }
}
