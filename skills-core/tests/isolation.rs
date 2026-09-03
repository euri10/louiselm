//! The isolation contract: what a backend must prove, and how it fails closed.
//!
//! The contract is Provider-neutral and versioned on purpose. An adapter must
//! not get to mean something different by "sandboxed", and a backend must not
//! get to be trusted for its name — bubblewrap, systemd, and Landlock are
//! candidates that pass the same suite as anything else would.

mod support;

use louiselm_skills::{
    isolation::{
        CONTRACT_VERSION, Dimension, DimensionEvidence, IsolationEvidence, IsolationFailure,
        KernelPrerequisites,
    },
    registry::{Registry, RegistryError},
};
use support::{Fixture, write_file};

fn satisfied(dimension: Dimension) -> DimensionEvidence {
    DimensionEvidence {
        dimension,
        satisfied: true,
        mechanism: "test".to_owned(),
        detail: "asserted by the test".to_owned(),
    }
}

fn complete_evidence() -> IsolationEvidence {
    IsolationEvidence {
        contract_version: CONTRACT_VERSION.to_owned(),
        backend: "test".to_owned(),
        backend_version: "0".to_owned(),
        kernel: KernelPrerequisites {
            user_namespaces: true,
            pid_namespaces: true,
            network_namespaces: true,
            cgroup_v2: true,
            details: Vec::new(),
        },
        dimensions: Dimension::ALL.iter().copied().map(satisfied).collect(),
    }
}

#[test]
fn every_dimension_must_be_covered_before_anything_is_verified() {
    complete_evidence()
        .check()
        .expect("complete, consistent evidence is accepted");

    let mut missing = complete_evidence();
    missing
        .dimensions
        .retain(|evidence| evidence.dimension != Dimension::NetworkDenial);

    match missing.check().expect_err("missing evidence fails closed") {
        IsolationFailure::Missing(dimensions) => {
            assert_eq!(dimensions, vec![Dimension::NetworkDenial]);
        }
        other => panic!("unexpected failure: {other}"),
    }
}

#[test]
fn a_dimension_that_is_reported_unsatisfied_fails_closed() {
    let mut denied = complete_evidence();
    denied
        .dimensions
        .iter_mut()
        .find(|evidence| evidence.dimension == Dimension::Identity)
        .expect("identity is covered")
        .satisfied = false;

    match denied
        .check()
        .expect_err("an unsatisfied dimension fails closed")
    {
        IsolationFailure::Unsatisfied(dimensions) => {
            assert_eq!(dimensions, vec![Dimension::Identity]);
        }
        other => panic!("unexpected failure: {other}"),
    }
}

#[test]
fn evidence_that_contradicts_itself_is_refused_rather_than_resolved() {
    let mut contradictory = complete_evidence();
    contradictory.dimensions.push(DimensionEvidence {
        dimension: Dimension::NetworkDenial,
        satisfied: false,
        mechanism: "other".to_owned(),
        detail: "a second, disagreeing answer".to_owned(),
    });

    match contradictory
        .check()
        .expect_err("two answers for one dimension fail closed")
    {
        IsolationFailure::Contradictory(dimensions) => {
            assert_eq!(dimensions, vec![Dimension::NetworkDenial]);
        }
        other => panic!("unexpected failure: {other}"),
    }
}

#[test]
fn evidence_from_another_contract_version_is_not_read() {
    let mut foreign = complete_evidence();
    foreign.contract_version = "louiselm.isolation/99".to_owned();

    assert!(matches!(
        foreign
            .check()
            .expect_err("a foreign contract fails closed"),
        IsolationFailure::ContractVersion { .. },
    ));
}

#[test]
fn a_kernel_without_the_prerequisites_cannot_satisfy_the_contract() {
    let mut evidence = complete_evidence();
    evidence.kernel.user_namespaces = false;

    assert!(matches!(
        evidence
            .check()
            .expect_err("a missing prerequisite fails closed"),
        IsolationFailure::KernelPrerequisite(_),
    ));
}

#[test]
fn the_registry_resolves_only_what_it_was_given() {
    let fixture = Fixture::new();
    let registry_root = fixture.path("registry");
    let runtime_root = fixture.path("runtime");
    write_file(&runtime_root.join("bin/agent"), "#!/bin/sh\nexec cat\n");
    write_file(&runtime_root.join("lib/adapter.js"), "// adapter\n");

    support::write_registry(&registry_root, &runtime_root);
    let registry = Registry::open(&registry_root).expect("the registry opens");

    let agent = registry.agent("demo").expect("the agent is registered");
    assert_eq!(agent.provider, "demo-provider");
    assert_eq!(agent.runtime_id, "demo-runtime");

    let runtime = registry
        .runtime(&agent.runtime_id)
        .expect("the runtime is registered");
    let measurement = runtime.measure().expect("the runtime measures");
    assert_eq!(measurement.executable_sha256, runtime.executable_sha256);
    assert_eq!(measurement.adapters.len(), 1);

    assert!(matches!(
        registry.agent("not-registered"),
        Err(RegistryError::Unknown { .. }),
    ));
    assert!(matches!(
        registry.runtime("not-registered"),
        Err(RegistryError::Unknown { .. }),
    ));
}

#[test]
fn a_runtime_that_changed_since_registration_cannot_be_launched() {
    let fixture = Fixture::new();
    let registry_root = fixture.path("registry");
    let runtime_root = fixture.path("runtime");
    write_file(&runtime_root.join("bin/agent"), "#!/bin/sh\nexec cat\n");
    write_file(&runtime_root.join("lib/adapter.js"), "// adapter\n");
    support::write_registry(&registry_root, &runtime_root);
    let registry = Registry::open(&registry_root).expect("the registry opens");
    let runtime = registry
        .runtime("demo-runtime")
        .expect("the runtime is registered");

    // Exactly what a self-updating Provider runtime does.
    write_file(
        &runtime_root.join("bin/agent"),
        "#!/bin/sh\nexec cat # updated\n",
    );

    let error = runtime.measure().expect_err("a mutated runtime is refused");

    assert!(
        matches!(error, RegistryError::RuntimeChanged { .. }),
        "unexpected error: {error}",
    );
}
