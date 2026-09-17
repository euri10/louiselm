//! Bounded HTTPS downloads. No registry resolution, redirects or archive execution.

use std::{
    io::Read,
    sync::Arc,
    thread::{self, JoinHandle},
    time::Duration,
};

use super::{FetchError, FetchPermit, Source};
use serde::{Deserialize, Serialize};

#[cfg(test)]
#[path = "transport_tests.rs"]
mod tests;

/// Completion on the transport worker; the owner must recheck authority before publication.
pub type DownloadCompletion = Box<dyn FnOnce(Result<(FetchPermit, Vec<u8>), FetchError>) + Send>;

/// Asynchronous external-I/O boundary, shared by actual and fake registry transports.
/// Implementations must never resolve a different name or follow a redirect.
pub trait DownloadTransport: Send + Sync {
    /// Starts one bounded attempt for an already authorized exact candidate.
    /// The caller owns and must join the returned worker before disposing state.
    /// # Errors
    /// Refuses unsupported destinations or worker startup; later failures use `complete`.
    fn start(
        &self,
        permit: FetchPermit,
        complete: DownloadCompletion,
    ) -> Result<JoinHandle<()>, FetchError>;
}

/// Fixed registry endpoint known before a Session starts. No remote index lookup.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryEndpoint {
    /// Exact identity referenced by typed registry candidates.
    pub registry: String,
    /// HTTPS archive template with only `{name}` and `{version}` substitutions.
    pub archive_template: String,
    /// Exact IP destinations selected by the trusted controller before start.
    /// The fetch worker performs no DNS lookup and cannot follow DNS rebinding.
    pub addresses: Vec<std::net::IpAddr>,
}

/// Broker-owned HTTPS transport. Construction performs no network activity.
pub struct HttpsTransport {
    registries: Arc<Vec<RegistryEndpoint>>,
    timeout: Duration,
}

impl HttpsTransport {
    /// Configures exact archive endpoints and a total per-attempt deadline.
    /// No proxy, credential, cookie, user configuration or content decoder is used.
    /// # Errors
    /// Rejects duplicate registries, malformed templates or deadlines outside 1..=30 seconds.
    pub fn new(registries: Vec<RegistryEndpoint>, timeout: Duration) -> Result<Self, FetchError> {
        if timeout < Duration::from_secs(1)
            || timeout > Duration::from_secs(30)
            || registries.len() > 32
        {
            return Err(FetchError::Transport);
        }
        let mut names = std::collections::BTreeSet::new();
        for registry in &registries {
            if registry.registry.is_empty()
                || !names.insert(&registry.registry)
                || registry.addresses.is_empty()
                || registry.addresses.len() > 16
                || registry
                    .addresses
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    != registry.addresses.len()
                || registry
                    .addresses
                    .iter()
                    .any(|address| address.is_unspecified() || address.is_multicast())
            {
                return Err(FetchError::Transport);
            }
            if registry
                .archive_template
                .split('/')
                .nth(2)
                .is_none_or(|authority| authority.contains(['{', '}']))
            {
                return Err(FetchError::Transport);
            }
            let sample = registry
                .archive_template
                .replace("{name}", "example")
                .replace("{version}", "1.2.3");
            validate_url(&sample)?;
            if sample.contains(['{', '}']) {
                return Err(FetchError::Transport);
            }
        }
        Ok(Self {
            registries: Arc::new(registries),
            timeout,
        })
    }
}

impl DownloadTransport for HttpsTransport {
    fn start(
        &self,
        permit: FetchPermit,
        complete: DownloadCompletion,
    ) -> Result<JoinHandle<()>, FetchError> {
        let candidate = permit.candidate();
        let target = match &candidate.source {
            Source::Registry { registry } => {
                let endpoint = self
                    .registries
                    .iter()
                    .find(|entry| entry.registry == *registry)
                    .ok_or(FetchError::Transport)?;
                endpoint
                    .archive_template
                    .replace("{name}", &candidate.name)
                    .replace("{version}", &candidate.version)
            }
            Source::Url { url } => url.clone(),
            // Approval is necessary but does not authorize running Git, helpers
            // or an unknown protocol. Such sources need a reviewed HTTPS artifact.
            Source::Git { .. } | Source::Other { .. } => return Err(FetchError::Transport),
        };
        validate_url(&target)?;
        let origin = url::Url::parse(&target).map_err(|_| FetchError::Transport)?;
        let endpoint = self
            .registries
            .iter()
            .find(|endpoint| {
                if let Source::Registry { registry } = &candidate.source {
                    return endpoint.registry == *registry;
                }
                url::Url::parse(
                    &endpoint
                        .archive_template
                        .replace("{name}", "example")
                        .replace("{version}", "1.2.3"),
                )
                .is_ok_and(|url| url.origin() == origin.origin())
            })
            .ok_or(FetchError::Transport)?;
        let addresses = endpoint.addresses.clone();
        let timeout = self.timeout;
        thread::Builder::new()
            .name("louiselm-dependency-fetch".into())
            .spawn(move || {
                let result = permit
                    .remaining()
                    .and_then(|remaining| {
                        let client = ureq::Agent::with_parts(
                            config(timeout.min(remaining)),
                            ureq::unversioned::transport::DefaultConnector::default(),
                            PinnedResolver { origin, addresses },
                        );
                        download(&client, &target, &permit)
                    })
                    .map(|bytes| (permit, bytes));
                complete(result);
            })
            .map_err(FetchError::Io)
    }
}

fn validate_url(value: &str) -> Result<(), FetchError> {
    let parsed = url::Url::parse(value).map_err(|_| FetchError::Transport)?;
    if value.len() > 4096
        || parsed.scheme() != "https"
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.fragment().is_some()
        || parsed.as_str() != value
    {
        return Err(FetchError::Transport);
    }
    Ok(())
}

fn config(timeout: Duration) -> ureq::config::Config {
    ureq::Agent::config_builder()
        .https_only(true)
        .proxy(None)
        .max_redirects(0)
        .max_redirects_will_error(true)
        .timeout_global(Some(timeout))
        .build()
}

#[derive(Debug)]
struct PinnedResolver {
    origin: url::Url,
    addresses: Vec<std::net::IpAddr>,
}

impl ureq::unversioned::resolver::Resolver for PinnedResolver {
    fn resolve(
        &self,
        uri: &ureq::http::Uri,
        _config: &ureq::config::Config,
        _timeout: ureq::unversioned::transport::NextTimeout,
    ) -> Result<ureq::unversioned::resolver::ResolvedSocketAddrs, ureq::Error> {
        if uri.scheme_str() != Some("https")
            || uri.host() != self.origin.host_str()
            || uri.port_u16().unwrap_or(443) != self.origin.port_or_known_default().unwrap_or(443)
        {
            return Err(ureq::Error::HostNotFound);
        }
        let mut addresses = self.empty();
        for address in &self.addresses {
            addresses.push(std::net::SocketAddr::new(
                *address,
                uri.port_u16().unwrap_or(443),
            ));
        }
        Ok(addresses)
    }
}

fn download(
    client: &ureq::Agent,
    target: &str,
    permit: &FetchPermit,
) -> Result<Vec<u8>, FetchError> {
    permit.check_active()?;
    let mut response = client
        .get(target)
        .call()
        .map_err(|error| FetchError::Http(Box::new(error)))?;
    if response.status() != 200 || response.headers().contains_key("content-encoding") {
        return Err(FetchError::Transport);
    }
    let mut bytes = Vec::new();
    let mut reader = response.body_mut().as_reader();
    let mut buffer = [0; 8192];
    loop {
        permit.check_active()?;
        let count = reader.read(&mut buffer).map_err(FetchError::Io)?;
        if count == 0 {
            break;
        }
        if bytes.len() as u64 + count as u64 > permit.max_bytes() {
            return Err(FetchError::Integrity);
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    Ok(bytes)
}
