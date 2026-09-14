//! Supply dimensions derive independently from trusted component artifacts.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Tests assert failures and abort on fixture errors."
)]

#[path = "support/discovery.rs"]
mod discovery_support;
#[path = "support/supply.rs"]
mod supply_support;
mod support;

use louiselm_skills::{
    Policy, Store,
    discovery::{AuthenticatedInputs, DiscoveryProof},
    discovery_source::{SourceControl, SourceKind},
    posture::{DimensionInput, DimensionName, DimensionState, FailureCode, Posture},
    preflight::{self, ComparisonState, IdentityField, ManifestState},
    registry::Registry,
    render, robot, supply_posture,
};
use supply_support::Supply;
use support::Fixture;

#[test]
fn prospective_preflight_checks_artifacts_without_promoting_proposed_controls() {
    let supply = Supply::new();
    let discovery = &supply.discovery;
    let preview = preflight::inspect(
        &discovery.request,
        Some(&discovery.manifest),
        Some(&discovery.fixture.store()),
        &Policy::embedded(),
        Some(&supply.registry),
        None,
    )
    .unwrap();
    assert_eq!(preview.manifest_state, ManifestState::Matched);
    assert_eq!(
        preview.posture.dimensions.managed_supply.state,
        DimensionState::Verified
    );
    assert_eq!(
        preview.posture.dimensions.runtime.state,
        DimensionState::Verified
    );
    for dimension in [
        &preview.posture.dimensions.native_supply,
        &preview.posture.dimensions.isolation,
        &preview.posture.dimensions.network,
        &preview.posture.dimensions.provider_disclosure,
    ] {
        assert_eq!(dimension.state, DimensionState::Failed);
    }
    assert!(!preview.posture.is_fully_verified());
    assert_eq!(preview.comparison.state, ComparisonState::NotRequested);
    assert_eq!(
        preview.request_digest,
        discovery.request.digest().to_string()
    );
    for output in [
        robot::payload(&preview).unwrap(),
        preflight::render(&preview),
    ] {
        assert!(!output.contains("private-marker-never-display"));
        assert!(!output.contains("provider-brand-never-display"));
        assert!(!output.contains(discovery.fixture.path("").to_str().unwrap()));
        assert!(output.contains(&discovery.request.session_input_manifest_id));
    }
}

#[test]
fn preflight_missing_or_contradictory_manifest_never_checks_different_inputs() {
    let supply = Supply::new();
    let discovery = &supply.discovery;
    let mut changed = discovery.manifest.clone();
    changed.envelope.revision += 1;
    for (manifest, state) in [
        (None, ManifestState::Missing),
        (Some(&changed), ManifestState::Contradictory),
    ] {
        let preview = preflight::inspect(
            &discovery.request,
            manifest,
            Some(&discovery.fixture.store()),
            &Policy::embedded(),
            Some(&supply.registry),
            None,
        )
        .unwrap();
        assert_eq!(preview.manifest_state, state);
        for (_, dimension) in preview.posture.dimensions.ordered() {
            assert_eq!(dimension.state, DimensionState::Failed);
        }
    }
}

#[test]
fn preflight_artifact_drift_is_dimension_specific_and_prior_selection_is_explicit() {
    let supply = Supply::new();
    let discovery = &supply.discovery;
    let prior = discovery.manifest.clone();
    let prior_request = discovery.request.clone();
    let mut current = prior.clone();
    current.envelope.revision += 1;
    let mut request = prior_request.clone();
    request.envelope_revision = current.envelope.revision;
    request.session_input_manifest_id = current.digest().to_string();
    std::fs::write(discovery.runtime.executable_path(), "changed bytes").unwrap();
    let preview = preflight::inspect(
        &request,
        Some(&current),
        Some(&discovery.fixture.store()),
        &Policy::embedded(),
        Some(&supply.registry),
        Some((&prior_request, &prior)),
    )
    .unwrap();
    assert_eq!(
        preview.posture.dimensions.managed_supply.state,
        DimensionState::Verified
    );
    assert_eq!(
        preview.posture.dimensions.runtime.failure_code,
        Some(FailureCode::RuntimeDrift)
    );
    assert_eq!(preview.comparison.state, ComparisonState::Compared);
    assert!(
        preview
            .comparison
            .changes
            .iter()
            .any(|change| change.field == IdentityField::EnvelopeRevision)
    );
    assert!(
        preview
            .comparison
            .unresolved
            .contains(&IdentityField::NetworkScope)
    );
    assert!(
        !preview
            .comparison
            .changes
            .iter()
            .any(|change| change.field == IdentityField::NetworkScope)
    );
}

#[test]
fn preflight_refuses_malformed_inputs_and_incomparable_priors() {
    let supply = Supply::new();
    let discovery = &supply.discovery;
    let inspect =
        |request: &louiselm_skills::launch::LaunchRequest,
         manifest: &louiselm_skills::session_manifest::SessionInputManifest| {
            preflight::inspect(
                request,
                Some(manifest),
                None,
                &Policy::embedded(),
                None,
                Some((&discovery.request, &discovery.manifest)),
            )
        };
    let mut manifest = discovery.manifest.clone();
    let mut request = discovery.request.clone();
    manifest.agent.id = "another-agent".into();
    request.agent_id = manifest.agent.id.clone();
    request.session_input_manifest_id = manifest.digest().to_string();
    let preview = inspect(&request, &manifest).unwrap();
    assert_eq!(preview.comparison.state, ComparisonState::DifferentAgent);
    assert!(preview.comparison.changes.is_empty());
    request.agent_id = discovery.request.agent_id.clone();
    let preview = inspect(&request, &manifest).unwrap();
    assert_eq!(preview.manifest_state, ManifestState::Contradictory);
    assert_eq!(preview.comparison.state, ComparisonState::InputsUnavailable);
    manifest.schema = "untrusted-marker".into();
    let error = inspect(&request, &manifest).unwrap_err().to_string();
    assert!(!error.contains("untrusted-marker"));
    request.schema = "untrusted-marker".into();
    assert!(inspect(&request, &discovery.manifest).is_err());
}

#[test]
fn absent_supply_store_does_not_hide_runtime_or_invent_an_isolation_contract() {
    let supply = Supply::new();
    let discovery = &supply.discovery;
    let preview = preflight::inspect(
        &discovery.request,
        Some(&discovery.manifest),
        None,
        &Policy::embedded(),
        Some(&supply.registry),
        None,
    )
    .unwrap();
    assert_eq!(
        preview.posture.dimensions.runtime.state,
        DimensionState::Verified
    );
    assert_eq!(
        preview.posture.dimensions.managed_supply.failure_code,
        Some(FailureCode::EvidenceMissing)
    );
    let mut manifest = discovery.manifest.clone();
    manifest.runtime.origin = "private-marker\nuntrusted-origin".into();
    let mut request = discovery.request.clone();
    request.session_input_manifest_id = manifest.digest().to_string();
    let preview = preflight::inspect(
        &request,
        Some(&manifest),
        None,
        &Policy::embedded(),
        None,
        None,
    )
    .unwrap();
    assert!(!robot::payload(&preview).unwrap().contains("private-marker"));
    assert!(preview.proposed.iter().any(|identity| identity.field
        == IdentityField::IsolationContract
        && identity.value.is_none()));
}

impl Supply {
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
