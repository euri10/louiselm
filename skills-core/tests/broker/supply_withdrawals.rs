//! Signed current-supply withdrawals through broker reconciliation without a Session.

use super::*;
use louiselm_skills::{
    Policy, Store,
    admission::{self, AdmissionMember, AdmissionRequest},
    broker::{
        admission_source::AdmissionSource,
        attention::{AttentionCondition, AttentionSubject, Outbox, Projection, ProjectionChange},
    },
    dossier::ReviewDepth,
    endorsement::{
        self, Batch, EndorsedPackage, EndorserRole, Signing, import, local,
        trust::{EndorserEntry, MAX_LIFETIME_MS},
    },
    quarantine,
    signer::SshKeygenSigner,
    sshsig::SkPolicy,
    trust::TrustStore,
    witness::GitWitness,
};

const NOW: u64 = 1_800_000_000_000;

#[path = "retained_withdrawals.rs"]
mod retained;

struct Supply {
    root: TempDir,
    keys: support::Fixture,
    primary: support::SshKey,
    publisher: support::SshKey,
    store: Store,
    source: AdmissionSource,
    members: [Digest; 2],
}

impl Supply {
    fn new() -> Self {
        let root = tempfile::tempdir_in(std::env::var_os("HOME").unwrap()).unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let keys = support::Fixture::new();
        let primary = support::SshKey::generate(&keys, "primary");
        let release = support::SshKey::generate(&keys, "release");
        let publisher = support::SshKey::generate(&keys, "publisher");
        let store = Store::open(&root.path().join("supply")).unwrap();
        TrustStore::bootstrap(
            &store,
            "louiselm/skills",
            &primary.public_key(),
            &release.public_key(),
            SkPolicy::none(),
            NOW,
        )
        .unwrap();
        local::add(
            &store,
            &EndorserEntry {
                label: "Fixture publisher".into(),
                public_key: publisher.public_key(),
                roles: vec![EndorserRole::Publisher],
                sk_policy: SkPolicy::none(),
                max_lifetime_ms: MAX_LIFETIME_MS,
            },
            &SshKeygenSigner::new(primary.private_key_path()),
            NOW,
        )
        .unwrap();
        let members = ["relied", "subject"].map(|name| {
            let candidate = keys.candidate(&format!("checkout/skills/{name}"));
            support::write_file(&candidate.join("SKILL.md"),
                &format!("---\nname: {name}\ndescription: Fixture skill.\n---\nPrivate candidate prose.\n"));
            store.capture(&candidate, &Policy::embedded(), NOW).unwrap().0.digest
        });
        let uid = rustix::process::geteuid().as_raw();
        let supply = Self {
            source: AdmissionSource {
                store: store.root().into(),
                trust_domain: "louiselm/skills".into(),
                operator_uid: uid,
                broker_uid: uid + 1,
            },
            root,
            keys,
            primary,
            publisher,
            store,
            members,
        };
        let endorsement = supply.sign(
            &Batch::Endorsements(vec![EndorsedPackage {
                package_digest: supply.members[0].clone(),
                policy_digest: Policy::embedded().digest().clone(),
                dossier_digest: None,
                assessment: None,
                review_depth: ReviewDepth::Read,
                assumptions: vec![],
            }]),
            NOW,
        );
        import::import_bytes(&supply.store, &endorsement).unwrap();
        supply.activate(&supply.members, NOW + 1);
        // Test-only trusted provenance; Admission, witness and signatures are real.
        fs::write(supply.store.root().join("provenance.json"),
            br#"{"schema":"louiselm.skills.store-provenance/1","trusted":true,"created_by_release":"fixture"}"#).unwrap();
        supply.protect();
        supply
    }

    fn sign(&self, batch: &Batch, at: u64) -> Vec<u8> {
        endorsement::sign_batch(
            &Signing {
                signer_role: EndorserRole::Publisher,
                signer_key: self.publisher.public_key(),
                issued_at_ms: at,
                expires_at_ms: at + 60_000,
            },
            batch,
            &SshKeygenSigner::new(self.publisher.private_key_path()),
        )
        .unwrap()
        .remove(0)
    }

    fn activate(&self, packages: &[Digest], at: u64) {
        let record = admission::admit(
            &self.store,
            &Policy::embedded(),
            &AdmissionRequest {
                members: packages
                    .iter()
                    .map(|package| AdmissionMember {
                        package: package.clone(),
                        depth: ReviewDepth::Read,
                        agents: vec!["codex".into()],
                    })
                    .collect(),
                signer: &SshKeygenSigner::new(self.primary.private_key_path()),
                admitted_at_ms: at,
                configured_agents: None,
            },
        )
        .unwrap();
        let witness = GitWitness::new(
            &self.keys.witness_remote(),
            "skill-generations",
            &self.keys.path("witness-work"),
        );
        admission::witness(&self.store, &record.digest(), &witness, at + 1).unwrap();
        // This test uses a development executable and synthetic release provenance.
        // Restore that fixture provenance before any protected broker read.
        let path = self.store.root().join("provenance.json");
        let provenance = fs::read(&path).unwrap();
        fs::write(&path, br#"{"schema":"louiselm.skills.store-provenance/1","trusted":false,"created_by_release":null}"#).unwrap();
        admission::activate(&self.store, &record.digest(), at + 2).unwrap();
        fs::write(path, provenance).unwrap();
    }

    fn protect(&self) {
        skill_requests::protect_fixture_directories(self.store.root());
    }

    fn broker(&self, name: &str) -> BrokerService {
        BrokerService::bind(
            &self.root.path().join(name),
            AuthorizationStore::open(&self.root.path().join("authorizations"), pool(4)).unwrap(),
            ReceiptStore::open(&self.root.path().join("receipts"), trusted_release()).unwrap(),
            AuditLog::open(&self.root.path().join("audit")).unwrap(),
            local_pin(),
        )
        .unwrap()
    }
}

fn upserts(changes: Vec<ProjectionChange>) -> Vec<AttentionCondition> {
    changes
        .into_iter()
        .filter_map(|change| match change {
            ProjectionChange::Upsert(condition) => Some(condition),
            _ => None,
        })
        .collect()
}

fn withdrawal(condition: &AttentionCondition) -> serde_json::Value {
    Projection {
        sequence: 1,
        mutation_id: "fixture".into(),
        change: ProjectionChange::Upsert(condition.clone()),
    }
    .wire()["change"]["attention"]["withdrawal"]
        .clone()
}

#[test]
fn signed_withdrawals_reconcile_without_sessions_and_preserve_unresolved_evidence() {
    assert_withdrawals(&mut |outbox| {
        let mut changes = Vec::new();
        while let Some(item) = outbox.next().unwrap() {
            outbox.acknowledge(item.sequence, &item.digest()).unwrap();
            changes.push(item.change);
        }
        changes
    });
}

#[test]
fn unproven_current_generation_cannot_clear_a_pending_withdrawal() {
    let supply = Supply::new();
    let bytes = supply.sign(
        &Batch::Withdrawals(vec![supply.members[0].clone()]),
        NOW + 10,
    );
    import::import_bytes(&supply.store, &bytes).unwrap();
    supply.protect();
    let broker = supply.broker("broker.sock");
    let outbox = Outbox::open(&supply.root.path().join("authorizations/attention-outbox")).unwrap();
    broker
        .reconcile_supply_withdrawals(&supply.source, NOW + 20, verify_fixture_signature)
        .unwrap();
    let pending = outbox.next().unwrap().unwrap();
    assert!(matches!(pending.change, ProjectionChange::Upsert(_)));
    outbox
        .acknowledge(pending.sequence, &pending.digest())
        .unwrap();

    let mut current = admission::current(&supply.store).unwrap().unwrap();
    let path = admission::record_path(&supply.store, &current.digest());
    let original = fs::read(&path).unwrap();
    // This mutable lifecycle field is outside the signed Generation payload.
    current.state = louiselm_skills::generation::GenerationState::PendingWitness;
    fs::write(&path, serde_json::to_vec(&current).unwrap()).unwrap();
    let failure = broker
        .reconcile_supply_withdrawals(&supply.source, NOW + 21, verify_fixture_signature)
        .unwrap_err();
    assert!(
        matches!(
            failure,
            BrokerError::AdmissionEvidence(admission::AdmissionError::Chain(_))
        ),
        "{failure:?}"
    );
    assert!(
        outbox.next().unwrap().is_none(),
        "unproven current state must retain the condition"
    );
    fs::write(path, original).unwrap();
    broker
        .reconcile_supply_withdrawals(&supply.source, NOW + 22, verify_fixture_signature)
        .unwrap();
    assert!(outbox.next().unwrap().is_none());
}

#[test]
fn missing_imported_withdrawal_cannot_resolve_pending_attention() {
    let supply = Supply::new();
    let bytes = supply.sign(
        &Batch::Withdrawals(vec![supply.members[0].clone()]),
        NOW + 10,
    );
    let imported = import::import_bytes(&supply.store, &bytes).unwrap();
    supply.protect();
    let broker = supply.broker("broker.sock");
    let outbox = Outbox::open(&supply.root.path().join("authorizations/attention-outbox")).unwrap();
    broker
        .reconcile_supply_withdrawals(&supply.source, NOW + 20, verify_fixture_signature)
        .unwrap();
    let pending = outbox.next().unwrap().unwrap();
    assert!(matches!(pending.change, ProjectionChange::Upsert(_)));
    outbox
        .acknowledge(pending.sequence, &pending.digest())
        .unwrap();
    drop(broker);
    let broker = supply.broker("restarted.sock");

    let path = supply
        .store
        .root()
        .join("endorsements")
        .join(format!("{}.json", imported.digest.directory_name()));
    fs::remove_file(&path).unwrap();
    let failure = broker
        .reconcile_supply_withdrawals(&supply.source, NOW + 21, verify_fixture_signature)
        .unwrap_err();
    assert!(
        matches!(
            failure,
            BrokerError::AdmissionEvidence(admission::AdmissionError::Chain(_))
        ),
        "{failure:?}"
    );
    assert!(
        outbox.next().unwrap().is_none(),
        "missing imported withdrawal cannot prove resolution"
    );
    fs::write(path, bytes).unwrap();
    supply.protect();
    broker
        .reconcile_supply_withdrawals(&supply.source, NOW + 22, verify_fixture_signature)
        .unwrap();
    assert!(outbox.next().unwrap().is_none());
}

#[test]
fn missing_local_endorser_layer_cannot_resolve_subject_only_attention() {
    let supply = Supply::new();
    let bytes = supply.sign(
        &Batch::Withdrawals(vec![supply.members[1].clone()]),
        NOW + 10,
    );
    import::import_bytes(&supply.store, &bytes).unwrap();
    supply.protect();
    let broker = supply.broker("broker.sock");
    let outbox = Outbox::open(&supply.root.path().join("authorizations/attention-outbox")).unwrap();
    broker
        .reconcile_supply_withdrawals(&supply.source, NOW + 20, verify_fixture_signature)
        .unwrap();
    let pending = outbox.next().unwrap().unwrap();
    let ProjectionChange::Upsert(condition) = &pending.change else {
        panic!("pending withdrawal projection");
    };
    assert_eq!(withdrawal(condition)["urgency"], "subject_only");
    outbox
        .acknowledge(pending.sequence, &pending.digest())
        .unwrap();
    drop(broker);
    let broker = supply.broker("restarted.sock");

    let path = supply.store.root().join("endorsers/local.json");
    let original = fs::read(&path).unwrap();
    fs::remove_file(&path).unwrap();
    let failure = broker
        .reconcile_supply_withdrawals(&supply.source, NOW + 21, verify_fixture_signature)
        .unwrap_err();
    assert!(
        matches!(
            failure,
            BrokerError::AdmissionEvidence(admission::AdmissionError::Chain(_))
        ),
        "{failure:?}"
    );
    assert!(
        outbox.next().unwrap().is_none(),
        "missing endorser layer cannot prove withdrawal of trust"
    );
    fs::write(path, original).unwrap();
    supply.protect();
    broker
        .reconcile_supply_withdrawals(&supply.source, NOW + 22, verify_fixture_signature)
        .unwrap();
    assert!(outbox.next().unwrap().is_none());
}

#[test]
fn never_observed_local_endorser_layer_preserves_verified_reliance() {
    let supply = Supply::new();
    let bytes = supply.sign(
        &Batch::Withdrawals(vec![supply.members[0].clone()]),
        NOW + 10,
    );
    import::import_bytes(&supply.store, &bytes).unwrap();
    fs::remove_file(supply.store.root().join("endorsers/local.json")).unwrap();
    supply.protect();
    let broker = supply.broker("broker.sock");
    let outbox = Outbox::open(&supply.root.path().join("authorizations/attention-outbox")).unwrap();
    broker
        .reconcile_supply_withdrawals(&supply.source, NOW + 20, verify_fixture_signature)
        .unwrap();
    let pending = outbox.next().unwrap().unwrap();
    let ProjectionChange::Upsert(condition) = &pending.change else {
        panic!("withdrawal of relied-on evidence remains relevant without local trust");
    };
    let metadata = withdrawal(condition);
    assert_eq!(metadata["urgency"], "relied_on");
    assert_eq!(
        metadata["endorser"],
        Digest::of(supply.publisher.public_key().as_bytes()).to_string()
    );
    outbox
        .acknowledge(pending.sequence, &pending.digest())
        .unwrap();
    drop(broker);
    let broker = supply.broker("restarted.sock");
    broker
        .reconcile_supply_withdrawals(&supply.source, NOW + 21, verify_fixture_signature)
        .unwrap();
    assert!(outbox.next().unwrap().is_none());
}

#[test]
fn unsettled_or_incomplete_evidence_cannot_resolve_withdrawals() {
    let supply = Supply::new();
    let bytes = supply.sign(
        &Batch::Withdrawals(vec![supply.members[0].clone()]),
        NOW + 10,
    );
    import::import_bytes(&supply.store, &bytes).unwrap();
    supply.protect();
    let broker = supply.broker("broker.sock");
    let outbox = Outbox::open(&supply.root.path().join("authorizations/attention-outbox")).unwrap();
    broker
        .reconcile_supply_withdrawals(&supply.source, NOW + 20, verify_fixture_signature)
        .unwrap();
    let pending = outbox.next().unwrap().unwrap();
    outbox
        .acknowledge(pending.sequence, &pending.digest())
        .unwrap();
    let local_path = supply.store.root().join("endorsers/local.json");
    let mut forged: serde_json::Value =
        serde_json::from_slice(&fs::read(&local_path).unwrap()).unwrap();
    forged["additions"][0]["payload"]["entry"]["label"] = "Forged label".into();
    let relied = import::records(&supply.store)
        .unwrap()
        .records
        .into_iter()
        .find(|record| record.kind() == endorsement::StatementKind::Endorsement)
        .unwrap();
    for (path, replacement, reason) in [
        (
            supply.store.root().join("activation.pending.json"),
            Some(b"{}".to_vec()),
            "activation recovery required",
        ),
        (
            local_path,
            Some(serde_json::to_vec(&forged).unwrap()),
            "unverified local Endorser evidence",
        ),
        (
            supply
                .store
                .root()
                .join("endorsements")
                .join(format!("{}.json", relied.digest().directory_name())),
            None,
            "relied-on Endorsement evidence is missing",
        ),
    ] {
        let original = if path.exists() {
            Some(fs::read(&path).unwrap())
        } else {
            None
        };
        match replacement {
            Some(bytes) => fs::write(&path, bytes).unwrap(),
            None => fs::remove_file(&path).unwrap(),
        }
        supply.protect();
        let failure = broker
            .reconcile_supply_withdrawals(&supply.source, NOW + 21, verify_fixture_signature)
            .unwrap_err();
        assert!(
            matches!(&failure, BrokerError::AdmissionEvidence(admission::AdmissionError::Chain(message)) if message == reason),
            "{failure:?}"
        );
        assert!(outbox.next().unwrap().is_none(), "{reason}");
        match original {
            Some(bytes) => fs::write(&path, bytes).unwrap(),
            None => fs::remove_file(&path).unwrap(),
        }
        supply.protect();
        broker
            .reconcile_supply_withdrawals(&supply.source, NOW + 22, verify_fixture_signature)
            .unwrap();
        assert!(outbox.next().unwrap().is_none());
    }
}

#[test]
#[ignore = "requires scripts/test-skill-requests capture-service and Neovim"]
fn withdrawals_reach_neovim() {
    let mut observer = crate::attention_delivery::Observer::new();
    assert_withdrawals(&mut |outbox| observer.drain(outbox));
}

#[test]
#[ignore = "requires scripts/test-broker-admission.py private mount namespace and distinct UIDs"]
fn installed_supply_withdrawal_source() {
    let source = AdmissionSource::installed().unwrap().unwrap();
    let root = std::path::PathBuf::from(std::env::var_os("LOUISELM_ADMISSION_STATE").unwrap());
    let expected = std::env::var("LOUISELM_ADMISSION_WITHDRAWAL_EXPECT").unwrap();
    let expected_change = match expected.as_str() {
        "raise" => "upsert",
        "clear" => "clear",
        _ => panic!("unsupported installed withdrawal expectation"),
    };
    let broker = BrokerService::bind(
        &root.join(format!("withdrawal-{expected}.sock")),
        AuthorizationStore::open(&root.join("authorizations"), pool(4)).unwrap(),
        ReceiptStore::open(&root.join("receipts"), trusted_release()).unwrap(),
        AuditLog::open(&root.join("audit")).unwrap(),
        local_pin(),
    )
    .unwrap();
    broker
        .reconcile_supply_withdrawals(&source, NOW, verify_fixture_signature)
        .unwrap();
    let outbox = Outbox::open(&root.join("authorizations/attention-outbox")).unwrap();
    let mut observed = 0;
    while let Some(item) = outbox.next().unwrap() {
        let wire = item.wire();
        let change = &wire["change"];
        if change["attention"]["kind"] == "endorsement_withdrawn"
            || change["key"]["kind"] == "endorsement_withdrawn"
        {
            assert_eq!(change["type"], expected_change);
            observed += 1;
        }
        outbox.acknowledge(item.sequence, &item.digest()).unwrap();
    }
    assert_eq!(observed, 1, "one installed withdrawal transition");
}

#[expect(
    clippy::too_many_lines,
    reason = "One signed supply history checks offline catch-up, durable identity, failure retention and exact resolution through the same broker owner."
)]
fn assert_withdrawals(observe: &mut dyn FnMut(&Outbox) -> Vec<ProjectionChange>) {
    let supply = Supply::new();
    let withdrawals = supply
        .members
        .each_ref()
        .map(|member| supply.sign(&Batch::Withdrawals(vec![member.clone()]), NOW + 10));
    let first = import::import_bytes(&supply.store, &withdrawals[0]).unwrap();
    // The packaging CLI imports the other withdrawal from its sibling directory.
    let incoming = supply.keys.candidate("checkout/skills/endorsements");
    fs::write(incoming.join("withdrawal.json"), &withdrawals[1]).unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_louiselm-skills"))
        .args([
            "package",
            supply
                .keys
                .path("checkout/skills/subject")
                .to_str()
                .unwrap(),
            "--robot-json",
        ])
        .env("LOUISELM_SKILLS_STORE", supply.store.root())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["endorsements"]["imported"], 1);
    supply.protect();
    let broker = supply.broker("broker.sock");
    let outbox = Outbox::open(&supply.root.path().join("authorizations/attention-outbox")).unwrap();
    broker
        .reconcile_supply_withdrawals(&supply.source, NOW + 20, verify_fixture_signature)
        .unwrap();
    let encoded = outbox.next().unwrap().unwrap().wire().to_string();
    for private in [
        "Private candidate prose.",
        "quarantine exclude",
        supply.store.root().to_str().unwrap(),
    ] {
        assert!(!encoded.contains(private));
    }
    let initial = upserts(observe(&outbox));
    assert_eq!(initial.len(), 2);
    for (index, member) in supply.members.iter().enumerate() {
        let condition = initial
            .iter()
            .find(|condition| withdrawal(condition)["package_digest"] == member.to_string())
            .unwrap();
        let metadata = withdrawal(condition);
        assert_eq!(
            condition.subject,
            AttentionSubject::Supply(supply.source.operator_uid.to_string())
        );
        assert_eq!(
            metadata["withdrawal_digest"],
            Digest::of(&withdrawals[index]).to_string()
        );
        assert_eq!(metadata["endorser"], "Fixture publisher");
        assert_eq!(
            metadata["urgency"],
            if index == 0 {
                "relied_on"
            } else {
                "subject_only"
            }
        );
    }
    assert_eq!(
        import::import_bytes(&supply.store, &withdrawals[0])
            .unwrap()
            .digest,
        first.digest
    );
    broker
        .reconcile_supply_withdrawals(&supply.source, NOW + 21, verify_fixture_signature)
        .unwrap();
    assert_eq!(observe(&outbox), [] as [ProjectionChange; 0]);
    drop(broker);
    let broker = supply.broker("restarted.sock");
    broker
        .reconcile_supply_withdrawals(&supply.source, NOW + 22, verify_fixture_signature)
        .unwrap();
    assert_eq!(observe(&outbox), [] as [ProjectionChange; 0]);

    let record = supply
        .store
        .root()
        .join("endorsements")
        .join(format!("{}.json", first.digest.directory_name()));
    fs::write(&record, b"malformed evidence").unwrap();
    let failure = broker
        .reconcile_supply_withdrawals(&supply.source, NOW + 23, verify_fixture_signature)
        .unwrap_err();
    assert!(
        matches!(&failure, BrokerError::AdmissionEvidence(admission::AdmissionError::Chain(reason))
            if reason.contains("unreadable Endorsement evidence")),
        "{failure:?}"
    );
    assert_eq!(observe(&outbox), [] as [ProjectionChange; 0]);
    fs::write(&record, &withdrawals[0]).unwrap();
    supply.protect();

    local::remove(&supply.store, &supply.publisher.public_key()).unwrap();
    supply.protect();
    broker
        .reconcile_supply_withdrawals(&supply.source, NOW + 24, verify_fixture_signature)
        .unwrap();
    let changed = observe(&outbox);
    assert_eq!(
        changed
            .iter()
            .filter(|change| matches!(change, ProjectionChange::Clear(_)))
            .count(),
        1
    );
    let retained = upserts(changed);
    assert_eq!(retained.len(), 1);
    let relied = initial
        .iter()
        .find(|condition| withdrawal(condition)["urgency"] == "relied_on")
        .unwrap();
    assert_eq!(retained[0].operation_id, relied.operation_id);

    local::restore(
        &supply.store,
        &supply.publisher.public_key(),
        &SshKeygenSigner::new(supply.primary.private_key_path()),
        NOW + 25,
    )
    .unwrap();
    supply.protect();
    broker
        .reconcile_supply_withdrawals(&supply.source, NOW + 26, verify_fixture_signature)
        .unwrap();
    let restored = upserts(observe(&outbox));
    assert_eq!(restored.len(), 2);
    let subject = restored
        .iter()
        .find(|condition| withdrawal(condition)["urgency"] == "subject_only")
        .unwrap();
    assert!(
        initial
            .iter()
            .all(|condition| condition.operation_id != subject.operation_id)
    );

    let unrelated = supply.keys.candidate("checkout/skills/unrelated");
    support::write_file(
        &unrelated.join("SKILL.md"),
        "---\nname: unrelated\ndescription: Unrelated member.\n---\nBody.\n",
    );
    let mut next_members = supply.members.to_vec();
    next_members.push(
        supply
            .store
            .capture(&unrelated, &Policy::embedded(), NOW + 27)
            .unwrap()
            .0
            .digest,
    );
    supply.activate(&next_members, NOW + 27);
    supply.protect();
    broker
        .reconcile_supply_withdrawals(&supply.source, NOW + 30, verify_fixture_signature)
        .unwrap();
    let updated = observe(&outbox);
    assert_eq!(
        updated.len(),
        1,
        "new review no longer relies on withdrawn evidence"
    );
    let updated = upserts(updated);
    assert_eq!(updated.len(), 1);
    assert_eq!(updated[0].operation_id, relied.operation_id);
    assert_eq!(withdrawal(&updated[0])["urgency"], "subject_only");

    quarantine::exclude(
        &supply.store,
        &supply
            .members
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        "operator decision",
        NOW + 31,
    )
    .unwrap();
    supply.protect();
    broker
        .reconcile_supply_withdrawals(&supply.source, NOW + 32, verify_fixture_signature)
        .unwrap();
    let cleared = observe(&outbox);
    assert_eq!(cleared.len(), 2);
    assert!(
        cleared
            .iter()
            .all(|change| matches!(change, ProjectionChange::Clear(_)))
    );
    broker
        .reconcile_supply_withdrawals(&supply.source, NOW + 33, verify_fixture_signature)
        .unwrap();
    assert_eq!(observe(&outbox), [] as [ProjectionChange; 0]);
}
