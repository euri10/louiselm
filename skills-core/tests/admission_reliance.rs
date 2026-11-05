//! Behavioral coverage for recording Endorsement reliance in Skill Admission.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Test fixtures abort on setup failure and assert failures directly."
)]

//! A signed Generation says which Endorsements framed each member's review.
//! The trusted tool recomputes that from its own focus; nothing an Agent
//! supplies reaches the record, and local trust never changes the portable
//! Dossier digest a member binds.

mod support;

use louiselm_skills::{
    Digest, Policy, ReviewDepth, SshKeygenSigner, TrustStore,
    admission::{self, AdmissionError, AdmissionMember, AdmissionRequest},
    dossier::{Dossier, DossierRequest},
    endorsement::{
        self, Batch, EndorsedPackage, EndorserRole, Signing, import, local,
        trust::{EndorserEntry, MAX_LIFETIME_MS},
    },
    generation::{GENERATION_SCHEMA, GenerationPayload, Member},
    sshsig::SkPolicy,
};
use support::{Fixture, SshKey, write_file};

const NOW: u64 = 1_800_000_000_000;

struct World {
    fixture: Fixture,
    primary: SshKey,
    publisher: SshKey,
}

impl World {
    fn new() -> Self {
        let fixture = Fixture::new();
        let primary = SshKey::generate(&fixture, "primary");
        let release = SshKey::generate(&fixture, "release");
        let publisher = SshKey::generate(&fixture, "publisher");
        TrustStore::bootstrap(
            &fixture.store(),
            "louiselm/skills",
            &primary.public_key(),
            &release.public_key(),
            SkPolicy::none(),
            NOW,
        )
        .unwrap();
        local::add(
            &fixture.store(),
            &EndorserEntry {
                label: "Publisher".into(),
                max_lifetime_ms: MAX_LIFETIME_MS,
                public_key: publisher.public_key(),
                roles: vec![EndorserRole::Publisher],
                sk_policy: SkPolicy::none(),
            },
            &SshKeygenSigner::new(primary.private_key_path()),
            NOW,
        )
        .unwrap();
        Self {
            fixture,
            primary,
            publisher,
        }
    }

    fn capture(&self) -> Digest {
        let candidate = self.fixture.candidate("skills/demo");
        write_file(
            &candidate.join("SKILL.md"),
            "---\nname: demo\ndescription: Demo.\n---\n\nBody.\n",
        );
        self.fixture.capture(&candidate).unwrap().0.digest
    }

    fn endorse(&self, package: &Digest) -> Digest {
        let bytes = endorsement::sign_batch(
            &Signing {
                signer_role: EndorserRole::Publisher,
                signer_key: self.publisher.public_key(),
                issued_at_ms: NOW - 1,
                expires_at_ms: NOW + 1_000,
            },
            &Batch::Endorsements(vec![EndorsedPackage {
                package_digest: package.clone(),
                policy_digest: Policy::embedded().digest().clone(),
                dossier_digest: None,
                assessment: None,
                review_depth: ReviewDepth::Read,
                assumptions: Vec::new(),
            }]),
            &SshKeygenSigner::new(self.publisher.private_key_path()),
        )
        .unwrap()
        .remove(0);
        import::import_bytes(&self.fixture.store(), &bytes)
            .unwrap()
            .digest
    }

    fn admit(&self, package: &Digest) -> louiselm_skills::GenerationRecord {
        admission::admit(
            &self.fixture.store(),
            &Policy::embedded(),
            &AdmissionRequest {
                members: vec![AdmissionMember {
                    package: package.clone(),
                    depth: ReviewDepth::Skimmed,
                    agents: vec!["claude".into()],
                }],
                signer: &SshKeygenSigner::new(self.primary.private_key_path()),
                admitted_at_ms: NOW,
            },
        )
        .unwrap()
    }
}

#[test]
fn admission_records_the_endorsements_that_framed_the_review() {
    let world = World::new();
    let package = world.capture();
    let record = world.endorse(&package);

    let generation = world.admit(&package);

    assert_eq!(
        generation.payload.members[0].relied_on,
        vec![record.to_string()]
    );
    let trust = TrustStore::load(&world.fixture.store()).unwrap().unwrap();
    admission::verify_record(&world.fixture.store(), &generation, &trust)
        .expect("the reliance is covered by the Primary signature");
}

#[test]
fn admission_without_trusted_evidence_records_no_reliance() {
    let world = World::new();
    let package = world.capture();

    let generation = world.admit(&package);

    assert_eq!(
        generation.payload.members[0].relied_on,
        Vec::<String>::new()
    );
}

#[test]
fn local_trust_never_changes_the_bound_dossier_digest() {
    let world = World::new();
    let package = world.capture();
    let portable = Dossier::build(
        &world.fixture.store(),
        &Policy::embedded(),
        &DossierRequest::new(&package).with_review_depth(ReviewDepth::Skimmed),
    )
    .unwrap()
    .portable_digest();
    world.endorse(&package);

    let generation = world.admit(&package);

    assert_eq!(
        generation.payload.members[0].dossier_digest,
        portable.to_string()
    );
}

#[test]
fn the_member_root_covers_reliance_and_reliance_is_normalized() {
    let member = |relied_on: &[&str]| Member {
        package_digest: "sha256:aa".to_owned(),
        dossier_digest: "sha256:bb".to_owned(),
        review_depth: "read".to_owned(),
        agents: vec!["claude".to_owned()],
        relied_on: relied_on
            .iter()
            .map(|digest| (*digest).to_owned())
            .collect(),
    };
    let payload = |relied_on: &[&str]| {
        GenerationPayload::new(
            "louiselm/skills",
            1,
            None,
            "sha256:cc",
            vec![member(relied_on)],
        )
    };

    assert_ne!(
        payload(&[]).recomputed_member_root(),
        payload(&["sha256:dd"]).recomputed_member_root(),
        "recording reliance must change what the signature covers"
    );
    assert_eq!(
        payload(&["sha256:ee", "sha256:dd", "sha256:ee"]).members[0].relied_on,
        vec!["sha256:dd".to_owned(), "sha256:ee".to_owned()]
    );
    assert_eq!(GENERATION_SCHEMA, "louiselm.skills.generation/3");
}

#[test]
fn a_generation_2_record_is_refused() {
    let world = World::new();
    let package = world.capture();
    let mut generation = world.admit(&package);
    generation.payload.schema = "louiselm.skills.generation/2".into();
    let trust = TrustStore::load(&world.fixture.store()).unwrap().unwrap();

    let error = admission::verify_record(&world.fixture.store(), &generation, &trust).unwrap_err();
    assert!(
        matches!(&error, AdmissionError::Chain(reason) if reason.contains("generation/2")),
        "{error:?}"
    );
}
