//! Separate machine-signed recovery facts. These are not Launcher receipts:
//! they bind existing history without inventing receipts or Launch measurements.

use crate::{Digest, launch_protocol::LaunchAuthorization, launch_receipt::ReceiptHead};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Independent SSHSIG namespace and payload schema for root recovery facts.
pub const SCHEMA: &str = "louiselm.launch.recovery/1";
/// Schema of an exact payload plus its machine signature.
pub const SIGNED_SCHEMA: &str = "louiselm.launch.signed-recovery/1";
const MAX_BYTES: usize = 64 * 1024;

/// Closed recovery record refusal, without external payloads in diagnostics.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProofError {
    /// Unsupported, malformed or oversized document.
    #[error("malformed recovery proof")]
    Malformed,
    /// The received bytes are not the canonical document.
    #[error("noncanonical recovery proof")]
    NonCanonical,
    /// Binding or digest evidence is invalid.
    #[error("invalid recovery binding")]
    InvalidBinding,
    /// Recovery must attest two different canonical kernel boots.
    #[error("invalid recovery boot evidence")]
    InvalidBoot,
    /// Machine signature verification failed.
    #[error("recovery signature refused")]
    InvalidSignature,
}

/// Non-secret binding to original consumption bytes and exact retained history.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    /// Original scope; never a new or renewed launch authorization.
    pub authorization: LaunchAuthorization,
    /// Digest of the exact retained consumption record.
    pub consumption_digest: String,
    /// Exact verified nonterminal head, absent only for receiptless recovery.
    /// Omitting absence preserves the canonical bytes of accepted receiptless proofs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt_head: Option<ReceiptHead>,
}

impl Binding {
    pub(crate) fn validate(&self) -> Result<(), ProofError> {
        self.authorization
            .validate()
            .map_err(|_| ProofError::InvalidBinding)?;
        for digest in std::iter::once(&self.consumption_digest)
            .chain(self.receipt_head.as_ref().map(|head| &head.digest))
        {
            if !Digest::parse(digest).is_ok_and(|value| value.to_string() == *digest) {
                return Err(ProofError::InvalidBinding);
            }
        }
        Ok(())
    }
}

/// Retained storage was checked behind the trusted root directory chain.
/// Absence alone is never proof that the old process tree ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetainedStorage {
    /// The exact Session directory does not exist below protected ancestors.
    Absent,
    /// Its root-owned directory is durably sealed 0700; bytes remain intact.
    Sealed,
}

/// What the privileged recovery owner actually proved after an explicit arm.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Payload {
    /// Independent recovery schema/signature namespace.
    pub schema: String,
    /// Original consumption, Run, operator and assigned identity.
    pub binding: Binding,
    /// Digest of the root-owned arming document.
    pub arm_digest: String,
    /// Exact root genesis registration, including signed but undelivered preparation.
    pub receipt_binding_digest: Option<String>,
    /// Kernel boot in which the root owner fenced the slot and armed recovery.
    pub armed_boot_id: String,
    /// Different kernel boot in which exclusive cleanup was proved.
    pub recovered_boot_id: String,
    /// Privacy barrier established before the identity lease was released.
    pub retained_storage: RetainedStorage,
    /// Trusted installed release executing this recovery operation.
    pub release_id: String,
    /// Machine-held launcher key signing this recovery fact.
    pub signing_key_id: String,
}

impl Payload {
    /// Exact bytes signed under the independent recovery namespace.
    /// # Panics
    /// Only if this JSON-native derived schema gains a fallible serializer.
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "The closed derived schema contains only JSON-native values."
    )]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("JSON-native recovery payload")
    }

    /// Validate closed structural scope and boot evidence, without authorizing I/O.
    /// # Errors
    /// Refuses malformed schemas, digests, scope or equal/invalid kernel boots.
    pub fn validate(&self) -> Result<(), ProofError> {
        if self.schema != SCHEMA || self.canonical_bytes().len() > MAX_BYTES {
            return Err(ProofError::Malformed);
        }
        self.binding.validate()?;
        for digest in [&self.arm_digest, &self.release_id, &self.signing_key_id] {
            if !Digest::parse(digest).is_ok_and(|value| value.to_string() == *digest) {
                return Err(ProofError::InvalidBinding);
            }
        }
        if self.receipt_binding_digest.as_ref().is_some_and(|digest| {
            !Digest::parse(digest).is_ok_and(|value| value.to_string() == *digest)
        }) {
            return Err(ProofError::InvalidBinding);
        }
        if !valid_boot_id(&self.armed_boot_id)
            || !valid_boot_id(&self.recovered_boot_id)
            || self.armed_boot_id == self.recovered_boot_id
        {
            return Err(ProofError::InvalidBoot);
        }
        Ok(())
    }
}

/// Exact signed recovery bytes, stored separately from all Session receipt chains.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Proof {
    /// Signed envelope schema.
    pub schema: String,
    /// Actual recovery assertion.
    pub payload: Payload,
    /// Machine SSHSIG signature; never a hardware/operator signature.
    pub signature: String,
}

impl Proof {
    /// Exact canonical envelope bytes retained by both recovery and broker owners.
    /// # Panics
    /// Only if the JSON-native derived schema gains a fallible serializer.
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "The closed derived schema contains only JSON-native values."
    )]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("JSON-native recovery proof")
    }

    /// Digest used for exact durable acknowledgement and idempotent retry.
    #[must_use]
    pub fn digest(&self) -> String {
        Digest::of(&self.canonical_bytes()).to_string()
    }

    /// Validate envelope shape and payload, without trusting its signature.
    /// # Errors
    /// Refuses invalid scope, boot evidence, schemas and signature/size bounds.
    pub fn validate(&self) -> Result<(), ProofError> {
        if self.schema != SIGNED_SCHEMA
            || self.signature.is_empty()
            || self.signature.len() > 16 * 1024
            || self.canonical_bytes().len() > MAX_BYTES
        {
            return Err(ProofError::Malformed);
        }
        self.payload.validate()
    }

    /// Parse exact canonical signed bytes at the persistence/transport boundary.
    /// # Errors
    /// Refuses malformed, noncanonical, oversized or structurally invalid records.
    pub fn parse(bytes: &[u8]) -> Result<Self, ProofError> {
        if bytes.len() > MAX_BYTES {
            return Err(ProofError::Malformed);
        }
        let proof: Self = serde_json::from_slice(bytes).map_err(|_| ProofError::Malformed)?;
        proof.validate()?;
        if proof.canonical_bytes() != bytes {
            return Err(ProofError::NonCanonical);
        }
        Ok(proof)
    }
}

/// The exact recovery proof became durable; this is not a receipt-chain head.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Acknowledgement {
    /// Independent durable recovery acknowledgement schema.
    pub schema: String,
    /// Original consumed authorization identity.
    pub authorization_id: String,
    /// Original subject Session, without claiming it was launched.
    pub session_id: String,
    /// Original Run; no counters are reset.
    pub run_id: String,
    /// Digest of the exact signed proof durably stored by the broker.
    pub proof_digest: String,
}

impl Acknowledgement {
    /// Construct only after the exact proof and directory are durable.
    #[must_use]
    pub fn for_proof(proof: &Proof) -> Self {
        let authorization = &proof.payload.binding.authorization;
        Self {
            schema: "louiselm.launch.recovery-stored/1".into(),
            authorization_id: authorization.authorization_id.clone(),
            session_id: authorization.session_id.clone(),
            run_id: authorization.run_id.clone(),
            proof_digest: proof.digest(),
        }
    }
}

pub(crate) fn valid_boot_id(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(index, byte)| {
            if [8, 13, 18, 23].contains(&index) {
                byte == b'-'
            } else {
                byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
            }
        })
}
