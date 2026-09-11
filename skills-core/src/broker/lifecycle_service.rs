//! Lifecycle requests on the broker's retained, authenticated supervisor channel.

use super::{BrokerError, BrokerService, BrokerSession, receive, send};
use crate::{
    broker::lifecycle::LifecycleCaller,
    launch::PROTOCOL_VERSION,
    launch_protocol::{
        CompletedRequest, LifecycleRequest, ProtocolMessage, ResponseResult, STATUS_REQUEST_SCHEMA,
        StatusRequest, SupervisorStatus, evaluate_request,
    },
    launch_receipt::SignedReceipt,
    launch_transport::{AuthenticatedPacket, LauncherPacket},
};

impl BrokerService {
    /// Revokes broker approvals before requesting an authorized quarantine Park.
    /// Revocation uses the existing command owner; no grants are reconstructed.
    /// A send only requests enforcement, and the signed Park receipt proves it.
    ///
    /// # Errors
    /// Refuses wrong action/caller or failed persistence, revocation or Park.
    pub fn quarantine<F>(
        &self,
        session: &mut BrokerSession,
        caller: &LifecycleCaller,
        request: &LifecycleRequest,
        now_ms: u64,
        mut verify: F,
    ) -> Result<SignedReceipt, BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        let clock = std::time::Instant::now();
        request.validate()?;
        if request.action != crate::launch_protocol::LifecycleAction::Park
            || request.session_id != session.authorization.session_id
            || request.run_id != session.authorization.run_id
            || request.envelope_revision != session.authorization.envelope_revision
            || !caller.permits(&session.authorization, request, now_ms)
        {
            return Err(BrokerError::InvalidGrant);
        }
        let result = (|| {
            self.lifecycle.quarantine(&request.session_id)?;
            if session.commands.is_some() && !session.command_revocation_complete() {
                let revocation = format!(
                    "quarantine-{}",
                    crate::Digest::of(request.request_id.as_bytes()).hex()
                );
                session.revoke_commands(&revocation)?;
            }
            let current_time = now_ms
                .saturating_add(u64::try_from(clock.elapsed().as_millis()).unwrap_or(u64::MAX));
            self.request_lifecycle(session, caller, request, current_time, &mut verify)
        })();
        if result.is_err() {
            session.close();
        }
        result
    }
    /// Authorizes, persists and executes one exact lifecycle request.
    ///
    /// Run on the Session's broker worker, never concurrently with another receive.
    /// The transport is asynchronous; this worker waits with bounded deadlines and
    /// continues servicing command packets and signed receipts while awaiting replies.
    /// Caller identity/scope must come from the authenticated trusted control boundary.
    ///
    /// # Errors
    /// Returns typed authorization/CAS failures or transport/signature/storage failure.
    /// Uncertain failures close the channel and leave the durable intent unresolved.
    pub fn request_lifecycle<F>(
        &self,
        session: &mut BrokerSession,
        caller: &LifecycleCaller,
        request: &LifecycleRequest,
        now_ms: u64,
        mut verify: F,
    ) -> Result<SignedReceipt, BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        let clock = std::time::Instant::now();
        let current_time = || {
            now_ms.saturating_add(u64::try_from(clock.elapsed().as_millis()).unwrap_or(u64::MAX))
        };
        let result = (|| {
            request.validate()?;
            if !caller.permits(&session.authorization, request, now_ms) {
                return Err(crate::launch_protocol::ProtocolError::new(
                    crate::launch_protocol::ErrorCode::InvalidRequest,
                    None,
                    None,
                )
                .into());
            }
            let status = self.supervisor_status(session, &mut verify)?;
            let receipts = self.receipts.chain(&session.authorization.session_id)?;
            let disposition = self.lifecycle.prepare(
                &session.authorization,
                &status,
                caller,
                request,
                current_time(),
                &receipts,
            )?;
            if let Some(receipt) = disposition.replayed_receipt() {
                return Ok(receipt.clone());
            }
            if !caller.permits(&session.authorization, request, current_time()) {
                let error = crate::launch_protocol::ProtocolError::new(
                    crate::launch_protocol::ErrorCode::InvalidRequest,
                    Some(status.state),
                    status.broker_head.as_ref().map(|head| head.sequence),
                );
                self.lifecycle.record_failure(request, &error)?;
                return Err(error.into());
            }
            send(&session.channel, request.canonical_bytes())?;
            loop {
                let packet = receive(&session.channel)?;
                if let LauncherPacket::Response(response) = &packet.packet {
                    if response.request_id != request.request_id {
                        return Err(BrokerError::InvalidGrant);
                    }
                    match &response.result {
                        ResponseResult::Receipt { receipt } => {
                            let completed = CompletedRequest::new(request, receipt.clone());
                            evaluate_request(&status, Some(&completed), request)
                                .map_err(|_| BrokerError::ReceiptUnauthorized)?;
                            let stored = self.receipts.stored_bytes(&request.session_id)?;
                            if !stored
                                .iter()
                                .any(|bytes| bytes == &receipt.canonical_bytes())
                            {
                                return Err(BrokerError::ReceiptUnauthorized);
                            }
                            return Ok(receipt.clone());
                        }
                        ResponseResult::Error { error } => {
                            self.lifecycle.record_failure(request, error)?;
                            return Err(error.clone().into());
                        }
                        _ => return Err(BrokerError::InvalidGrant),
                    }
                }
                self.lifecycle_packet(session, packet, &mut verify)?;
            }
        })();
        if result.is_err() && !matches!(result, Err(BrokerError::Policy(_))) {
            session.close();
        }
        result
    }

    /// Reads and correlates authenticated mechanical state with the durable broker head.
    ///
    /// This is blocking broker-worker I/O, not a liveness inference from stored receipts.
    /// # Errors
    /// Refuses malformed, foreign, stale-head or unavailable supervisor responses.
    fn supervisor_status<F>(
        &self,
        session: &mut BrokerSession,
        mut verify: F,
    ) -> Result<SupervisorStatus, BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        let request = StatusRequest {
            schema: STATUS_REQUEST_SCHEMA.into(),
            protocol_version: PROTOCOL_VERSION,
            request_id: "broker-status".into(),
            session_id: session.authorization.session_id.clone(),
            run_id: session.authorization.run_id.clone(),
        };
        send(&session.channel, request.canonical_bytes())?;
        loop {
            let packet = receive(&session.channel)?;
            if let LauncherPacket::Response(response) = &packet.packet {
                if response.request_id != request.request_id {
                    return Err(BrokerError::InvalidGrant);
                }
                let ResponseResult::SupervisorStatus { status } = &response.result else {
                    return Err(BrokerError::InvalidGrant);
                };
                status.validate()?;
                if status.session_id != request.session_id
                    || status.run_id != request.run_id
                    || status.envelope_revision != session.authorization.envelope_revision
                    || status.broker_head != self.receipts.head(&request.session_id)?
                {
                    return Err(BrokerError::InvalidGrant);
                }
                return Ok(status.clone());
            }
            self.lifecycle_packet(session, packet, &mut verify)?;
        }
    }

    pub(in crate::broker) fn lifecycle_packet<F>(
        &self,
        session: &mut BrokerSession,
        packet: AuthenticatedPacket,
        verify: &mut F,
    ) -> Result<(), BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        match &packet.packet {
            LauncherPacket::SignedReceipt(receipt) => {
                self.lifecycle.check_receipt(receipt)?;
                let ack = self
                    .receipts
                    .append(&session.authorization, &packet.bytes, verify)?;
                send(&session.channel, ack.canonical_bytes())
            }
            LauncherPacket::Request(ProtocolMessage::Command(_)) => session.handle_command(packet),
            _ => Err(BrokerError::InvalidGrant),
        }
    }
}
