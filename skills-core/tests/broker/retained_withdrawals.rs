//! Retained launch evidence feeds the same durable supply condition as current supply.

use super::*;
use louiselm_skills::session_manifest::SkillInputs;

fn drain(outbox: &Outbox) -> Vec<ProjectionChange> {
    let mut changes = Vec::new();
    while let Some(item) = outbox.next().unwrap() {
        outbox.acknowledge(item.sequence, &item.digest()).unwrap();
        changes.push(item.change);
    }
    changes
}

fn retained(
    supply: &Supply,
    broker: &BrokerService,
    id: &str,
    agent: &str,
    skills: SkillInputs,
) -> LaunchAuthorization {
    let mut request = request(id);
    request.agent_id = agent.into();
    request.skills = skills.clone();
    let mut approval = grant(&request);
    approval.controller_uid = supply.source.operator_uid;
    broker.authorizations().authorize(&approval, 1000).unwrap();
    let auth = broker
        .authorizations()
        .consume(&request, approval.controller_uid, 2000)
        .unwrap();
    let mut launch = launch_receipt(&auth).payload;
    let ReceiptOutcome::Launch { evidence, .. } = &mut launch.outcome else {
        unreachable!()
    };
    evidence.skills = skills;
    let launch = signed(launch);
    let start = start_receipt(&auth, &launch);
    for receipt in [launch, start] {
        broker
            .receipts()
            .append(
                &auth,
                &receipt.canonical_bytes(),
                None,
                verify_fixture_signature,
            )
            .unwrap();
    }
    auth
}

fn transition(broker: &BrokerService, auth: &LaunchAuthorization, state: SessionState) {
    let head = broker.receipts().head(&auth.session_id).unwrap().unwrap();
    let authority = ReceiptAuthority::Authorized(Authorization {
        authorization_id: format!("lifecycle-{}", head.sequence),
        request_id: format!("lifecycle-{}", head.sequence),
        request_digest: Digest::of(b"fixture lifecycle").to_string(),
    });
    let outcome = match state {
        SessionState::Parked => ReceiptOutcome::Park { authority },
        SessionState::Terminal => ReceiptOutcome::Disposal { authority },
        _ => panic!("unsupported fixture transition"),
    };
    let receipt = signed(payload(
        auth,
        &format!("lifecycle-{}", head.sequence),
        head.sequence + 1,
        Some(head.digest),
        outcome,
        state,
    ));
    broker
        .receipts()
        .append(
            auth,
            &receipt.canonical_bytes(),
            None,
            verify_fixture_signature,
        )
        .unwrap();
}

fn pin(supply: &Supply) -> SkillInputs {
    SkillInputs::Enabled {
        generation_digest: admission::current(&supply.store)
            .unwrap()
            .unwrap()
            .generation,
        view_digest: Digest::of(b"fixture-view").to_string(),
    }
}

#[test]
fn parked_pins_preserve_identity_and_strongest_urgency_until_last_disposal() {
    assert_retained(&mut drain);
}

#[test]
#[ignore = "requires scripts/test-skill-requests capture-service and Neovim"]
fn retained_withdrawals_reach_neovim() {
    let mut observer = crate::attention_delivery::Observer::new();
    assert_retained(&mut |outbox| observer.drain(outbox));
}

fn assert_retained(observe: &mut dyn FnMut(&Outbox) -> Vec<ProjectionChange>) {
    let supply = Supply::new();
    let broker = supply.broker("broker.sock");
    let first = retained(&supply, &broker, "first", "codex", pin(&supply));
    let second = retained(&supply, &broker, "second", "codex", pin(&supply));
    transition(&broker, &first, SessionState::Parked);
    transition(&broker, &second, SessionState::Parked);
    let bytes = supply.sign(
        &Batch::Withdrawals(vec![supply.members[0].clone()]),
        NOW + 10,
    );
    import::import_bytes(&supply.store, &bytes).unwrap();
    supply.protect();
    let outbox = Outbox::open(&supply.root.path().join("authorizations/attention-outbox")).unwrap();
    broker
        .reconcile_supply_withdrawals(&supply.source, NOW + 20, verify_fixture_signature)
        .unwrap();
    let original = upserts(observe(&outbox)).remove(0);
    assert_eq!(withdrawal(&original)["urgency"], "relied_on");

    // New Admission no longer relies on withdrawn evidence; parked views still do.
    supply.activate(&supply.members, NOW + 21);
    supply.protect();
    broker
        .reconcile_supply_withdrawals(&supply.source, NOW + 24, verify_fixture_signature)
        .unwrap();
    assert_eq!(observe(&outbox), [], "retained reliance prevents downgrade");
    drop(broker);
    let broker = supply.broker("restarted.sock");
    transition(&broker, &first, SessionState::Terminal);
    broker
        .reconcile_supply_withdrawals(&supply.source, NOW + 25, verify_fixture_signature)
        .unwrap();
    assert_eq!(observe(&outbox), [], "the second parked pin still relies");
    transition(&broker, &second, SessionState::Terminal);
    broker
        .reconcile_supply_withdrawals(&supply.source, NOW + 26, verify_fixture_signature)
        .unwrap();
    let weaker = upserts(observe(&outbox));
    assert_eq!(weaker.len(), 1);
    assert_eq!(weaker[0].operation_id, original.operation_id);
    assert_eq!(weaker[0].created_at_ms, original.created_at_ms);
    assert_eq!(withdrawal(&weaker[0])["urgency"], "subject_only");

    let third = retained(&supply, &broker, "third", "codex", pin(&supply));
    supply.activate(&[supply.members[1].clone()], NOW + 27);
    supply.protect();
    broker
        .reconcile_supply_withdrawals(&supply.source, NOW + 30, verify_fixture_signature)
        .unwrap();
    assert_eq!(
        observe(&outbox),
        [],
        "leaving current supply is not disposal"
    );
    transition(&broker, &third, SessionState::Terminal);
    broker
        .reconcile_supply_withdrawals(&supply.source, NOW + 31, verify_fixture_signature)
        .unwrap();
    assert_eq!(
        observe(&outbox),
        [ProjectionChange::Clear(weaker[0].clone())]
    );
    broker
        .reconcile_supply_withdrawals(&supply.source, NOW + 32, verify_fixture_signature)
        .unwrap();
    assert_eq!(observe(&outbox), []);
}

#[test]
fn unrelated_agent_and_skills_off_pins_do_not_retain_other_agents_members() {
    let mut supply = Supply::new();
    let broker = supply.broker("broker.sock");
    retained(&supply, &broker, "other-agent", "claude", pin(&supply));
    retained(
        &supply,
        &broker,
        "skills-off",
        "codex",
        SkillInputs::Off {
            view_digest: louiselm_skills::instruction_view::empty_digest().to_string(),
        },
    );
    let operator = supply.source.operator_uid;
    supply.source.operator_uid += 10;
    retained(&supply, &broker, "foreign-operator", "codex", pin(&supply));
    supply.source.operator_uid = operator;
    let bytes = supply.sign(
        &Batch::Withdrawals(vec![supply.members[0].clone()]),
        NOW + 10,
    );
    import::import_bytes(&supply.store, &bytes).unwrap();
    supply.protect();
    let outbox = Outbox::open(&supply.root.path().join("authorizations/attention-outbox")).unwrap();
    broker
        .reconcile_supply_withdrawals(&supply.source, NOW + 20, verify_fixture_signature)
        .unwrap();
    let original = upserts(drain(&outbox)).remove(0);
    supply.activate(&[supply.members[1].clone()], NOW + 21);
    supply.protect();
    broker
        .reconcile_supply_withdrawals(&supply.source, NOW + 24, verify_fixture_signature)
        .unwrap();
    assert_eq!(drain(&outbox), [ProjectionChange::Clear(original)]);
}

#[test]
fn reconciliation_serializes_launch_consumption_through_the_supply_observation() {
    let supply = Supply::new();
    let broker = supply.broker("broker.sock");
    retained(&supply, &broker, "retained", "codex", pin(&supply));
    let mut request = request("arriving");
    request.agent_id = "codex".into();
    request.skills = pin(&supply);
    let mut approval = grant(&request);
    approval.controller_uid = supply.source.operator_uid;
    broker.authorizations().authorize(&approval, 1000).unwrap();
    thread::scope(|scope| {
        let (start, started) = mpsc::channel();
        let (begin, beginning) = mpsc::channel();
        let (done, completed) = mpsc::channel();
        let broker_ref = &broker;
        let worker = scope.spawn(move || {
            beginning.recv().unwrap();
            start.send(()).unwrap();
            let result =
                broker_ref
                    .authorizations()
                    .consume(&request, approval.controller_uid, 2000);
            done.send(result).unwrap();
        });
        let mut first = true;
        broker
            .reconcile_supply_withdrawals(&supply.source, NOW + 20, |key, payload, signature| {
                if first {
                    first = false;
                    begin.send(()).unwrap();
                    started.recv().unwrap();
                    assert!(
                        matches!(
                            completed.recv_timeout(Duration::from_millis(100)),
                            Err(mpsc::RecvTimeoutError::Timeout)
                        ),
                        "new consumption must wait for the complete supply observation"
                    );
                }
                verify_fixture_signature(key, payload, signature)
            })
            .unwrap();
        assert!(!first, "verified existing receipt evidence");
        assert_eq!(
            completed
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .unwrap()
                .session_id,
            "arriving"
        );
        worker.join().unwrap();
    });
}

#[test]
fn verified_consumed_recovery_removes_retained_use_but_a_forged_proof_does_not() {
    use louiselm_skills::launch_recovery::{Payload, Proof, SCHEMA, SIGNED_SCHEMA};
    let supply = Supply::new();
    let broker = supply.broker("broker.sock");
    let auth = retained(&supply, &broker, "recovered", "codex", pin(&supply));
    transition(&broker, &auth, SessionState::Parked);
    let bytes = supply.sign(
        &Batch::Withdrawals(vec![supply.members[0].clone()]),
        NOW + 10,
    );
    import::import_bytes(&supply.store, &bytes).unwrap();
    supply.activate(&[supply.members[1].clone()], NOW + 11);
    supply.protect();
    let outbox = Outbox::open(&supply.root.path().join("authorizations/attention-outbox")).unwrap();
    broker
        .reconcile_supply_withdrawals(&supply.source, NOW + 20, verify_fixture_signature)
        .unwrap();
    let original = upserts(drain(&outbox)).remove(0);
    let snapshot = broker
        .consumed_recovery_snapshot(&auth.authorization_id, verify_fixture_signature)
        .unwrap();
    let payload = Payload {
        schema: SCHEMA.into(),
        binding: snapshot.binding,
        arm_digest: Digest::of(b"fixture root recovery arm").to_string(),
        receipt_binding_digest: None,
        armed_boot_id: "00000000-0000-0000-0000-000000000001".into(),
        recovered_boot_id: "00000000-0000-0000-0000-000000000002".into(),
        retained_storage: louiselm_skills::launch_recovery::RetainedStorage::Sealed,
        release_id: trusted_release().release_id,
        signing_key_id: trusted_release().signing_key_id,
    };
    let proof = Proof {
        schema: SIGNED_SCHEMA.into(),
        signature: Digest::of(&payload.canonical_bytes()).to_string(),
        payload,
    };
    broker
        .accept_consumed_recovery(&proof, verify_fixture_signature)
        .unwrap();
    let path = supply
        .root
        .path()
        .join("receipts/consumed-recoveries/authorization-recovered.json");
    let mut forged = proof.clone();
    forged.signature = Digest::of(b"forged recovery").to_string();
    fs::write(&path, forged.canonical_bytes()).unwrap();
    let error = broker
        .reconcile_supply_withdrawals(&supply.source, NOW + 21, verify_fixture_signature)
        .unwrap_err();
    assert!(
        matches!(
            error,
            BrokerError::Recovery(louiselm_skills::launch_recovery::ProofError::InvalidSignature)
        ),
        "{error:?}"
    );
    assert!(outbox.next().unwrap().is_none());
    fs::write(path, proof.canonical_bytes()).unwrap();
    broker
        .reconcile_supply_withdrawals(&supply.source, NOW + 22, verify_fixture_signature)
        .unwrap();
    assert_eq!(drain(&outbox), [ProjectionChange::Clear(original)]);
}

#[test]
fn unobserved_pin_with_missing_authorization_cannot_clear_current_attention() {
    let supply = Supply::new();
    let broker = supply.broker("broker.sock");
    let bytes = supply.sign(
        &Batch::Withdrawals(vec![supply.members[0].clone()]),
        NOW + 10,
    );
    import::import_bytes(&supply.store, &bytes).unwrap();
    supply.protect();
    let outbox = Outbox::open(&supply.root.path().join("authorizations/attention-outbox")).unwrap();
    broker
        .reconcile_supply_withdrawals(&supply.source, NOW + 20, verify_fixture_signature)
        .unwrap();
    assert_eq!(upserts(drain(&outbox)).len(), 1);
    let auth = retained(&supply, &broker, "unobserved", "codex", pin(&supply));
    transition(&broker, &auth, SessionState::Parked);
    fs::remove_file(
        supply
            .root
            .path()
            .join("authorizations/consumed/authorization-unobserved.json"),
    )
    .unwrap();
    supply.activate(&[supply.members[1].clone()], NOW + 21);
    supply.protect();
    let error = broker
        .reconcile_supply_withdrawals(&supply.source, NOW + 24, verify_fixture_signature)
        .unwrap_err();
    assert!(
        matches!(&error, BrokerError::Storage(error) if error.to_string() == "stored Session launch authorization is missing"),
        "{error:?}"
    );
    assert!(outbox.next().unwrap().is_none());
}

#[test]
fn missing_or_unverifiable_retained_evidence_preserves_attention_after_restart() {
    let supply = Supply::new();
    let broker = supply.broker("broker.sock");
    let auth = retained(&supply, &broker, "retained", "codex", pin(&supply));
    transition(&broker, &auth, SessionState::Parked);
    let bytes = supply.sign(
        &Batch::Withdrawals(vec![supply.members[0].clone()]),
        NOW + 10,
    );
    import::import_bytes(&supply.store, &bytes).unwrap();
    supply.activate(&[supply.members[1].clone()], NOW + 11);
    supply.protect();
    let outbox = Outbox::open(&supply.root.path().join("authorizations/attention-outbox")).unwrap();
    broker
        .reconcile_supply_withdrawals(&supply.source, NOW + 20, verify_fixture_signature)
        .unwrap();
    assert_eq!(
        upserts(drain(&outbox)).len(),
        1,
        "offline catch-up sees superseded parked pins"
    );
    drop(broker);
    let broker = supply.broker("restarted.sock");
    let receipt = supply
        .root
        .path()
        .join("receipts/sessions/retained/00000000000000000000.receipt.json");
    let consumed = supply
        .root
        .path()
        .join("authorizations/consumed/authorization-retained.json");
    for path in [&receipt, &consumed] {
        let original = fs::read(path).unwrap();
        for replacement in [Some(b"invalid".as_slice()), None] {
            if let Some(bytes) = replacement {
                fs::write(path, bytes).unwrap();
            } else {
                fs::remove_file(path).unwrap();
            }
            let error = broker
                .reconcile_supply_withdrawals(&supply.source, NOW + 21, verify_fixture_signature)
                .unwrap_err();
            assert!(matches!(error, BrokerError::Storage(_)), "{error:?}");
            assert!(outbox.next().unwrap().is_none());
            fs::write(path, &original).unwrap();
            broker
                .reconcile_supply_withdrawals(&supply.source, NOW + 22, verify_fixture_signature)
                .unwrap();
            assert!(
                outbox.next().unwrap().is_none(),
                "restoring evidence is not a reminder"
            );
        }
    }
    let original = fs::read(&receipt).unwrap();
    let mut forged = SignedReceipt::parse_canonical(&original).unwrap();
    forged.signature = Digest::of(b"forged").to_string();
    fs::write(&receipt, forged.canonical_bytes()).unwrap();
    let error = broker
        .reconcile_supply_withdrawals(&supply.source, NOW + 23, verify_fixture_signature)
        .unwrap_err();
    assert!(
        matches!(&error, BrokerError::Storage(error) if error.to_string() == "stored chain no longer verifies"),
        "{error:?}"
    );
    assert!(outbox.next().unwrap().is_none());
    let lifecycle = louiselm_skills::broker::lifecycle::LifecycleStore::open(
        &supply.root.path().join("authorizations/lifecycle"),
    )
    .unwrap();
    assert!(
        !lifecycle.is_quarantined(&auth.session_id).unwrap(),
        "failed reconciliation cannot quarantine"
    );
    fs::write(receipt, original).unwrap();
    broker
        .reconcile_supply_withdrawals(&supply.source, NOW + 24, verify_fixture_signature)
        .unwrap();
    assert!(outbox.next().unwrap().is_none());
}

#[test]
fn session_and_package_quarantine_remove_only_their_usable_sources() {
    let supply = Supply::new();
    let broker = supply.broker("broker.sock");
    let first = retained(&supply, &broker, "first", "codex", pin(&supply));
    let second = retained(&supply, &broker, "second", "codex", pin(&supply));
    let bytes = supply.sign(
        &Batch::Withdrawals(vec![supply.members[0].clone()]),
        NOW + 10,
    );
    import::import_bytes(&supply.store, &bytes).unwrap();
    supply.activate(&[supply.members[1].clone()], NOW + 11);
    supply.protect();
    let outbox = Outbox::open(&supply.root.path().join("authorizations/attention-outbox")).unwrap();
    broker
        .reconcile_supply_withdrawals(&supply.source, NOW + 20, verify_fixture_signature)
        .unwrap();
    let original = upserts(drain(&outbox)).remove(0);
    let lifecycle = louiselm_skills::broker::lifecycle::LifecycleStore::open(
        &supply.root.path().join("authorizations/lifecycle"),
    )
    .unwrap();
    lifecycle.quarantine(&first.session_id).unwrap();
    broker
        .reconcile_supply_withdrawals(&supply.source, NOW + 21, verify_fixture_signature)
        .unwrap();
    assert!(
        outbox.next().unwrap().is_none(),
        "second pin remains usable"
    );
    quarantine::exclude(
        &supply.store,
        &[supply.members[0].to_string()],
        "operator exclusion",
        NOW + 22,
    )
    .unwrap();
    supply.protect();
    broker
        .reconcile_supply_withdrawals(&supply.source, NOW + 23, verify_fixture_signature)
        .unwrap();
    assert_eq!(drain(&outbox), [ProjectionChange::Clear(original)]);
    assert!(!lifecycle.is_quarantined(&second.session_id).unwrap());
    assert_eq!(
        broker.receipts().state(&second.session_id).unwrap(),
        Some(SessionState::Running)
    );
}

#[test]
fn retained_member_keeps_withdrawn_base_reliance_without_admitting_that_base() {
    let supply = Supply::new();
    let candidate = supply.keys.candidate("checkout/skills/derived");
    let capture = |body: &str, at| {
        support::write_file(
            &candidate.join("SKILL.md"),
            &format!("---\nname: derived\ndescription: Derived skill.\n---\n{body}\n"),
        );
        supply
            .store
            .capture(&candidate, &Policy::embedded(), at)
            .unwrap()
            .0
            .digest
    };
    let base = capture("Base", NOW + 3);
    let endorsement = supply.sign(
        &Batch::Endorsements(vec![EndorsedPackage {
            package_digest: base.clone(),
            policy_digest: Policy::embedded().digest().clone(),
            dossier_digest: None,
            assessment: None,
            review_depth: ReviewDepth::Read,
            assumptions: vec![],
        }]),
        NOW + 4,
    );
    import::import_bytes(&supply.store, &endorsement).unwrap();
    let derived = capture("Changed", NOW + 5);
    supply.activate(std::slice::from_ref(&derived), NOW + 6);
    let generation = admission::current(&supply.store).unwrap().unwrap();
    assert_eq!(
        generation.payload.members[0].relied_on,
        [Digest::of(&endorsement).to_string()]
    );
    let broker = supply.broker("broker.sock");
    retained(&supply, &broker, "derived", "codex", pin(&supply));
    let bytes = supply.sign(&Batch::Withdrawals(vec![base]), NOW + 10);
    import::import_bytes(&supply.store, &bytes).unwrap();
    supply.activate(&[supply.members[1].clone()], NOW + 11);
    supply.protect();
    let outbox = Outbox::open(&supply.root.path().join("authorizations/attention-outbox")).unwrap();
    broker
        .reconcile_supply_withdrawals(&supply.source, NOW + 20, verify_fixture_signature)
        .unwrap();
    let observed = upserts(drain(&outbox));
    assert_eq!(observed.len(), 1);
    assert_eq!(
        withdrawal(&observed[0])["package_digest"],
        derived.to_string()
    );
    assert_eq!(withdrawal(&observed[0])["urgency"], "relied_on");
    assert_eq!(
        withdrawal(&observed[0])["withdrawal_digest"],
        Digest::of(&bytes).to_string()
    );

    let generation_path = admission::record_path(&supply.store, &generation.digest());
    let original = fs::read(&generation_path).unwrap();
    let mut changed: louiselm_skills::generation::GenerationRecord =
        serde_json::from_slice(&original).unwrap();
    changed.payload.members[0].agents = vec!["claude".into()];
    fs::write(&generation_path, serde_json::to_vec(&changed).unwrap()).unwrap();
    let error = broker
        .reconcile_supply_withdrawals(&supply.source, NOW + 21, verify_fixture_signature)
        .unwrap_err();
    assert!(
        matches!(
            error,
            BrokerError::AdmissionEvidence(admission::AdmissionError::DigestMismatch { .. })
        ),
        "{error:?}"
    );
    assert!(outbox.next().unwrap().is_none());
    fs::write(generation_path, original).unwrap();
    let endorsement_path = supply.store.root().join("endorsements").join(format!(
        "{}.json",
        Digest::of(&endorsement).directory_name()
    ));
    fs::remove_file(&endorsement_path).unwrap();
    let error = broker
        .reconcile_supply_withdrawals(&supply.source, NOW + 22, verify_fixture_signature)
        .unwrap_err();
    assert!(
        matches!(&error, BrokerError::AdmissionEvidence(admission::AdmissionError::Chain(reason)) if reason == "relied-on Endorsement evidence is missing"),
        "{error:?}"
    );
    assert!(outbox.next().unwrap().is_none());
    fs::write(endorsement_path, endorsement).unwrap();
    supply.protect();
    broker
        .reconcile_supply_withdrawals(&supply.source, NOW + 23, verify_fixture_signature)
        .unwrap();
    assert!(outbox.next().unwrap().is_none());
}
