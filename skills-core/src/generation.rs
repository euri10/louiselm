//! The Skill Generation: the signed root binding one complete approved set.
//!
//! A Generation is the whole supply or it is nothing. It binds every admitted
//! package, the Dossier each was approved from, the claimed review depth, the
//! governing policy, the Provider view roots, and its place in the chain —
//! sequence and predecessor — so that additions, deletions, replacements,
//! policy changes, and rollback are all visible as changes to a signed record
//! rather than as edits to a directory.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{canonical::Digest, witness::WitnessEvidence};

/// The Generation payload schema this build signs and reads.
pub const GENERATION_SCHEMA: &str = "louiselm.skills.generation/1";

/// The stored record schema.
pub const RECORD_SCHEMA: &str = "louiselm.skills.generation-record/1";

/// One admitted package and the review it was admitted on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    /// Digest of the admitted package.
    pub package_digest: String,
    /// Digest of the portable Dossier the reviewer approved from.
    pub dossier_digest: String,
    /// The reviewer's claimed review depth. A claim, not a proof.
    pub review_depth: String,
}

/// The bytes a hardware key signs to admit a Generation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerationPayload {
    /// Schema identifier.
    pub schema: String,
    /// Trust domain the signature is scoped to.
    pub trust_domain: String,
    /// Position in the chain; the first Generation is one.
    pub sequence: u64,
    /// Digest of the Generation this one follows, or none for the first.
    pub predecessor: Option<String>,
    /// Role that signed, which must be `primary` for a routine Admission.
    pub signer_role: String,
    /// Digest of the Inspection policy in force when the set was reviewed.
    pub policy_digest: String,
    /// Root over the member list, recomputed by every verifier.
    pub member_root: String,
    /// Admitted members, sorted by package digest.
    pub members: Vec<Member>,
    /// Provider view roots, keyed by Provider name.
    pub view_roots: BTreeMap<String, String>,
}

impl GenerationPayload {
    /// Builds a payload, sorting members and computing the set root.
    pub fn new(
        trust_domain: &str,
        sequence: u64,
        predecessor: Option<String>,
        policy_digest: &str,
        mut members: Vec<Member>,
        view_roots: BTreeMap<String, String>,
    ) -> Self {
        members.sort_by(|left, right| left.package_digest.cmp(&right.package_digest));
        let member_root = member_root(&members).to_string();
        Self {
            schema: GENERATION_SCHEMA.to_owned(),
            trust_domain: trust_domain.to_owned(),
            sequence,
            predecessor,
            signer_role: "primary".to_owned(),
            policy_digest: policy_digest.to_owned(),
            member_root,
            members,
            view_roots,
        }
    }

    /// Serializes the payload to the exact bytes that are signed.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("a generation payload is always serializable")
    }

    /// Returns the Generation's identity: the digest of its signed bytes.
    pub fn digest(&self) -> Digest {
        Digest::of(&self.canonical_bytes())
    }

    /// Recomputes the set root from the members the payload actually lists.
    ///
    /// A verifier calls this instead of reading `member_root`, so a payload
    /// cannot claim a root that does not cover what it contains.
    pub fn recomputed_member_root(&self) -> Digest {
        member_root(&self.members)
    }

    /// Returns every admitted package digest, in payload order.
    pub fn member_digests(&self) -> Vec<String> {
        self.members
            .iter()
            .map(|member| member.package_digest.clone())
            .collect()
    }
}

/// Where a Generation stands in its lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GenerationState {
    /// Signed and stored, but not yet confirmed on the witness remote. This is
    /// the candidate state: it governs nothing.
    PendingWitness,
    /// Witnessed and activated; this is the supply in force.
    Current,
    /// Replaced by a Generation with a higher sequence.
    Superseded,
    /// Current, but narrowed by an active quarantine.
    Quarantined,
    /// Failed verification and must not be activated.
    Invalid,
}

impl GenerationState {
    /// Returns the name used in robot output.
    pub fn name(self) -> &'static str {
        match self {
            Self::PendingWitness => "pending_witness",
            Self::Current => "current",
            Self::Superseded => "superseded",
            Self::Quarantined => "quarantined",
            Self::Invalid => "invalid",
        }
    }
}

/// A stored Generation: the signed bytes, the signature, and local state.
///
/// Only `payload` and `signature` are witnessed. Everything else is local
/// bookkeeping, so a witness remote never becomes a source of authority about
/// what is current here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerationRecord {
    /// Schema identifier.
    pub schema: String,
    /// The Generation's identity, recomputed on every load.
    pub generation: String,
    /// The signed payload.
    pub payload: GenerationPayload,
    /// The armored SSH signature over the payload's canonical bytes.
    pub signature: String,
    /// Local lifecycle state.
    pub state: GenerationState,
    /// Witness evidence, once the exact bytes were confirmed on the remote.
    pub witness: Option<WitnessEvidence>,
    /// When the Admission ceremony completed.
    pub admitted_at_ms: u64,
    /// Why the record is invalid, when it is.
    pub invalid_reason: Option<String>,
}

impl GenerationRecord {
    /// Returns the Generation's identity.
    pub fn digest(&self) -> Digest {
        self.payload.digest()
    }

    /// Returns the exact bytes a witness must hold for this Generation.
    ///
    /// Payload and signature only: local state must never change what the
    /// remote is asked to confirm.
    pub fn witness_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(&WitnessedGeneration {
            payload: &self.payload,
            signature: &self.signature,
        })
        .expect("a witnessed generation is always serializable")
    }
}

#[derive(Serialize)]
struct WitnessedGeneration<'a> {
    payload: &'a GenerationPayload,
    signature: &'a str,
}

fn member_root(members: &[Member]) -> Digest {
    Digest::of(&serde_json::to_vec(members).expect("members are always serializable"))
}
