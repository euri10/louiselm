//! Brokered Provider requests: the grant a controller issues and the one
//! reviewed operation a confined Agent runtime may ask the broker to perform.
//!
//! The Session never chooses a destination or holds a credential. It sends a
//! stock Responses request to its local broker endpoint; the broker validates
//! the complete request, spends one unit of the Run's durable budget, and makes
//! the upstream call itself (`louiselm-qbr.5.1.3.2`).

use std::{fmt, net::IpAddr};

use serde::{Deserialize, Serialize};

pub mod disclosure;
mod framing;

pub use framing::Frames;

/// Largest total request budget one Run may be granted.
pub const MAX_RUN_REQUESTS: u32 = 10_000;

/// Responses API `reasoning.effort` values, in increasing cost order.
///
/// Values the API does not define (for example Codex's subscription-only
/// `ultra`) have no place in this order and are always refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    /// No reasoning.
    None,
    /// Minimal reasoning.
    Minimal,
    /// Low reasoning.
    Low,
    /// Medium reasoning.
    Medium,
    /// High reasoning.
    High,
    /// Extra-high reasoning.
    Xhigh,
    /// Maximum reasoning.
    Max,
}

impl ReasoningEffort {
    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "none" => Self::None,
            "minimal" => Self::Minimal,
            "low" => Self::Low,
            "medium" => Self::Medium,
            "high" => Self::High,
            "xhigh" => Self::Xhigh,
            "max" => Self::Max,
            _ => return None,
        })
    }
}

/// Controller-issued permission to spend a Run's shared Provider request budget.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovedProviderRequests {
    /// Exact reviewed metadata profile digest approved through the existing policy.
    /// No default: old or omitted disclosure approval cannot authorize requests.
    pub disclosure_profile: String,
    /// Configured Provider whose broker-held credential authenticates requests.
    pub provider: String,
    /// Exact HTTPS URL the broker sends every admitted request to.
    pub upstream: String,
    /// Exact IP destinations selected by the trusted controller before start.
    /// The broker performs no DNS lookup and cannot follow DNS rebinding.
    pub addresses: Vec<IpAddr>,
    /// Non-refundable total upstream attempts shared by every Session of the Run.
    /// Every grant for one Run must state the same total.
    pub max_run_requests: u32,
    /// Sorted unique exact Model ids a request may name (1..=16).
    pub models: Vec<String>,
    /// Highest reasoning effort a request may state. Raised only by a new
    /// grant, never by the request.
    pub max_effort: ReasoningEffort,
    /// Exclusive absolute expiry; retries never renew permission.
    pub expires_at_ms: u64,
}

impl ApprovedProviderRequests {
    /// Displays the exact metadata scope through the existing permission flow.
    /// Does not authorize, auto-upgrade a profile, or require a human prompt.
    /// # Errors
    /// Refuses unsupported or missing profile approval without echoing its value.
    pub fn disclosure_notice(&self) -> Result<String, crate::launch_protocol::ProtocolError> {
        disclosure::notice(&self.disclosure_profile)
    }

    /// Checks bounded scope, destination and lifetime without external effects.
    #[must_use]
    pub fn valid(&self, now_ms: u64) -> bool {
        let provider = !self.provider.is_empty()
            && self.provider.len() <= 64
            && self
                .provider
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
        let upstream = url::Url::parse(&self.upstream).is_ok_and(|url| {
            url.scheme() == "https"
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
                && url.as_str() == self.upstream
                && url.path() == "/v1/responses"
        });
        let mut unique = self.addresses.clone();
        unique.sort_unstable();
        unique.dedup();
        provider
            && self.disclosure_profile == disclosure::profile_digest()
            && upstream
            && self.upstream.len() <= 2048
            && !self.addresses.is_empty()
            && self.addresses.len() <= 16
            && unique.len() == self.addresses.len()
            && self
                .addresses
                .iter()
                .all(|address| !address.is_unspecified() && !address.is_multicast())
            && (1..=MAX_RUN_REQUESTS).contains(&self.max_run_requests)
            && (1..=16).contains(&self.models.len())
            && self.models.windows(2).all(|pair| pair[0] < pair[1])
            && self
                .models
                .iter()
                .all(|model| !model.is_empty() && model.len() <= 128)
            && now_ms < self.expires_at_ms
    }

    /// Whether a request naming `model` at `effort` is inside this grant.
    ///
    /// A request must state its effort: the Provider's default is not a
    /// value this grant approved.
    #[must_use]
    pub fn permits(&self, model: &str, effort: Option<&str>) -> bool {
        self.models
            .binary_search_by(|allowed| allowed.as_str().cmp(model))
            .is_ok()
            && effort
                .and_then(ReasoningEffort::parse)
                .is_some_and(|effort| effort <= self.max_effort)
    }
}

/// One complete, reviewed Responses request received from a confined runtime.
///
/// Holds prompt content, so its [`Debug`] reports only sizes and the policy
/// fields the broker reads.
pub struct ProviderRequest {
    /// Requested Model, read for policy and attribution.
    pub model: String,
    /// Requested reasoning effort, when the request states one.
    pub effort: Option<String>,
    /// Reviewed headers to forward upstream, lower-case names, original order.
    pub headers: Vec<(String, String)>,
    /// Exact JSON body to forward upstream.
    pub body: Vec<u8>,
}

impl ProviderRequest {
    /// Validates metadata at the broker entrypoint, including manually built requests.
    /// Does not rewrite or retain any metadata values.
    /// # Errors
    /// Refuses unreviewed or malformed metadata with a safe typed disclosure error.
    pub fn validate_disclosure(&self) -> Result<(), crate::launch_protocol::ProtocolError> {
        framing::policy_fields(&self.body)?;
        disclosure::validate(&self.body, &self.headers)
    }
}

impl fmt::Debug for ProviderRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderRequest")
            .field("model", &self.model)
            .field("effort", &self.effort)
            .field("headers", &self.headers.len())
            .field("body_bytes", &self.body.len())
            .finish()
    }
}

#[cfg(test)]
#[path = "provider_request_tests.rs"]
mod tests;
