//! Receiver policy for the supervisor's authenticated descriptor handoff.

use super::{BrokerError, BrokerService, BrokerSession};
use crate::{
    launch_protocol::ResponseResult,
    launch_receipt::ReceiptOutcome,
    launch_transport::{AuthenticatedPacket, LauncherPacket},
};

impl BrokerService {
    /// Receives one supervisor-created upstream socket after request admission.
    /// The expected destination must belong to this Session's stored Provider
    /// grant. The caller owns receive ordering, TLS and explicit socket retirement.
    /// # Errors
    /// Refuses wrong scope/destination, repeated cookie, malformed descriptors or
    /// lost acknowledgement. The Session channel closes on uncertain handoff.
    pub fn receive_guarded_upstream(
        &self,
        session: &mut BrokerSession,
        request_id: &str,
        destination: std::net::SocketAddr,
        now_ms: u64,
    ) -> Result<super::GuardedUpstream, BrokerError> {
        let result = (|| {
            let enrollment = &session
                .provider_listener
                .as_ref()
                .ok_or(BrokerError::InvalidGrant)?
                .enrollment;
            let approved = self
                .authorizations()
                .consumed_for_session(&enrollment.scope.session_id)?
                .ok_or(BrokerError::UnknownAuthorization)?;
            let permission = approved
                .provider_requests
                .as_ref()
                .ok_or(BrokerError::InvalidGrant)?;
            let url =
                url::Url::parse(&permission.upstream).map_err(|_| BrokerError::InvalidGrant)?;
            if !permission.valid(now_ms)
                || now_ms >= approved.expires_at_ms
                || !permission.addresses.contains(&destination.ip())
                || url.port_or_known_default() != Some(destination.port())
                || session.provider_sockets.len()
                    >= crate::provider_request::MAX_RUN_REQUESTS as usize
            {
                return Err(BrokerError::InvalidGrant);
            }
            let packet = super::service::receive(session.channel())?;
            let socket = super::GuardedUpstream::adopt(
                packet,
                session.channel().peer_credentials(),
                enrollment,
                request_id,
                destination,
            )?;
            let evidence = socket.evidence().clone();
            if session
                .provider_sockets
                .contains_key(&evidence.socket_cookie)
            {
                return Err(BrokerError::RequestMismatch);
            }
            session.provider_sockets.insert(
                evidence.socket_cookie,
                std::sync::Arc::downgrade(&socket.lease),
            );
            super::service::send(
                session.channel(),
                super::service::response(
                    request_id,
                    ResponseResult::SenderGuardUpstreamAccepted { socket: evidence },
                ),
            )?;
            Ok(socket)
        })();
        if result.is_err() {
            session.close();
        }
        result
    }

    pub(super) fn close_provider_listener(
        session: &mut BrokerSession,
        packet: AuthenticatedPacket,
    ) -> Result<(), BrokerError> {
        if packet.peer_credentials != session.channel().peer_credentials()
            || packet.message_credentials != packet.peer_credentials
            || packet.descriptors.is_some()
        {
            return Err(BrokerError::InvalidGrant);
        }
        let LauncherPacket::Response(response) = packet.packet else {
            return Err(BrokerError::InvalidGrant);
        };
        let ResponseResult::SenderGuardClosing { enrollment } = response.result else {
            return Err(BrokerError::InvalidGrant);
        };
        let authorization = session.authorization();
        if enrollment.scope.session_id != authorization.session_id
            || enrollment.scope.run_id != authorization.run_id
            || enrollment.scope.revision != authorization.envelope_revision
            || enrollment.broker_pid != std::process::id()
            || session
                .provider_listener
                .as_ref()
                .is_some_and(|listener| listener.enrollment != enrollment)
        {
            return Err(BrokerError::RequestMismatch);
        }
        // This is the serialized Session owner; serving returned and closed its
        // accepted stream before it can process a closure packet.
        let mut retained = false;
        for socket in session
            .provider_sockets
            .values()
            .filter_map(std::sync::Weak::upgrade)
        {
            socket
                .shutdown()
                .map_err(|_| BrokerError::ProviderUnavailable)?;
            retained = true;
        }
        if retained {
            // A socket owner still exists: stop writes, but never claim all
            // descriptors were closed. The supervisor must poison uncertain cleanup.
            return Err(BrokerError::ProviderUnavailable);
        }
        session.provider_sockets.clear();
        session.provider_listener = None;
        super::service::send(
            session.channel(),
            super::service::response(
                &response.request_id,
                ResponseResult::SenderGuardClosed { enrollment },
            ),
        )
    }

    pub(super) fn accept_provider_listener<F>(
        &self,
        session: &mut BrokerSession,
        packet: AuthenticatedPacket,
        now_ms: u64,
        verify: &mut F,
    ) -> Result<(), BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        let LauncherPacket::Response(response) = &packet.packet else {
            return Err(BrokerError::InvalidGrant);
        };
        let ResponseResult::SenderGuardEnrolled { enrollment } = &response.result else {
            return Err(BrokerError::InvalidGrant);
        };
        let authorization = session.authorization();
        let approved = self
            .authorizations()
            .consumed_for_session(&authorization.session_id)?
            .ok_or(BrokerError::UnknownAuthorization)?;
        let permission = approved
            .provider_requests
            .as_ref()
            .ok_or(BrokerError::InvalidGrant)?;
        let scope = &enrollment.scope;
        let expires = permission
            .expires_at_ms
            .min(approved.expires_at_ms)
            .min(authorization.expires_at_ms);
        let now = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
        let monotonic = u64::try_from(now.tv_sec)
            .ok()
            .and_then(|secs| secs.checked_mul(1_000_000_000))
            .and_then(|secs| {
                u64::try_from(now.tv_nsec)
                    .ok()
                    .and_then(|ns| secs.checked_add(ns))
            })
            .ok_or(BrokerError::InvalidGrant)?;
        let maximum = monotonic
            .checked_add(expires.saturating_sub(now_ms).saturating_mul(1_000_000))
            .ok_or(BrokerError::InvalidGrant)?;
        if session.provider_listener.is_some()
            || session
                .provider_revision
                .is_some_and(|revision| scope.revision <= revision)
            || scope.session_id != authorization.session_id
            || scope.run_id != authorization.run_id
            || scope.revision != authorization.envelope_revision
            || now_ms >= expires
            || scope.deadline_ns <= monotonic
            || scope.deadline_ns > maximum
            || approved.run_id != authorization.run_id
            || approved.envelope_revision != authorization.envelope_revision
        {
            return Err(BrokerError::RequestMismatch);
        }
        let history = self.verified_history(authorization, verify)?;
        let runtime = history
            .iter()
            .find_map(|receipt| match &receipt.payload.outcome {
                ReceiptOutcome::Start { evidence, .. } => Some(evidence.agent_pid),
                _ => None,
            })
            .ok_or(BrokerError::ReceiptUnauthorized)?;
        let scope = scope.clone();
        let (listener, request_id) = super::provider_listener::ProviderListener::adopt(
            packet,
            session.channel().peer_credentials(),
            &scope,
            runtime,
        )?;
        let accepted = listener.enrollment.clone();
        session.provider_revision = Some(scope.revision);
        session.provider_listener = Some(listener);
        let result = super::service::send(
            session.channel(),
            super::service::response(
                &request_id,
                ResponseResult::SenderGuardAccepted {
                    enrollment: accepted,
                },
            ),
        );
        if result.is_err() {
            session.provider_listener = None;
        }
        result
    }

    /// Serves one available connection on the Session's authenticated listener.
    /// Runs on its serialized broker worker; every complete frame uses the
    /// production admission, durable budget and relay path. No listener is a no-op.
    /// The transport must enforce the guard on upstream writes before activation.
    /// # Errors
    /// Refuses lost/stale scope and returns request, upstream or local I/O failures.
    pub fn serve_guarded_provider(
        &self,
        session: &mut BrokerSession,
        credentials: &super::provider_credentials::ProviderCredentialStore,
        transport: &dyn super::provider_transport::ProviderTransport,
        now_ms: u64,
        mut verify: impl FnMut(&str, &[u8], &str) -> bool,
    ) -> Result<bool, BrokerError> {
        let Some(listener) = session.provider_listener.take() else {
            return Ok(false);
        };
        let result = (|| {
            let scope = &listener.enrollment.scope;
            if session.channel().is_closed()
                || scope.session_id != session.authorization().session_id
                || scope.run_id != session.authorization().run_id
                || scope.revision != session.authorization().envelope_revision
            {
                return Err(BrokerError::InvalidGrant);
            }
            let Some(stream) = listener.accept()? else {
                return Ok(false);
            };
            let clock = std::time::Instant::now();
            super::provider_endpoint::serve_provider_connection(
                stream,
                listener.enrollment.address.to_string(),
                |request| {
                    let now = now_ms.saturating_add(
                        u64::try_from(clock.elapsed().as_millis()).unwrap_or(u64::MAX),
                    );
                    self.serve_provider_request(
                        session,
                        credentials,
                        transport,
                        request,
                        now,
                        &mut verify,
                    )
                },
            )?;
            Ok(true)
        })();
        // The accepted connection has closed before the listener's leases can
        // be released or its Session can process disposal/revision commands.
        session.provider_listener = Some(listener);
        result
    }
}
