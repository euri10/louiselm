//! Portable Endorsements: signed evidence about exact portable skill bytes.
//!
//! An Endorsement says who claimed what about one normalized package
//! ([`crate::Manifest::digest`]) under stated assumptions. It is evidence,
//! never authority: nothing in this module trusts an endorser, installs bytes,
//! or performs Skill Admission. A valid signature proves only which key made
//! the claim, not that the review happened or that the bytes are safe here.
//!
//! The subject is the portable package digest, which already excludes local
//! paths, link targets and Supply lineage, so byte-identical packages on two
//! machines share one subject. The record carries no local name, inventory,
//! machine identity or attestation blob.
//!
//! Encoding: every struct lists its fields in alphabetical order and every
//! field is always present, so the canonical bytes are exactly sorted-key
//! compact JSON. Another implementation can reproduce them without this
//! crate's field order. Signatures use their own SSHSIG namespace, so an
//! Endorsement never verifies as an Admission or a release, nor the reverse.

use serde::{Deserialize, Serialize};
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

/// The schema of the signed statement.
pub const STATEMENT_SCHEMA: &str = "louiselm.skills.endorsement/1";

/// The schema of the portable record wrapping a statement and its signature.
pub const RECORD_SCHEMA: &str = "louiselm.skills.endorsement-record/1";

/// The only signature algorithm this build verifies: `OpenSSH` SSHSIG.
///
/// The identifier is versioned and bound inside the signed statement, so a
/// later algorithm is a new identifier rather than a new meaning for this one.
pub const SSHSIG_ALGORITHM: &str = "sshsig/1";

/// Largest record accepted, checked before any parsing.
pub const MAX_RECORD_BYTES: usize = 16 * 1024;

const MAX_ASSUMPTIONS: usize = 16;
const MAX_TEXT: usize = 256;

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

/// The Assessment an endorser states they considered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatedAssessment {
    /// Model that produced the Assessment.
    pub model: String,
    /// Prompt version the Model was given.
    pub prompt_version: String,
}

/// What an endorser claims, before signing or after parsing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Statement {
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
    /// Role the endorser claims.
    pub signer_role: EndorserRole,
    /// The endorser's `OpenSSH` public key, `<algorithm> <base64>`, no comment.
    pub signer_key: String,
    /// Start of validity, in Unix milliseconds.
    pub issued_at_ms: u64,
    /// End of validity (exclusive), in Unix milliseconds.
    pub expires_at_ms: u64,
}

/// A parsed, canonical Endorsement record whose signature is not yet checked.
#[derive(Clone, Debug)]
pub struct Endorsement {
    statement: Statement,
    statement_bytes: Vec<u8>,
    signature: String,
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
    /// The record or statement names a schema this build does not read.
    #[error("unsupported endorsement schema '{0}'")]
    UnsupportedSchema(String),
    /// The statement names a signature algorithm this build does not verify.
    #[error("unsupported endorsement signature algorithm '{0}'")]
    UnsupportedAlgorithm(String),
    /// A field holds a value the format forbids.
    #[error("invalid endorsement field {field}: {reason}")]
    Invalid {
        /// The offending field.
        field: &'static str,
        /// What is wrong with it.
        reason: &'static str,
    },
    /// The signature does not verify for the stated key and namespace.
    #[error("endorsement signature refused: {0}")]
    Signature(#[from] SignatureError),
    /// The signer could not produce a signature.
    #[error("cannot sign endorsement: {0}")]
    Signing(#[from] SignerError),
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

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireAssessment {
    model: String,
    prompt_version: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireStatement {
    assessment: Option<WireAssessment>,
    assumptions: Vec<String>,
    dossier_digest: Option<String>,
    expires_at_ms: u64,
    issued_at_ms: u64,
    package_digest: String,
    policy_digest: String,
    review_depth: String,
    schema: String,
    signature_algorithm: String,
    signer_key: String,
    signer_role: EndorserRole,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRecord {
    payload: WireStatement,
    schema: String,
    signature: String,
}

/// Signs `statement` with `signer` and returns the canonical record bytes.
///
/// The fresh signature is verified against the stated key before it is
/// returned, so a signer holding a different key cannot produce a record.
///
/// # Errors
/// Returns [`EndorsementError::Invalid`] for an impossible statement,
/// [`EndorsementError::Signing`] when the signer fails, and
/// [`EndorsementError::Signature`] when the signature is not by the stated key.
pub fn sign(statement: &Statement, signer: &dyn Signer) -> Result<Vec<u8>, EndorsementError> {
    validate(statement)?;
    let payload = to_wire(statement);
    let statement_bytes = encode(&payload);
    let signature = signer.sign(ENDORSEMENT_NAMESPACE, &statement_bytes)?;
    sshsig::verify(
        &signature,
        ENDORSEMENT_NAMESPACE,
        &statement_bytes,
        &statement.signer_key,
        SkPolicy::none(),
    )?;
    let bytes = encode(&WireRecord {
        payload,
        schema: RECORD_SCHEMA.to_owned(),
        signature,
    });
    if bytes.len() > MAX_RECORD_BYTES {
        return Err(EndorsementError::Oversized {
            size: bytes.len(),
            limit: MAX_RECORD_BYTES,
        });
    }
    Ok(bytes)
}

/// Parses exact canonical record bytes without fetching or executing anything.
///
/// Schemas and the algorithm are checked first, so a record from a newer
/// format is named as unsupported rather than as malformed.
///
/// # Errors
/// Rejects oversized, malformed, unsupported-schema/algorithm, noncanonical
/// and invalid-field input. The signature is checked by [`Endorsement::verify`].
pub fn parse(bytes: &[u8]) -> Result<Endorsement, EndorsementError> {
    if bytes.len() > MAX_RECORD_BYTES {
        return Err(EndorsementError::Oversized {
            size: bytes.len(),
            limit: MAX_RECORD_BYTES,
        });
    }
    let value: Value = serde_json::from_slice(bytes).map_err(malformed)?;
    expect_schema(value.get("schema"), RECORD_SCHEMA)?;
    let payload = value.get("payload");
    expect_schema(
        payload.and_then(|payload| payload.get("schema")),
        STATEMENT_SCHEMA,
    )?;
    match payload.and_then(|payload| payload.get("signature_algorithm")) {
        Some(Value::String(found)) if found == SSHSIG_ALGORITHM => (),
        Some(Value::String(found)) => {
            return Err(EndorsementError::UnsupportedAlgorithm(escape(found)));
        }
        _ => {
            return Err(EndorsementError::Malformed(
                "missing signature_algorithm".into(),
            ));
        }
    }
    let record: WireRecord = serde_json::from_value(value).map_err(malformed)?;
    if encode(&record) != bytes {
        return Err(EndorsementError::NonCanonical);
    }
    let statement = from_wire(&record.payload)?;
    Ok(Endorsement {
        statement,
        statement_bytes: encode(&record.payload),
        signature: record.signature,
        bytes: bytes.to_vec(),
    })
}

impl Endorsement {
    /// Verifies the signature by the stated key, then validity at `now_ms`.
    ///
    /// Success means only that the stated key signed this statement and it is
    /// within `[issued_at_ms, expires_at_ms)`. Whether that key is trusted, and
    /// for what, is local policy this method does not decide.
    ///
    /// # Errors
    /// Returns [`EndorsementError::Signature`] for any signature refusal,
    /// including `sk_policy` violations, then
    /// [`EndorsementError::NotYetValid`] or [`EndorsementError::Expired`].
    pub fn verify(&self, now_ms: u64, sk_policy: SkPolicy) -> Result<&Statement, EndorsementError> {
        sshsig::verify(
            &self.signature,
            ENDORSEMENT_NAMESPACE,
            &self.statement_bytes,
            &self.statement.signer_key,
            sk_policy,
        )?;
        if now_ms < self.statement.issued_at_ms {
            return Err(EndorsementError::NotYetValid {
                issued_at_ms: self.statement.issued_at_ms,
            });
        }
        if now_ms >= self.statement.expires_at_ms {
            return Err(EndorsementError::Expired {
                expires_at_ms: self.statement.expires_at_ms,
            });
        }
        Ok(&self.statement)
    }

    /// Reports whether this Endorsement is about `package`.
    #[must_use]
    pub fn covers(&self, package: &Digest) -> bool {
        &self.statement.package_digest == package
    }

    /// The parsed statement. Unverified until [`Self::verify`] succeeds.
    #[must_use]
    pub fn statement(&self) -> &Statement {
        &self.statement
    }

    /// The exact signed statement bytes.
    #[must_use]
    pub fn statement_bytes(&self) -> &[u8] {
        &self.statement_bytes
    }

    /// The armored SSH signature.
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

fn expect_schema(found: Option<&Value>, expected: &str) -> Result<(), EndorsementError> {
    match found {
        Some(Value::String(found)) if found == expected => Ok(()),
        Some(Value::String(found)) => Err(EndorsementError::UnsupportedSchema(escape(found))),
        _ => Err(EndorsementError::Malformed(format!(
            "missing schema; expected {expected}"
        ))),
    }
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
    reason = "Wire types hold only strings, integers, options, vectors and a unit enum; serialization cannot fail."
)]
fn encode<T: Serialize>(value: &T) -> Vec<u8> {
    serde_json::to_vec(value).expect("endorsement wire types are always serializable")
}

fn to_wire(statement: &Statement) -> WireStatement {
    WireStatement {
        assessment: statement.assessment.as_ref().map(|stated| WireAssessment {
            model: stated.model.clone(),
            prompt_version: stated.prompt_version.clone(),
        }),
        assumptions: statement.assumptions.clone(),
        dossier_digest: statement.dossier_digest.as_ref().map(ToString::to_string),
        expires_at_ms: statement.expires_at_ms,
        issued_at_ms: statement.issued_at_ms,
        package_digest: statement.package_digest.to_string(),
        policy_digest: statement.policy_digest.to_string(),
        review_depth: statement.review_depth.name().to_owned(),
        schema: STATEMENT_SCHEMA.to_owned(),
        signature_algorithm: SSHSIG_ALGORITHM.to_owned(),
        signer_key: statement.signer_key.clone(),
        signer_role: statement.signer_role,
    }
}

fn from_wire(wire: &WireStatement) -> Result<Statement, EndorsementError> {
    let statement = Statement {
        package_digest: digest_field("package_digest", &wire.package_digest)?,
        policy_digest: digest_field("policy_digest", &wire.policy_digest)?,
        dossier_digest: wire
            .dossier_digest
            .as_deref()
            .map(|raw| digest_field("dossier_digest", raw))
            .transpose()?,
        assessment: wire.assessment.as_ref().map(|stated| StatedAssessment {
            model: stated.model.clone(),
            prompt_version: stated.prompt_version.clone(),
        }),
        review_depth: ReviewDepth::parse(&wire.review_depth).ok_or(EndorsementError::Invalid {
            field: "review_depth",
            reason: "not a known review depth",
        })?,
        assumptions: wire.assumptions.clone(),
        signer_role: wire.signer_role,
        signer_key: wire.signer_key.clone(),
        issued_at_ms: wire.issued_at_ms,
        expires_at_ms: wire.expires_at_ms,
    };
    validate(&statement)?;
    Ok(statement)
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

fn validate(statement: &Statement) -> Result<(), EndorsementError> {
    let invalid = |field, reason| Err(EndorsementError::Invalid { field, reason });
    if statement.expires_at_ms <= statement.issued_at_ms {
        return invalid("expires_at_ms", "must be after issued_at_ms");
    }
    if !valid_public_key(&statement.signer_key) {
        return invalid(
            "signer_key",
            "must be '<algorithm> <base64>' with no comment",
        );
    }
    if statement.assumptions.len() > MAX_ASSUMPTIONS {
        return invalid("assumptions", "too many assumptions");
    }
    if !statement.assumptions.iter().all(|text| valid_text(text)) {
        return invalid("assumptions", "each must be one non-empty printable line");
    }
    if let Some(stated) = &statement.assessment
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
