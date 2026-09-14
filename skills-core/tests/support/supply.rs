//! Admitted supply plus authenticated discovery fixtures shared by consumer tests.

use crate::discovery_support::DiscoveryFixture;
use crate::support::{SshKey, write_file};
use louiselm_skills::{
    Policy, TrustStore,
    admission::{self, AdmissionMember, AdmissionRequest},
    discovery_source::{SourceControl, SourceKind},
    dossier::ReviewDepth,
    instruction_view,
    registry::Registry,
    signer::SshKeygenSigner,
    sshsig::SkPolicy,
    witness::GitWitness,
};

pub struct Supply {
    pub discovery: DiscoveryFixture,
    pub registry: Registry,
}

impl Supply {
    pub fn new() -> Self {
        let mut discovery = DiscoveryFixture::new();
        let fixture = &discovery.fixture;
        let primary = SshKey::generate(fixture, "primary");
        let release = SshKey::generate(fixture, "release");
        TrustStore::bootstrap(
            &fixture.store(),
            "louiselm/skills",
            &primary.public_key(),
            &release.public_key(),
            SkPolicy::none(),
            1,
        )
        .unwrap();
        let candidate = fixture.candidate("supply");
        write_file(
            &candidate.join("SKILL.md"),
            "---\nname: supply\ndescription: Fixture.\n---\nBody.\n",
        );
        let package = fixture.capture(&candidate).unwrap().0.digest;
        let record = admission::admit(
            &fixture.store(),
            &Policy::embedded(),
            &AdmissionRequest {
                members: vec![AdmissionMember {
                    package,
                    depth: ReviewDepth::Read,
                    agents: vec![discovery.request.agent_id.clone()],
                }],
                signer: &SshKeygenSigner::new(primary.private_key_path()),
                admitted_at_ms: 2,
            },
        )
        .unwrap();
        let witness = GitWitness::new(
            &fixture.witness_remote(),
            "generations",
            &fixture.path("witness-work"),
        );
        admission::witness(&fixture.store(), &record.digest(), &witness, 3).unwrap();
        admission::activate(&fixture.store(), &record.digest(), 4).unwrap();
        discovery.manifest.agent.environment.insert(
            "FIXTURE_SECRET".into(),
            "private-marker-never-display".into(),
        );
        discovery.manifest.agent.provider =
            louiselm_skills::registry::Provider::Fixed("provider-brand-never-display".into());
        let registry_root = fixture.path("registry");
        for (name, entries) in [
            ("agents", serde_json::json!([discovery.manifest.agent])),
            ("runtimes", serde_json::json!([discovery.runtime])),
        ] {
            write_file(
                &registry_root.join(format!("{name}.json")),
                &serde_json::json!({"schema":"louiselm.launch.registry/1", "entries":entries})
                    .to_string(),
            );
        }
        let registry = Registry::open(&registry_root).unwrap();
        let views = instruction_view::materialize(&fixture.store(), &Policy::embedded(), &registry)
            .unwrap();
        let view = &views[&discovery.request.agent_id];
        discovery.manifest.skill_generation.generation_digest = record.generation;
        discovery.manifest.skill_generation.view_digest = view.digest().to_string();
        discovery.manifest.policy_digest = Policy::embedded().digest().to_string();
        discovery.manifest.provider_disclosure.providers =
            vec!["provider-brand-never-display".into()];
        discovery.control(
            SourceKind::ManagedSkills,
            SourceControl::FrozenSnapshot {
                digest: view.digest().to_string(),
            },
        );
        discovery.sign();
        Self {
            discovery,
            registry,
        }
    }
}
