//! The Dossier: the canonical review artifact, recomputed from package bytes.
//!
//! One normalized state feeds both the human render and the robot view, so a
//! reviewer and an Agent cannot be shown different facts about the same
//! package. Building a Dossier always re-reads and re-hashes the stored bytes,
//! re-runs Inspection, and re-derives the diff. Nothing recorded earlier — by
//! this tool or by an Agent — is accepted as authority; a recorded digest is
//! a claim to be checked, never a conclusion.

use serde::Serialize;
use thiserror::Error;

use crate::{
    assessment::{Assessment, AssessmentKey},
    canonical::Digest,
    diff::PackageDiff,
    inspect::Inspection,
    lineage::SupplyLineage,
    policy::Policy,
    scan,
    store::{Store, StoreError},
};

/// The Dossier schema this build produces.
pub const DOSSIER_SCHEMA: &str = "louiselm.skills.dossier/1";

/// The version of the tool that produced a Dossier.
pub const TOOL_VERSION: &str = env!("CARGO_PKG_VERSION");

/// How deeply a human claims to have reviewed a package.
///
/// This is a recorded claim, not a proof: nothing here is established by the
/// tool, and no cryptography later makes it true.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDepth {
    /// No claim was made.
    Unstated,
    /// The reviewer looked over the Dossier without reading every file.
    Skimmed,
    /// The reviewer read every file in the package.
    Read,
    /// The reviewer read the package and reproduced what it does.
    Reproduced,
}

impl ReviewDepth {
    /// Parses the value accepted on the command line.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "unstated" => Some(Self::Unstated),
            "skimmed" => Some(Self::Skimmed),
            "read" => Some(Self::Read),
            "reproduced" => Some(Self::Reproduced),
            _ => None,
        }
    }

    /// Returns the value as it is spelled on the command line.
    pub fn name(self) -> &'static str {
        match self {
            Self::Unstated => "unstated",
            Self::Skimmed => "skimmed",
            Self::Read => "read",
            Self::Reproduced => "reproduced",
        }
    }
}

/// Whether a current Assessment exists for what is being reviewed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AssessmentState {
    /// No Assessment was asked for, or none is recorded.
    Absent,
    /// A recorded Assessment describes exactly this package, Model, and prompt.
    Current,
    /// An Assessment is recorded for other bytes, Model, or prompt, so it is
    /// treated as no Assessment at all.
    Superseded,
}

/// What a reviewer or Agent should do next, as a typed instruction.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct NextAction {
    /// Stable action identifier.
    pub id: String,
    /// Escaped reviewer-facing sentence.
    pub detail: String,
}

/// What the manifest says about the package as a whole.
#[derive(Clone, Debug, Serialize)]
pub struct PackageSummary {
    /// Package digest.
    pub digest: String,
    /// Number of files.
    pub entry_count: usize,
    /// Total content size in bytes.
    pub total_bytes: u64,
    /// Skill name from frontmatter, when the package has a usable one.
    pub skill_name: Option<String>,
    /// Skill description from frontmatter, when it has one.
    pub skill_description: Option<String>,
}

/// The outcome of recomputing the package from its stored bytes.
#[derive(Clone, Debug, Serialize)]
pub struct VerificationSummary {
    /// Whether the stored bytes are exactly what the digest names.
    pub intact: bool,
    /// Digest recomputed from the stored manifest bytes.
    pub recomputed_digest: String,
    /// Escaped description of each mismatch found.
    pub failures: Vec<String>,
}

/// The rules that produced the findings.
#[derive(Clone, Debug, Serialize)]
pub struct PolicySummary {
    /// Policy version.
    pub version: String,
    /// Content address of the exact policy bytes.
    pub digest: String,
    /// Unicode profile version.
    pub unicode_profile_version: String,
}

/// A Dossier that could not be built.
#[derive(Debug, Error)]
pub enum DossierError {
    /// The store could not produce the package or its records.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// What to build a Dossier about.
#[derive(Clone, Debug)]
pub struct DossierRequest<'a> {
    digest: &'a Digest,
    against: Option<&'a Digest>,
    review_depth: ReviewDepth,
    assessment_key: Option<(String, String)>,
}

impl<'a> DossierRequest<'a> {
    /// Reviews the package named by `digest`.
    pub fn new(digest: &'a Digest) -> Self {
        Self {
            digest,
            against: None,
            review_depth: ReviewDepth::Unstated,
            assessment_key: None,
        }
    }

    /// Compares the package against the one it would replace.
    pub fn against(mut self, base: &'a Digest) -> Self {
        self.against = Some(base);
        self
    }

    /// Records the reviewer's claimed review depth.
    pub fn with_review_depth(mut self, depth: ReviewDepth) -> Self {
        self.review_depth = depth;
        self
    }

    /// Asks for the Assessment produced by `model` under `prompt_version`.
    pub fn with_assessment_key(mut self, model: &str, prompt_version: &str) -> Self {
        self.assessment_key = Some((model.to_owned(), prompt_version.to_owned()));
        self
    }
}

/// The complete normalized review state for one package.
#[derive(Clone, Debug, Serialize)]
pub struct Dossier {
    /// Schema identifier.
    pub schema: String,
    /// Version of the tool that produced this Dossier.
    pub tool_version: String,
    /// Manifest-level summary.
    pub package: PackageSummary,
    /// Result of recomputing the package from stored bytes.
    pub verification: VerificationSummary,
    /// Rules that produced the findings.
    pub policy: PolicySummary,
    /// Deterministic Inspection of the package's bytes.
    pub inspection: Inspection,
    /// Executable inventory, in package order.
    pub executables: Vec<String>,
    /// Diff against the package this one would replace, when one was named.
    pub diff: Option<PackageDiff>,
    /// Local Supply lineage for the package.
    pub lineage: SupplyLineage,
    /// Whether a current Assessment exists.
    pub assessment_state: AssessmentState,
    /// The Assessment, only when it describes exactly what is under review.
    pub assessment: Option<Assessment>,
    /// The reviewer's claimed review depth.
    pub review_depth: ReviewDepth,
    /// Typed next actions, most urgent first.
    pub next_actions: Vec<NextAction>,
}

impl Dossier {
    /// Recomputes everything a reviewer needs about one package.
    pub fn build(
        store: &Store,
        policy: &Policy,
        request: &DossierRequest<'_>,
    ) -> Result<Self, DossierError> {
        let package = store.open_package(request.digest, policy)?;
        let report = store.verify(request.digest, policy)?;
        let inspection = Inspection::run(&package, policy)?;
        let diff = match request.against {
            Some(base) => {
                let base = store.open_package(base, policy)?;
                Some(PackageDiff::between(&base, &package)?)
            }
            None => None,
        };
        let lineage = store
            .lineage(request.digest)?
            .unwrap_or_else(|| SupplyLineage::new(&request.digest.to_string()));

        let (assessment_state, assessment) = match &request.assessment_key {
            None => (AssessmentState::Absent, None),
            Some((model, prompt_version)) => {
                let key = AssessmentKey {
                    package_digest: request.digest.to_string(),
                    model: model.clone(),
                    prompt_version: prompt_version.clone(),
                };
                match store.assessment(request.digest)? {
                    None => (AssessmentState::Absent, None),
                    Some(recorded) => match recorded.current_for(&key) {
                        Some(current) => (AssessmentState::Current, Some(current.clone())),
                        None => (AssessmentState::Superseded, None),
                    },
                }
            }
        };

        let mut dossier = Self {
            schema: DOSSIER_SCHEMA.to_owned(),
            tool_version: TOOL_VERSION.to_owned(),
            package: PackageSummary {
                digest: package.digest.to_string(),
                entry_count: package.manifest.entries.len(),
                total_bytes: package.manifest.total_size(),
                skill_name: inspection.skill_name.clone(),
                skill_description: inspection.skill_description.clone(),
            },
            verification: VerificationSummary {
                intact: report.is_intact(),
                recomputed_digest: report.recomputed_digest.to_string(),
                failures: report
                    .failures
                    .iter()
                    .map(|failure| scan::escape(&failure.summary()))
                    .collect(),
            },
            policy: PolicySummary {
                version: policy.document().version.clone(),
                digest: policy.digest().to_string(),
                unicode_profile_version: policy.document().unicode.profile_version.clone(),
            },
            executables: inspection
                .executables()
                .into_iter()
                .map(str::to_owned)
                .collect(),
            inspection,
            diff,
            lineage,
            assessment_state,
            assessment,
            review_depth: request.review_depth,
            next_actions: Vec::new(),
        };
        dossier.next_actions = dossier.derive_next_actions();
        Ok(dossier)
    }

    /// Returns the bytes a Skill Admission binds this Dossier by.
    ///
    /// Supply lineage is removed first. Lineage is local — absolute paths and
    /// capture timestamps — so leaving it in would make the digest differ
    /// between two machines that reviewed byte-identical bytes, and a
    /// Generation would stop verifying the moment it left the machine that
    /// signed it.
    pub fn portable_bytes(&self) -> Vec<u8> {
        let mut value = serde_json::to_value(self).expect("a dossier is always serializable");
        if let Some(object) = value.as_object_mut() {
            object.remove("lineage");
        }
        serde_json::to_vec(&value).expect("a dossier value is always serializable")
    }

    /// Returns the digest of [`Dossier::portable_bytes`].
    pub fn portable_digest(&self) -> Digest {
        Digest::of(&self.portable_bytes())
    }

    /// Reports whether the package may be put in front of a reviewer at all.
    pub fn reviewable(&self) -> bool {
        self.verification.intact && !self.inspection.is_fatal()
    }

    /// Returns every link origin that reached outside the candidate root.
    pub fn escaping_link_count(&self) -> usize {
        self.lineage.escaping_links().len()
    }

    fn derive_next_actions(&self) -> Vec<NextAction> {
        let mut actions = Vec::new();
        if !self.verification.intact {
            actions.push(NextAction {
                id: "refuse_unverified".to_owned(),
                detail: format!(
                    "Stored bytes do not match {}; refuse this package and re-capture it.",
                    self.package.digest
                ),
            });
            return actions;
        }
        if self.inspection.is_fatal() {
            actions.push(NextAction {
                id: "resolve_fatal".to_owned(),
                detail: format!(
                    "{} fatal finding(s) make this package unreviewable; fix the candidate and re-capture.",
                    self.inspection.fatal.len()
                ),
            });
            return actions;
        }
        let findings = self.inspection.findings.len();
        if findings > 0 {
            actions.push(NextAction {
                id: "review_findings".to_owned(),
                detail: format!(
                    "Read {findings} finding(s) across {} file(s) before approving.",
                    self.inspection
                        .findings
                        .iter()
                        .map(|finding| finding.path.as_str())
                        .collect::<std::collections::BTreeSet<_>>()
                        .len()
                ),
            });
        }
        if self.escaping_link_count() > 0 {
            actions.push(NextAction {
                id: "review_external_sources".to_owned(),
                detail: format!(
                    "{} packaged file(s) came from outside the candidate root; check Supply lineage.",
                    self.escaping_link_count()
                ),
            });
        }
        if self.assessment_state == AssessmentState::Superseded {
            actions.push(NextAction {
                id: "reassess".to_owned(),
                detail: "The recorded Assessment describes other bytes, Model, or prompt; it is ignored.".to_owned(),
            });
        }
        if self.review_depth == ReviewDepth::Unstated {
            actions.push(NextAction {
                id: "state_review_depth".to_owned(),
                detail: "Record how deeply this package was reviewed; it is a claim, not a proof."
                    .to_owned(),
            });
        }
        actions.push(NextAction {
            id: "admit".to_owned(),
            detail: format!(
                "Package {} verified against its bytes and is ready for Skill Admission.",
                self.package.digest
            ),
        });
        actions
    }
}
