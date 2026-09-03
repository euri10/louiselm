//! Trusted packaging, Inspection, and Dossier rendering for Skill candidates.
//!
//! This crate is the only writer of immutable Skill packages. Everything it
//! produces is recomputed from bytes it read itself: an Agent may propose a
//! candidate, a digest, or an analysis, but nothing an Agent produces is ever
//! treated as authority here.

pub mod admission;
pub mod assessment;
pub mod canonical;
pub mod capture;
pub mod cli;
pub mod diff;
pub mod dossier;
pub mod generation;
pub mod inspect;
pub mod install;
pub mod isolation;
pub mod lineage;
pub mod manifest;
pub mod policy;
pub mod quarantine;
pub mod registry;
pub mod release;
pub mod render;
pub mod robot;
pub mod sandbox;
pub mod scan;
pub mod signer;
pub mod sshsig;
pub mod store;
pub mod trust;
pub mod witness;

pub use assessment::{Assessment, AssessmentKey, Assessor, CapabilityEnvelope, Verdict};
pub use canonical::{CanonicalPath, Digest, PathError};
pub use capture::{CaptureError, LinkOrigin, StagedPackage};
pub use diff::{Change, DiffEntry, PackageDiff};
pub use dossier::{Dossier, DossierRequest, NextAction, ReviewDepth};
pub use generation::{GenerationRecord, GenerationState};
pub use inspect::{Finding, FindingKind, Inspection};
pub use lineage::{CaptureRecord, LineageLink, SupplyLineage};
pub use manifest::{Manifest, ManifestEntry, ManifestError};
pub use policy::{Policy, PolicyDocument, PolicyError};
pub use signer::{Signer, SshKeygenSigner};
pub use store::{Package, PublishOutcome, Store, StoreError, VerifyFailure, VerifyReport};
pub use trust::{Role, TrustStore};
