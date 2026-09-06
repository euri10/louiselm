//! Assessment: a Model's advisory opinion about a package.
//!
//! Assessment has no authority. It never admits, blocks, or ranks anything; it
//! is one input a reviewer may read alongside Inspection, which is the part
//! that actually examined the bytes. Three properties keep it that way:
//!
//! * It runs with an empty capability envelope. An assessor that is offered
//!   any capability is refused before it is called, not trusted to decline.
//! * It is keyed to the exact package digest, Model, and prompt version. An
//!   opinion about anything else is treated as no opinion at all, never as a
//!   stale one worth showing.
//! * Its verdict is about whether effects are bounded, not whether a skill is
//!   safe — a question no model is in a position to answer.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The Assessment schema this build reads and writes.
pub const ASSESSMENT_SCHEMA: &str = "louiselm.skills.assessment/1";

/// What an assessor concluded about a package's effects.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// The described effects stay inside the skill's stated purpose.
    Bounded,
    /// The described effects reach beyond the skill's stated purpose.
    Unbounded,
    /// The assessor could not form an opinion.
    Undetermined,
}

/// What an Assessment is an opinion *about*.
///
/// Every field is part of the key. Change any one and the opinion no longer
/// describes the thing being reviewed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssessmentKey {
    /// Digest of the package assessed.
    pub package_digest: String,
    /// Model that produced the opinion.
    pub model: String,
    /// Version of the prompt the Model was given.
    pub prompt_version: String,
}

/// The capabilities an assessor is granted; always none.
///
/// The struct exists so that "empty" is asserted rather than assumed, and so a
/// later change that grants a capability has to say so in a type the refusal
/// path already reads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityEnvelope {
    /// Whether the assessor may reach the network.
    pub network: bool,
    /// Whether the assessor may read the filesystem.
    pub filesystem: bool,
    /// Whether the assessor may start processes.
    pub process: bool,
}

impl CapabilityEnvelope {
    /// Returns the only envelope an assessor may run under.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Reports whether the envelope grants nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        !self.network && !self.filesystem && !self.process
    }

    /// Names the granted capabilities, for a refusal message.
    #[must_use]
    pub fn granted(&self) -> Vec<&'static str> {
        let mut granted = Vec::new();
        if self.network {
            granted.push("network");
        }
        if self.filesystem {
            granted.push("filesystem");
        }
        if self.process {
            granted.push("process");
        }
        granted
    }
}

/// What an assessor is asked.
#[derive(Clone, Debug)]
pub struct AssessmentRequest {
    /// Package, Model, and prompt version this opinion will be keyed to.
    pub key: AssessmentKey,
    /// Capabilities offered to the assessor; anything but empty is refused.
    pub envelope: CapabilityEnvelope,
    /// The material the assessor reads, drawn from the package's own bytes.
    pub excerpt: String,
}

/// A recorded advisory opinion.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assessment {
    /// Schema identifier.
    pub schema: String,
    /// What the opinion is about.
    pub key: AssessmentKey,
    /// The opinion.
    pub verdict: Verdict,
    /// The assessor's reasoning, shown to the reviewer as advisory text.
    pub rationale: String,
    /// When the opinion was produced, supplied by the caller.
    pub produced_at_ms: u64,
}

impl Assessment {
    /// Returns the assessment when it describes `key`, and nothing otherwise.
    ///
    /// A mismatch is absence, not staleness: showing a reviewer an opinion
    /// about different bytes is worse than showing none.
    #[must_use]
    pub fn current_for(&self, key: &AssessmentKey) -> Option<&Self> {
        (&self.key == key).then_some(self)
    }
}

/// An Assessment that could not be produced.
#[derive(Debug, Error)]
pub enum AssessmentError {
    /// The assessor was offered capabilities, so it was never called.
    #[error("an assessor may hold no capabilities; this request granted {0}")]
    EnvelopeNotEmpty(String),
    /// The assessor failed.
    #[error("assessor failed: {0}")]
    Failed(String),
}

/// A source of advisory opinions.
pub trait Assessor {
    /// Returns a verdict and its rationale for `request`.
    ///
    /// # Errors
    /// Returns an assessor-specific failure when no advisory opinion can be produced.
    fn assess(&self, request: &AssessmentRequest) -> Result<(Verdict, String), AssessmentError>;
}

/// Runs `assessor` against `request`, refusing any granted capability.
///
/// # Errors
/// Refuses a nonempty capability envelope before calling the assessor; otherwise propagates the assessor's failure.
pub fn run(
    assessor: &dyn Assessor,
    request: &AssessmentRequest,
    produced_at_ms: u64,
) -> Result<Assessment, AssessmentError> {
    if !request.envelope.is_empty() {
        return Err(AssessmentError::EnvelopeNotEmpty(
            request.envelope.granted().join(", "),
        ));
    }
    let (verdict, rationale) = assessor.assess(request)?;
    Ok(Assessment {
        schema: ASSESSMENT_SCHEMA.to_owned(),
        key: request.key.clone(),
        verdict,
        rationale,
        produced_at_ms,
    })
}
