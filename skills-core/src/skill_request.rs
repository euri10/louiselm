//! Exact requests to ask for Skill Admission; never Admission authority.

use crate::Digest;
use serde::{Deserialize, Serialize};

/// Subject kind selected within the authenticated Agent's own binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillSubject {
    /// This Session only.
    Session,
    /// The Run already bound to this Session.
    Run,
}

/// Immutable request content. No paths, prose or caller-selected subject IDs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillRequest {
    /// Client retry identity; changing content requires a fresh identity.
    pub request_id: String,
    /// Subject derived from the authenticated Session's binding.
    pub subject: SkillSubject,
    /// Sorted, unique canonical SHA-256 package digests.
    pub packages: Vec<String>,
    /// Sorted, unique intended configured Agent names.
    pub agents: Vec<String>,
}

impl SkillRequest {
    /// Checks bounded, exact content without accessing files or admitting supply.
    #[must_use]
    pub fn valid(&self) -> bool {
        identifier(&self.request_id)
            && names(&self.agents)
            && !self.packages.is_empty()
            && self.packages.len() <= 32
            && self.packages.windows(2).all(|pair| pair[0] < pair[1])
            && self
                .packages
                .iter()
                .all(|value| Digest::parse(value).is_ok_and(|digest| digest.to_string() == *value))
    }
}

/// Explicit trusted envelope permission to request (not perform) Admission.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovedSkillRequests {
    /// Exact Agent scope the requester may ask to admit packages for.
    pub agents: Vec<String>,
    /// Whether this approval includes the authenticated Session's owning Run.
    pub allow_run: bool,
    /// Exclusive absolute expiry; retries never renew it.
    pub expires_at_ms: u64,
}

impl ApprovedSkillRequests {
    /// Validates explicit scope and its original lifetime.
    #[must_use]
    pub fn valid(&self, now_ms: u64) -> bool {
        names(&self.agents) && now_ms < self.expires_at_ms
    }

    /// Checks attenuation against the existing approved envelope.
    #[must_use]
    pub fn permits(&self, request: &SkillRequest, now_ms: u64) -> bool {
        self.valid(now_ms)
            && request.valid()
            && (request.subject == SkillSubject::Session || self.allow_run)
            && request
                .agents
                .iter()
                .all(|agent| self.agents.contains(agent))
    }
}

/// Durable request outcome; no variant changes launch or supply authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillRequestOutcome {
    /// Waiting for exact signed Admission or an operator decision.
    Pending,
    /// Exact signed Admission persisted; not witnessed, activated or usable supply.
    Approved,
    /// Explicit operator rejection.
    Rejected,
    /// Operator cancellation or authoritative subject end.
    Cancelled,
}

/// Bounded result returned to the exact requesting subject.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillRequestStatus {
    /// Original caller retry identity.
    pub request_id: String,
    /// Broker-created stable Attention operation UUID.
    pub operation_id: String,
    /// Latest durable result; a terminal result never becomes pending again.
    pub outcome: SkillRequestOutcome,
    /// Exact package set, also used by the trusted Admission CLI before signing.
    pub packages: Vec<String>,
    /// Exact literal Agent scope for every requested package.
    pub agents: Vec<String>,
    /// Verified signed Generation, present only for approved outcomes.
    pub admission: Option<String>,
}

impl SkillRequestStatus {
    /// Checks the bounded retry identity and canonical operation UUID.
    #[must_use]
    pub fn valid(&self) -> bool {
        identifier(&self.request_id)
            && names(&self.agents)
            && !self.packages.is_empty()
            && self.packages.len() <= 32
            && self.packages.windows(2).all(|pair| pair[0] < pair[1])
            && self
                .packages
                .iter()
                .all(|value| Digest::parse(value).is_ok_and(|digest| digest.to_string() == *value))
            && match (&self.outcome, &self.admission) {
                (SkillRequestOutcome::Approved, Some(digest)) => {
                    Digest::parse(digest).is_ok_and(|parsed| parsed.to_string() == *digest)
                }
                (SkillRequestOutcome::Approved, None) | (_, Some(_)) => false,
                (_, None) => true,
            }
            && self.operation_id.len() == 36
            && self.operation_id.bytes().enumerate().all(|(index, byte)| {
                if matches!(index, 8 | 13 | 18 | 23) {
                    byte == b'-'
                } else {
                    byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
                }
            })
    }
}

fn names(values: &[String]) -> bool {
    !values.is_empty()
        && values.len() <= 32
        && values.windows(2).all(|pair| pair[0] < pair[1])
        && values.iter().all(|value| identifier(value))
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}
