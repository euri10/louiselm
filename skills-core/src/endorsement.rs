//! Portable Endorsements: signed evidence about exact portable skill bytes.
//!
//! An Endorsement says who claimed what about one normalized package
//! ([`crate::Manifest::digest`]) under stated assumptions. A withdrawal is the
//! same family of statement taking that claim back. Both are evidence, never
//! authority: nothing in this module trusts an endorser, installs bytes, or
//! performs Skill Admission. A valid signature proves only which key made the
//! claim, not that the review happened or that the bytes are safe here.
//!
//! One signature covers a batch. The signer signs a header binding a Merkle
//! root over blinded per-package leaves; each package is then exported as a
//! standalone record holding the header, the signature, its own leaf and an
//! inclusion proof. A record reveals no other subject: siblings appear only
//! as hashes of leaves carrying a random nonce, so a known package digest
//! cannot be matched against them.
//!
//! The subject is the portable package digest, which already excludes local
//! paths, link targets and Supply lineage, so byte-identical packages on two
//! machines share one subject. Records carry no local name, inventory,
//! machine identity or attestation blob.
//!
//! Encoding: every struct lists its fields in alphabetical order and every
//! field is always present, so the canonical bytes are exactly sorted-key
//! compact JSON. Signatures use their own SSHSIG namespace, so an Endorsement
//! never verifies as an Admission or a release, nor the reverse.

pub mod focus;
pub mod import;
pub mod local;
mod merkle;
pub mod trust;

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use thiserror::Error;

use crate::{
    canonical::Digest,
    dossier::ReviewDepth,
    scan::escape,
    signer::{Signer, SignerError},
    sshsig::{self, SignatureError, SkPolicy},
};

/// The SSHSIG namespace Endorsement signatures are made in.
pub const ENDORSEMENT_NAMESPACE: &str = "louiselm.skills.endorsement/1";

/// The schema of the signed batch header.
pub const STATEMENT_SCHEMA: &str = "louiselm.skills.endorsement/1";

/// The schema of one exported per-package record.
pub const RECORD_SCHEMA: &str = "louiselm.skills.endorsement-record/1";

/// The only signature algorithm this build verifies: `OpenSSH` SSHSIG.
///
/// The identifier is versioned and bound inside the signed header, so a later
/// algorithm is a new identifier rather than a new meaning for this one.
pub const SSHSIG_ALGORITHM: &str = "sshsig/1";

/// Largest record accepted, checked before any parsing.
pub const MAX_RECORD_BYTES: usize = 16 * 1024;

/// Most packages one batch signature may cover.
pub const MAX_BATCH: usize = 1024;

const MAX_ASSUMPTIONS: usize = 16;
const MAX_TEXT: usize = 256;
const NONCE_BYTES: usize = 32;

/// The role an endorser claims. Deliberately disjoint from local authority:
/// Primary, Release and recovery can never be stated here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndorserRole {
    /// Ships the package and vouches for distributing these exact bytes.
    Publisher,
    /// Reviewed the package and states a review depth.
    Reviewer,
}

/// What a signed batch states about its subjects.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatementKind {
    /// The signer endorses each subject.
    Endorsement,
    /// The signer withdraws any Endorsement it made of each subject.
    Withdrawal,
}

/// Who signs a batch and for how long it is valid. Shared by every record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signing {
    /// Role the endorser claims.
    pub signer_role: EndorserRole,
    /// The endorser's `OpenSSH` public key, `<algorithm> <base64>`, no comment.
    pub signer_key: String,
    /// Start of validity, in Unix milliseconds.
    pub issued_at_ms: u64,
    /// End of validity (exclusive), in Unix milliseconds.
    pub expires_at_ms: u64,
}

/// The Assessment an endorser states they considered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatedAssessment {
    /// Model that produced the Assessment.
    pub model: String,
    /// Prompt version the Model was given.
    pub prompt_version: String,
}

/// What an endorser claims about one package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EndorsedPackage {
    /// Portable digest of the endorsed package.
    pub package_digest: Digest,
    /// Digest of the Inspection policy the review assumed.
    pub policy_digest: Digest,
    /// Portable digest of the Dossier reviewed, when one was.
    pub dossier_digest: Option<Digest>,
    /// Assessment considered, when one was.
    pub assessment: Option<StatedAssessment>,
    /// Claimed review depth. A claim, not a proof.
    pub review_depth: ReviewDepth,
    /// Explicit single-line review assumptions.
    pub assumptions: Vec<String>,
}

/// The statements one signature covers. The variant fixes the kind, so a
/// batch cannot mix endorsements with withdrawals.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Batch {
    /// Endorse each package.
    Endorsements(Vec<EndorsedPackage>),
    /// Withdraw any Endorsement of each package digest.
    Withdrawals(Vec<Digest>),
}

/// What one record states about its single subject.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Subject {
    /// An Endorsement of one package.
    Endorsed(EndorsedPackage),
    /// A withdrawal naming one package digest.
    Withdrawn(Digest),
}

impl Subject {
    /// The package digest this statement is about.
    #[must_use]
    pub fn package_digest(&self) -> &Digest {
        match self {
            Self::Endorsed(package) => &package.package_digest,
            Self::Withdrawn(digest) => digest,
        }
    }
}

/// A parsed, canonical per-package record whose proof and signature are not
/// yet checked.
#[derive(Clone, Debug)]
pub struct Record {
    signing: Signing,
    kind: StatementKind,
    subject: Subject,
    header_bytes: Vec<u8>,
    signature: String,
    leaf: merkle::Node,
    leaf_index: u64,
    leaf_count: u64,
    proof: Vec<merkle::Node>,
    root: merkle::Node,
    bytes: Vec<u8>,
}

/// Why an Endorsement could not be produced, parsed or verified.
#[derive(Debug, Error)]
pub enum EndorsementError {
    /// The input exceeds [`MAX_RECORD_BYTES`].
    #[error("endorsement is {size} bytes; the limit is {limit}")]
    Oversized {
        /// Size of the input.
        size: usize,
        /// The accepted maximum.
        limit: usize,
    },
    /// The input is not a well-formed record of a supported schema.
    #[error("malformed endorsement: {0}")]
    Malformed(String),
    /// The input is valid JSON for the schema but not its one canonical encoding.
    #[error("endorsement is not in canonical encoding")]
    NonCanonical,
    /// The record or header names a schema this build does not read.
    #[error("unsupported endorsement schema '{0}'")]
    UnsupportedSchema(String),
    /// The header names a signature algorithm this build does not verify.
    #[error("unsupported endorsement signature algorithm '{0}'")]
    UnsupportedAlgorithm(String),
    /// The header names a statement kind this build does not read.
    #[error("unsupported endorsement statement kind '{0}'")]
    UnsupportedKind(String),
    /// A field holds a value the format forbids.
    #[error("invalid endorsement field {field}: {reason}")]
    Invalid {
        /// The offending field.
        field: &'static str,
        /// What is wrong with it.
        reason: &'static str,
    },
    /// The leaf and proof do not lead to the signed root.
    #[error("endorsement inclusion proof does not match the signed root")]
    ProofMismatch,
    /// The signature does not verify for the stated key and namespace.
    #[error("endorsement signature refused: {0}")]
    Signature(#[from] SignatureError),
    /// The signer could not produce a signature.
    #[error("cannot sign endorsement: {0}")]
    Signing(#[from] SignerError),
    /// Random blinding could not be drawn.
    #[error("cannot draw endorsement blinding nonce: {0}")]
    Random(std::io::Error),
    /// The Endorsement is past its expiry.
    #[error("endorsement expired at {expires_at_ms}")]
    Expired {
        /// The stated expiry.
        expires_at_ms: u64,
    },
    /// The Endorsement's validity has not started.
    #[error("endorsement is not valid before {issued_at_ms}")]
    NotYetValid {
        /// The stated issue time.
        issued_at_ms: u64,
    },
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireHeader {
    expires_at_ms: u64,
    issued_at_ms: u64,
    kind: StatementKind,
    leaf_count: u64,
    root: String,
    schema: String,
    signature_algorithm: String,
    signer_key: String,
    signer_role: EndorserRole,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireAssessment {
    model: String,
    prompt_version: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EndorsedLeaf {
    assessment: Option<WireAssessment>,
    assumptions: Vec<String>,
    dossier_digest: Option<String>,
    nonce: String,
    package_digest: String,
    policy_digest: String,
    review_depth: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WithdrawnLeaf {
    nonce: String,
    package_digest: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRecord<L> {
    header: WireHeader,
    leaf: L,
    leaf_index: u64,
    proof: Vec<String>,
    schema: String,
    signature: String,
}

/// Signs `batch` with one call to `signer` and returns one canonical record
/// per subject.
///
/// Leaves are blinded with fresh randomness and ordered by leaf hash, so a
/// record's position reveals nothing about its subject. The signature is
/// verified against the stated key before any record is returned, so a signer
/// holding a different key cannot produce one.
///
/// # Errors
/// Returns [`EndorsementError::Invalid`] for an empty, oversized or
/// duplicate-subject batch or an impossible field (before any signature is
/// requested), [`EndorsementError::Random`] when blinding cannot be drawn,
/// [`EndorsementError::Signing`] when the signer fails, and
/// [`EndorsementError::Signature`] when the signature is not by the stated key.
pub fn sign_batch(
    signing: &Signing,
    batch: &Batch,
    signer: &dyn Signer,
) -> Result<Vec<Vec<u8>>, EndorsementError> {
    validate_signing(signing)?;
    match batch {
        Batch::Endorsements(packages) => {
            check_subjects(packages.iter().map(|package| &package.package_digest))?;
            for package in packages {
                validate_package(package)?;
            }
            let leaves = packages
                .iter()
                .map(|package| Ok(endorsed_leaf(package, nonce()?)))
                .collect::<Result<_, EndorsementError>>()?;
            assemble(signing, StatementKind::Endorsement, leaves, signer)
        }
        Batch::Withdrawals(digests) => {
            check_subjects(digests.iter())?;
            let leaves = digests
                .iter()
                .map(|digest| {
                    Ok(WithdrawnLeaf {
                        nonce: nonce()?,
                        package_digest: digest.to_string(),
                    })
                })
                .collect::<Result<_, EndorsementError>>()?;
            assemble(signing, StatementKind::Withdrawal, leaves, signer)
        }
    }
}

/// Parses exact canonical record bytes without fetching or executing anything.
///
/// Schemas, the algorithm and the statement kind are checked first, so a
/// record from a newer format is named as unsupported rather than malformed.
///
/// # Errors
/// Rejects oversized, malformed, unsupported-schema/algorithm/kind,
/// noncanonical and invalid-field input. The proof and signature are checked
/// by [`Record::verify`].
pub fn parse(bytes: &[u8]) -> Result<Record, EndorsementError> {
    if bytes.len() > MAX_RECORD_BYTES {
        return Err(EndorsementError::Oversized {
            size: bytes.len(),
            limit: MAX_RECORD_BYTES,
        });
    }
    let value: Value = serde_json::from_slice(bytes).map_err(malformed)?;
    expect_schema(value.get("schema"), RECORD_SCHEMA)?;
    let header = value.get("header");
    let field = |name: &str| header.and_then(|header| header.get(name));
    expect_schema(field("schema"), STATEMENT_SCHEMA)?;
    match field("signature_algorithm") {
        Some(Value::String(found)) if found == SSHSIG_ALGORITHM => (),
        Some(Value::String(found)) => {
            return Err(EndorsementError::UnsupportedAlgorithm(escape(found)));
        }
        _ => return Err(missing("signature_algorithm")),
    }
    match field("kind") {
        Some(Value::String(kind)) if kind == "endorsement" => {
            let record: WireRecord<EndorsedLeaf> = typed(value, bytes)?;
            let subject = Subject::Endorsed(package_from_leaf(&record.leaf)?);
            finish(&record, subject, bytes)
        }
        Some(Value::String(kind)) if kind == "withdrawal" => {
            let record: WireRecord<WithdrawnLeaf> = typed(value, bytes)?;
            check_nonce(&record.leaf.nonce)?;
            let subject =
                Subject::Withdrawn(digest_field("package_digest", &record.leaf.package_digest)?);
            finish(&record, subject, bytes)
        }
        Some(Value::String(kind)) => Err(EndorsementError::UnsupportedKind(escape(kind))),
        _ => Err(missing("kind")),
    }
}

impl Record {
    /// Verifies the inclusion proof, then the signature by the stated key,
    /// then validity at `now_ms`.
    ///
    /// Success means only that the stated key signed a batch containing this
    /// statement and that it is within `[issued_at_ms, expires_at_ms)`.
    /// Whether that key is trusted, and for what, is local policy this method
    /// does not decide.
    ///
    /// # Errors
    /// Returns [`EndorsementError::ProofMismatch`], then
    /// [`EndorsementError::Signature`] for any signature refusal including
    /// `sk_policy` violations, then [`EndorsementError::NotYetValid`] or
    /// [`EndorsementError::Expired`].
    pub fn verify(&self, now_ms: u64, sk_policy: SkPolicy) -> Result<&Subject, EndorsementError> {
        self.verify_signature(sk_policy)?;
        if now_ms < self.signing.issued_at_ms {
            return Err(EndorsementError::NotYetValid {
                issued_at_ms: self.signing.issued_at_ms,
            });
        }
        if now_ms >= self.signing.expires_at_ms {
            return Err(EndorsementError::Expired {
                expires_at_ms: self.signing.expires_at_ms,
            });
        }
        Ok(&self.subject)
    }

    /// Verifies the inclusion proof and the signature by the stated key,
    /// ignoring the validity window.
    ///
    /// Only for statements whose effect must outlive their window — a
    /// withdrawal keeps cancelling focus after it expires. Anything that may
    /// raise trust uses [`Self::verify`].
    ///
    /// # Errors
    /// Returns [`EndorsementError::ProofMismatch`] or
    /// [`EndorsementError::Signature`].
    pub fn verify_signature(&self, sk_policy: SkPolicy) -> Result<(), EndorsementError> {
        if !merkle::verify(
            &self.leaf,
            self.leaf_index,
            self.leaf_count,
            &self.proof,
            &self.root,
        ) {
            return Err(EndorsementError::ProofMismatch);
        }
        sshsig::verify(
            &self.signature,
            ENDORSEMENT_NAMESPACE,
            &self.header_bytes,
            &self.signing.signer_key,
            sk_policy,
        )?;
        Ok(())
    }

    /// Who signed, in which role, and the validity window.
    #[must_use]
    pub fn signing(&self) -> &Signing {
        &self.signing
    }

    /// Whether this record endorses or withdraws.
    #[must_use]
    pub fn kind(&self) -> StatementKind {
        self.kind
    }

    /// The parsed statement. Unverified until [`Self::verify`] succeeds.
    #[must_use]
    pub fn subject(&self) -> &Subject {
        &self.subject
    }

    /// Reports whether this record is about `package`.
    #[must_use]
    pub fn covers(&self, package: &Digest) -> bool {
        self.subject.package_digest() == package
    }

    /// The exact signed header bytes.
    #[must_use]
    pub fn header_bytes(&self) -> &[u8] {
        &self.header_bytes
    }

    /// The armored SSH signature over [`Self::header_bytes`].
    #[must_use]
    pub fn signature(&self) -> &str {
        &self.signature
    }

    /// The exact canonical record bytes, suitable for export.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        self.bytes.clone()
    }

    /// The record's identity: the digest of its canonical bytes.
    #[must_use]
    pub fn digest(&self) -> Digest {
        Digest::of(&self.bytes)
    }
}

fn assemble<L: Serialize>(
    signing: &Signing,
    kind: StatementKind,
    leaves: Vec<L>,
    signer: &dyn Signer,
) -> Result<Vec<Vec<u8>>, EndorsementError> {
    let mut hashed: Vec<(merkle::Node, L)> = leaves
        .into_iter()
        .map(|leaf| (merkle::leaf_hash(&encode(&leaf)), leaf))
        .collect();
    hashed.sort_by_key(|(node, _)| *node);
    let nodes: Vec<merkle::Node> = hashed.iter().map(|(node, _)| *node).collect();
    let root = merkle::root(&nodes).ok_or(EndorsementError::Invalid {
        field: "batch",
        reason: "must name at least one package",
    })?;
    let header = WireHeader {
        expires_at_ms: signing.expires_at_ms,
        issued_at_ms: signing.issued_at_ms,
        kind,
        leaf_count: count(nodes.len())?,
        root: merkle::encode(&root),
        schema: STATEMENT_SCHEMA.to_owned(),
        signature_algorithm: SSHSIG_ALGORITHM.to_owned(),
        signer_key: signing.signer_key.clone(),
        signer_role: signing.signer_role,
    };
    let header_bytes = encode(&header);
    let signature = signer.sign(ENDORSEMENT_NAMESPACE, &header_bytes)?;
    sshsig::verify(
        &signature,
        ENDORSEMENT_NAMESPACE,
        &header_bytes,
        &signing.signer_key,
        SkPolicy::none(),
    )?;
    hashed
        .into_iter()
        .enumerate()
        .map(|(index, (_, leaf))| {
            let bytes = encode(&WireRecord {
                header: header.clone(),
                leaf,
                leaf_index: count(index)?,
                proof: merkle::path(&nodes, index)
                    .iter()
                    .map(merkle::encode)
                    .collect(),
                schema: RECORD_SCHEMA.to_owned(),
                signature: signature.clone(),
            });
            if bytes.len() > MAX_RECORD_BYTES {
                return Err(EndorsementError::Oversized {
                    size: bytes.len(),
                    limit: MAX_RECORD_BYTES,
                });
            }
            Ok(bytes)
        })
        .collect()
}

fn typed<L: DeserializeOwned + Serialize>(
    value: Value,
    bytes: &[u8],
) -> Result<WireRecord<L>, EndorsementError> {
    let record: WireRecord<L> = serde_json::from_value(value).map_err(malformed)?;
    if encode(&record) != bytes {
        return Err(EndorsementError::NonCanonical);
    }
    Ok(record)
}

fn finish<L: Serialize>(
    record: &WireRecord<L>,
    subject: Subject,
    bytes: &[u8],
) -> Result<Record, EndorsementError> {
    let header = &record.header;
    let signing = Signing {
        signer_role: header.signer_role,
        signer_key: header.signer_key.clone(),
        issued_at_ms: header.issued_at_ms,
        expires_at_ms: header.expires_at_ms,
    };
    validate_signing(&signing)?;
    let leaf_count = header.leaf_count;
    if leaf_count == 0 || leaf_count > count(MAX_BATCH)? {
        return Err(EndorsementError::Invalid {
            field: "leaf_count",
            reason: "must be between 1 and the batch limit",
        });
    }
    if record.leaf_index >= leaf_count {
        return Err(EndorsementError::Invalid {
            field: "leaf_index",
            reason: "must be below leaf_count",
        });
    }
    let root = merkle::decode(&header.root).ok_or(EndorsementError::Invalid {
        field: "root",
        reason: "must be sha256:<64 lowercase hex>",
    })?;
    let proof = record
        .proof
        .iter()
        .map(|node| {
            merkle::decode(node).ok_or(EndorsementError::Invalid {
                field: "proof",
                reason: "each entry must be sha256:<64 lowercase hex>",
            })
        })
        .collect::<Result<_, _>>()?;
    if subject_matches_kind(&subject) != header.kind {
        return Err(EndorsementError::Malformed(
            "leaf does not match statement kind".into(),
        ));
    }
    Ok(Record {
        signing,
        kind: header.kind,
        subject,
        header_bytes: encode(header),
        signature: record.signature.clone(),
        leaf: merkle::leaf_hash(&encode(&record.leaf)),
        leaf_index: record.leaf_index,
        leaf_count,
        proof,
        root,
        bytes: bytes.to_vec(),
    })
}

fn subject_matches_kind(subject: &Subject) -> StatementKind {
    match subject {
        Subject::Endorsed(_) => StatementKind::Endorsement,
        Subject::Withdrawn(_) => StatementKind::Withdrawal,
    }
}

fn endorsed_leaf(package: &EndorsedPackage, nonce: String) -> EndorsedLeaf {
    EndorsedLeaf {
        assessment: package.assessment.as_ref().map(|stated| WireAssessment {
            model: stated.model.clone(),
            prompt_version: stated.prompt_version.clone(),
        }),
        assumptions: package.assumptions.clone(),
        dossier_digest: package.dossier_digest.as_ref().map(ToString::to_string),
        nonce,
        package_digest: package.package_digest.to_string(),
        policy_digest: package.policy_digest.to_string(),
        review_depth: package.review_depth.name().to_owned(),
    }
}

fn package_from_leaf(leaf: &EndorsedLeaf) -> Result<EndorsedPackage, EndorsementError> {
    check_nonce(&leaf.nonce)?;
    let package = EndorsedPackage {
        package_digest: digest_field("package_digest", &leaf.package_digest)?,
        policy_digest: digest_field("policy_digest", &leaf.policy_digest)?,
        dossier_digest: leaf
            .dossier_digest
            .as_deref()
            .map(|raw| digest_field("dossier_digest", raw))
            .transpose()?,
        assessment: leaf.assessment.as_ref().map(|stated| StatedAssessment {
            model: stated.model.clone(),
            prompt_version: stated.prompt_version.clone(),
        }),
        review_depth: ReviewDepth::parse(&leaf.review_depth).ok_or(EndorsementError::Invalid {
            field: "review_depth",
            reason: "not a known review depth",
        })?,
        assumptions: leaf.assumptions.clone(),
    };
    validate_package(&package)?;
    Ok(package)
}

fn check_subjects<'a>(digests: impl Iterator<Item = &'a Digest>) -> Result<(), EndorsementError> {
    let mut seen = BTreeSet::new();
    for digest in digests {
        if !seen.insert(digest) {
            return Err(EndorsementError::Invalid {
                field: "package_digest",
                reason: "a batch names each package once",
            });
        }
    }
    if seen.is_empty() {
        return Err(EndorsementError::Invalid {
            field: "batch",
            reason: "must name at least one package",
        });
    }
    if seen.len() > MAX_BATCH {
        return Err(EndorsementError::Invalid {
            field: "batch",
            reason: "exceeds the batch limit",
        });
    }
    Ok(())
}

fn nonce() -> Result<String, EndorsementError> {
    let mut bytes = [0_u8; NONCE_BYTES];
    let mut offset = 0;
    while offset < bytes.len() {
        let drawn =
            rustix::rand::getrandom(&mut bytes[offset..], rustix::rand::GetRandomFlags::empty())
                .map_err(|error| EndorsementError::Random(error.into()))?;
        if drawn == 0 {
            return Err(EndorsementError::Random(std::io::Error::other(
                "getrandom returned no bytes",
            )));
        }
        offset += drawn;
    }
    Ok(merkle::encode(&bytes).replace("sha256:", ""))
}

fn check_nonce(nonce: &str) -> Result<(), EndorsementError> {
    let valid = nonce.len() == NONCE_BYTES * 2
        && nonce
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    if valid {
        Ok(())
    } else {
        Err(EndorsementError::Invalid {
            field: "nonce",
            reason: "must be 64 lowercase hex characters",
        })
    }
}

fn count(value: usize) -> Result<u64, EndorsementError> {
    u64::try_from(value).map_err(|_| EndorsementError::Invalid {
        field: "batch",
        reason: "exceeds the batch limit",
    })
}

fn expect_schema(found: Option<&Value>, expected: &str) -> Result<(), EndorsementError> {
    match found {
        Some(Value::String(found)) if found == expected => Ok(()),
        Some(Value::String(found)) => Err(EndorsementError::UnsupportedSchema(escape(found))),
        _ => Err(EndorsementError::Malformed(format!(
            "missing schema; expected {expected}"
        ))),
    }
}

fn missing(field: &str) -> EndorsementError {
    EndorsementError::Malformed(format!("missing {field}"))
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "Used as a map_err adapter, which hands over the error by value."
)]
fn malformed(error: serde_json::Error) -> EndorsementError {
    EndorsementError::Malformed(escape(&error.to_string()))
}

#[expect(
    clippy::expect_used,
    reason = "Wire types hold only strings, integers, options, vectors and unit enums; serialization cannot fail."
)]
fn encode<T: Serialize>(value: &T) -> Vec<u8> {
    serde_json::to_vec(value).expect("endorsement wire types are always serializable")
}

/// Accepts only the `sha256:<hex>` spelling, so one digest has one encoding.
fn digest_field(field: &'static str, raw: &str) -> Result<Digest, EndorsementError> {
    match Digest::parse(raw) {
        Ok(digest) if digest.to_string() == raw => Ok(digest),
        _ => Err(EndorsementError::Invalid {
            field,
            reason: "must be sha256:<64 lowercase hex>",
        }),
    }
}

fn validate_signing(signing: &Signing) -> Result<(), EndorsementError> {
    if signing.expires_at_ms <= signing.issued_at_ms {
        return Err(EndorsementError::Invalid {
            field: "expires_at_ms",
            reason: "must be after issued_at_ms",
        });
    }
    if !valid_public_key(&signing.signer_key) {
        return Err(EndorsementError::Invalid {
            field: "signer_key",
            reason: "must be '<algorithm> <base64>' with no comment",
        });
    }
    Ok(())
}

fn validate_package(package: &EndorsedPackage) -> Result<(), EndorsementError> {
    let invalid = |field, reason| Err(EndorsementError::Invalid { field, reason });
    if package.assumptions.len() > MAX_ASSUMPTIONS {
        return invalid("assumptions", "too many assumptions");
    }
    if !package.assumptions.iter().all(|text| valid_text(text)) {
        return invalid("assumptions", "each must be one non-empty printable line");
    }
    if let Some(stated) = &package.assessment
        && !(valid_text(&stated.model) && valid_text(&stated.prompt_version))
    {
        return invalid(
            "assessment",
            "model and prompt_version must be printable lines",
        );
    }
    Ok(())
}

fn valid_text(text: &str) -> bool {
    !text.trim().is_empty() && text.len() <= MAX_TEXT && !text.chars().any(char::is_control)
}

fn valid_public_key(key: &str) -> bool {
    let mut parts = key.split(' ');
    let (Some(algorithm), Some(blob), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    let printable = |part: &str| {
        !part.is_empty() && part.len() <= 1024 && part.bytes().all(|byte| byte.is_ascii_graphic())
    };
    printable(algorithm)
        && printable(blob)
        && (algorithm.starts_with("ssh-")
            || algorithm.starts_with("sk-")
            || algorithm.starts_with("ecdsa-"))
}
