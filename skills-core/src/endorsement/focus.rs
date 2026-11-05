//! Focus: how trusted Endorsements frame human review of one package.
//!
//! Focus changes what a reviewer is shown, never what Skill Admission accepts.
//! It is computed beside the Dossier, not inside it: the portable Dossier a
//! reviewer approves from stays byte-identical whatever this machine trusts.
//!
//! Two kinds of focus exist. A trusted, unwithdrawn Endorsement of the exact
//! bytes is shown prominently over the full Dossier. Otherwise, when Supply
//! lineage links the package to an earlier capture from the same source, the
//! newest such package that is locally admitted or carries a trusted
//! Endorsement becomes the focus base, and the review is framed as the diff
//! since those bytes. Lineage only chooses among trusted bases: a diff from
//! any base still shows every changed byte, so a forged lineage cannot hide
//! content, and an untrusted base never earns the focus label.
//!
//! A withdrawal cancels its own signer's Endorsements of its subject. It is
//! checked for a valid proof and signature but never for expiry: an expired
//! withdrawal must not bring focus back.

use std::collections::BTreeSet;

use serde::Serialize;
use thiserror::Error;

use super::{
    Record, StatementKind, Subject,
    import::{self, ImportError},
    local::{self, LocalError},
    trust::{EndorserSet, TrustDecision},
};
use crate::{
    canonical::Digest,
    diff::PackageDiff,
    policy::Policy,
    sshsig::SkPolicy,
    store::{Store, StoreError},
};

/// Everything focus needs, loaded once and reused across packages.
pub struct FocusContext {
    endorsers: EndorserSet,
    records: Vec<Record>,
    admitted: BTreeSet<Digest>,
    now_ms: u64,
}

/// One stored record about the reviewed package, with this machine's view.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Evidence {
    /// Digest of the record.
    pub record: String,
    /// Whether it endorses or withdraws.
    pub kind: StatementKind,
    /// Local label of the endorser, when trusted.
    pub endorser: Option<String>,
    /// Claimed review depth, for an Endorsement. A claim, not a proof.
    pub review_depth: Option<String>,
    /// End of validity.
    pub expires_at_ms: u64,
    /// Whether this record may focus review now.
    pub trusted: bool,
    /// The trust decision, in words.
    pub trust: String,
    /// Whether its signer has withdrawn it.
    pub withdrawn: bool,
}

/// Why a base earns the focus label.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "basis", rename_all = "snake_case")]
pub enum Basis {
    /// The base is a member of the Generation in force here.
    Admitted {},
    /// A trusted endorser endorsed the base's exact bytes.
    Endorsed {
        /// The endorser's local label.
        endorser: String,
        /// The record relied on.
        record: String,
    },
}

/// A trusted earlier package the review is framed against.
#[derive(Clone, Debug, Serialize)]
pub struct FocusBase {
    /// The base package.
    pub package_digest: String,
    /// Why it is trusted.
    pub basis: Basis,
    /// Every difference from the base to the reviewed package.
    pub diff: PackageDiff,
}

/// A trusted, unwithdrawn Endorsement of the exact reviewed bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ExactEndorsement {
    /// The endorser's local label.
    pub endorser: String,
    /// The record relied on.
    pub record: String,
}

/// How trusted evidence frames the review of one package.
#[derive(Clone, Debug, Serialize)]
pub struct Focus {
    /// Every stored record about this package.
    pub evidence: Vec<Evidence>,
    /// Trusted Endorsements of exactly these bytes.
    pub exact: Vec<ExactEndorsement>,
    /// The trusted base the review is framed against, when one exists.
    pub base: Option<FocusBase>,
    /// Digests of the records this focus relies on, sorted and unique.
    pub relied_on: Vec<String>,
}

impl Focus {
    /// Whether any trusted evidence frames this review.
    #[must_use]
    pub fn is_focused(&self) -> bool {
        !self.exact.is_empty() || self.base.is_some()
    }
}

/// Why focus could not be computed.
#[derive(Debug, Error)]
pub enum FocusError {
    /// A package, its lineage, or the diff could not be read.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// The endorser layers could not be read.
    #[error(transparent)]
    Endorsers(#[from] LocalError),
    /// Stored records could not be read.
    #[error(transparent)]
    Records(#[from] ImportError),
}

impl FocusContext {
    /// Loads the effective endorsers and stored records for `now_ms`.
    ///
    /// `admitted` lists the members of the Generation in force; the caller
    /// supplies it so focus never depends on Admission's internals.
    ///
    /// # Errors
    /// Returns endorser-layer and stored-record read errors.
    pub fn load(store: &Store, admitted: &[Digest], now_ms: u64) -> Result<Self, FocusError> {
        let (endorsers, _) = local::effective(store)?;
        Ok(Self {
            endorsers,
            records: import::records(store)?.records,
            admitted: admitted.iter().cloned().collect(),
            now_ms,
        })
    }

    /// Computes focus for `package`. With `against`, only that base is
    /// considered; without, Supply lineage proposes earlier captures.
    ///
    /// # Errors
    /// Returns store errors reading lineage, packages or the diff.
    pub fn evaluate(
        &self,
        store: &Store,
        policy: &Policy,
        package: &Digest,
        against: Option<&Digest>,
    ) -> Result<Focus, FocusError> {
        let evidence = self.evidence(package);
        let exact = self.trusted_endorsements(package);
        let base = match against {
            Some(base) => self.basis(base).map(|basis| (base.clone(), basis)),
            None => self.lineage_base(store, package)?,
        };
        let base = match base {
            Some((digest, basis)) => {
                let base_package = store.open_package(&digest, policy)?;
                let target = store.open_package(package, policy)?;
                Some(FocusBase {
                    package_digest: digest.to_string(),
                    basis,
                    diff: PackageDiff::between(&base_package, &target)?,
                })
            }
            None => None,
        };
        let mut relied_on: Vec<String> = exact.iter().map(|item| item.record.clone()).collect();
        if let Some(FocusBase {
            basis: Basis::Endorsed { record, .. },
            ..
        }) = &base
        {
            relied_on.push(record.clone());
        }
        relied_on.sort();
        relied_on.dedup();
        Ok(Focus {
            evidence,
            exact,
            base,
            relied_on,
        })
    }

    fn evidence(&self, package: &Digest) -> Vec<Evidence> {
        self.records
            .iter()
            .filter(|record| record.covers(package))
            .map(|record| {
                let decision = self.endorsers.decide(record, self.now_ms);
                let (trusted, endorser, trust) = match &decision {
                    TrustDecision::Trusted { label } => (
                        true,
                        Some(label.clone()),
                        format!("trusted endorser {label}"),
                    ),
                    TrustDecision::Untrusted(reason) => {
                        (false, None, format!("untrusted: {reason}"))
                    }
                };
                let review_depth = match record.subject() {
                    Subject::Endorsed(stated) => Some(stated.review_depth.name().to_owned()),
                    Subject::Withdrawn(_) => None,
                };
                Evidence {
                    record: record.digest().to_string(),
                    kind: record.kind(),
                    endorser,
                    review_depth,
                    expires_at_ms: record.signing().expires_at_ms,
                    trusted,
                    trust,
                    withdrawn: record.kind() == StatementKind::Endorsement
                        && self.withdrawn(&record.signing().signer_key, package),
                }
            })
            .collect()
    }

    fn trusted_endorsements(&self, package: &Digest) -> Vec<ExactEndorsement> {
        self.records
            .iter()
            .filter(|record| record.kind() == StatementKind::Endorsement && record.covers(package))
            .filter(|record| !self.withdrawn(&record.signing().signer_key, package))
            .filter_map(|record| match self.endorsers.decide(record, self.now_ms) {
                TrustDecision::Trusted { label } => Some(ExactEndorsement {
                    endorser: label,
                    record: record.digest().to_string(),
                }),
                TrustDecision::Untrusted(_) => None,
            })
            .collect()
    }

    fn withdrawn(&self, signer_key: &str, package: &Digest) -> bool {
        self.records.iter().any(|record| {
            record.kind() == StatementKind::Withdrawal
                && record.covers(package)
                && record.signing().signer_key == signer_key
                && record.verify_signature(SkPolicy::none()).is_ok()
        })
    }

    fn basis(&self, base: &Digest) -> Option<Basis> {
        if self.admitted.contains(base) {
            return Some(Basis::Admitted {});
        }
        self.trusted_endorsements(base)
            .into_iter()
            .next()
            .map(|endorsement| Basis::Endorsed {
                endorser: endorsement.endorser,
                record: endorsement.record,
            })
    }

    fn lineage_base(
        &self,
        store: &Store,
        package: &Digest,
    ) -> Result<Option<(Digest, Basis)>, FocusError> {
        let Some(lineage) = store.lineage(package)? else {
            return Ok(None);
        };
        let roots: BTreeSet<&str> = lineage
            .captures
            .iter()
            .map(|capture| capture.source_root.as_str())
            .collect();
        let mut candidates = Vec::new();
        for other in store.list()? {
            if &other == package {
                continue;
            }
            let Some(history) = store.lineage(&other)? else {
                continue;
            };
            let latest = history
                .captures
                .iter()
                .filter(|capture| roots.contains(capture.source_root.as_str()))
                .map(|capture| capture.captured_at_ms)
                .max();
            if let Some(captured_at_ms) = latest {
                candidates.push((captured_at_ms, other));
            }
        }
        candidates.sort_by(|left, right| right.cmp(left));
        Ok(candidates
            .into_iter()
            .find_map(|(_, digest)| self.basis(&digest).map(|basis| (digest, basis))))
    }
}
