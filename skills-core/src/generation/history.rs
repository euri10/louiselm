//! Closed, version-aware wire decoding. Legacy bytes remain legacy bytes;
//! reading them does not authorize them as active supply.

use serde::{Deserialize, Deserializer, Serialize, Serializer, ser::SerializeStruct};

use super::{GENERATION_SCHEMA, GenerationPayload, HISTORICAL_SCHEMA, Member};
use crate::canonical::Digest;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoricalMember {
    package_digest: String,
    dossier_digest: String,
    review_depth: String,
    agents: Vec<String>,
}

impl From<&Member> for HistoricalMember {
    fn from(member: &Member) -> Self {
        Self {
            package_digest: member.package_digest.clone(),
            dossier_digest: member.dossier_digest.clone(),
            review_depth: member.review_depth.clone(),
            agents: member.agents.clone(),
        }
    }
}

impl From<HistoricalMember> for Member {
    fn from(member: HistoricalMember) -> Self {
        Self {
            package_digest: member.package_digest,
            dossier_digest: member.dossier_digest,
            review_depth: member.review_depth,
            agents: member.agents,
            // An in-memory projection only; never added to historical bytes.
            relied_on: Vec::new(),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Fields<M> {
    trust_domain: String,
    sequence: u64,
    predecessor: Option<String>,
    signer_role: String,
    policy_digest: String,
    member_root: String,
    members: Vec<M>,
    approval_operation: Option<String>,
}

#[derive(Deserialize)]
#[serde(tag = "schema")]
enum WirePayload {
    #[serde(rename = "louiselm.skills.generation/2")]
    Historical(Fields<HistoricalMember>),
    #[serde(rename = "louiselm.skills.generation/3")]
    Current(Fields<Member>),
}

impl<'de> Deserialize<'de> for GenerationPayload {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match WirePayload::deserialize(deserializer)? {
            WirePayload::Historical(fields) => Ok(fields.into_payload(HISTORICAL_SCHEMA)),
            WirePayload::Current(fields) => Ok(fields.into_payload(GENERATION_SCHEMA)),
        }
    }
}

impl<M: Into<Member>> Fields<M> {
    fn into_payload(self, schema: &str) -> GenerationPayload {
        GenerationPayload {
            schema: schema.to_owned(),
            trust_domain: self.trust_domain,
            sequence: self.sequence,
            predecessor: self.predecessor,
            signer_role: self.signer_role,
            policy_digest: self.policy_digest,
            member_root: self.member_root,
            members: self.members.into_iter().map(Into::into).collect(),
            approval_operation: self.approval_operation,
        }
    }
}

impl Serialize for GenerationPayload {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // Field order is part of the signed protocol, for both versions.
        let mut state = serializer.serialize_struct(
            "GenerationPayload",
            8 + usize::from(self.approval_operation.is_some()),
        )?;
        state.serialize_field("schema", &self.schema)?;
        state.serialize_field("trust_domain", &self.trust_domain)?;
        state.serialize_field("sequence", &self.sequence)?;
        state.serialize_field("predecessor", &self.predecessor)?;
        state.serialize_field("signer_role", &self.signer_role)?;
        state.serialize_field("policy_digest", &self.policy_digest)?;
        state.serialize_field("member_root", &self.member_root)?;
        if self.schema == HISTORICAL_SCHEMA {
            state.serialize_field("members", &historical_members(&self.members))?;
        } else {
            state.serialize_field("members", &self.members)?;
        }
        if let Some(operation) = &self.approval_operation {
            state.serialize_field("approval_operation", operation)?;
        }
        state.end()
    }
}

fn historical_members(members: &[Member]) -> Vec<HistoricalMember> {
    members.iter().map(HistoricalMember::from).collect()
}

#[expect(
    clippy::expect_used,
    reason = "Only JSON-native fields are serialized."
)]
pub(super) fn member_root(members: &[Member]) -> Digest {
    Digest::of(
        &serde_json::to_vec(&historical_members(members))
            .expect("historical members are always serializable"),
    )
}
