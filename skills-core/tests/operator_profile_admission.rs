//! Behavioral coverage for admitting the Operator profile.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Test fixtures abort on setup failure and assert failures directly."
)]

//! One profile per Generation, reaching every configured Agent. Admission
//! proves the reach from the configured Agent list it is given; without that
//! list a profile is refused rather than admitted to an unknown audience.

mod support;

use louiselm_skills::{
    Digest, Policy, ReviewDepth, SshKeygenSigner, TrustStore,
    admission::{self, AdmissionError, AdmissionMember, AdmissionRequest},
    sshsig::SkPolicy,
};
use support::{Fixture, SshKey, write_file};

const NOW: u64 = 1_800_000_000_000;

struct World {
    fixture: Fixture,
    primary: SshKey,
}

impl World {
    fn new() -> Self {
        let fixture = Fixture::new();
        let primary = SshKey::generate(&fixture, "primary");
        let release = SshKey::generate(&fixture, "release");
        TrustStore::bootstrap(
            &fixture.store(),
            "louiselm/skills",
            &primary.public_key(),
            &release.public_key(),
            SkPolicy::none(),
            NOW,
        )
        .unwrap();
        Self { fixture, primary }
    }

    fn capture(&self, name: &str, file: &str, body: &str) -> Digest {
        let candidate = self.fixture.candidate(name);
        write_file(&candidate.join(file), body);
        self.fixture.capture(&candidate).unwrap().0.digest
    }

    fn profile(&self, name: &str, line: &str) -> Digest {
        self.capture(
            name,
            "PROFILE.md",
            &format!("---\ntype: louiselm/operator-profile\n---\n\n{line}\n"),
        )
    }

    fn skill(&self) -> Digest {
        self.capture(
            "skill",
            "SKILL.md",
            "---\nname: demo\ndescription: Demo.\n---\n\nBody.\n",
        )
    }

    fn admit(
        &self,
        members: &[(&Digest, &[&str])],
        configured: Option<&[&str]>,
    ) -> Result<louiselm_skills::GenerationRecord, AdmissionError> {
        admission::admit(
            &self.fixture.store(),
            &Policy::embedded(),
            &AdmissionRequest {
                members: members
                    .iter()
                    .map(|(package, agents)| AdmissionMember {
                        package: (*package).clone(),
                        depth: ReviewDepth::Read,
                        agents: agents.iter().map(|agent| (*agent).to_owned()).collect(),
                    })
                    .collect(),
                signer: &SshKeygenSigner::new(self.primary.private_key_path()),
                admitted_at_ms: NOW,
                configured_agents: configured
                    .map(|agents| agents.iter().map(|agent| (*agent).to_owned()).collect()),
            },
        )
    }
}

const ALL: &[&str] = &["claude", "codex", "opencode"];

#[test]
fn a_profile_reaching_every_configured_agent_is_admitted_beside_skills() {
    let world = World::new();
    let profile = world.profile("profile", "Use plain English.");
    let skill = world.skill();

    let generation = world
        .admit(&[(&profile, ALL), (&skill, &["claude"])], Some(ALL))
        .unwrap();

    assert_eq!(generation.payload.members.len(), 2);
}

#[test]
fn a_second_profile_is_refused() {
    let world = World::new();
    let first = world.profile("one", "Use plain English.");
    let second = world.profile("two", "Prefer option framing.");

    let error = world
        .admit(&[(&first, ALL), (&second, ALL)], Some(ALL))
        .unwrap_err();
    assert!(matches!(error, AdmissionError::ProfileTwice), "{error:?}");
}

#[test]
fn a_profile_missing_a_configured_agent_is_refused_by_name() {
    let world = World::new();
    let profile = world.profile("profile", "Use plain English.");

    let error = world
        .admit(&[(&profile, &["claude", "codex"])], Some(ALL))
        .unwrap_err();
    assert!(
        matches!(&error, AdmissionError::ProfileScope { missing, .. } if missing == &vec!["opencode".to_owned()]),
        "{error:?}"
    );
}

#[test]
fn a_profile_without_the_configured_agent_list_is_refused() {
    let world = World::new();
    let profile = world.profile("profile", "Use plain English.");

    let error = world.admit(&[(&profile, ALL)], None).unwrap_err();
    assert!(
        matches!(error, AdmissionError::ProfileScopeUnknown { .. }),
        "{error:?}"
    );
}

#[test]
fn skills_are_admitted_without_a_configured_agent_list() {
    let world = World::new();
    let skill = world.skill();

    assert!(world.admit(&[(&skill, &["claude"])], None).is_ok());
}

#[test]
fn status_names_the_profile_and_the_agents_it_does_not_reach() {
    let world = World::new();
    let profile = world.profile("profile", "Use plain English.");
    let generation = world.admit(&[(&profile, ALL)], Some(ALL)).unwrap();
    let store = world.fixture.store();

    let delivery = admission::profile_delivery(
        &store,
        &generation,
        &[
            "claude".into(),
            "codex".into(),
            "opencode".into(),
            "grok".into(),
        ],
    )
    .unwrap();

    assert_eq!(delivery.profile, Some(profile.to_string()));
    assert_eq!(delivery.undelivered, vec!["grok".to_owned()]);
}
