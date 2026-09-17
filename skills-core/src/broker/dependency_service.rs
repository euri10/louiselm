//! Authenticated dependency policy and delivery on the retained supervisor channel.

use serde::{Deserialize, Serialize};

use super::{
    BrokerError, BrokerService, BrokerSession, PendingAuthorization,
    dependencies::Admission,
    service::{receive, send},
};
use crate::{
    Digest,
    dependency_fetch::{
        Artifact, CACHE_CHUNK_BYTES, CacheChunk, Candidate, DependencyStatus, StartingLockfile,
        transport::{DownloadTransport, HttpsTransport},
    },
    launch_protocol::{ChannelState, CommandMessage, CommandOperation, ErrorCode, ProtocolMessage},
    launch_receipt::{ReceiptHead, SessionState},
    launch_transport::LauncherPacket,
};

/// One bounded dependency proposal in the authenticated operator's local view.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingDependency {
    /// Exact review identity; approval always names the complete proposal.
    pub candidate_id: String,
    /// Typed proposed coordinate and integrity, not an external resolution result.
    pub candidate: Candidate,
}

/// Local review result; reading it grants no authority and performs no fetch.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DependencyInspection {
    /// Exact owning Session.
    pub session_id: String,
    /// Immutable capability revision.
    pub envelope_revision: u64,
    /// Typed local proposals available for a batch decision.
    pub pending: Vec<PendingDependency>,
    /// More candidates remain; approving this page allows the next page to surface.
    pub has_more: bool,
    /// Exact batch durably approved by this call; empty for a read-only inspection.
    pub approved: Vec<String>,
    /// Exclusive expiry of all dependency authority for this Session.
    pub expires_at_ms: u64,
}

impl BrokerService {
    /// Locally inspects candidates or records an exact interactive approval batch.
    /// The caller must be authenticated by the dedicated operator endpoint.
    /// # Errors
    /// Refuses foreign operators, unattended/expired approvals, unknown IDs or storage failure.
    pub fn dependency_control(
        &self,
        operator_uid: u32,
        session_id: &str,
        candidates: Option<&[String]>,
        now_ms: u64,
    ) -> Result<DependencyInspection, BrokerError> {
        let auth = self
            .authorizations()
            .consumed_for_session(session_id)?
            .ok_or(BrokerError::UnknownAuthorization)?;
        self.check_history(session_id)?;
        self.dependencies
            .control(&auth, operator_uid, candidates, now_ms)
    }

    pub(super) fn answer_dependency<F>(
        &self,
        session: &mut BrokerSession,
        query: &CommandMessage,
        now_ms: u64,
        verify: &mut F,
    ) -> Result<(), BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        let result = (|| {
            let auth = self
                .authorizations()
                .consumed_for_session(&query.session_id)?
                .ok_or(BrokerError::UnknownAuthorization)?;
            let permission = auth
                .dependencies
                .as_ref()
                .ok_or(BrokerError::InvalidGrant)?;
            let transport = HttpsTransport::new(
                permission.registries.clone(),
                std::time::Duration::from_secs(30),
            )
            .map_err(|_| BrokerError::InvalidGrant)?;
            self.dependency_request(session, query, now_ms, verify, &transport)
        })();
        let operation = match result {
            Ok(status) => CommandOperation::DependencyResult { status },
            Err(BrokerError::Storage(error)) => return Err(BrokerError::Storage(error)),
            Err(_) => CommandOperation::DependencyRefused {
                error: ErrorCode::InvalidRequest,
            },
        };
        send(
            session.channel(),
            CommandMessage {
                operation,
                ..query.clone()
            }
            .canonical_bytes(),
        )
    }

    fn dependency_request<F>(
        &self,
        session: &mut BrokerSession,
        query: &CommandMessage,
        now_ms: u64,
        verify: &mut F,
        transport: &dyn DownloadTransport,
    ) -> Result<DependencyStatus, BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        let clock = std::time::Instant::now();
        query.validate()?;
        self.require_trusted_history(session, verify)?;
        let CommandOperation::DependencyFetch { request } = &query.operation else {
            return Err(BrokerError::InvalidGrant);
        };
        let binding = session.authorization();
        if query.session_id != binding.session_id
            || query.run_id != binding.run_id
            || query.envelope_revision != binding.envelope_revision
        {
            return Err(BrokerError::RequestMismatch);
        }
        let auth = self
            .authorizations()
            .consumed_for_session(&query.session_id)?
            .ok_or(BrokerError::UnknownAuthorization)?;
        let permission = auth
            .dependencies
            .as_ref()
            .ok_or(BrokerError::InvalidGrant)?;
        let starting = self.starting_dependencies(&auth)?;
        let head = self.dependency_head(session, verify)?;
        session.require_dependency_posture(now_ms)?;
        let current =
            now_ms.saturating_add(u64::try_from(clock.elapsed().as_millis()).unwrap_or(u64::MAX));
        let admission = self.dependencies.admit(&auth, starting, request, current)?;
        let Admission::Fetch { permit, record } = admission else {
            let Admission::Status(status) = admission else {
                return Err(BrokerError::InvalidGrant);
            };
            return Ok(status);
        };
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let worker = transport
            .start(
                permit,
                Box::new(move |result| {
                    let _ = sender.send(result);
                }),
            )
            .map_err(|_| BrokerError::InvalidGrant)?;
        worker.join().map_err(|_| BrokerError::InvalidGrant)?;
        let result = receiver.recv().map_err(|_| BrokerError::InvalidGrant)?;
        let status = match result {
            Ok((permit, bytes)) => {
                let current = now_ms
                    .saturating_add(u64::try_from(clock.elapsed().as_millis()).unwrap_or(u64::MAX));
                permission.validate(current)?;
                session.require_dependency_posture(current)?;
                permit
                    .check_active()
                    .map_err(|_| BrokerError::InvalidGrant)?;
                let digest = Digest::of(&bytes);
                if bytes.is_empty()
                    || permit.candidate() != &request.candidate
                    || bytes.len() as u64 > permit.max_bytes()
                    || request
                        .candidate
                        .integrity
                        .as_ref()
                        .is_some_and(|expected| expected != &digest.to_string())
                    || self.dependency_head(session, verify)? != head
                {
                    DependencyStatus::Unknown
                } else {
                    match self.deliver_dependency(
                        session,
                        query,
                        &head,
                        &bytes,
                        permission.expires_at_ms,
                        verify,
                    ) {
                        Ok(artifact) => DependencyStatus::Complete { artifact },
                        Err(BrokerError::Storage(error)) => {
                            return Err(BrokerError::Storage(error));
                        }
                        Err(_) => DependencyStatus::Unknown,
                    }
                }
            }
            Err(_) => DependencyStatus::Unknown,
        };
        self.dependencies.finish(&auth, &record, &status)?;
        Ok(status)
    }

    fn starting_dependencies(
        &self,
        auth: &PendingAuthorization,
    ) -> Result<StartingLockfile, BrokerError> {
        let permission = auth
            .dependencies
            .as_ref()
            .ok_or(BrokerError::InvalidGrant)?;
        let digest = Digest::parse(&permission.input_manifest_digest)
            .map_err(|_| BrokerError::InvalidGrant)?;
        let root = self
            .verification_inputs
            .parent()
            .ok_or(BrokerError::InvalidGrant)?
            .join("workspace-inputs")
            .join(digest.hex());
        let inputs = crate::workspace::launch_inputs::load(&root, &digest)?;
        let bytes = inputs
            .source_bytes(&permission.lockfile_path)
            .ok_or(BrokerError::InvalidGrant)?;
        if Digest::of(bytes).to_string() != permission.lockfile_digest {
            return Err(BrokerError::RequestMismatch);
        }
        Ok(StartingLockfile::cargo(bytes)?)
    }

    fn dependency_head<F>(
        &self,
        session: &mut BrokerSession,
        verify: &mut F,
    ) -> Result<ReceiptHead, BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        let status = self.supervisor_status(session, verify)?;
        if status.state != SessionState::Running
            || status.channel_state != ChannelState::Enabled
            || status.pending_operation.is_some()
            || self.lifecycle.is_quarantined(&status.session_id)?
        {
            return Err(BrokerError::InvalidGrant);
        }
        status.broker_head.ok_or(BrokerError::InvalidGrant)
    }

    fn deliver_dependency<F>(
        &self,
        session: &mut BrokerSession,
        query: &CommandMessage,
        head: &ReceiptHead,
        bytes: &[u8],
        expires_at_ms: u64,
        verify: &mut F,
    ) -> Result<Artifact, BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        let digest = Digest::of(bytes);
        let CommandOperation::DependencyFetch { request } = &query.operation else {
            return Err(BrokerError::InvalidGrant);
        };
        let candidate_id = request.candidate.id()?;
        let expected = Artifact {
            name: format!("artifact-{}", digest.hex()),
            digest: digest.to_string(),
            size: bytes.len() as u64,
            integrity_verified: request.candidate.integrity.is_some(),
        };
        let mut offset = 0_u64;
        for chunk in bytes.chunks(CACHE_CHUNK_BYTES) {
            let message = CommandMessage {
                operation: CommandOperation::DependencyChunk {
                    chunk: CacheChunk {
                        candidate_id: candidate_id.clone(),
                        artifact_digest: digest.to_string(),
                        total_size: bytes.len() as u64,
                        offset,
                        bytes: chunk.to_vec(),
                        head: head.clone(),
                        expires_at_ms,
                    },
                },
                ..query.clone()
            };
            send(session.channel(), message.canonical_bytes())?;
            offset += chunk.len() as u64;
            loop {
                let packet = receive(session.channel())?;
                if let LauncherPacket::Request(ProtocolMessage::Command(reply)) = &packet.packet {
                    if packet.peer_credentials != session.channel().peer_credentials()
                        || packet.message_credentials != packet.peer_credentials
                    {
                        return Err(BrokerError::InvalidGrant);
                    }
                    if reply.request_id != query.request_id
                        || reply.session_id != query.session_id
                        || reply.run_id != query.run_id
                        || reply.envelope_revision != query.envelope_revision
                    {
                        return Err(BrokerError::RequestMismatch);
                    }
                    let CommandOperation::DependencyChunkResult { received, artifact } =
                        &reply.operation
                    else {
                        return Err(BrokerError::InvalidGrant);
                    };
                    if *received != offset || (*received < expected.size && artifact.is_some()) {
                        return Err(BrokerError::InvalidGrant);
                    }
                    if offset == expected.size {
                        if artifact.as_ref() != Some(&expected) {
                            return Err(BrokerError::InvalidGrant);
                        }
                        return Ok(expected);
                    }
                    break;
                }
                self.control_packet(session, packet, super::now_ms()?, verify)?;
            }
        }
        Err(BrokerError::InvalidGrant)
    }
}
