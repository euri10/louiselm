//! Lifecycle requests on the broker's retained, authenticated supervisor channel.

use super::{BrokerError, BrokerService, BrokerSession, receive, response, send};
use crate::{
    broker::lifecycle::LifecycleCaller,
    launch::PROTOCOL_VERSION,
    launch_protocol::{
        CompletedRequest, ErrorCode, LifecycleRequest, ProtocolError, ProtocolMessage,
        ResponseResult, STATUS_REQUEST_SCHEMA, SessionStatus, StatusRequest, SupervisorStatus,
        evaluate_request,
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

    /// Serves one read-only status request arriving on the Agent capability channel.
    ///
    /// The Agent has no lifecycle authority, so the answer always advertises no
    /// actions; it is otherwise the same composition every other consumer reads.
    /// Self-scope is enforced against this Session's own authorization, and a
    /// foreign subject is refused identically whether or not it exists, so a
    /// refusal cannot be used to probe for other Sessions.
    ///
    /// Reading status grants nothing: it never resets a budget, revives a grant
    /// or counts as recovery admission.
    ///
    /// Run at the top of the Session's broker worker, where no other receive is
    /// armed. Requests that arrive while another operation holds the worker are
    /// refused as retryable `OperationPending` instead, for the Agent to retry.
    ///
    /// # Errors
    /// Returns transport failure, or the typed refusal sent for a foreign subject.
    pub fn serve_agent_status<F>(
        &self,
        session: &mut BrokerSession,
        now_ms: u64,
        mut verify: F,
    ) -> Result<SessionStatus, BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        let result = (|| {
            let packet = receive(&session.channel)?;
            let LauncherPacket::Request(ProtocolMessage::Status(query)) = &packet.packet else {
                return Err(BrokerError::InvalidGrant);
            };
            query.validate()?;
            let request_id = query.request_id.clone();
            if query.session_id != session.authorization.session_id
                || query.run_id != session.authorization.run_id
            {
                // Identical refusal for a live sibling and for a Session that was
                // never authorized; the Agent learns only that it was not this one.
                let error = ProtocolError::new(ErrorCode::SubjectMismatch, None, None);
                send(
                    &session.channel,
                    response(
                        &request_id,
                        ResponseResult::Error {
                            error: error.clone(),
                        },
                    ),
                )?;
                return Err(error.into());
            }
            let status =
                self.session_status(session, &LifecycleCaller::Agent, now_ms, &mut verify)?;
            send(
                &session.channel,
                response(
                    &request_id,
                    ResponseResult::SessionStatus {
                        status: status.clone(),
                    },
                ),
            )?;
            Ok(status)
        })();
        // A refused subject is a typed answer, not a lost channel.
        if result.is_err() && !matches!(result, Err(BrokerError::Policy(_))) {
            session.close();
        }
        result
    }

    /// Composes canonical Session status for one authenticated caller.
    ///
    /// Mechanical facts come from the supervisor over the retained channel;
    /// posture is derived from the Session's retained launch proof, and the
    /// advertised actions are broker-owned. The action set is
    /// narrowed to what `caller` could actually request, and a serialized
    /// operation still in flight withdraws all of them, so status never offers
    /// a mutation the next request would refuse.
    ///
    /// Run on the Session's broker worker, never concurrently with another
    /// receive. This reads mechanical state, runs no posture evidence probes,
    /// preserves the original proof-validation time and authorizes nothing.
    ///
    /// # Errors
    /// Refuses malformed, foreign, stale-head or unavailable supervisor responses,
    /// unreadable quarantine state, or a composition the status schema rejects.
    pub fn session_status<F>(
        &self,
        session: &mut BrokerSession,
        caller: &LifecycleCaller,
        now_ms: u64,
        mut verify: F,
    ) -> Result<SessionStatus, BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        let result = (|| {
            let status = self.supervisor_status(session, &mut verify)?;
            let quarantined = self
                .lifecycle
                .is_quarantined(&session.authorization.session_id)?;
            // A pending operation and an advertised action contradict each other;
            // the status schema rejects the pair, so offer nothing while one runs.
            let actions = if status.pending_operation.is_some() {
                Vec::new()
            } else {
                caller.allowed_actions(&session.authorization, status.state, quarantined, now_ms)
            };
            let posture = session
                .posture_evidence
                .status(&status, quarantined, now_ms)?;
            Ok(SessionStatus::compose(status, posture, actions)?)
        })();
        if result.is_err() {
            session.close();
        }
        result
    }

    /// Reads and correlates authenticated mechanical state with the durable broker head.
    ///
    /// This is blocking broker-worker I/O, not a liveness inference from stored receipts.
    /// # Errors
    /// Refuses malformed, foreign, stale-head or unavailable supervisor responses.
    pub(in crate::broker) fn supervisor_status<F>(
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
        self.control_packet(session, packet, crate::broker::now_ms()?, verify)
            .map(|_| ())
    }
}

/// Refuses a status request that arrived while the worker holds another operation.
///
/// Answering inline would arm a receive competing with the one already waiting,
/// so the Agent is told to retry rather than served out of order. The reply is
/// retryable and carries no Session state.
///
/// # Errors
/// Returns transport failure while sending the refusal.
pub(in crate::broker) fn refuse_nested_status(
    session: &BrokerSession,
    query: &StatusRequest,
) -> Result<(), BrokerError> {
    send(
        session.channel(),
        response(
            &query.request_id,
            ResponseResult::Error {
                error: ProtocolError::new(ErrorCode::OperationPending, None, None),
            },
        ),
    )
}
