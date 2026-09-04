use std::collections::BTreeSet;

use louiselm_skills::{
    posture::{
        DimensionInput, DimensionName, DimensionPosture, EvidenceKind, EvidenceRef, FailureCode,
        POSTURE_SCHEMA, PROVIDER_DISCLOSURE_NOTICE, Posture, PostureError, PostureState,
        Requirement,
    },
    render, robot,
};

const SESSION_ID: &str = "session-123";
const RUN_ID: &str = "11111111-2222-4333-8444-555555555555";

fn evidence(kind: EvidenceKind, id: &str) -> EvidenceRef {
    EvidenceRef::new(kind, id).expect("fixture evidence identifier is valid")
}

fn verified_inputs() -> Vec<DimensionInput> {
    vec![
        DimensionInput::verified(
            DimensionName::ManagedSupply,
            vec![evidence(
                EvidenceKind::SkillGeneration,
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            )],
        ),
        DimensionInput::verified(
            DimensionName::NativeSupply,
            vec![evidence(
                EvidenceKind::SessionInputManifest,
                "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
            )],
        ),
        DimensionInput::verified(
            DimensionName::Runtime,
            vec![evidence(
                EvidenceKind::RuntimeMeasurement,
                "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            )],
        ),
        DimensionInput::verified(
            DimensionName::Isolation,
            vec![evidence(
                EvidenceKind::IsolationReceipt,
                "isolation-contract-1",
            )],
        ),
        DimensionInput::verified(
            DimensionName::Network,
            vec![evidence(EvidenceKind::CapabilityEnvelope, "envelope-7")],
        ),
        DimensionInput::verified(
            DimensionName::ProviderDisclosure,
            vec![evidence(
                EvidenceKind::SessionInputManifest,
                "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
            )],
        ),
    ]
}

fn fixture_posture() -> Posture {
    let mut inputs = verified_inputs();
    inputs[1] = DimensionInput::failed(
        DimensionName::NativeSupply,
        FailureCode::NativeSupplyUncertain,
        Vec::new(),
    );
    inputs[4] = DimensionInput::waived(
        DimensionName::Network,
        FailureCode::BrokerUnavailable,
        vec![evidence(EvidenceKind::CapabilityEnvelope, "envelope-7")],
        evidence(EvidenceKind::WaiverReceipt, "waiver-42"),
    );
    Posture::evaluate(SESSION_ID, RUN_ID, inputs).expect("fixture posture is valid")
}

fn dimension(posture: &Posture, name: DimensionName) -> &DimensionPosture {
    match name {
        DimensionName::ManagedSupply => &posture.dimensions.managed_supply,
        DimensionName::NativeSupply => &posture.dimensions.native_supply,
        DimensionName::Runtime => &posture.dimensions.runtime,
        DimensionName::Isolation => &posture.dimensions.isolation,
        DimensionName::Network => &posture.dimensions.network,
        DimensionName::ProviderDisclosure => &posture.dimensions.provider_disclosure,
    }
}

#[test]
fn fully_verified_requires_all_six_trusted_dimensions() {
    let posture = Posture::evaluate(SESSION_ID, RUN_ID, verified_inputs())
        .expect("six trusted dimensions evaluate");

    assert_eq!(posture.schema, POSTURE_SCHEMA);
    assert_eq!(posture.state, PostureState::FullyVerified);
    assert!(posture.is_fully_verified());
    assert_eq!(
        posture.dimensions.managed_supply.requirement,
        Requirement::WitnessedGeneration
    );
    assert_eq!(
        posture.dimensions.provider_disclosure.requirement,
        Requirement::CloudPlaintextDisclosure
    );
    assert_eq!(
        posture.provider_disclosure_notice,
        PROVIDER_DISCLOSURE_NOTICE
    );
}

#[test]
fn missing_duplicate_and_wrong_evidence_fail_closed() {
    let mut missing = verified_inputs();
    missing.pop();
    assert!(matches!(
        Posture::evaluate(SESSION_ID, RUN_ID, missing),
        Err(PostureError::MissingDimension(
            DimensionName::ProviderDisclosure
        ))
    ));

    let mut duplicate = verified_inputs();
    duplicate.push(DimensionInput::verified(
        DimensionName::ManagedSupply,
        vec![evidence(EvidenceKind::SkillGeneration, "generation-2")],
    ));
    assert!(matches!(
        Posture::evaluate(SESSION_ID, RUN_ID, duplicate),
        Err(PostureError::DuplicateDimension(
            DimensionName::ManagedSupply
        ))
    ));

    let mut wrong_evidence = verified_inputs();
    wrong_evidence[3] = DimensionInput::verified(
        DimensionName::Isolation,
        vec![evidence(EvidenceKind::SkillGeneration, "generation-1")],
    );
    assert!(matches!(
        Posture::evaluate(SESSION_ID, RUN_ID, wrong_evidence),
        Err(PostureError::EvidenceKind {
            dimension: DimensionName::Isolation,
            kind: EvidenceKind::SkillGeneration,
        })
    ));

    let mut empty_evidence = verified_inputs();
    empty_evidence[2] = DimensionInput::verified(DimensionName::Runtime, Vec::new());
    assert!(matches!(
        Posture::evaluate(SESSION_ID, RUN_ID, empty_evidence),
        Err(PostureError::EvidenceRequired(DimensionName::Runtime))
    ));
}

#[test]
fn failure_codes_are_dimension_specific_and_derive_the_next_action() {
    let mut inputs = verified_inputs();
    inputs[0] = DimensionInput::failed(
        DimensionName::ManagedSupply,
        FailureCode::WitnessMissing,
        vec![evidence(EvidenceKind::SkillGeneration, "generation-1")],
    );
    let posture = Posture::evaluate(SESSION_ID, RUN_ID, inputs).expect("known failure evaluates");

    assert_eq!(posture.state, PostureState::Unverified);
    assert_eq!(
        posture.dimensions.managed_supply.failure_code,
        Some(FailureCode::WitnessMissing)
    );
    assert_eq!(
        posture.dimensions.managed_supply.next_action.id,
        "publish_generation_witness"
    );

    let mut contradictory = verified_inputs();
    contradictory[3] = DimensionInput::failed(
        DimensionName::Isolation,
        FailureCode::RuntimeDrift,
        Vec::new(),
    );
    assert!(matches!(
        Posture::evaluate(SESSION_ID, RUN_ID, contradictory),
        Err(PostureError::FailureCode {
            dimension: DimensionName::Isolation,
            code: FailureCode::RuntimeDrift,
        })
    ));
}

#[test]
fn every_failure_has_a_distinct_stable_code_and_action() {
    let cases = [
        (
            DimensionName::Runtime,
            FailureCode::RootTrustFailed,
            "root_trust_failed",
            "install_trusted_release",
        ),
        (
            DimensionName::ManagedSupply,
            FailureCode::SignatureInvalid,
            "signature_invalid",
            "restore_trusted_signature",
        ),
        (
            DimensionName::ManagedSupply,
            FailureCode::WitnessMissing,
            "witness_missing",
            "publish_generation_witness",
        ),
        (
            DimensionName::NativeSupply,
            FailureCode::NativeSupplyUncertain,
            "native_supply_uncertain",
            "mask_or_measure_native_supply",
        ),
        (
            DimensionName::Runtime,
            FailureCode::RuntimeDrift,
            "runtime_drift",
            "restage_runtime",
        ),
        (
            DimensionName::Isolation,
            FailureCode::IsolationFailed,
            "isolation_failed",
            "repair_isolation",
        ),
        (
            DimensionName::Network,
            FailureCode::BrokerUnavailable,
            "broker_unavailable",
            "restore_control_broker",
        ),
        (
            DimensionName::Network,
            FailureCode::AuditPersistenceUnavailable,
            "audit_persistence_unavailable",
            "restore_audit_persistence",
        ),
        (
            DimensionName::ProviderDisclosure,
            FailureCode::ProviderDisclosureMissing,
            "provider_disclosure_missing",
            "record_provider_disclosure",
        ),
        (
            DimensionName::Isolation,
            FailureCode::EvidenceMissing,
            "evidence_missing",
            "collect_trusted_evidence",
        ),
        (
            DimensionName::NativeSupply,
            FailureCode::UnknownFailure,
            "unknown_failure",
            "inspect_unknown_failure",
        ),
    ];
    let mut actions = BTreeSet::new();

    for (name, code, serialized, action) in cases {
        let mut inputs = verified_inputs();
        let index = DimensionName::ALL
            .iter()
            .position(|candidate| *candidate == name)
            .expect("dimension is canonical");
        inputs[index] = DimensionInput::failed(name, code, Vec::new());
        let posture =
            Posture::evaluate(SESSION_ID, RUN_ID, inputs).expect("known failure evaluates");

        assert_eq!(serde_json::to_value(code).unwrap(), serialized);
        assert_eq!(dimension(&posture, name).next_action.id, action);
        assert!(
            actions.insert(action),
            "next action {action} is not distinct"
        );
    }
}

#[test]
fn a_waiver_degrades_posture_and_requires_a_waiver_receipt() {
    let mut inputs = verified_inputs();
    inputs[4] = DimensionInput::waived(
        DimensionName::Network,
        FailureCode::BrokerUnavailable,
        vec![evidence(EvidenceKind::CapabilityEnvelope, "envelope-7")],
        evidence(EvidenceKind::WaiverReceipt, "waiver-42"),
    );
    let posture = Posture::evaluate(SESSION_ID, RUN_ID, inputs).expect("waiver is represented");

    assert_eq!(posture.state, PostureState::Waived);
    assert!(!posture.is_fully_verified());

    let mut wrong_receipt = verified_inputs();
    wrong_receipt[4] = DimensionInput::waived(
        DimensionName::Network,
        FailureCode::BrokerUnavailable,
        Vec::new(),
        evidence(EvidenceKind::BrokerReceipt, "broker-1"),
    );
    assert!(matches!(
        Posture::evaluate(SESSION_ID, RUN_ID, wrong_receipt),
        Err(PostureError::WaiverReceipt(DimensionName::Network))
    ));
}

#[test]
fn evidence_identifiers_cannot_carry_paths_or_hostile_text() {
    for value in ["", "/home/operator/secret", "line\nbreak", "../../escape"] {
        assert!(EvidenceRef::new(EvidenceKind::AuditReceipt, value).is_err());
    }
}

#[test]
fn the_shared_fixture_is_the_robot_view_and_drives_the_human_view() {
    let posture = fixture_posture();
    let expected: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/verified_posture_v1.json"
    ))
    .expect("shared fixture is JSON");
    let actual: serde_json::Value =
        serde_json::from_str(&robot::payload(&posture).expect("robot view serializes"))
            .expect("robot view is JSON");

    assert_eq!(actual, expected);

    let human = render::posture(&posture);
    assert!(human.contains("Verified posture: unverified"));
    assert!(human.contains("native_supply: failed (native_supply_uncertain)"));
    assert!(human.contains("network: waived (broker_unavailable)"));
    assert!(human.contains("next: restore_control_broker"));
    assert!(human.contains(PROVIDER_DISCLOSURE_NOTICE));
}
