//! Upstream HTTPS for admitted Provider requests (`louiselm-qbr.5.1.3.2`).
//!
//! Mirrors the upstream vendor's `codex-responses-api-proxy`: only the broker holds the
//! key and sets `Authorization`; the confined runtime never sends or sees it.
//! Unlike that proxy, the destination comes from the grant, never the request.

use std::{
    fmt,
    io::{self, Read, Write},
    sync::Mutex,
    time::{Duration, Instant},
};

use super::{BrokerError, GuardedUpstream};
use crate::{
    dependency_fetch::transport::PinnedResolver,
    provider_request::{ApprovedProviderRequests, ProviderRequest},
};
use ureq::unversioned::transport::{
    Buffers, ConnectionDetails, Connector, LazyBuffers, NextTimeout, RustlsConnector, Transport,
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

/// One broker-owned HTTPS attempt over an authenticated supervisor socket.
/// There is deliberately no default constructor or ambient connector.
pub(crate) struct GuardedHttpsProviderTransport {
    socket: Mutex<Option<GuardedUpstream>>,
    #[cfg(test)]
    test_root: Option<ureq::tls::Certificate<'static>>,
}

impl GuardedHttpsProviderTransport {
    pub(crate) fn new(socket: GuardedUpstream) -> Self {
        Self {
            socket: Mutex::new(Some(socket)),
            #[cfg(test)]
            test_root: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_test_root(
        socket: GuardedUpstream,
        root: ureq::tls::Certificate<'static>,
    ) -> Self {
        Self {
            socket: Mutex::new(Some(socket)),
            test_root: Some(root),
        }
    }
}

impl ProviderTransport for GuardedHttpsProviderTransport {
    fn send(
        &self,
        approved: &ApprovedProviderRequests,
        bearer: &str,
        request: &ProviderRequest,
        deadline: Instant,
    ) -> Result<UpstreamResponse, BrokerError> {
        let socket = self
            .socket
            .lock()
            .map_err(|_| BrokerError::ProviderUnavailable)?
            .take()
            .ok_or(BrokerError::ProviderUnavailable)?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        #[cfg(test)]
        let settings = config(remaining, self.test_root.as_ref());
        #[cfg(not(test))]
        let settings = config(remaining, None);
        let client = ureq::Agent::with_parts(
            settings,
            GuardedConnector {
                socket: Mutex::new(Some(socket)),
                deadline,
            }
            .chain(RustlsConnector::default()),
            resolver(approved)?,
        );
        exchange(&client, approved, bearer, request)
    }
}

struct GuardedConnector {
    socket: Mutex<Option<GuardedUpstream>>,
    deadline: Instant,
}

impl fmt::Debug for GuardedConnector {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("GuardedConnector")
    }
}

impl Connector for GuardedConnector {
    type Out = GuardedTransport;

    fn connect(
        &self,
        details: &ConnectionDetails<'_>,
        _chained: Option<()>,
    ) -> Result<Option<Self::Out>, ureq::Error> {
        let socket = self
            .socket
            .lock()
            .map_err(|_| ureq::Error::ConnectionFailed)?
            .take()
            .ok_or(ureq::Error::ConnectionFailed)?;
        if !details.addrs.contains(&socket.evidence().destination) {
            return Err(ureq::Error::Io(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "guarded destination differs from granted address",
            )));
        }
        Ok(Some(GuardedTransport {
            socket,
            buffers: LazyBuffers::new(8192, 8192),
            deadline: self.deadline,
        }))
    }
}

struct GuardedTransport {
    socket: GuardedUpstream,
    buffers: LazyBuffers,
    deadline: Instant,
}

impl fmt::Debug for GuardedTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("GuardedTransport")
    }
}

impl GuardedTransport {
    fn timeout(&self, next: NextTimeout) -> Result<Duration, ureq::Error> {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        let duration = next
            .not_zero()
            .map_or(remaining, |timeout| remaining.min(*timeout));
        if duration.is_zero() {
            Err(ureq::Error::Timeout(next.reason))
        } else {
            Ok(duration)
        }
    }
}

impl Transport for GuardedTransport {
    fn buffers(&mut self) -> &mut dyn Buffers {
        &mut self.buffers
    }

    fn transmit_output(&mut self, amount: usize, next: NextTimeout) -> Result<(), ureq::Error> {
        self.socket.set_timeout(self.timeout(next)?)?;
        self.socket
            .write_all(&self.buffers.output()[..amount])
            .map_err(|error| match error.kind() {
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => {
                    ureq::Error::Timeout(next.reason)
                }
                _ => error.into(),
            })
    }

    fn await_input(&mut self, next: NextTimeout) -> Result<bool, ureq::Error> {
        self.socket.set_timeout(self.timeout(next)?)?;
        let count =
            self.socket
                .read(self.buffers.input_append_buf())
                .map_err(|error| match error.kind() {
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => {
                        ureq::Error::Timeout(next.reason)
                    }
                    _ => error.into(),
                })?;
        self.buffers.input_appended(count);
        Ok(count > 0)
    }

    fn is_open(&mut self) -> bool {
        false
    }
}

fn config(
    remaining: Duration,
    root: Option<&ureq::tls::Certificate<'static>>,
) -> ureq::config::Config {
    let mut builder = ureq::Agent::config_builder()
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
        .timeout_global(Some(remaining));
    if let Some(root) = root {
        builder = builder.tls_config(
            ureq::tls::TlsConfig::builder()
                .root_certs(ureq::tls::RootCerts::new_with_certs(std::slice::from_ref(
                    root,
                )))
                .build(),
        );
    }
    builder.build()
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
