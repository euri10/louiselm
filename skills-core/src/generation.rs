//! The Skill Generation: the signed root binding one complete approved set.
//!
//! A Generation is the whole supply or it is nothing. It binds every admitted
//! package, the Dossier each was approved from, the claimed review depth, the
//! Agents whose Instruction view the package enters, the governing policy, and
//! its place in the chain — sequence and predecessor — so that additions,
//! deletions, replacements, re-scoping, policy changes, and rollback are all
//! visible as changes to a signed record rather than as edits to a directory.
//!
//! Membership is per Agent, never per Provider: the harness decides whether a
//! skill applies, and one Agent may route through several Providers
//! (louiselm-5qzq). It is stored as literal Agent names, never a wildcard, so
//! the record's meaning cannot change when the registry gains an entry.

use serde::{Deserialize, Serialize};

use crate::{canonical::Digest, witness::WitnessEvidence};

/// The Generation payload schema this build signs and reads.
pub const GENERATION_SCHEMA: &str = "louiselm.skills.generation/2";

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
    /// Agents whose Instruction view this package enters, sorted and unique.
    ///
    /// Literal names only. An Agent this host does not run is valid: the same
    /// Generation is meant to be readable on another machine.
    pub agents: Vec<String>,
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
}

impl GenerationPayload {
    /// Builds a payload, normalizing membership and computing the set root.
    ///
    /// Members sort by package digest and each Agent list sorts and
    /// deduplicates, so the same scoping decision always signs to the same
    /// bytes.
    #[must_use]
    pub fn new(
        trust_domain: &str,
        sequence: u64,
        predecessor: Option<String>,
        policy_digest: &str,
        mut members: Vec<Member>,
    ) -> Self {
        members.sort_by(|left, right| left.package_digest.cmp(&right.package_digest));
        for member in &mut members {
            member.agents.sort();
            member.agents.dedup();
        }
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
        }
    }

    /// Serializes the payload to the exact bytes that are signed.
    ///
    /// # Panics
    /// Panics only if serialization fails after a future schema change introduces
    /// a fallible serializer. The current derived schema has only JSON-native values.
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "Derived schema has only JSON-native values and string-keyed maps, with no custom serializers."
    )]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("a generation payload is always serializable")
    }

    /// Returns the Generation's identity: the digest of its signed bytes.
    #[must_use]
    pub fn digest(&self) -> Digest {
        Digest::of(&self.canonical_bytes())
    }

    /// Recomputes the set root from the members the payload actually lists.
    ///
    /// A verifier calls this instead of reading `member_root`, so a payload
    /// cannot claim a root that does not cover what it contains.
    #[must_use]
    pub fn recomputed_member_root(&self) -> Digest {
        member_root(&self.members)
    }

    /// Returns every admitted package digest, in payload order.
    #[must_use]
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
    #[must_use]
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
    #[must_use]
    pub fn digest(&self) -> Digest {
        self.payload.digest()
    }

    /// Returns the exact bytes a witness must hold for this Generation.
    ///
    /// Payload and signature only: local state must never change what the
    /// remote is asked to confirm.
    ///
    /// # Panics
    /// Panics only if serialization fails after a future schema change introduces
    /// a fallible serializer. The current derived schema has only JSON-native values.
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "Derived schema has only JSON-native values and string-keyed maps, with no custom serializers."
    )]
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

#[expect(
    clippy::expect_used,
    reason = "Derived schema has only JSON-native values and string-keyed maps, with no custom serializers."
)]
fn member_root(members: &[Member]) -> Digest {
    Digest::of(&serde_json::to_vec(members).expect("members are always serializable"))
}
