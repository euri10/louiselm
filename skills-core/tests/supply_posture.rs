//! Supply dimensions derive independently from trusted component artifacts.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Tests assert failures and abort on fixture errors."
)]

#[path = "support/discovery.rs"]
mod discovery_support;
mod support;

use discovery_support::DiscoveryFixture;
use louiselm_skills::{
    Policy, Store, TrustStore,
    admission::{self, AdmissionMember, AdmissionRequest},
    discovery::{AuthenticatedInputs, DiscoveryProof},
    discovery_source::{SourceControl, SourceKind},
    dossier::ReviewDepth,
    instruction_view,
    posture::{DimensionInput, DimensionName, DimensionState, FailureCode, Posture},
    registry::Registry,
    render, robot,
    signer::SshKeygenSigner,
    sshsig::SkPolicy,
    supply_posture,
    witness::GitWitness,
};
use support::{Fixture, SshKey, write_file};

struct Supply {
    discovery: DiscoveryFixture,
    registry: Registry,
}

impl Supply {
    fn new() -> Self {
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

    fn posture(
        &self,
        store: &Store,
        bound: Option<&AuthenticatedInputs>,
        proof: Option<&DiscoveryProof>,
    ) -> Posture {
        let request = &self.discovery.request;
        let mut inputs = supply_posture::derive(
            request,
            store,
            &Policy::embedded(),
            &self.registry,
            bound,
            proof,
        )
        .unwrap()
        .to_vec();
        inputs.extend(
            [DimensionName::Isolation, DimensionName::Network].map(|dimension| {
                DimensionInput::failed(dimension, FailureCode::EvidenceMissing, vec![])
            }),
        );
        Posture::evaluate(&request.session_id, &request.run_id, inputs).unwrap()
    }
}

fn assert_supply_states(posture: &Posture, failed: Option<DimensionName>) {
    for (name, dimension) in posture.dimensions.ordered() {
        if matches!(name, DimensionName::Isolation | DimensionName::Network) {
            continue;
        }
        assert_eq!(
            dimension.state,
            if Some(name) == failed {
                DimensionState::Failed
            } else {
                DimensionState::Verified
            },
            "{name:?}"
        );
        assert_eq!(dimension.failure_code.is_some(), Some(name) == failed);
        assert!(!dimension.next_action.id.is_empty());
    }
    assert!(
        !posture.is_fully_verified(),
        "supply proof alone is not a confined launch"
    );
}

#[test]
fn four_supply_dimensions_verify_without_claiming_full_launch_verification() {
    let supply = Supply::new();
    let bound = supply.discovery.authenticate().unwrap();
    let proof = DiscoveryProof::verify(&bound, &supply.discovery.runtime).unwrap();
    assert_supply_states(
        &supply.posture(
            &supply.discovery.fixture.store(),
            Some(&bound),
            Some(&proof),
        ),
        None,
    );
}

#[test]
fn each_failed_dimension_leaves_the_other_three_visible() {
    let supply = Supply::new();
    let bound = supply.discovery.authenticate().unwrap();
    let proof = DiscoveryProof::verify(&bound, &supply.discovery.runtime).unwrap();
    let store = supply.discovery.fixture.store();
    assert_supply_states(
        &supply.posture(&Fixture::new().store(), Some(&bound), Some(&proof)),
        Some(DimensionName::ManagedSupply),
    );
    assert_supply_states(
        &supply.posture(&store, Some(&bound), None),
        Some(DimensionName::NativeSupply),
    );
    assert_supply_states(
        &supply.posture(&store, None, Some(&proof)),
        Some(DimensionName::ProviderDisclosure),
    );
    std::fs::write(
        supply.discovery.runtime.executable_path(),
        "runtime changed",
    )
    .unwrap();
    let posture = supply.posture(&store, Some(&bound), Some(&proof));
    assert_supply_states(&posture, Some(DimensionName::Runtime));
    assert_eq!(
        posture.dimensions.runtime.failure_code,
        Some(FailureCode::RuntimeDrift)
    );
}

#[test]
fn missing_artifacts_never_verify_by_default() {
    let mut supply = Supply::new();
    let empty = Fixture::new();
    supply.registry = Registry::open(&empty.path("empty-registry")).unwrap();
    let posture = supply.posture(&empty.store(), None, None);
    for (_, dimension) in posture.dimensions.ordered() {
        assert_eq!(dimension.state, DimensionState::Failed);
        assert!(dimension.failure_code.is_some());
        assert!(dimension.evidence.is_empty());
        assert!(!dimension.next_action.id.is_empty());
    }
}

#[test]
fn evidence_from_another_request_cannot_verify_native_supply_or_disclosure() {
    let mut supply = Supply::new();
    let bound = supply.discovery.authenticate().unwrap();
    let proof = DiscoveryProof::verify(&bound, &supply.discovery.runtime).unwrap();
    supply.discovery.request.run_id = "other-run".into();
    let posture = supply.posture(
        &supply.discovery.fixture.store(),
        Some(&bound),
        Some(&proof),
    );
    assert_eq!(
        posture.dimensions.native_supply.state,
        DimensionState::Failed
    );
    assert_eq!(
        posture.dimensions.provider_disclosure.state,
        DimensionState::Failed
    );
}

#[test]
fn bound_manifest_does_not_hide_changed_view_or_agent_configuration() {
    let mut supply = Supply::new();
    supply.discovery.manifest.skill_generation.view_digest =
        louiselm_skills::Digest::of(b"wrong view").to_string();
    supply.discovery.control(
        SourceKind::ManagedSkills,
        SourceControl::FrozenSnapshot {
            digest: supply
                .discovery
                .manifest
                .skill_generation
                .view_digest
                .clone(),
        },
    );
    supply.discovery.sign();
    let bound = supply.discovery.authenticate().unwrap();
    let proof = DiscoveryProof::verify(&bound, &supply.discovery.runtime).unwrap();
    assert_supply_states(
        &supply.posture(
            &supply.discovery.fixture.store(),
            Some(&bound),
            Some(&proof),
        ),
        Some(DimensionName::ManagedSupply),
    );

    let mut supply = Supply::new();
    supply
        .discovery
        .manifest
        .agent
        .arguments
        .push("changed-argument".into());
    supply.discovery.sign();
    let bound = supply.discovery.authenticate().unwrap();
    let proof = DiscoveryProof::verify(&bound, &supply.discovery.runtime).unwrap();
    assert_supply_states(
        &supply.posture(
            &supply.discovery.fixture.store(),
            Some(&bound),
            Some(&proof),
        ),
        Some(DimensionName::Runtime),
    );
}

#[test]
fn mismatched_signed_receipts_do_not_get_combined_into_native_proof() {
    let mut supply = Supply::new();
    let first = supply.discovery.authenticate().unwrap();
    let proof = DiscoveryProof::verify(&first, &supply.discovery.runtime).unwrap();
    supply
        .discovery
        .isolation
        .native_sources
        .as_mut()
        .unwrap()
        .sources
        .clear();
    supply.discovery.sign();
    let later = supply.discovery.authenticate().unwrap();
    assert_eq!(first.request(), later.request());
    assert_ne!(first.receipt_id(), later.receipt_id());
    assert_supply_states(
        &supply.posture(
            &supply.discovery.fixture.store(),
            Some(&later),
            Some(&proof),
        ),
        Some(DimensionName::NativeSupply),
    );
}

#[test]
fn signed_unsafe_runtime_controls_fail_even_while_registered_bytes_match() {
    for disable_fixed_path in [true, false] {
        let mut supply = Supply::new();
        let sources = supply.discovery.isolation.native_sources.as_mut().unwrap();
        if disable_fixed_path {
            sources.fixed_executable = false;
        } else {
            sources.self_update_disabled = false;
        }
        supply.discovery.sign();
        let bound = supply.discovery.authenticate().unwrap();
        let posture = supply.posture(&supply.discovery.fixture.store(), Some(&bound), None);
        assert_eq!(
            posture.dimensions.runtime.failure_code,
            Some(FailureCode::RuntimeDrift)
        );
        assert_eq!(
            posture.dimensions.managed_supply.state,
            DimensionState::Verified
        );
        assert_eq!(
            posture.dimensions.provider_disclosure.state,
            DimensionState::Verified
        );
    }
}

#[test]
fn human_and_robot_status_share_safe_structured_supply_state() {
    let supply = Supply::new();
    let bound = supply.discovery.authenticate().unwrap();
    let proof = DiscoveryProof::verify(&bound, &supply.discovery.runtime).unwrap();
    let posture = supply.posture(
        &supply.discovery.fixture.store(),
        Some(&bound),
        Some(&proof),
    );
    let human = render::posture(&posture);
    let encoded = robot::payload(&posture).unwrap();
    let json: serde_json::Value = serde_json::from_str(&encoded).unwrap();
    assert_eq!(json["schema"], louiselm_skills::posture::POSTURE_SCHEMA);
    for (name, dimension) in posture.dimensions.ordered() {
        let value = &json["dimensions"][name.name()];
        assert_eq!(value["state"], dimension.state.name());
        assert_eq!(value["requirement"], dimension.requirement.name());
        assert!(human.contains(&format!("{}: {}", name.name(), dimension.state.name())));
        assert!(human.contains(dimension.requirement.name()));
        assert!(human.contains(&dimension.next_action.id));
        assert_eq!(
            value["next_action"],
            serde_json::to_value(&dimension.next_action).unwrap()
        );
        assert_eq!(
            value["evidence"],
            serde_json::to_value(&dimension.evidence).unwrap()
        );
        assert_eq!(
            value["failure_code"],
            serde_json::to_value(dimension.failure_code).unwrap()
        );
    }
    for output in [&human, &encoded] {
        assert!(output.contains("visible to that Provider despite local containment"));
        assert!(output.contains("not admitted Skill supply"));
        for secret in [
            "private-marker-never-display",
            "provider-brand-never-display",
            "initial instructions",
            "AGENTS.md",
        ] {
            assert!(!output.contains(secret));
        }
    }
}
