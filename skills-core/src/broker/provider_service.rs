//! Per-request admission of brokered Provider calls on the Session owner.
//!
//! Follows the `louiselm-qbr.5.1.3.8` boundary: every complete request is
//! checked against current authority on the worker that owns the
//! `BrokerSession`, one durable Run unit is spent, time and channel are
//! rechecked after that storage I/O, and only then does the upstream attempt
//! start. No permit leaves the owner turn.

use std::time::Instant;

use super::{
    BrokerError, BrokerService, BrokerSession, provider_credentials::ProviderCredentialStore,
};
use crate::{
    broker::provider_transport::{ProviderTransport, UpstreamResponse},
    launch_protocol::{ChannelState, ErrorCode, ProtocolError},
    launch_receipt::SessionState,
    provider_request::ProviderRequest,
};

impl BrokerService {
    /// Admits one complete Provider request and starts its upstream attempt.
    ///
    /// Run on the Session's broker worker. The broker-held credential for the
    /// granted Provider authenticates the attempt; it never reaches the Session.
    ///
    /// # Errors
    /// Refuses without spending a unit when the launch grants no valid Provider
    /// permission ([`BrokerError::InvalidGrant`], [`BrokerError::Expired`]), the
    /// credential is not configured, or the Session is not running with an
    /// enabled channel. Refuses with [`BrokerError::ProviderBudgetExhausted`]
    /// once the Run's total is spent. After a unit is spent, an expiry or channel
    /// loss discovered before the attempt, an unreachable upstream, or a rejected
    /// key ([`ErrorCode::CredentialUnavailable`]) keeps that unit spent; nothing
    /// is retried.
    pub fn serve_provider_request<F>(
        &self,
        session: &mut BrokerSession,
        credentials: &ProviderCredentialStore,
        transport: &dyn ProviderTransport,
        request: &ProviderRequest,
        now_ms: u64,
        mut verify: F,
    ) -> Result<UpstreamResponse, BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        let clock = Instant::now();
        let elapsed = || {
            now_ms.saturating_add(u64::try_from(clock.elapsed().as_millis()).unwrap_or(u64::MAX))
        };
        let authorization = session.authorization().clone();
        let approved = self
            .authorizations()
            .consumed_for_session(&authorization.session_id)?
            .ok_or(BrokerError::UnknownAuthorization)?;
        if approved.run_id != authorization.run_id
            || approved.envelope_revision != authorization.envelope_revision
        {
            return Err(BrokerError::RequestMismatch);
        }
        let permission = approved
            .provider_requests
            .as_ref()
            .ok_or(BrokerError::InvalidGrant)?;
        let live = |at: u64| {
            permission.valid(at) && at < authorization.expires_at_ms && at < approved.expires_at_ms
        };
        if !live(now_ms) {
            return Err(BrokerError::Expired);
        }
        let handle = credentials.handle(&permission.provider)?;
        let status = self.supervisor_status(session, &mut verify)?;
        if status.state != SessionState::Running
            || status.channel_state != ChannelState::Enabled
            || status.pending_operation.is_some()
            || self.lifecycle.is_quarantined(&authorization.session_id)?
        {
            return Err(BrokerError::InvalidGrant);
        }
        if !live(elapsed()) || session.channel().is_closed() {
            return Err(BrokerError::Expired);
        }
        self.provider_requests.reserve(
            &authorization.run_id,
            &authorization.session_id,
            permission.max_run_requests,
            elapsed(),
        )?;
        // The unit is spent. Storage I/O cannot extend expiry or a lost channel.
        if !live(elapsed()) || session.channel().is_closed() {
            return Err(BrokerError::Expired);
        }
        let response = credentials.with_secret(&handle, |bearer| {
            transport.send(permission, bearer, request)
        })??;
        if matches!(response.status, 401 | 403) {
            return Err(ProtocolError::new(ErrorCode::CredentialUnavailable, None, None).into());
        }
        Ok(response)
    }

    /// Units the Run has spent, including attempts whose outcome is unknown.
    ///
    /// # Errors
    /// Returns [`BrokerError::Storage`] or [`BrokerError::InvalidGrant`] when
    /// the durable ledger is unreadable or holds unexpected entries.
    pub fn provider_requests_spent(&self, run_id: &str) -> Result<u32, BrokerError> {
        self.provider_requests.spent(run_id)
    }
}
