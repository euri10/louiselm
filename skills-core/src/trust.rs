//! Enrolled signing roles and the rules for changing them.
//!
//! Distinct primary and release credentials may share one physical `YubiKey`.
//! Recovery methods are paper and passkey proofs, never ordinary signing roles.
//! Provisional bootstrap prepares the first release in a development store;
//! installed onboarding must create fresh protected authority separately.
//!
//! Mutations hold a nonblocking process lock, re-read enrollment, and publish
//! by atomic rename after syncing the new file. Success follows directory sync.
//! The store root must be protected from untrusted writers; the lock coordinates
//! tools, not an attacker who can edit that root. Never delete `trust/roles.lock`.

use std::{collections::BTreeSet, io};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{canonical::Digest, sshsig::SkPolicy, store::Store};

pub(crate) mod browser;
pub mod onboarding;
pub mod paper;
pub mod passkey;
pub(crate) mod persistence;
pub mod recovery;
pub mod status;
pub(crate) mod terminal;

/// The trust store schema this build reads and writes.
pub const TRUST_SCHEMA: &str = "louiselm.skills.trust/4";

/// A signing role.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Signs routine Skill Admissions.
    Primary,
    /// Authorizes trusted releases.
    Release,
}

impl Role {
    /// Returns the name used in payloads and robot output.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::Release => "release",
        }
    }

    /// Parses the name accepted on the command line.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "primary" => Some(Self::Primary),
            "release" => Some(Self::Release),
            _ => None,
        }
    }
}

/// Hardware attestation bytes recorded alongside an enrolled key.
///
/// Recorded as evidence only. Nothing in this build validates a manufacturer
/// certificate chain, so nothing here may be read as proof the key is genuine
/// hardware; `validated` says so in the record itself rather than in a comment
/// someone has to find.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attestation {
    /// Base64 attestation blob as produced by `ssh-keygen -O write-attestation`.
    pub blob: String,
    /// Always false in this build.
    pub validated: bool,
}

/// One enrolled key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnrolledKey {
    /// The role this key holds.
    pub role: Role,
    /// The public key in `authorized_keys` form.
    pub public_key: String,
    /// What the key's signatures must prove about how it was touched.
    pub sk_policy: SkPolicy,
    /// When the key was enrolled.
    pub enrolled_at_ms: u64,
    /// What authorized the enrollment: provisional bootstrap, setup or recovery.
    pub enrolled_by: String,
    /// Optional attestation evidence.
    pub attestation: Option<Attestation>,
}

/// The enrolled keys for one trust domain.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustStore {
    /// Schema identifier.
    pub schema: String,
    /// Trust domain every signature must be scoped to.
    pub trust_domain: String,
    /// Number of trust changes applied since bootstrap.
    pub sequence: u64,
    /// Enrolled keys, at most one per role.
    pub keys: Vec<EnrolledKey>,
    /// Keys a rotation replaced.
    ///
    /// Kept so a Generation signed before a rotation can still be verified.
    /// Dropping them would make replacing a token silently invalidate the
    /// supply that token approved, which is an outage disguised as a security
    /// improvement. A retired key may verify history; it may never sign.
    pub retired: Vec<EnrolledKey>,
    /// Exact Generation payloads recorded by successful local Admission.
    pub approved_admissions: BTreeSet<String>,
    /// Exact release manifests recorded by successful local release signing.
    pub approved_releases: BTreeSet<String>,
    /// Current domain-scoped paper verifier; no phrase or entropy is persisted.
    pub paper_verifier: Option<String>,
    /// Consumed or replaced paper verifiers, which must never be re-enrolled.
    pub retired_paper_verifiers: BTreeSet<String>,
    /// Current synced-capable recovery credential; never an ordinary signer.
    pub passkey: Option<passkey::EnrolledPasskey>,
    /// Retired credential fingerprints, refused on re-enrollment.
    pub retired_passkeys: BTreeSet<String>,
}

/// A trust operation that could not report success.
#[derive(Debug, Error)]
pub enum TrustError {
    /// Another process is changing this trust store; retry after it finishes.
    #[error("trust store is busy at '{0}'; another trust change is in progress")]
    Busy(String),
    /// The trust store could not be read, written, or synced.
    ///
    /// A directory-sync failure can follow publication or removal. Inspect the
    /// current state before retrying; failure does not imply it is unchanged.
    #[error("trust store I/O failed at '{path}': {source}")]
    Io {
        /// Path being operated on.
        path: String,
        /// Underlying failure.
        source: io::Error,
    },
    /// The trust store on disk is unreadable.
    #[error("trust store is malformed: {0}")]
    Malformed(String),
    /// Trust is already bootstrapped, so bootstrap would replace it silently.
    #[error("trust is already enrolled for '{0}'; use recovery management, or reset explicitly")]
    AlreadyBootstrapped(String),
    /// No trust store exists yet.
    #[error("trust is not bootstrapped")]
    NotBootstrapped,
    /// The requested role has no enrolled key.
    #[error("no key is enrolled for the {0} role")]
    NoKeyForRole(&'static str),
    /// The change does not apply to this trust store.
    #[error("trust change does not apply: {0}")]
    DoesNotApply(String),
    /// The counter has no representable successor; reset requires explicit consent.
    #[error("trust change sequence is exhausted")]
    SequenceExhausted,
    /// Provisional initialization must never write production authority.
    #[error(
        "provisional bootstrap requires a development store; use installed recovery setup for production"
    )]
    ProvisionalOnly,
    /// Store provenance could not be validated.
    #[error(transparent)]
    Store(#[from] crate::store::StoreError),
}

impl TrustStore {
    pub(crate) fn validate(&self) -> Result<(), TrustError> {
        if self.schema != TRUST_SCHEMA {
            return Err(TrustError::Malformed("unsupported trust schema".to_owned()));
        }
        if self
            .approved_admissions
            .iter()
            .chain(&self.approved_releases)
            .chain(self.paper_verifier.iter())
            .chain(&self.retired_paper_verifiers)
            .any(|digest| Digest::parse(digest).is_err())
            || self
                .paper_verifier
                .as_ref()
                .is_some_and(|current| self.retired_paper_verifiers.contains(current))
        {
            return Err(TrustError::Malformed(
                "invalid approval history or paper authority".to_owned(),
            ));
        }
        let mut roles = BTreeSet::new();
        if self
            .retired_passkeys
            .iter()
            .any(|id| Digest::parse(id).is_err())
        {
            return Err(TrustError::Malformed("invalid retired passkey".to_owned()));
        }
        if let Some(passkey) = &self.passkey {
            passkey
                .validate()
                .map_err(|_| TrustError::Malformed("invalid passkey".to_owned()))?;
            let id = passkey
                .fingerprint()
                .map_err(|_| TrustError::Malformed("invalid passkey".to_owned()))?;
            if self.retired_passkeys.contains(&id) {
                return Err(TrustError::Malformed(
                    "retired passkey is current".to_owned(),
                ));
            }
        }
        if self.keys.iter().any(|key| !roles.insert(key.role.name())) {
            return Err(TrustError::Malformed(
                "duplicate current signing role".to_owned(),
            ));
        }
        Ok(())
    }

    /// Enrolls provisional primary and release public keys in a development store.
    /// This prepares first-release signing, never production recovery readiness.
    ///
    /// # Errors
    /// Refuses a busy or already-bootstrapped store; propagates trust read/JSON and persistence errors.
    pub fn bootstrap(
        store: &Store,
        trust_domain: &str,
        primary: &str,
        release: &str,
        sk_policy: SkPolicy,
        enrolled_at_ms: u64,
    ) -> Result<Self, TrustError> {
        if store.provenance()?.trusted {
            return Err(TrustError::ProvisionalOnly);
        }
        let locked = persistence::LockedTrust::acquire(store)?;
        if locked.load()?.is_some() {
            return Err(TrustError::AlreadyBootstrapped(trust_domain.to_owned()));
        }
        let trust = Self::initial(trust_domain, primary, release, sk_policy, enrolled_at_ms)?;
        locked.write(&trust)?;
        Ok(trust)
    }

    pub(crate) fn initial(
        trust_domain: &str,
        primary: &str,
        release: &str,
        sk_policy: SkPolicy,
        enrolled_at_ms: u64,
    ) -> Result<Self, TrustError> {
        let primary = normalize(primary);
        let release = normalize(release);
        if trust_domain.trim().is_empty()
            || primary.is_empty()
            || release.is_empty()
            || primary == release
        {
            return Err(TrustError::DoesNotApply(
                "domain and distinct signing keys are required".to_owned(),
            ));
        }
        Ok(Self {
            schema: TRUST_SCHEMA.to_owned(),
            trust_domain: trust_domain.to_owned(),
            sequence: 0,
            retired: Vec::new(),
            approved_admissions: BTreeSet::new(),
            approved_releases: BTreeSet::new(),
            paper_verifier: None,
            retired_paper_verifiers: BTreeSet::new(),
            passkey: None,
            retired_passkeys: BTreeSet::new(),
            keys: vec![
                EnrolledKey {
                    role: Role::Primary,
                    public_key: primary,
                    sk_policy,
                    enrolled_at_ms,
                    enrolled_by: "bootstrap".to_owned(),
                    attestation: None,
                },
                EnrolledKey {
                    role: Role::Release,
                    public_key: release,
                    sk_policy,
                    enrolled_at_ms,
                    enrolled_by: "bootstrap".to_owned(),
                    attestation: None,
                },
            ],
        })
    }

    /// Reads the trust store, when one exists.
    ///
    /// # Errors
    /// Returns read/JSON errors, including aliased or non-regular files;
    /// absent enrollment is `Ok(None)`. Concurrent publication yields a complete
    /// old or new snapshot, without waiting for the writer.
    pub fn load(store: &Store) -> Result<Option<Self>, TrustError> {
        persistence::load(store)
    }

    /// Returns the key enrolled for `role`, when there is one.
    #[must_use]
    pub fn key_for(&self, role: Role) -> Option<&EnrolledKey> {
        self.keys.iter().find(|key| key.role == role)
    }

    /// Returns keys that may verify this exact approved subject.
    ///
    /// Current keys may authorize new subjects. Retired keys verify only digests
    /// registered while a key for that role was current, never backdated claims.
    #[must_use]
    pub fn verification_keys(&self, role: Role, digest: &str) -> Vec<&EnrolledKey> {
        let recorded = match role {
            Role::Primary => self.approved_admissions.contains(digest),
            Role::Release => self.approved_releases.contains(digest),
        };
        self.key_for(role)
            .into_iter()
            .chain(
                self.retired
                    .iter()
                    .filter(|key| recorded && key.role == role)
                    .rev(),
            )
            .collect()
    }

    /// Returns the key that may sign a routine Skill Admission.
    ///
    /// # Errors
    /// Returns `NoKeyForRole` when no primary key is enrolled.
    pub fn admission_key(&self) -> Result<&EnrolledKey, TrustError> {
        self.key_for(Role::Primary)
            .ok_or(TrustError::NoKeyForRole("primary"))
    }

    /// Returns the digest of the trust store's canonical bytes.
    ///
    /// # Panics
    /// Panics only if serialization fails after a future schema change introduces
    /// a fallible serializer. The current derived schema has only JSON-native values.
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "Derived schema has only JSON-native values and string-keyed maps, with no custom serializers."
    )]
    pub fn digest(&self) -> Digest {
        Digest::of(&serde_json::to_vec(self).expect("a trust store is always serializable"))
    }

    /// Returns the next trust-change sequence, without wrapping.
    ///
    /// # Errors
    /// Returns [`TrustError::SequenceExhausted`] at the largest counter.
    pub fn next_sequence(&self) -> Result<u64, TrustError> {
        self.sequence
            .checked_add(1)
            .ok_or(TrustError::SequenceExhausted)
    }

    /// Discards all enrolled keys, invalidating every Generation they signed.
    ///
    /// The last resort after losing all functioning signing/recovery methods.
    /// This is explicit and destructive: it discards paper recovery too, and
    /// requires re-enrollment plus re-Admission rather than reviving old trust.
    ///
    /// # Errors
    /// Refuses a busy store or an aliased/non-regular file; propagates removal
    /// and directory-sync errors. Already-absent enrollment is a success.
    /// Explicit reset may discard malformed JSON but retains the shared lock file.
    pub fn reset(store: &Store) -> Result<(), TrustError> {
        if store.provenance()?.trusted {
            return Err(TrustError::ProvisionalOnly);
        }
        persistence::LockedTrust::acquire(store)?.reset()
    }
}

fn normalize(public_key: &str) -> String {
    public_key
        .split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ")
}
