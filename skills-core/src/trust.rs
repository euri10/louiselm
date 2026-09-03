//! Enrolled signing roles and the rules for changing them.
//!
//! Three roles, kept logically distinct even when they live on one physical
//! token: the **primary** signs routine Skill Admissions, the **recovery** key
//! exists only to replace key policy or the primary, and the **release** role
//! authorizes trusted builds (louiselm-d6fv.7). A recovery key that could also
//! sign an ordinary Admission would not be a recovery path; it would be a
//! second primary, so verification refuses it by role rather than by key.
//!
//! Bootstrap requires both a primary and a recovery key. Enrolling a primary
//! alone would produce a trust store with no path out of a lost token, which
//! is a state the operator cannot leave without an explicit trust reset.

use std::{fs, io, path::PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    canonical::Digest,
    sshsig::{self, SkPolicy, TRUST_NAMESPACE},
    store::Store,
};

/// The trust store schema this build reads and writes.
pub const TRUST_SCHEMA: &str = "louiselm.skills.trust/1";

/// The trust-change payload schema.
pub const TRUST_CHANGE_SCHEMA: &str = "louiselm.skills.trust-change/1";

/// A signing role.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Signs routine Skill Admissions.
    Primary,
    /// Replaces key policy or the primary; never an ordinary signer.
    Recovery,
    /// Authorizes trusted releases.
    Release,
}

impl Role {
    /// Returns the name used in payloads and robot output.
    pub fn name(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::Recovery => "recovery",
            Self::Release => "release",
        }
    }

    /// Parses the name accepted on the command line.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "primary" => Some(Self::Primary),
            "recovery" => Some(Self::Recovery),
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
    /// What authorized the enrollment: `bootstrap`, or `recovery`.
    pub enrolled_by: String,
    /// Optional attestation evidence.
    pub attestation: Option<Attestation>,
}

/// A requested change to the enrolled keys.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustChange {
    /// Schema identifier.
    pub schema: String,
    /// Trust domain the change applies to.
    pub trust_domain: String,
    /// Sequence of this change; must be the store's sequence plus one.
    pub sequence: u64,
    /// Digest of the trust store this change applies to.
    pub predecessor: String,
    /// The role being replaced.
    pub role: Role,
    /// The key replacing it.
    pub public_key: String,
    /// The assertion policy the replacement must satisfy.
    pub sk_policy: SkPolicy,
}

impl TrustChange {
    /// Serializes the change to the bytes a recovery key signs.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("a trust change is always serializable")
    }
}

/// The enrolled keys for one trust domain.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
}

/// A trust operation that was refused.
#[derive(Debug, Error)]
pub enum TrustError {
    /// The trust store could not be read or written.
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
    #[error("trust is already bootstrapped for '{0}'; use rotate, or reset explicitly")]
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
    /// The change was not authorized by the recovery key.
    #[error("trust change is not authorized by the recovery key: {0}")]
    Unauthorized(#[from] sshsig::SignatureError),
}

impl TrustStore {
    /// Enrolls the first primary and recovery keys for a domain.
    pub fn bootstrap(
        store: &Store,
        trust_domain: &str,
        primary: &str,
        recovery: &str,
        sk_policy: SkPolicy,
        enrolled_at_ms: u64,
    ) -> Result<Self, TrustError> {
        if Self::load(store)?.is_some() {
            return Err(TrustError::AlreadyBootstrapped(trust_domain.to_owned()));
        }
        let trust = Self {
            schema: TRUST_SCHEMA.to_owned(),
            trust_domain: trust_domain.to_owned(),
            sequence: 0,
            retired: Vec::new(),
            keys: vec![
                EnrolledKey {
                    role: Role::Primary,
                    public_key: normalize(primary),
                    sk_policy,
                    enrolled_at_ms,
                    enrolled_by: "bootstrap".to_owned(),
                    attestation: None,
                },
                EnrolledKey {
                    role: Role::Recovery,
                    public_key: normalize(recovery),
                    sk_policy,
                    enrolled_at_ms,
                    enrolled_by: "bootstrap".to_owned(),
                    attestation: None,
                },
            ],
        };
        trust.write(store)?;
        Ok(trust)
    }

    /// Reads the trust store, when one exists.
    pub fn load(store: &Store) -> Result<Option<Self>, TrustError> {
        let path = Self::path(store);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(TrustError::Io {
                    path: path.display().to_string(),
                    source,
                });
            }
        };
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| TrustError::Malformed(error.to_string()))
    }

    /// Returns the key enrolled for `role`, when there is one.
    pub fn key_for(&self, role: Role) -> Option<&EnrolledKey> {
        self.keys.iter().find(|key| key.role == role)
    }

    /// Returns every key whose signature may verify a stored Generation.
    ///
    /// The current primary plus retired primaries, newest first.
    pub fn admission_verification_keys(&self) -> Vec<&EnrolledKey> {
        self.key_for(Role::Primary)
            .into_iter()
            .chain(
                self.retired
                    .iter()
                    .filter(|key| key.role == Role::Primary)
                    .rev(),
            )
            .collect()
    }

    /// Returns the key that may sign a routine Skill Admission.
    pub fn admission_key(&self) -> Result<&EnrolledKey, TrustError> {
        self.key_for(Role::Primary)
            .ok_or(TrustError::NoKeyForRole("primary"))
    }

    /// Returns the digest of the trust store's canonical bytes.
    pub fn digest(&self) -> Digest {
        Digest::of(&serde_json::to_vec(self).expect("a trust store is always serializable"))
    }

    /// Builds the change that would replace `role` with `public_key`.
    pub fn rotation_payload(
        &self,
        role: Role,
        public_key: &str,
        sk_policy: SkPolicy,
    ) -> TrustChange {
        TrustChange {
            schema: TRUST_CHANGE_SCHEMA.to_owned(),
            trust_domain: self.trust_domain.clone(),
            sequence: self.sequence + 1,
            predecessor: self.digest().to_string(),
            role,
            public_key: normalize(public_key),
            sk_policy,
        }
    }

    /// Applies a recovery-signed change to the enrolled keys.
    pub fn rotate(
        store: &Store,
        change: &TrustChange,
        signature: &str,
        applied_at_ms: u64,
    ) -> Result<Self, TrustError> {
        let mut trust = Self::load(store)?.ok_or(TrustError::NotBootstrapped)?;
        if change.schema != TRUST_CHANGE_SCHEMA {
            return Err(TrustError::DoesNotApply(format!(
                "unsupported schema '{}'",
                change.schema
            )));
        }
        if change.trust_domain != trust.trust_domain {
            return Err(TrustError::DoesNotApply(format!(
                "trust domain '{}' is not '{}'",
                change.trust_domain, trust.trust_domain
            )));
        }
        if change.sequence != trust.sequence + 1 {
            return Err(TrustError::DoesNotApply(format!(
                "sequence {} does not follow {}",
                change.sequence, trust.sequence
            )));
        }
        if change.predecessor != trust.digest().to_string() {
            return Err(TrustError::DoesNotApply(
                "predecessor does not name the current trust store".to_owned(),
            ));
        }
        if change.role == Role::Recovery {
            // Replacing recovery with a recovery signature would let one lost
            // token quietly become the only token. That is a trust reset.
            return Err(TrustError::DoesNotApply(
                "the recovery role is replaced by an explicit trust reset, not a rotation"
                    .to_owned(),
            ));
        }

        let recovery = trust
            .key_for(Role::Recovery)
            .ok_or(TrustError::NoKeyForRole("recovery"))?;
        sshsig::verify(
            signature,
            TRUST_NAMESPACE,
            &change.canonical_bytes(),
            &recovery.public_key,
            recovery.sk_policy,
        )?;

        let replaced = trust
            .keys
            .iter()
            .filter(|key| key.role == change.role)
            .cloned()
            .collect::<Vec<_>>();
        trust.retired.extend(replaced);
        trust.keys.retain(|key| key.role != change.role);
        trust.keys.push(EnrolledKey {
            role: change.role,
            public_key: change.public_key.clone(),
            sk_policy: change.sk_policy,
            enrolled_at_ms: applied_at_ms,
            enrolled_by: "recovery".to_owned(),
            attestation: None,
        });
        trust.keys.sort_by_key(|key| key.role.name());
        trust.sequence = change.sequence;
        trust.write(store)?;
        Ok(trust)
    }

    /// Discards all enrolled keys, invalidating every Generation they signed.
    ///
    /// This is the documented path out of losing both tokens. It is explicit
    /// and destructive by design: there is no seed phrase and no extractable
    /// master secret, so recovery is re-enrollment plus re-Admission.
    pub fn reset(store: &Store) -> Result<(), TrustError> {
        let path = Self::path(store);
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(TrustError::Io {
                path: path.display().to_string(),
                source,
            }),
        }
    }

    fn write(&self, store: &Store) -> Result<(), TrustError> {
        let path = Self::path(store);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| TrustError::Io {
                path: parent.display().to_string(),
                source,
            })?;
        }
        let bytes = serde_json::to_vec(self).expect("a trust store is always serializable");
        fs::write(&path, bytes).map_err(|source| TrustError::Io {
            path: path.display().to_string(),
            source,
        })
    }

    fn path(store: &Store) -> PathBuf {
        store.root().join("trust/roles.json")
    }
}

fn normalize(public_key: &str) -> String {
    public_key
        .split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ")
}
