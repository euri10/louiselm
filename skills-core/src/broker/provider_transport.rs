//! Upstream HTTPS for admitted Provider requests (`louiselm-qbr.5.1.3.2`).
//!
//! Mirrors the upstream vendor's `codex-responses-api-proxy`: only the broker holds the
//! key and sets `Authorization`; the confined runtime never sends or sees it.
//! Unlike that proxy, the destination comes from the grant, never the request.

use std::{
    io::Read,
    time::{Duration, Instant},
};

use super::BrokerError;
use crate::{
    dependency_fetch::transport::PinnedResolver,
    provider_request::{ApprovedProviderRequests, ProviderRequest},
};

/// Upstream answer whose body the endpoint relays as it arrives.
pub struct UpstreamResponse {
    /// Upstream HTTP status.
    pub status: u16,
    /// Upstream content type, when stated.
    pub content_type: Option<String>,
    /// Streaming body; the relay owns and drains it.
    pub body: Box<dyn Read + Send>,
}

/// External-I/O boundary for one admitted attempt, shared by real and fake upstreams.
///
/// Called only after the Run unit is durably spent. Implementations must use
/// exactly the granted destination, never follow a redirect, never retry, and
/// bound the whole exchange, body included, by `deadline` so no read blocks
/// past the permission's expiry. The broker also refuses every body read after
/// `deadline` itself; this bound is what unblocks a stalled read.
pub trait ProviderTransport: Send + Sync {
    /// Sends one request and returns once response headers arrive.
    ///
    /// # Errors
    /// Returns [`BrokerError::ProviderUnavailable`] when no response arrived;
    /// the attempt's outcome is then unknown and its unit stays spent.
    fn send(
        &self,
        approved: &ApprovedProviderRequests,
        bearer: &str,
        request: &ProviderRequest,
        deadline: Instant,
    ) -> Result<UpstreamResponse, BrokerError>;
}

/// Broker-owned HTTPS transport. Construction performs no network activity.
#[derive(Debug, Default)]
pub struct HttpsProviderTransport;

impl ProviderTransport for HttpsProviderTransport {
    fn send(
        &self,
        approved: &ApprovedProviderRequests,
        bearer: &str,
        request: &ProviderRequest,
        deadline: Instant,
    ) -> Result<UpstreamResponse, BrokerError> {
        let client = ureq::Agent::with_parts(
            config(deadline.saturating_duration_since(Instant::now())),
            ureq::unversioned::transport::DefaultConnector::default(),
            resolver(approved)?,
        );
        exchange(&client, approved, bearer, request)
    }
}

fn config(remaining: Duration) -> ureq::config::Config {
    ureq::Agent::config_builder()
        .https_only(true)
        .proxy(None)
        .max_redirects(0)
        .max_redirects_will_error(true)
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_send_request(Some(Duration::from_secs(30)))
        .timeout_send_body(Some(Duration::from_mins(2)))
        .timeout_recv_response(Some(Duration::from_mins(5)))
        // Long reasoning streams; the Run's expiry remains the outer bound.
        .timeout_recv_body(Some(Duration::from_hours(1)))
        .timeout_global(Some(remaining))
        .build()
}

fn resolver(approved: &ApprovedProviderRequests) -> Result<PinnedResolver, BrokerError> {
    Ok(PinnedResolver {
        origin: url::Url::parse(&approved.upstream).map_err(|_| BrokerError::InvalidGrant)?,
        addresses: approved.addresses.clone(),
    })
}

fn exchange(
    client: &ureq::Agent,
    approved: &ApprovedProviderRequests,
    bearer: &str,
    request: &ProviderRequest,
) -> Result<UpstreamResponse, BrokerError> {
    let mut outgoing = client.post(&approved.upstream);
    for (name, value) in &request.headers {
        outgoing = outgoing.header(name.as_str(), value.as_str());
    }
    let response = outgoing
        .header("authorization", format!("Bearer {bearer}"))
        .send(&request.body[..])
        .map_err(|_| BrokerError::ProviderUnavailable)?;
    let status = response.status().as_u16();
    // Not followed (no redirects configured), and not relayed either: the
    // runtime must never learn an alternative destination from the upstream.
    if response.status().is_redirection() || response.headers().contains_key("content-encoding") {
        return Err(BrokerError::ProviderUnavailable);
    }
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    Ok(UpstreamResponse {
        status,
        content_type,
        body: Box::new(response.into_body().into_reader()),
    })
}

#[cfg(test)]
#[path = "provider_transport_tests.rs"]
mod tests;
