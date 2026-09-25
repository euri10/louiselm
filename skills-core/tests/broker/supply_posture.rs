//! Producer-to-canonical-status coverage over the real broker transport.

use super::*;
use louiselm_skills::{
    Policy,
    broker::{BrokerSession, lifecycle::LifecycleCaller},
    discovery::AuthenticatedInputs,
    launch_protocol::{BrokerConnection, ChannelState},
    launch_receipt::ChainAnchor,
    posture::{DimensionName, DimensionState, FailureCode},
    supply_posture::SupplyEvidence,
};
use supply_support::Supply;

fn launch_supply(
    supply: &Supply,
    status_reads: usize,
) -> (
    TempDir,
    BrokerService,
    BrokerSession,
    AuthenticatedInputs,
    thread::JoinHandle<()>,
) {
    launch_supply_connections(supply, vec![BrokerConnection::Connected; status_reads])
}

fn launch_supply_connections(
    supply: &Supply,
    connections: Vec<BrokerConnection>,
) -> (
    TempDir,
    BrokerService,
    BrokerSession,
    AuthenticatedInputs,
    thread::JoinHandle<()>,
) {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("control.sock");
    let request = supply.discovery.request.clone();
    let service = lifecycle::bound_service(root.path(), &socket, &request);
    let manifest = supply.discovery.manifest.clone();
    let isolation = supply.discovery.isolation.clone();
    let (sent, received) = mpsc::sync_channel(1);
    let peer = thread::spawn(move || {
        let (authorization, channel) = supervisor_authorization(&socket, &request, 2000);
        let mut launch = launch_receipt(&authorization);
        let ReceiptOutcome::Launch { evidence, .. } = &mut launch.payload.outcome else {
            panic!("launch fixture");
        };
        evidence
            .skill_generation_id
            .clone_from(&request.skill_generation_id);
        evidence
            .session_input_manifest_id
            .clone_from(&request.session_input_manifest_id);
        evidence.runtime_measurement_digest =
            Digest::of(&serde_json::to_vec(&manifest.runtime).unwrap()).to_string();
        evidence.isolation_evidence_digest =
            Digest::of(&serde_json::to_vec(&isolation).unwrap()).to_string();
        let launch = signed(launch.payload);
        let bound = AuthenticatedInputs::verify(
            &request,
            &manifest,
            &isolation,
            &launch,
            &ChainAnchor {
                session_id: request.session_id.clone(),
                run_id: request.run_id.clone(),
                release_id: trusted_release().release_id,
                signing_key_id: trusted_release().signing_key_id,
            },
            verify_fixture_signature,
        )
        .unwrap();
        settle(|done| channel.send(launch.canonical_bytes(), done));
        expect_acknowledgement(&channel);
        let start = start_receipt(&authorization, &launch);
        settle(|done| channel.send(start.canonical_bytes(), done));
        expect_acknowledgement(&channel);
        sent.send(bound).unwrap();
        let mut current = lifecycle::status(&authorization);
        current.broker_head.as_mut().unwrap().digest = start.digest().to_string();
        current.launcher_head = current.broker_head.clone();
        for connection in connections {
            current.broker_connection = connection;
            current.channel_state = if connection == BrokerConnection::Connected {
                ChannelState::Enabled
            } else {
                ChannelState::Revoked
            };
            lifecycle::answer_one_status_query(&channel, &current);
        }
    });
    let session = service
        .serve_launch(2000, verify_fixture_signature)
        .unwrap();
    let bound = received.recv_timeout(Duration::from_secs(5)).unwrap();
    (root, service, session, bound, peer)
}

#[test]
fn admitted_supply_reaches_status_without_reprobing_or_rewriting_history() {
    let supply = Supply::new();
    let (_root, service, mut session, bound, peer) = launch_supply(&supply, 2);
    let proof =
        louiselm_skills::discovery::DiscoveryProof::verify(&bound, &supply.discovery.runtime)
            .unwrap();
    let evidence = SupplyEvidence::derive(
        bound.request(),
        &supply.discovery.fixture.store(),
        &Policy::embedded(),
        &supply.registry,
        Some(&bound),
        Some(&proof),
        3000,
    )
    .unwrap();
    service
        .retain_supply_posture(&mut session, evidence, 3000)
        .unwrap();
    let admission = service
        .receipts()
        .stored_bytes(&bound.request().session_id)
        .unwrap();
    let caller = LifecycleCaller::Operator {
        uid: CONTROLLER_UID,
    };
    let before = service
        .session_status(&mut session, &caller, 4000, verify_fixture_signature)
        .unwrap();
    // Retained launch inputs outlive ordinary changes to mutable producer files.
    // A status read must neither remeasure runtime nor rematerialize the store.
    fs::write(
        supply.discovery.runtime.executable_path(),
        "ordinary update",
    )
    .unwrap();
    fs::rename(
        supply.discovery.fixture.store_root(),
        supply.discovery.fixture.path("retired-store"),
    )
    .unwrap();
    let after = service
        .session_status(&mut session, &caller, 9000, verify_fixture_signature)
        .unwrap();
    peer.join().unwrap();
    assert_eq!(before.posture, after.posture);
    for dimension in &after.posture.dimensions {
        if matches!(
            dimension.dimension,
            DimensionName::Isolation | DimensionName::Network
        ) {
            assert_eq!(dimension.failure_code, Some(FailureCode::EvidenceMissing));
        } else {
            assert_eq!(
                dimension.state,
                DimensionState::Verified,
                "{:?}",
                dimension.dimension
            );
        }
    }
    assert_eq!(
        service
            .receipts()
            .stored_bytes(&bound.request().session_id)
            .unwrap(),
        admission
    );
    let output = String::from_utf8(after.canonical_bytes()).unwrap();
    for private in [
        "private-marker-never-display",
        "provider-brand-never-display",
        "AGENTS.md",
        supply.discovery.fixture.path("").to_str().unwrap(),
    ] {
        assert!(!output.contains(private));
    }
}

#[test]
fn native_controls_follow_connection_lifetime_without_expiring_frozen_inputs() {
    let supply = Supply::new();
    let connections = vec![
        BrokerConnection::Connected,
        BrokerConnection::Grace,
        BrokerConnection::Reconciling,
        BrokerConnection::Disconnected,
        BrokerConnection::Connected,
    ];
    let (_root, service, mut session, bound, peer) =
        launch_supply_connections(&supply, connections.clone());
    let proof =
        louiselm_skills::discovery::DiscoveryProof::verify(&bound, &supply.discovery.runtime)
            .unwrap();
    service
        .retain_supply_posture(
            &mut session,
            observed(&supply, Some(&bound), Some(&proof), 3000),
            3000,
        )
        .unwrap();
    for connection in connections {
        let status = service
            .session_status(
                &mut session,
                &LifecycleCaller::Agent,
                90_000,
                verify_fixture_signature,
            )
            .unwrap();
        assert!(status.allowed_actions.is_empty());
        assert_eq!(status.posture.dimensions[0].state, DimensionState::Verified);
        assert_eq!(status.posture.dimensions[5].state, DimensionState::Verified);
        assert_eq!(
            status.posture.dimensions[1].state,
            if connection == BrokerConnection::Connected {
                DimensionState::Verified
            } else {
                DimensionState::Failed
            }
        );
        assert_eq!(
            status.posture.dimensions[1].freshness.last_verified_at_ms,
            Some(3000)
        );
    }
    peer.join().unwrap();
}

#[test]
fn wrong_view_or_policy_fails_managed_supply_without_hiding_other_evidence() {
    for field in ["view", "policy", "generation"] {
        let mut supply = Supply::new();
        if field == "view" {
            let digest = Digest::of(b"wrong view").to_string();
            supply.discovery.manifest.skill_generation.view_digest = digest.clone();
            supply.discovery.control(
                louiselm_skills::discovery_source::SourceKind::ManagedSkills,
                louiselm_skills::discovery_source::SourceControl::FrozenSnapshot { digest },
            );
        } else if field == "policy" {
            supply.discovery.manifest.policy_digest = Digest::of(b"wrong policy").to_string();
        } else {
            supply.discovery.manifest.skill_generation.generation_digest =
                Digest::of(b"wrong generation").to_string();
        }
        supply.discovery.sign();
        let (_root, service, mut session, bound, peer) = launch_supply(&supply, 1);
        let proof =
            louiselm_skills::discovery::DiscoveryProof::verify(&bound, &supply.discovery.runtime)
                .unwrap();
        service
            .retain_supply_posture(
                &mut session,
                observed(&supply, Some(&bound), Some(&proof), 3000),
                3000,
            )
            .unwrap();
        let status = service
            .session_status(
                &mut session,
                &LifecycleCaller::Agent,
                4000,
                verify_fixture_signature,
            )
            .unwrap();
        peer.join().unwrap();
        assert_eq!(
            status.posture.dimensions[0].failure_code,
            Some(FailureCode::RootTrustFailed)
        );
        assert_eq!(
            status.posture.dimensions[0].freshness.last_verified_at_ms,
            None
        );
        assert_eq!(status.posture.dimensions[1].state, DimensionState::Verified);
        assert_eq!(status.posture.dimensions[5].state, DimensionState::Verified);
    }
}

#[test]
fn native_proof_cannot_be_combined_with_another_receipt() {
    let supply = Supply::new();
    let (_root, _service, _session, bound, peer) = launch_supply(&supply, 0);
    let other = supply.discovery.authenticate().unwrap();
    let proof =
        louiselm_skills::discovery::DiscoveryProof::verify(&other, &supply.discovery.runtime)
            .unwrap();
    assert!(
        SupplyEvidence::derive(
            bound.request(),
            &supply.discovery.fixture.store(),
            &Policy::embedded(),
            &supply.registry,
            Some(&bound),
            Some(&proof),
            3000,
        )
        .is_err()
    );
    peer.join().unwrap();
}

#[test]
fn existing_quarantine_owner_invalidates_supply_without_rewriting_admission() {
    let supply = Supply::new();
    let (root, service, mut session, bound, peer) = launch_supply(&supply, 1);
    let proof =
        louiselm_skills::discovery::DiscoveryProof::verify(&bound, &supply.discovery.runtime)
            .unwrap();
    service
        .retain_supply_posture(
            &mut session,
            observed(&supply, Some(&bound), Some(&proof), 3000),
            3000,
        )
        .unwrap();
    let original = service
        .receipts()
        .stored_bytes(&bound.request().session_id)
        .unwrap();
    // Exercise the existing durable owner; this test makes no claim about Park mechanics.
    louiselm_skills::broker::lifecycle::LifecycleStore::open(
        &root.path().join("authorizations/lifecycle"),
    )
    .unwrap()
    .quarantine(&bound.request().session_id)
    .unwrap();
    let status = service
        .session_status(
            &mut session,
            &LifecycleCaller::Agent,
            4000,
            verify_fixture_signature,
        )
        .unwrap();
    peer.join().unwrap();
    for dimension in &status.posture.dimensions {
        if matches!(
            dimension.dimension,
            DimensionName::ManagedSupply
                | DimensionName::NativeSupply
                | DimensionName::ProviderDisclosure
        ) {
            assert_eq!(dimension.failure_code, Some(FailureCode::Quarantined));
            assert_eq!(dimension.freshness.last_verified_at_ms, Some(3000));
            assert!(!dimension.evidence.is_empty());
        }
    }
    assert_eq!(
        service
            .receipts()
            .stored_bytes(&bound.request().session_id)
            .unwrap(),
        original
    );
}

#[test]
fn producer_refuses_a_different_registered_agent_or_runtime() {
    for field in ["agent", "runtime"] {
        let mut supply = Supply::new();
        if field == "agent" {
            supply
                .discovery
                .manifest
                .agent
                .arguments
                .push("different".into());
        } else {
            supply.discovery.manifest.runtime.version = "different".into();
        }
        supply.discovery.sign();
        let bound = supply.discovery.authenticate().unwrap();
        assert!(
            SupplyEvidence::derive(
                bound.request(),
                &supply.discovery.fixture.store(),
                &Policy::embedded(),
                &supply.registry,
                Some(&bound),
                None,
                3000,
            )
            .is_err()
        );
    }
}

fn observed(
    supply: &Supply,
    bound: Option<&AuthenticatedInputs>,
    proof: Option<&louiselm_skills::discovery::DiscoveryProof>,
    at: u64,
) -> SupplyEvidence {
    SupplyEvidence::derive(
        &supply.discovery.request,
        &supply.discovery.fixture.store(),
        &Policy::embedded(),
        &supply.registry,
        bound,
        proof,
        at,
    )
    .unwrap()
}

#[test]
fn foreign_requests_receipts_and_stale_results_cannot_replace_retained_facts() {
    let supply = Supply::new();
    let (_root, service, mut session, bound, peer) = launch_supply(&supply, 1);
    for change in ["session", "run", "generation", "manifest", "revision"] {
        let mut request = bound.request().clone();
        match change {
            "session" => request.session_id = "different".into(),
            "run" => request.run_id = "different".into(),
            "generation" => request.skill_generation_id = Digest::of(b"different").to_string(),
            "manifest" => request.session_input_manifest_id = Digest::of(b"different").to_string(),
            "revision" => request.envelope_revision += 1,
            _ => unreachable!(),
        }
        assert!(
            SupplyEvidence::derive(
                &request,
                &supply.discovery.fixture.store(),
                &Policy::embedded(),
                &supply.registry,
                Some(&bound),
                None,
                3000,
            )
            .is_err(),
            "{change}"
        );
        // Even a producer's missing-source result must match the authorized request.
        let missing = SupplyEvidence::derive(
            &request,
            &supply.discovery.fixture.store(),
            &Policy::embedded(),
            &supply.registry,
            None,
            None,
            3000,
        )
        .unwrap();
        assert!(
            service
                .retain_supply_posture(&mut session, missing, 3000)
                .is_err()
        );
    }
    let other_receipt = supply.discovery.authenticate().unwrap();
    assert_eq!(other_receipt.request(), bound.request());
    assert_ne!(other_receipt.receipt_id(), bound.receipt_id());
    assert!(
        service
            .retain_supply_posture(
                &mut session,
                observed(&supply, Some(&other_receipt), None, 3000),
                3000,
            )
            .is_err()
    );
    service
        .retain_supply_posture(
            &mut session,
            observed(&supply, Some(&bound), None, 4000),
            4000,
        )
        .unwrap();
    for at in [1999, 3000, 4000, 5001] {
        assert!(
            service
                .retain_supply_posture(&mut session, observed(&supply, None, None, at), 5000,)
                .is_err(),
            "{at}"
        );
    }
    let status = service
        .session_status(
            &mut session,
            &LifecycleCaller::Operator {
                uid: CONTROLLER_UID,
            },
            5000,
            verify_fixture_signature,
        )
        .unwrap();
    peer.join().unwrap();
    assert_eq!(status.posture.dimensions[0].state, DimensionState::Verified);
    assert_eq!(
        status.posture.dimensions[0].freshness.last_verified_at_ms,
        Some(4000)
    );
}

#[test]
fn producer_failures_are_independent_and_keep_last_success() {
    let supply = Supply::new();
    let (_root, service, mut session, bound, peer) = launch_supply(&supply, 4);
    let proof =
        louiselm_skills::discovery::DiscoveryProof::verify(&bound, &supply.discovery.runtime)
            .unwrap();
    let operator = LifecycleCaller::Operator {
        uid: CONTROLLER_UID,
    };
    service
        .retain_supply_posture(
            &mut session,
            observed(&supply, Some(&bound), Some(&proof), 3000),
            3000,
        )
        .unwrap();
    for (at, bound_input, proof_input, failed) in [
        (4000, Some(&bound), None, DimensionName::NativeSupply),
        (5000, None, Some(&proof), DimensionName::ProviderDisclosure),
    ] {
        service
            .retain_supply_posture(
                &mut session,
                observed(&supply, bound_input, proof_input, at),
                at,
            )
            .unwrap();
        let status = service
            .session_status(&mut session, &operator, at, verify_fixture_signature)
            .unwrap();
        let failed = status
            .posture
            .dimensions
            .iter()
            .find(|d| d.dimension == failed)
            .unwrap();
        assert_eq!(failed.state, DimensionState::Failed);
        assert_eq!(
            failed.freshness.last_verified_at_ms,
            Some(if at == 4000 { 3000 } else { 4000 })
        );
        assert_eq!(
            failed.freshness.basis,
            louiselm_skills::launch_protocol::FreshnessBasis::Invalidated
        );
        assert!(!failed.evidence.is_empty());
        assert_eq!(
            status.posture.dimensions[2].state,
            DimensionState::Verified,
            "runtime keeps its own evidence"
        );
        if at == 5000 {
            assert_eq!(status.posture.dimensions[1].state, DimensionState::Verified);
        } else {
            assert_eq!(status.posture.dimensions[0].state, DimensionState::Verified);
            assert_eq!(status.posture.dimensions[5].state, DimensionState::Verified);
        }
    }
    let empty = support::Fixture::new();
    let missing_store = SupplyEvidence::derive(
        bound.request(),
        &empty.store(),
        &Policy::embedded(),
        &supply.registry,
        Some(&bound),
        Some(&proof),
        6000,
    )
    .unwrap();
    service
        .retain_supply_posture(&mut session, missing_store, 6000)
        .unwrap();
    let status = service
        .session_status(&mut session, &operator, 6000, verify_fixture_signature)
        .unwrap();
    assert_eq!(status.posture.dimensions[0].state, DimensionState::Failed);
    assert_eq!(status.posture.dimensions[1].state, DimensionState::Verified);
    assert_eq!(status.posture.dimensions[5].state, DimensionState::Verified);
    let status = service
        .session_status(&mut session, &operator, 5999, verify_fixture_signature)
        .unwrap();
    assert_eq!(
        status.posture.dimensions[1].failure_code,
        Some(FailureCode::EvidenceInvalidated)
    );
    peer.join().unwrap();
}

#[test]
fn incomplete_authenticated_discovery_cannot_verify_native_supply() {
    let mut supply = Supply::new();
    supply
        .discovery
        .isolation
        .native_sources
        .as_mut()
        .unwrap()
        .sources
        .clear();
    supply.discovery.sign();
    let (_root, service, mut session, bound, peer) = launch_supply(&supply, 1);
    assert!(
        louiselm_skills::discovery::DiscoveryProof::verify(&bound, &supply.discovery.runtime)
            .is_err()
    );
    service
        .retain_supply_posture(
            &mut session,
            observed(&supply, Some(&bound), None, 3000),
            3000,
        )
        .unwrap();
    let status = service
        .session_status(
            &mut session,
            &LifecycleCaller::Operator {
                uid: CONTROLLER_UID,
            },
            4000,
            verify_fixture_signature,
        )
        .unwrap();
    peer.join().unwrap();
    assert_eq!(status.posture.dimensions[0].state, DimensionState::Verified);
    assert_eq!(
        status.posture.dimensions[1].failure_code,
        Some(FailureCode::NativeSupplyUncertain)
    );
    assert_eq!(
        status.posture.dimensions[1].freshness.last_verified_at_ms,
        None
    );
    assert_eq!(status.posture.dimensions[5].state, DimensionState::Verified);
}
