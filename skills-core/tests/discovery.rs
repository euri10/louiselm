//! Discovery evidence is authenticated, complete, and pinned to frozen inputs.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Test fixtures abort on setup failure."
)]

#[path = "support/discovery.rs"]
mod discovery_support;
mod support;
use discovery_support::DiscoveryFixture;

use louiselm_skills::{
    discovery::{DiscoveryError, DiscoveryProof, Inventory},
    discovery_source::{SourceControl, SourceKind},
    posture::{DimensionName, FailureCode},
};

#[test]
fn complete_authenticated_sources_produce_a_bound_proof() {
    let fixture = DiscoveryFixture::new();
    let bound = fixture.authenticate().unwrap();
    let proof = DiscoveryProof::verify(&bound, &fixture.runtime).unwrap();
    assert_eq!(proof.manifest_id(), fixture.manifest.digest().to_string());
    assert_eq!(proof.sources().len(), SourceKind::ALL.len());
    assert!(
        proof
            .embedded_instructions_notice()
            .contains("runtime trust")
    );
}

#[test]
fn proof_cannot_be_replayed_for_another_launch_of_identical_inputs() {
    let mut fixture = DiscoveryFixture::new();
    let proof = DiscoveryProof::verify(&fixture.authenticate().unwrap(), &fixture.runtime).unwrap();
    assert!(proof.matches_request(&fixture.request));
    fixture.request.run_id = "another-run".into();
    assert!(!proof.matches_request(&fixture.request));
    assert!(fixture.authenticate().is_err());
}

#[test]
fn missing_or_misbound_signed_observations_cannot_prove_discovery() {
    let mutations: [fn(&mut DiscoveryFixture); 7] = [
        |f| {
            f.isolation.native_sources = None;
        },
        |f| {
            f.isolation.native_sources.as_mut().unwrap().schema = "unknown".into();
        },
        |f| {
            f.isolation
                .native_sources
                .as_mut()
                .unwrap()
                .inventory_digest = "unknown".into();
        },
        |f| {
            f.isolation.native_sources.as_mut().unwrap().evidence_id = "another-receipt".into();
        },
        |f| {
            f.isolation.native_sources.as_mut().unwrap().sources[0]
                .source
                .path = "other".into();
        },
        |f| {
            f.control(
                SourceKind::NativeMcp,
                SourceControl::Masked {
                    evidence_id: "other".into(),
                },
            );
        },
        |f| {
            f.isolation.dimensions[0].satisfied = false;
        },
    ];
    for mutate in mutations {
        let mut fixture = DiscoveryFixture::new();
        mutate(&mut fixture);
        fixture.sign();
        assert!(
            DiscoveryProof::verify(&fixture.authenticate().unwrap(), &fixture.runtime).is_err()
        );
    }
}

#[test]
fn receipt_subject_and_manifest_substitution_are_refused() {
    let mutations: [fn(&mut DiscoveryFixture); 4] = [
        |f| {
            f.anchor.session_id = "other".into();
        },
        |f| {
            f.request.authorization_id = "other".into();
        },
        |f| {
            f.manifest.agent.arguments.push("secret-argument".into());
        },
        |f| {
            f.manifest.envelope.revision += 1;
        },
    ];
    for mutate in mutations {
        let mut fixture = DiscoveryFixture::new();
        mutate(&mut fixture);
        let error = fixture.authenticate().unwrap_err();
        assert!(!error.to_string().contains("secret-argument"));
    }
}

#[test]
fn inventory_is_closed_canonical_complete_and_path_confined() {
    let fixture = DiscoveryFixture::new();
    let bytes = std::fs::read(
        fixture
            .runtime
            .root
            .join(louiselm_skills::discovery::INVENTORY_PATH),
    )
    .unwrap();
    let inventory = Inventory::parse(&bytes).unwrap();
    let mutations: [fn(&mut Inventory); 5] = [
        |i| {
            i.sources.pop();
        },
        |i| {
            i.sources.push(i.sources[0].clone());
        },
        |i| {
            i.sources[0].path = "../escape".into();
        },
        |i| {
            i.schema = "unknown".into();
        },
        |i| {
            i.sources[0].id = "bad/id".into();
        },
    ];
    for mutate in mutations {
        let mut invalid = inventory.clone();
        mutate(&mut invalid);
        assert!(Inventory::parse(&invalid.canonical_bytes()).is_err());
    }
    let mut json = serde_json::to_value(&inventory).unwrap();
    json["unknown"] = true.into();
    assert!(Inventory::parse(&serde_json::to_vec(&json).unwrap()).is_err());
    assert!(Inventory::parse(&serde_json::to_vec_pretty(&inventory).unwrap()).is_err());
    assert!(Inventory::parse(&vec![b' '; 65_537]).is_err());
}

#[test]
fn raw_or_tampered_evidence_cannot_be_promoted_to_authority() {
    let mut fixture = DiscoveryFixture::new();
    fixture.receipt.signature.push('x');
    assert!(fixture.authenticate().is_err());
    fixture.sign();
    fixture
        .isolation
        .native_sources
        .as_mut()
        .unwrap()
        .sources
        .clear();
    assert!(
        fixture.authenticate().is_err(),
        "changed observations are not covered by the signature"
    );
    fixture.sign();
    let error =
        DiscoveryProof::verify(&fixture.authenticate().unwrap(), &fixture.runtime).unwrap_err();
    assert_eq!(error.code(), "source_set_mismatch");
}

#[test]
fn every_source_requires_exactly_one_matching_control() {
    for kind in SourceKind::ALL {
        let mut fixture = DiscoveryFixture::new();
        let evidence = fixture.isolation.native_sources.as_mut().unwrap();
        evidence.sources.retain(|source| source.source.kind != kind);
        fixture.sign();
        assert!(
            DiscoveryProof::verify(&fixture.authenticate().unwrap(), &fixture.runtime).is_err(),
            "{kind:?}"
        );
    }
    let mut fixture = DiscoveryFixture::new();
    let evidence = fixture.isolation.native_sources.as_mut().unwrap();
    evidence.sources.push(evidence.sources[0].clone());
    fixture.sign();
    assert!(DiscoveryProof::verify(&fixture.authenticate().unwrap(), &fixture.runtime).is_err());
}

#[test]
fn undeclared_sources_and_unbound_snapshots_are_refused() {
    let mut fixture = DiscoveryFixture::new();
    let evidence = fixture.isolation.native_sources.as_mut().unwrap();
    let mut extra = evidence.sources[0].clone();
    extra.source.id = "late-source".into();
    extra.source.path = ".new/skills".into();
    evidence.sources.push(extra);
    fixture.sign();
    assert_eq!(
        DiscoveryProof::verify(&fixture.authenticate().unwrap(), &fixture.runtime)
            .unwrap_err()
            .code(),
        "source_set_mismatch"
    );

    let mut fixture = DiscoveryFixture::new();
    fixture.control(
        SourceKind::ProjectInstructions,
        SourceControl::FrozenSnapshot {
            digest: louiselm_skills::Digest::of(b"unbound").to_string(),
        },
    );
    fixture.sign();
    assert_eq!(
        DiscoveryProof::verify(&fixture.authenticate().unwrap(), &fixture.runtime)
            .unwrap_err()
            .code(),
        "snapshot_mismatch"
    );
}

#[test]
fn native_mcp_live_lookup_self_update_and_rediscovery_fail_closed() {
    let mutations: [fn(&mut DiscoveryFixture); 4] = [
        |f| {
            f.control(
                SourceKind::NativeMcp,
                SourceControl::FrozenSnapshot {
                    digest: louiselm_skills::Digest::of(b"mcp").to_string(),
                },
            );
        },
        |f| {
            f.isolation
                .native_sources
                .as_mut()
                .unwrap()
                .fixed_executable = false;
        },
        |f| {
            f.isolation
                .native_sources
                .as_mut()
                .unwrap()
                .self_update_disabled = false;
        },
        |f| {
            f.isolation
                .native_sources
                .as_mut()
                .unwrap()
                .workspace_rediscovery_disabled = false;
        },
    ];
    for mutate in mutations {
        let mut fixture = DiscoveryFixture::new();
        mutate(&mut fixture);
        fixture.sign();
        let error =
            DiscoveryProof::verify(&fixture.authenticate().unwrap(), &fixture.runtime).unwrap_err();
        assert!(matches!(
            error.dimension(),
            DimensionName::NativeSupply | DimensionName::Runtime
        ));
        assert!(matches!(
            error.failure_code(),
            FailureCode::NativeSupplyUncertain | FailureCode::RuntimeDrift
        ));
    }
}

#[test]
fn runtime_refresh_and_inventory_replacement_are_refused() {
    let fixture = DiscoveryFixture::new();
    let bound = fixture.authenticate().unwrap();
    std::fs::write(fixture.runtime.executable_path(), b"self update").unwrap();
    assert!(matches!(
        DiscoveryProof::verify(&bound, &fixture.runtime),
        Err(DiscoveryError::Runtime(_))
    ));
    let fixture = DiscoveryFixture::new();
    let bound = fixture.authenticate().unwrap();
    std::fs::write(
        fixture
            .runtime
            .root
            .join(louiselm_skills::discovery::INVENTORY_PATH),
        b"new inventory",
    )
    .unwrap();
    assert!(DiscoveryProof::verify(&bound, &fixture.runtime).is_err());
}

#[test]
fn workspace_edits_do_not_replace_the_session_instruction_snapshot() {
    let fixture = DiscoveryFixture::new();
    let bound = fixture.authenticate().unwrap();
    let workspace_file = fixture.fixture.path("workspace/AGENTS.md");
    support::write_file(&workspace_file, "later instructions");
    let proof = DiscoveryProof::verify(&bound, &fixture.runtime).unwrap();
    assert_eq!(proof.manifest_id(), fixture.manifest.digest().to_string());
    assert_ne!(
        fixture.manifest.project_instructions[0].sha256,
        louiselm_skills::Digest::of(b"later instructions").hex()
    );
    assert_eq!(
        std::fs::read_to_string(workspace_file).unwrap(),
        "later instructions"
    );
}
