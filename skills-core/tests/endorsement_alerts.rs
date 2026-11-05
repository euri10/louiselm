//! Behavioral coverage for withdrawal alerts on admitted supply.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Test fixtures abort on setup failure and assert failures directly."
)]

//! A withdrawal never changes a Generation. It tells the operator which
//! admitted members depended on the withdrawn evidence, so they can decide
//! about quarantine themselves.

mod support;

use louiselm_skills::{
    Digest, Policy, ReviewDepth, SshKeygenSigner, TrustStore,
    admission::{self, AdmissionMember, AdmissionRequest},
    endorsement::{
        self, Batch, EndorsedPackage, EndorserRole, Signing,
        alerts::{self, Urgency},
        import, local,
        trust::{EndorserEntry, MAX_LIFETIME_MS},
    },
    generation::GenerationRecord,
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

    fn capture(&self, body: &str) -> Digest {
        let candidate = self.fixture.candidate("skills/demo");
        write_file(
            &candidate.join("SKILL.md"),
            &format!("---\nname: demo\ndescription: Demo.\n---\n\n{body}\n"),
        );
        self.fixture.capture(&candidate).unwrap().0.digest
    }

    fn publish(&self, key: &SshKey, batch: &Batch) {
        for bytes in endorsement::sign_batch(
            &Signing {
                signer_role: EndorserRole::Publisher,
                signer_key: key.public_key(),
                issued_at_ms: NOW - 1,
                expires_at_ms: NOW + 1_000,
            },
            batch,
            &SshKeygenSigner::new(key.private_key_path()),
        )
        .unwrap()
        {
            import::import_bytes(&self.fixture.store(), &bytes).unwrap();
        }
    }

    fn endorse(&self, package: &Digest) {
        self.publish(
            &self.publisher,
            &Batch::Endorsements(vec![EndorsedPackage {
                package_digest: package.clone(),
                policy_digest: Policy::embedded().digest().clone(),
                dossier_digest: None,
                assessment: None,
                review_depth: ReviewDepth::Read,
                assumptions: Vec::new(),
            }]),
        );
    }

    fn admit(&self, package: &Digest) -> GenerationRecord {
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

    fn alerts(&self, generation: &GenerationRecord) -> Vec<alerts::WithdrawalAlert> {
        alerts::withdrawal_alerts(
            &self.fixture.store(),
            generation,
            &generation.payload.member_digests(),
        )
        .unwrap()
    }
}

#[test]
fn withdrawing_relied_on_evidence_alerts_urgently_and_changes_nothing() {
    let world = World::new();
    let package = world.capture("v1");
    world.endorse(&package);
    let generation = world.admit(&package);
    assert_eq!(world.alerts(&generation), Vec::new());

    world.publish(&world.publisher, &Batch::Withdrawals(vec![package.clone()]));

    let alerts = world.alerts(&generation);
    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0].member, package.to_string());
    assert_eq!(alerts[0].endorser, "Publisher");
    assert_eq!(alerts[0].urgency, Urgency::ReliedOn);
    let reloaded = admission::load(
        &world.fixture.store(),
        &Digest::parse(&generation.generation).unwrap(),
    )
    .unwrap();
    assert_eq!(reloaded.payload, generation.payload, "no Generation change");
}

#[test]
fn withdrawing_the_endorsed_base_alerts_the_member_framed_by_it() {
    let world = World::new();
    let first = world.capture("v1");
    world.endorse(&first);
    let second = world.capture("v2");
    let generation = world.admit(&second);
    assert_eq!(generation.payload.members[0].relied_on.len(), 1);

    world.publish(&world.publisher, &Batch::Withdrawals(vec![first]));

    let alerts = world.alerts(&generation);
    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0].member, second.to_string());
    assert_eq!(alerts[0].urgency, Urgency::ReliedOn);
}

#[test]
fn a_trusted_withdrawal_the_member_did_not_rely_on_is_lower_urgency() {
    let world = World::new();
    let package = world.capture("v1");
    let generation = world.admit(&package);

    world.publish(&world.publisher, &Batch::Withdrawals(vec![package.clone()]));

    let alerts = world.alerts(&generation);
    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0].urgency, Urgency::SubjectOnly);
}

#[test]
fn an_untrusted_strangers_withdrawal_raises_nothing() {
    let world = World::new();
    let stranger = SshKey::generate(&world.fixture, "stranger");
    let package = world.capture("v1");
    let generation = world.admit(&package);

    world.publish(&stranger, &Batch::Withdrawals(vec![package]));

    assert_eq!(world.alerts(&generation), Vec::new());
}

#[test]
fn a_quarantined_member_is_not_alerted() {
    let world = World::new();
    let package = world.capture("v1");
    world.endorse(&package);
    let generation = world.admit(&package);
    world.publish(&world.publisher, &Batch::Withdrawals(vec![package]));

    let alerts = alerts::withdrawal_alerts(&world.fixture.store(), &generation, &[]).unwrap();
    assert_eq!(alerts, Vec::new());
}
