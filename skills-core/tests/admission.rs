//! Behavioral coverage for admission.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Test fixtures abort on setup failure and assert failures directly."
)]

//! Skill Admission: the local ceremony, the signed chain, and what it refuses.
//!
//! Signing uses a real software key here. That covers the chain, the state
//! machine, and the witness protocol; it does not cover a physical touch,
//! which no automated test can. The manual ceremony in skills-core/README.md
//! is the part a person still has to run.

mod support;

use louiselm_skills::{
    Policy,
    admission::{self, AdmissionError, AdmissionRequest},
    dossier::ReviewDepth,
    generation::GenerationState,
    quarantine,
    signer::SshKeygenSigner,
    sshsig::SkPolicy,
    trust::{Role, TrustStore},
    witness::{GitWitness, Witness},
};
use support::{Fixture, SshKey, write_file};

struct Ceremony {
    fixture: Fixture,
    primary: SshKey,
    recovery: SshKey,
}

impl Ceremony {
    fn new() -> Self {
        let fixture = Fixture::new();
        let primary = SshKey::generate(&fixture, "primary");
        let recovery = SshKey::generate(&fixture, "recovery");
        TrustStore::bootstrap(
            &fixture.store(),
            "louiselm/skills",
            &primary.public_key(),
            &recovery.public_key(),
            SkPolicy::none(),
            1_756_800_000_000,
        )
        .expect("trust bootstraps");
        Self {
            fixture,
            primary,
            recovery,
        }
    }

    fn skill(&self, name: &str) -> louiselm_skills::Digest {
        let candidate = self.fixture.candidate(name);
        write_file(
            &candidate.join("SKILL.md"),
            &format!("---\nname: {name}\ndescription: The {name} skill.\n---\n\nBody.\n"),
        );
        let (package, _) = self.fixture.capture(&candidate).expect("capture succeeds");
        package.digest
    }

    fn admit(
        &self,
        members: &[(louiselm_skills::Digest, ReviewDepth)],
        key: &SshKey,
    ) -> Result<louiselm_skills::generation::GenerationRecord, AdmissionError> {
        admission::admit(
            &self.fixture.store(),
            &Policy::embedded(),
            &AdmissionRequest {
                members: members.to_vec(),
                view_roots: std::collections::BTreeMap::default(),
                signer: &SshKeygenSigner::new(key.private_key_path()),
                admitted_at_ms: 1_756_800_000_000,
            },
        )
    }

    fn witness(&self) -> GitWitness {
        GitWitness::new(
            &self.fixture.witness_remote(),
            "skill-generations",
            &self.fixture.path("witness-work"),
        )
    }
}

#[test]
fn admission_signs_the_set_and_leaves_it_unwitnessed() {
    let ceremony = Ceremony::new();
    let first = ceremony.skill("alpha");
    let second = ceremony.skill("beta");

    let record = ceremony
        .admit(
            &[
                (first.clone(), ReviewDepth::Read),
                (second.clone(), ReviewDepth::Skimmed),
            ],
            &ceremony.primary,
        )
        .expect("admission succeeds");

    assert_eq!(record.state, GenerationState::PendingWitness);
    assert_eq!(record.payload.sequence, 1);
    assert_eq!(record.payload.predecessor, None);
    assert_eq!(record.payload.signer_role, "primary");
    assert_eq!(record.payload.members.len(), 2);
    assert_eq!(
        record.payload.policy_digest,
        Policy::embedded().digest().to_string(),
    );
    assert!(record.witness.is_none());
    assert_eq!(
        admission::current(&ceremony.fixture.store()).expect("current is readable"),
        None,
        "nothing is current until a Generation is witnessed and activated",
    );
}

#[test]
fn the_signed_payload_binds_the_recomputed_set_root_not_a_supplied_one() {
    let ceremony = Ceremony::new();
    let package = ceremony.skill("alpha");

    let record = ceremony
        .admit(&[(package, ReviewDepth::Read)], &ceremony.primary)
        .expect("admission succeeds");

    assert_eq!(
        record.payload.member_root,
        record.payload.recomputed_member_root().to_string(),
        "the set root is recomputed from the members it claims to cover",
    );
    let verified = admission::verify_record(
        &ceremony.fixture.store(),
        &record,
        &TrustStore::load(&ceremony.fixture.store())
            .expect("trust is readable")
            .expect("trust was bootstrapped"),
    );
    assert!(verified.is_ok(), "unexpected: {verified:?}");
}

#[test]
fn an_unreviewable_package_cannot_be_admitted() {
    let ceremony = Ceremony::new();
    let candidate = ceremony.fixture.candidate("no-skill-file");
    write_file(&candidate.join("notes.md"), "no skill file here\n");
    let (package, _) = ceremony
        .fixture
        .capture(&candidate)
        .expect("capture succeeds");

    let error = ceremony
        .admit(&[(package.digest, ReviewDepth::Read)], &ceremony.primary)
        .expect_err("an unreviewable package is refused");

    assert!(
        matches!(error, AdmissionError::NotReviewable { .. }),
        "unexpected error: {error}",
    );
}

#[test]
fn the_recovery_key_cannot_sign_an_ordinary_admission() {
    let ceremony = Ceremony::new();
    let package = ceremony.skill("alpha");

    let error = ceremony
        .admit(&[(package, ReviewDepth::Read)], &ceremony.recovery)
        .expect_err("the recovery key is not an ordinary signer");

    assert!(
        matches!(error, AdmissionError::Signature(_)),
        "unexpected error: {error}",
    );
}

#[test]
fn a_generation_becomes_current_only_after_the_exact_bytes_are_witnessed() {
    let ceremony = Ceremony::new();
    let package = ceremony.skill("alpha");
    let record = ceremony
        .admit(&[(package, ReviewDepth::Read)], &ceremony.primary)
        .expect("admission succeeds");
    let store = ceremony.fixture.store();

    let error = admission::activate(&store, &record.digest(), 1_756_800_000_001)
        .expect_err("an unwitnessed Generation cannot be activated");
    assert!(
        matches!(error, AdmissionError::NotWitnessed { .. }),
        "unexpected error: {error}",
    );

    let witnessed = admission::witness(
        &store,
        &record.digest(),
        &ceremony.witness(),
        1_756_800_000_002,
    )
    .expect("witnessing succeeds");
    assert!(witnessed.witness.is_some());
    assert_eq!(witnessed.state, GenerationState::PendingWitness);

    let activated = admission::activate(&store, &record.digest(), 1_756_800_000_003)
        .expect("activation succeeds");
    assert_eq!(activated.state, GenerationState::Current);
    assert_eq!(
        admission::current(&store)
            .expect("current is readable")
            .map(|record| record.payload.sequence),
        Some(1),
    );
}

#[test]
fn a_witness_that_holds_other_bytes_is_refused() {
    let ceremony = Ceremony::new();
    let package = ceremony.skill("alpha");
    let record = ceremony
        .admit(&[(package, ReviewDepth::Read)], &ceremony.primary)
        .expect("admission succeeds");
    let store = ceremony.fixture.store();
    let witness = ceremony.witness();

    witness
        .publish(&record.digest(), b"not the signed record at all")
        .expect("the remote accepts a commit");

    let error = admission::witness(&store, &record.digest(), &witness, 1_756_800_000_002)
        .expect_err("a remote holding other bytes is refused");

    assert!(
        matches!(error, AdmissionError::WitnessMismatch { .. }),
        "unexpected error: {error}",
    );
    assert_eq!(
        admission::current(&store).expect("current is readable"),
        None,
        "a failed witness leaves the prior Generation current",
    );
}

#[test]
fn the_chain_refuses_gaps_wrong_predecessors_and_rollback() {
    let ceremony = Ceremony::new();
    let store = ceremony.fixture.store();
    let alpha = ceremony.skill("alpha");
    let beta = ceremony.skill("beta");

    let first = ceremony
        .admit(&[(alpha.clone(), ReviewDepth::Read)], &ceremony.primary)
        .expect("first admission succeeds");
    admission::witness(&store, &first.digest(), &ceremony.witness(), 1)
        .expect("witnessing succeeds");
    admission::activate(&store, &first.digest(), 2).expect("activation succeeds");

    let second = ceremony
        .admit(
            &[(alpha, ReviewDepth::Read), (beta, ReviewDepth::Reproduced)],
            &ceremony.primary,
        )
        .expect("second admission succeeds");
    assert_eq!(second.payload.sequence, 2);
    assert_eq!(
        second.payload.predecessor.as_deref(),
        Some(first.digest().to_string().as_str()),
    );
    admission::witness(&store, &second.digest(), &ceremony.witness(), 3)
        .expect("witnessing succeeds");
    admission::activate(&store, &second.digest(), 4).expect("activation succeeds");

    assert_eq!(
        admission::load(&store, &first.digest())
            .expect("the first record is readable")
            .state,
        GenerationState::Superseded,
    );

    let error = admission::activate(&store, &first.digest(), 5)
        .expect_err("restoring an older Generation is refused");
    assert!(
        matches!(error, AdmissionError::Rollback { .. }),
        "unexpected error: {error}",
    );
    assert_eq!(
        admission::current(&store)
            .expect("current is readable")
            .map(|record| record.payload.sequence),
        Some(2),
        "a refused rollback leaves the higher Generation current",
    );
}

#[test]
fn a_tampered_record_is_reported_as_invalid_rather_than_trusted() {
    let ceremony = Ceremony::new();
    let store = ceremony.fixture.store();
    let package = ceremony.skill("alpha");
    let record = ceremony
        .admit(&[(package, ReviewDepth::Read)], &ceremony.primary)
        .expect("admission succeeds");

    let path = admission::record_path(&store, &record.digest());
    let text = std::fs::read_to_string(&path).expect("the record is readable");
    std::fs::write(&path, text.replace("\"read\"", "\"reproduced\""))
        .expect("the record is writable");

    let error = admission::load_verified(&store, &record.digest())
        .expect_err("a tampered record is refused");

    assert!(
        matches!(
            error,
            AdmissionError::Signature(_) | AdmissionError::DigestMismatch { .. }
        ),
        "unexpected error: {error}",
    );
}

#[test]
fn quarantine_narrows_the_current_generation_without_the_token() {
    let ceremony = Ceremony::new();
    let store = ceremony.fixture.store();
    let alpha = ceremony.skill("alpha");
    let beta = ceremony.skill("beta");
    let record = ceremony
        .admit(
            &[
                (alpha.clone(), ReviewDepth::Read),
                (beta.clone(), ReviewDepth::Read),
            ],
            &ceremony.primary,
        )
        .expect("admission succeeds");
    admission::witness(&store, &record.digest(), &ceremony.witness(), 1)
        .expect("witnessing succeeds");
    admission::activate(&store, &record.digest(), 2).expect("activation succeeds");

    quarantine::exclude(&store, &[beta.to_string()], "suspected prompt injection", 3)
        .expect("quarantine narrows immediately");

    let status = admission::status(&store).expect("status is readable");
    assert_eq!(status.state, Some(GenerationState::Quarantined));
    assert_eq!(status.effective_members, vec![alpha.to_string()]);
    assert_eq!(status.excluded_members, vec![beta.to_string()]);

    let error = quarantine::clear(&store, 4).expect_err("quarantine cannot widen authority");
    assert!(
        error.to_string().contains("Generation"),
        "the refusal names what would widen authority: {error}",
    );
}

#[test]
fn status_reports_every_state_with_a_next_action() {
    let ceremony = Ceremony::new();
    let store = ceremony.fixture.store();

    let empty = admission::status(&store).expect("status is readable");
    assert_eq!(empty.state, None);
    assert_eq!(empty.next_action.id, "admit_first_generation");

    let package = ceremony.skill("alpha");
    let record = ceremony
        .admit(&[(package, ReviewDepth::Read)], &ceremony.primary)
        .expect("admission succeeds");
    let pending = admission::status(&store).expect("status is readable");
    assert_eq!(pending.next_action.id, "witness_pending_generation");
    assert_eq!(pending.pending.len(), 1);

    admission::witness(&store, &record.digest(), &ceremony.witness(), 1)
        .expect("witnessing succeeds");
    let witnessed = admission::status(&store).expect("status is readable");
    assert_eq!(witnessed.next_action.id, "activate_witnessed_generation");

    admission::activate(&store, &record.digest(), 2).expect("activation succeeds");
    let current = admission::status(&store).expect("status is readable");
    assert_eq!(current.state, Some(GenerationState::Current));
    assert_eq!(current.next_action.id, "none");
    assert_eq!(current.signer_role.as_deref(), Some("primary"));
    assert!(current.witness.is_some());
}

#[test]
fn the_recovery_key_can_replace_the_primary_and_the_old_primary_then_fails() {
    let ceremony = Ceremony::new();
    let store = ceremony.fixture.store();
    let replacement = SshKey::generate(&ceremony.fixture, "primary-2");
    let trust = TrustStore::load(&store)
        .expect("trust is readable")
        .expect("trust was bootstrapped");

    let change = trust
        .rotation_payload(Role::Primary, &replacement.public_key(), SkPolicy::none())
        .expect("rotation payload");
    let signature = ceremony.recovery.sign(
        louiselm_skills::sshsig::TRUST_NAMESPACE,
        &change.canonical_bytes(),
    );
    TrustStore::rotate(&store, &change, &signature, 1_756_800_000_100)
        .expect("the recovery key may replace the primary");

    let package = ceremony.skill("alpha");
    let error = ceremony
        .admit(&[(package.clone(), ReviewDepth::Read)], &ceremony.primary)
        .expect_err("the replaced primary no longer signs");
    assert!(
        matches!(error, AdmissionError::Signature(_)),
        "unexpected error: {error}",
    );

    ceremony
        .admit(&[(package, ReviewDepth::Read)], &replacement)
        .expect("the new primary signs");
}

#[test]
fn a_rotation_the_recovery_key_did_not_sign_is_refused() {
    let ceremony = Ceremony::new();
    let store = ceremony.fixture.store();
    let attacker = SshKey::generate(&ceremony.fixture, "attacker");
    let trust = TrustStore::load(&store)
        .expect("trust is readable")
        .expect("trust was bootstrapped");

    let change = trust
        .rotation_payload(Role::Primary, &attacker.public_key(), SkPolicy::none())
        .expect("rotation payload");
    for signer in [&ceremony.primary, &attacker] {
        let signature = signer.sign(
            louiselm_skills::sshsig::TRUST_NAMESPACE,
            &change.canonical_bytes(),
        );
        assert!(
            TrustStore::rotate(&store, &change, &signature, 1_756_800_000_100).is_err(),
            "only the recovery key may change trust",
        );
    }
}

#[test]
fn retired_keys_verify_only_admissions_recorded_before_retirement() {
    let ceremony = Ceremony::new();
    let store = ceremony.fixture.store();
    let package = ceremony.skill("alpha");
    let historical = ceremony
        .admit(&[(package, ReviewDepth::Read)], &ceremony.primary)
        .expect("historical Admission");
    let trust = TrustStore::load(&store).expect("trust").expect("enrolled");
    let replacement = SshKey::generate(&ceremony.fixture, "replacement");
    let change = trust
        .rotation_payload(Role::Primary, &replacement.public_key(), SkPolicy::none())
        .expect("rotation payload");
    let signature = ceremony.recovery.sign(
        louiselm_skills::sshsig::TRUST_NAMESPACE,
        &change.canonical_bytes(),
    );
    let rotated = TrustStore::rotate(&store, &change, &signature, 2).expect("retire primary");
    admission::verify_record(&store, &historical, &rotated).expect("real history still verifies");

    let mut forged = historical;
    forged
        .payload
        .view_roots
        .insert("new-after-retirement".to_owned(), "new root".to_owned());
    forged.generation = forged.payload.digest().to_string();
    forged.signature = ceremony.primary.sign(
        louiselm_skills::sshsig::ADMISSION_NAMESPACE,
        &forged.payload.canonical_bytes(),
    );
    forged.admitted_at_ms = 0; // Unsigned local timestamps cannot prove prior approval.
    assert!(
        admission::verify_record(&store, &forged, &rotated).is_err(),
        "a retired key must not authorize a new payload, even when backdated"
    );
}

#[test]
fn key_retirement_cannot_overtake_an_admission_waiting_for_hardware() {
    use louiselm_skills::signer::{Signer, SignerError};
    use std::{sync::mpsc, time::Duration};
    struct PausedSigner {
        key: SshKeygenSigner,
        started: mpsc::Sender<()>,
        resume: mpsc::Receiver<()>,
    }
    impl Signer for PausedSigner {
        fn sign(&self, namespace: &str, bytes: &[u8]) -> Result<String, SignerError> {
            self.started.send(()).unwrap();
            self.resume.recv_timeout(Duration::from_secs(5)).unwrap();
            self.key.sign(namespace, bytes)
        }
    }
    let ceremony = Ceremony::new();
    let store = ceremony.fixture.store();
    let trust = TrustStore::load(&store).unwrap().unwrap();
    let replacement = SshKey::generate(&ceremony.fixture, "replacement");
    let change = trust
        .rotation_payload(Role::Primary, &replacement.public_key(), SkPolicy::none())
        .unwrap();
    let signature = ceremony.recovery.sign(
        louiselm_skills::sshsig::TRUST_NAMESPACE,
        &change.canonical_bytes(),
    );
    let (started, waiting) = mpsc::channel();
    let (resume, ready) = mpsc::channel();
    let signer = PausedSigner {
        key: SshKeygenSigner::new(ceremony.primary.private_key_path()),
        started,
        resume: ready,
    };
    std::thread::scope(|scope| {
        let store_ref = &store;
        let worker = scope.spawn(move || {
            admission::admit(
                store_ref,
                &Policy::embedded(),
                &AdmissionRequest {
                    members: vec![],
                    view_roots: std::collections::BTreeMap::new(),
                    signer: &signer,
                    admitted_at_ms: 2,
                },
            )
        });
        waiting.recv_timeout(Duration::from_secs(5)).unwrap();
        let rotation = TrustStore::rotate(&store, &change, &signature, 3);
        resume.send(()).unwrap();
        let record = worker.join().unwrap().unwrap();
        assert!(matches!(
            rotation,
            Err(louiselm_skills::trust::TrustError::Busy(_))
        ));
        let latest = TrustStore::load(&store).unwrap().unwrap();
        assert!(latest.approved_admissions.contains(&record.generation));
        assert!(
            TrustStore::rotate(&store, &change, &signature, 3).is_err(),
            "newly registered approval invalidates the old trust-change snapshot"
        );
    });
}

#[test]
fn failed_approval_registration_never_becomes_retired_key_history() {
    use louiselm_skills::signer::{Signer, SignerError};
    struct FailRegistration {
        signer: SshKeygenSigner,
        state: std::path::PathBuf,
        alias: std::path::PathBuf,
    }
    impl Signer for FailRegistration {
        fn sign(&self, namespace: &str, bytes: &[u8]) -> Result<String, SignerError> {
            let signature = self.signer.sign(namespace, bytes)?;
            // Fixture owner injects an actual publication refusal after signing;
            // an unprivileged operator cannot alias a protected production store.
            std::fs::hard_link(&self.state, &self.alias)?;
            Ok(signature)
        }
    }
    let ceremony = Ceremony::new();
    let store = ceremony.fixture.store();
    let trust = TrustStore::load(&store).unwrap().unwrap();
    let signer = FailRegistration {
        signer: SshKeygenSigner::new(ceremony.primary.private_key_path()),
        state: ceremony.fixture.path("store/trust/roles.json"),
        alias: ceremony.fixture.path("test-registration-fault"),
    };
    let result = admission::admit(
        &store,
        &Policy::embedded(),
        &AdmissionRequest {
            members: vec![],
            view_roots: std::collections::BTreeMap::new(),
            signer: &signer,
            admitted_at_ms: 2,
        },
    );
    assert!(matches!(
        result,
        Err(AdmissionError::Trust(
            louiselm_skills::trust::TrustError::Io { .. }
        ))
    ));
    std::fs::remove_file(&signer.alias).unwrap();
    assert_eq!(TrustStore::load(&store).unwrap(), Some(trust.clone()));
    let pending = admission::list(&store)
        .unwrap()
        .pop()
        .expect("signature was stored before refusal");
    let replacement = SshKey::generate(&ceremony.fixture, "replacement");
    let change = trust
        .rotation_payload(Role::Primary, &replacement.public_key(), SkPolicy::none())
        .unwrap();
    let trust = TrustStore::rotate(
        &store,
        &change,
        &ceremony.recovery.sign(
            louiselm_skills::sshsig::TRUST_NAMESPACE,
            &change.canonical_bytes(),
        ),
        3,
    )
    .unwrap();
    assert!(admission::verify_record(&store, &pending, &trust).is_err());
}
