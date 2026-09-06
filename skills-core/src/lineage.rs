//! Supply lineage: where a package's bytes came from on this machine.
//!
//! Lineage answers "how did this arrive here", never "may this be used". It is
//! deliberately kept outside the package: two machines that capture the same
//! candidate produce identical package bytes and different lineage, and a
//! reviewer comparing digests must not have to reason about local paths.
//!
//! Unlike a package, lineage grows. Capturing the same bytes again from a
//! second source appends a record rather than replacing one.

use serde::{Deserialize, Serialize};

use crate::capture::LinkOrigin;

/// The lineage schema this build reads and writes.
pub const LINEAGE_SCHEMA: &str = "louiselm.skills.lineage/1";

/// One local symlink origin, as recorded in lineage.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineageLink {
    /// Package-relative path the content was captured as.
    pub path: String,
    /// Symlink target as declared, before resolution.
    pub declared_target: String,
    /// Fully resolved target at capture time.
    pub resolved_target: String,
    /// Whether the resolved target lay outside the candidate root.
    pub escapes_root: bool,
}

impl From<&LinkOrigin> for LineageLink {
    fn from(origin: &LinkOrigin) -> Self {
        Self {
            path: origin.path.clone(),
            declared_target: origin.declared_target.clone(),
            resolved_target: origin.resolved_target.clone(),
            escapes_root: origin.escapes_root,
        }
    }
}

/// One capture of a package on this machine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureRecord {
    /// Capture time in milliseconds since the Unix epoch, supplied by the caller.
    pub captured_at_ms: u64,
    /// Absolute candidate root the capture read.
    pub source_root: String,
    /// Symlink origins observed during the capture.
    pub links: Vec<LineageLink>,
}

/// The complete local history of one package digest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupplyLineage {
    /// Schema identifier.
    pub schema: String,
    /// Package the lineage describes.
    pub package_digest: String,
    /// Captures in the order they were recorded.
    pub captures: Vec<CaptureRecord>,
}

impl SupplyLineage {
    /// Starts an empty lineage for `package_digest`.
    #[must_use]
    pub fn new(package_digest: &str) -> Self {
        Self {
            schema: LINEAGE_SCHEMA.to_owned(),
            package_digest: package_digest.to_owned(),
            captures: Vec::new(),
        }
    }

    /// Appends `record` unless an identical capture is already recorded.
    pub fn record(&mut self, record: CaptureRecord) {
        if !self.captures.contains(&record) {
            self.captures.push(record);
        }
    }

    /// Returns every recorded link origin that escaped its candidate root.
    #[must_use]
    pub fn escaping_links(&self) -> Vec<&LineageLink> {
        self.captures
            .iter()
            .flat_map(|capture| capture.links.iter())
            .filter(|link| link.escapes_root)
            .collect()
    }
}
