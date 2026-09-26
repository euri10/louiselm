//! Safe, portable provenance for workspace outputs.

use crate::{Digest, workspace::WorkspaceError};
use serde::{Deserialize, Serialize};

const SCHEMA: &str = "louiselm.workspace.output-provenance/1";

/// Current trust result for Session-authored workspace output.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputProvenanceCode {
    /// The broker currently has no Session output taint for this producer.
    Untainted,
    /// A canonical Session output taint applies to the producer's full lifetime.
    SessionOutputTainted,
    /// Required provenance evidence is absent or cannot establish a clean result.
    Unknown,
}

/// Bounded projection; it contains no Session identity or source content.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputProvenance {
    /// Closed versioned format.
    pub schema: String,
    /// Stable result code.
    pub code: OutputProvenanceCode,
    /// Canonical broker taint digest when the producer is tainted.
    pub taint_digest: Option<String>,
    /// Exact clean-review references, empty until separately authorized.
    pub clean_review_refs: Vec<String>,
}

impl OutputProvenance {
    pub(crate) fn untainted() -> Self {
        Self::new(OutputProvenanceCode::Untainted, None)
    }

    pub(crate) fn tainted(digest: &str) -> Self {
        Self::new(OutputProvenanceCode::SessionOutputTainted, Some(digest))
    }

    /// Constructs the fail-closed state for output without trusted provenance.
    #[must_use]
    pub fn unknown() -> Self {
        Self::new(OutputProvenanceCode::Unknown, None)
    }

    fn new(code: OutputProvenanceCode, digest: Option<&str>) -> Self {
        Self {
            schema: SCHEMA.into(),
            code,
            taint_digest: digest.map(str::to_owned),
            clean_review_refs: Vec::new(),
        }
    }

    /// Checks untrusted producer metadata before it can be propagated.
    /// Review references are refused here; the broker validates its own exact-use
    /// review record when publishing a promotion result.
    /// # Errors
    /// Refuses unsupported, contradictory or noncanonical metadata.
    pub fn validate(&self) -> Result<(), WorkspaceError> {
        if self.schema != SCHEMA || !self.clean_review_refs.is_empty() {
            return Err(WorkspaceError::Invalid(
                "invalid workspace output provenance",
            ));
        }
        match (&self.code, &self.taint_digest) {
            (OutputProvenanceCode::SessionOutputTainted, Some(digest))
                if Digest::parse(digest).is_ok_and(|parsed| parsed.to_string() == *digest) =>
            {
                Ok(())
            }
            (OutputProvenanceCode::Untainted | OutputProvenanceCode::Unknown, None) => Ok(()),
            _ => Err(WorkspaceError::Invalid(
                "invalid workspace output provenance",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portable_metadata_refuses_contradictions_and_unauthorized_review_references() {
        assert!(
            OutputProvenance::tainted("not-a-digest")
                .validate()
                .is_err()
        );
        let mut unknown = OutputProvenance::unknown();
        unknown.taint_digest = Some(Digest::of(b"unexpected").to_string());
        assert!(unknown.validate().is_err());
        unknown.taint_digest = None;
        unknown.clean_review_refs.push("not-authorized".into());
        assert!(unknown.validate().is_err());
    }
}
