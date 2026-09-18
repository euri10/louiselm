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
    admission::{self, AdmissionError, AdmissionMember, AdmissionRequest},
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
    release_key: SshKey,
}

#[test]
fn linked_admission_is_signed_replayable_and_read_only_verifiable() {
    let ceremony = Ceremony::new();
    let package = ceremony.skill("linked");
    let store = ceremony.fixture.store();
    let operation = "12345678-1234-4234-8234-123456789abc";
    let request = AdmissionRequest {
        members: vec![AdmissionMember {
            package: package.clone(),
            depth: ReviewDepth::Read,
            agents: vec!["claude".into()],
        }],
        signer: &SshKeygenSigner::new(ceremony.primary.private_key_path()),
        admitted_at_ms: 1_756_800_000_000,
    };
    let record = admission::admit_linked(&store, &Policy::embedded(), &request, operation).unwrap();
    assert_eq!(record.state, GenerationState::PendingWitness);
    let replay = admission::admit_linked(
        &store,
        &Policy::embedded(),
        &AdmissionRequest {
            signer: &RefuseSigning,
            ..request
        },
        operation,
    )
    .unwrap();
    assert_eq!(record, replay);
    let read = admission::verify_linked(
        &store,
        &Policy::embedded(),
        "louiselm/skills",
        operation,
        &[package.to_string()],
        &["claude".into()],
    )
    .unwrap()
    .unwrap();
    assert_eq!(read, record.generation);
    for (domain, packages) in [
        ("wrong-domain", vec![package.to_string()]),
        (
            "louiselm/skills",
            vec![louiselm_skills::Digest::of(b"wrong-package").to_string()],
        ),
    ] {
        assert!(
            admission::verify_linked(
                &store,
                &Policy::embedded(),
                domain,
                operation,
                &packages,
                &["claude".into()]
            )
            .is_err()
        );
    }
    assert!(
        admission::verify_linked(
            &store,
            &Policy::embedded(),
            "louiselm/skills",
            operation,
            &[package.to_string()],
            &["other".into()],
        )
        .is_err()
    );
    write_file(&store.root().join("activation.pending.json"), "unsettled");
    assert!(
        admission::verify_linked(
            &store,
            &Policy::embedded(),
            "louiselm/skills",
            operation,
            &[package.to_string()],
            &["claude".into()],
        )
        .is_err()
    );
    assert_eq!(
        std::fs::read_to_string(store.root().join("activation.pending.json")).unwrap(),
        "unsettled"
    );
}

struct RefuseSigning;
impl louiselm_skills::signer::Signer for RefuseSigning {
    fn sign(&self, _: &str, _: &[u8]) -> Result<String, louiselm_skills::signer::SignerError> {
        Err(louiselm_skills::signer::SignerError::Failed(
            "must not sign again".into(),
        ))
    }
}

#[test]
fn evidence_sharing_is_opt_in_and_survives_atomic_replacement() {
    use std::os::unix::fs::PermissionsExt;
    let ceremony = Ceremony::new();
    let store = ceremony.fixture.store();
    let package = ceremony.skill("private");
    let first = ceremony
        .admit(&[(package, ReviewDepth::Read)], &ceremony.primary)
        .unwrap();
    let mode =
        |path: &std::path::Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        mode(&admission::record_path(&store, &first.digest())),
        0o600
    );
    assert_eq!(mode(&store.root().join("trust/roles.json")), 0o600);
    for directory in ["trust", "generations", "packages", "staging"] {
        std::fs::set_permissions(
            store.root().join(directory),
            std::fs::Permissions::from_mode(0o2750),
        )
        .unwrap();
    }
    let package = ceremony.skill("shared");
    let second = ceremony
        .admit(&[(package.clone(), ReviewDepth::Read)], &ceremony.primary)
        .unwrap();
    assert_eq!(
        mode(&admission::record_path(&store, &second.digest())),
        0o640
    );
    assert_eq!(mode(&store.root().join("trust/roles.json")), 0o640);
    assert_eq!(
        mode(&store.root().join("packages").join(package.directory_name())),
        0o750
    );
}

#[test]
fn linked_partial_registration_stays_pending_until_tool_recovery_without_signing() {
    use std::os::unix::fs::PermissionsExt;
    let ceremony = Ceremony::new();
    let package = ceremony.skill("partial");
    let store = ceremony.fixture.store();
    let operation = "12345678-1234-4234-8234-123456789abc";
    let members = vec![AdmissionMember {
        package: package.clone(),
        depth: ReviewDepth::Read,
        agents: vec!["claude".into()],
    }];
    let signer = SshKeygenSigner::new(ceremony.primary.private_key_path());
    let request = AdmissionRequest {
        members,
        signer: &signer,
        admitted_at_ms: 1000,
    };
    let record = admission::admit_linked(&store, &Policy::embedded(), &request, operation).unwrap();
    let verify = || {
        admission::verify_linked(
            &store,
            &Policy::embedded(),
            "louiselm/skills",
            operation,
            &[package.to_string()],
            &["claude".into()],
        )
    };
    let trust_path = store.root().join("trust/roles.json");
    let mut trust = TrustStore::load(&store).unwrap().unwrap();
    trust.approved_admissions.clear();
    std::fs::write(&trust_path, serde_json::to_vec(&trust).unwrap()).unwrap();
    let before = std::fs::read(&trust_path).unwrap();
    assert!(verify().is_err());
    assert_eq!(
        std::fs::read(&trust_path).unwrap(),
        before,
        "reader must not repair trust"
    );
    let recovered = admission::admit_linked(
        &store,
        &Policy::embedded(),
        &AdmissionRequest {
            signer: &RefuseSigning,
            ..request
        },
        operation,
    )
    .unwrap();
    assert_eq!(recovered, record);
    assert_eq!(verify().unwrap(), Some(record.generation.clone()));
    let generation_path = admission::record_path(&store, &record.digest());
    let original = std::fs::read(&generation_path).unwrap();
    let mut corrupt = record.clone();
    corrupt.signature = "not a signature".into();
    std::fs::write(&generation_path, serde_json::to_vec(&corrupt).unwrap()).unwrap();
    assert!(verify().is_err());
    std::fs::write(&generation_path, original).unwrap();
    let packaged = store
        .root()
        .join("packages")
        .join(package.directory_name())
        .join("files/SKILL.md");
    std::fs::set_permissions(&packaged, std::fs::Permissions::from_mode(0o644)).unwrap();
    std::fs::write(&packaged, "changed").unwrap();
    assert!(verify().is_err());
}

impl Ceremony {
    fn new() -> Self {
        let fixture = Fixture::new();
        let primary = SshKey::generate(&fixture, "primary");
        let release_key = SshKey::generate(&fixture, "release_key");
        TrustStore::bootstrap(
            &fixture.store(),
            "louiselm/skills",
            &primary.public_key(),
            &release_key.public_key(),
            SkPolicy::none(),
            1_756_800_000_000,
        )
        .expect("trust bootstraps");
        Self {
            fixture,
            primary,
            release_key,
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

    /// Admits with a single-Agent scope, which is what most cases care about.
    fn admit(
        &self,
        members: &[(louiselm_skills::Digest, ReviewDepth)],
        key: &SshKey,
    ) -> Result<louiselm_skills::generation::GenerationRecord, AdmissionError> {
        let scoped = members
            .iter()
            .map(|(package, depth)| (package.clone(), *depth, vec!["claude".to_owned()]))
            .collect::<Vec<_>>();
        self.admit_scoped(&scoped, key)
    }

    fn admit_scoped(
        &self,
        members: &[(louiselm_skills::Digest, ReviewDepth, Vec<String>)],
        key: &SshKey,
    ) -> Result<louiselm_skills::generation::GenerationRecord, AdmissionError> {
        admission::admit(
            &self.fixture.store(),
            &Policy::embedded(),
            &AdmissionRequest {
                members: members
                    .iter()
                    .map(|(package, depth, agents)| AdmissionMember {
                        package: package.clone(),
                        depth: *depth,
                        agents: agents.clone(),
                    })
                    .collect(),
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
        .admit(&[(package, ReviewDepth::Read)], &ceremony.release_key)
        .expect_err("the release_key key is not an ordinary signer");

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
    assert_eq!(activated.admitted_at_ms, 1_756_800_000_000);
    assert_eq!(
        admission::current(&store)
            .expect("current is readable")
            .map(|record| record.payload.sequence),
        Some(1),
    );
    let pins_path = store.root().join("pins.jsonl");
    let pins = std::fs::read(&pins_path).expect("activated lineage is readable");
    let pin: serde_json::Value = serde_json::from_slice(&pins).expect("one pin was committed");
    assert_eq!(pin["activated_at_ms"], 1_756_800_000_003_u64);
    assert_eq!(
        admission::activate(&store, &record.digest(), 1_756_800_000_004)
            .expect("retrying the current Generation confirms activation"),
        activated,
    );
    assert_eq!(
        std::fs::read(&pins_path).expect("lineage is readable"),
        pins
    );
}

#[test]
fn activation_preserves_unreadable_pin_lineage_and_reports_the_read_error() {
    let ceremony = Ceremony::new();
    let package = ceremony.skill("alpha");
    let record = ceremony
        .admit(&[(package, ReviewDepth::Read)], &ceremony.primary)
        .expect("admission succeeds");
    let store = ceremony.fixture.store();
    admission::witness(&store, &record.digest(), &ceremony.witness(), 1)
        .expect("witnessing succeeds");
    let path = store.root().join("pins.jsonl");
    let existing = b"previous lineage\xff\n";
    std::fs::write(&path, existing).expect("unreadable lineage fixture is writable");

    let result = admission::activate(&store, &record.digest(), 2);

    assert_eq!(
        std::fs::read(&path).expect("lineage bytes are readable"),
        existing,
        "activation must not replace lineage it could not read",
    );
    assert!(
        matches!(
            &result,
            Err(AdmissionError::Io { path: failed_path, source })
                if failed_path == &path.display().to_string()
                    && source.kind() == std::io::ErrorKind::InvalidData
        ),
        "the original lineage read error must reach the caller: {result:?}",
    );
}

#[test]
fn failed_activation_preserves_current_supply_and_allows_retry() {
    let ceremony = Ceremony::new();
    let store = ceremony.fixture.store();
    let first = ceremony
        .admit(
            &[(ceremony.skill("alpha"), ReviewDepth::Read)],
            &ceremony.primary,
        )
        .expect("first admission succeeds");
    admission::witness(&store, &first.digest(), &ceremony.witness(), 1)
        .expect("first witnessing succeeds");
    let first = admission::activate(&store, &first.digest(), 2).expect("first activation succeeds");
    let second = ceremony
        .admit(
            &[(ceremony.skill("beta"), ReviewDepth::Read)],
            &ceremony.primary,
        )
        .expect("second admission succeeds");
    let second = admission::witness(&store, &second.digest(), &ceremony.witness(), 3)
        .expect("second witnessing succeeds");
    let pins = store.root().join("pins.jsonl");
    let saved_pins = ceremony.fixture.path("saved-pins.jsonl");
    std::fs::rename(&pins, &saved_pins).expect("save the existing lineage");
    std::fs::create_dir(&pins).expect("inject a lineage persistence failure");

    let result = admission::activate(&store, &second.digest(), 4);

    assert!(matches!(result, Err(AdmissionError::Io { .. })));
    assert_eq!(
        admission::current(&store).expect("current supply remains readable"),
        Some(first),
        "failed activation must leave the prior Generation current",
    );
    assert_eq!(
        admission::load(&store, &second.digest()).expect("candidate remains readable"),
        second,
    );
    std::fs::remove_dir(&pins).expect("remove the injected directory");
    std::fs::rename(&saved_pins, &pins).expect("restore lineage");
    admission::activate(&store, &second.digest(), 5).expect("activation can be retried");
    assert_eq!(
        std::fs::read_to_string(&pins)
            .expect("lineage is readable")
            .lines()
            .count(),
        2,
    );
}

#[test]
fn a_witness_refresh_cannot_undo_activation_during_network_io() {
    struct ActivateDuringFetch {
        store: louiselm_skills::Store,
        witness: GitWitness,
    }
    impl Witness for ActivateDuringFetch {
        fn fetch(
            &self,
            digest: &louiselm_skills::Digest,
        ) -> Result<
            Option<(Vec<u8>, louiselm_skills::witness::WitnessEvidence)>,
            louiselm_skills::witness::WitnessError,
        > {
            admission::activate(&self.store, digest, 3)
                .expect("another operation can activate while network I/O is pending");
            self.witness.fetch(digest)
        }
        fn publish(
            &self,
            digest: &louiselm_skills::Digest,
            bytes: &[u8],
        ) -> Result<louiselm_skills::witness::WitnessEvidence, louiselm_skills::witness::WitnessError>
        {
            self.witness.publish(digest, bytes)
        }
        fn describe(&self) -> String {
            self.witness.describe()
        }
    }
    let ceremony = Ceremony::new();
    let record = ceremony
        .admit(
            &[(ceremony.skill("alpha"), ReviewDepth::Read)],
            &ceremony.primary,
        )
        .expect("admission succeeds");
    let store = ceremony.fixture.store();
    admission::witness(&store, &record.digest(), &ceremony.witness(), 1)
        .expect("initial witnessing succeeds");
    let witness = ActivateDuringFetch {
        store: store.clone(),
        witness: ceremony.witness(),
    };
    let refreshed = admission::witness(&store, &record.digest(), &witness, 4)
        .expect("refresh preserves the concurrent activation");
    assert_eq!(refreshed.state, GenerationState::Current);
    assert_eq!(
        admission::current(&store).expect("current is readable"),
        Some(refreshed)
    );
    assert_eq!(
        std::fs::read_to_string(store.root().join("pins.jsonl"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}

#[test]
fn a_held_trust_lock_refuses_activation_readers_and_other_writers() {
    use rustix::fs::{FlockOperation, flock};
    let ceremony = Ceremony::new();
    let store = ceremony.fixture.store();
    let package = ceremony.skill("alpha");
    let record = ceremony
        .admit(&[(package.clone(), ReviewDepth::Read)], &ceremony.primary)
        .unwrap();
    admission::witness(&store, &record.digest(), &ceremony.witness(), 1).unwrap();
    let lock = std::fs::File::open(store.root().join("trust/roles.lock")).unwrap();
    flock(&lock, FlockOperation::NonBlockingLockExclusive).unwrap();
    // dup retains the same open-file description as a concurrent fork before exec.
    // Keep it alive past release to make louiselm-h157 deterministic.
    let inherited = lock.try_clone().unwrap();
    let operations = [
        admission::activate(&store, &record.digest(), 2).map(|_| ()),
        admission::current(&store).map(|_| ()),
        admission::load(&store, &record.digest()).map(|_| ()),
        admission::load_verified(&store, &record.digest()).map(|_| ()),
        admission::list(&store).map(|_| ()),
        admission::status(&store).map(|_| ()),
        admission::witness(&store, &record.digest(), &ceremony.witness(), 2).map(|_| ()),
        ceremony
            .admit(&[(package, ReviewDepth::Read)], &ceremony.primary)
            .map(|_| ()),
    ];
    for result in operations {
        assert!(
            matches!(
                result,
                Err(AdmissionError::Trust(
                    louiselm_skills::trust::TrustError::Busy(_)
                ))
            ),
            "{result:?}"
        );
    }
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_louiselm-skills"))
        .args(["generation", "status", "--robot-json"])
        .env("LOUISELM_SKILLS_STORE", store.root())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("busy"));
    flock(&lock, FlockOperation::Unlock).unwrap();
    drop(lock);
    assert_eq!(admission::current(&store).unwrap(), None);
    admission::activate(&store, &record.digest(), 3).expect("released lock permits activation");
    drop(inherited);
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
    let pins_path = store.root().join("pins.jsonl");
    assert!(!pins_path.exists(), "the first activation creates lineage");
    admission::activate(&store, &first.digest(), 2).expect("activation succeeds");
    let first_pin = std::fs::read_to_string(&pins_path).expect("first pin is readable");
    let pin: serde_json::Value = serde_json::from_str(&first_pin).expect("pin is JSON");
    assert_eq!(pin["generation"], first.generation);
    assert_eq!(pin["sequence"], 1);

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
    let pins = std::fs::read_to_string(&pins_path).expect("pins are readable");
    assert!(
        pins.starts_with(&first_pin),
        "existing lineage stays intact"
    );
    assert_eq!(pins.lines().count(), 2);
    let pin: serde_json::Value =
        serde_json::from_str(pins.lines().nth(1).expect("second pin exists"))
            .expect("second pin is JSON");
    assert_eq!(pin["generation"], second.generation);
    assert_eq!(pin["sequence"], 2);

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

    let change = support::key_change(&trust, Role::Primary, &replacement.public_key());
    let signature = ceremony.release_key.sign(
        louiselm_skills::trust::recovery::RECOVERY_NAMESPACE,
        &change.canonical_bytes(),
    );
    support::apply_key_change(&store, &change, &signature, &replacement, 1_756_800_000_100)
        .expect("the release_key key may replace the primary");

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
fn role_confusion_and_outsider_signatures_cannot_change_trust() {
    let ceremony = Ceremony::new();
    let store = ceremony.fixture.store();
    let attacker = SshKey::generate(&ceremony.fixture, "attacker");
    let trust = TrustStore::load(&store)
        .expect("trust is readable")
        .expect("trust was bootstrapped");

    let change = support::key_change(&trust, Role::Primary, &attacker.public_key());
    for signer in [&ceremony.primary, &attacker] {
        let signature = signer.sign(
            louiselm_skills::trust::recovery::RECOVERY_NAMESPACE,
            &change.canonical_bytes(),
        );
        assert!(
            support::apply_key_change(&store, &change, &signature, &attacker, 1_756_800_000_100)
                .is_err(),
            "a primary or outsider signature cannot impersonate the release role",
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
    let change = support::key_change(&trust, Role::Primary, &replacement.public_key());
    let signature = ceremony.release_key.sign(
        louiselm_skills::trust::recovery::RECOVERY_NAMESPACE,
        &change.canonical_bytes(),
    );
    let rotated = support::apply_key_change(&store, &change, &signature, &replacement, 2)
        .expect("retire primary");
    admission::verify_record(&store, &historical, &rotated).expect("real history still verifies");

    let mut forged = historical;
    forged.payload.members[0]
        .agents
        .push("added-after-retirement".to_owned());
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
    let change = support::key_change(&trust, Role::Primary, &replacement.public_key());
    let signature = ceremony.release_key.sign(
        louiselm_skills::trust::recovery::RECOVERY_NAMESPACE,
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
                    signer: &signer,
                    admitted_at_ms: 2,
                },
            )
        });
        waiting.recv_timeout(Duration::from_secs(5)).unwrap();
        let rotation = support::apply_key_change(&store, &change, &signature, &replacement, 3);
        resume.send(()).unwrap();
        let record = worker.join().unwrap().unwrap();
        assert!(matches!(
            rotation,
            Err(louiselm_skills::trust::recovery::RecoveryError::Trust(
                louiselm_skills::trust::TrustError::Busy(_)
            ))
        ));
        let latest = TrustStore::load(&store).unwrap().unwrap();
        assert!(latest.approved_admissions.contains(&record.generation));
        assert!(
            support::apply_key_change(&store, &change, &signature, &replacement, 3).is_err(),
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
    let change = support::key_change(&trust, Role::Primary, &replacement.public_key());
    let trust = support::apply_key_change(
        &store,
        &change,
        &ceremony.release_key.sign(
            louiselm_skills::trust::recovery::RECOVERY_NAMESPACE,
            &change.canonical_bytes(),
        ),
        &replacement,
        3,
    )
    .unwrap();
    assert!(admission::verify_record(&store, &pending, &trust).is_err());
}

// louiselm-d6fv.3.5: per-Agent Instruction view membership, signed.

#[test]
fn the_member_root_covers_the_agent_list() {
    let member = |agents: &[&str]| louiselm_skills::generation::Member {
        package_digest: "sha256:aa".to_owned(),
        dossier_digest: "sha256:bb".to_owned(),
        review_depth: "read".to_owned(),
        agents: agents.iter().map(|name| (*name).to_owned()).collect(),
    };
    let payload = |agents: &[&str]| {
        louiselm_skills::generation::GenerationPayload::new(
            "louiselm/skills",
            1,
            None,
            "sha256:cc",
            vec![member(agents)],
        )
    };

    assert_ne!(
        payload(&["claude"]).recomputed_member_root(),
        payload(&["claude", "codex"]).recomputed_member_root(),
        "widening a view must change the root the signature covers",
    );
}

#[test]
fn the_agent_list_is_sorted_and_deduplicated() {
    let payload = louiselm_skills::generation::GenerationPayload::new(
        "louiselm/skills",
        1,
        None,
        "sha256:cc",
        vec![louiselm_skills::generation::Member {
            package_digest: "sha256:aa".to_owned(),
            dossier_digest: "sha256:bb".to_owned(),
            review_depth: "read".to_owned(),
            agents: vec!["codex".to_owned(), "claude".to_owned(), "codex".to_owned()],
        }],
    );

    assert_eq!(
        payload.members[0].agents,
        vec!["claude".to_owned(), "codex".to_owned()],
        "the same scoping decision must sign to the same bytes",
    );
}

#[test]
fn a_member_naming_no_agent_is_refused() {
    let ceremony = Ceremony::new();
    let package = ceremony.skill("alpha");

    let refusal = ceremony.admit_scoped(&[(package, ReviewDepth::Read, vec![])], &ceremony.primary);

    assert!(
        matches!(refusal, Err(AdmissionError::NotReviewable { .. })),
        "a package that enters no view is not admissible: {refusal:?}",
    );
}

#[test]
fn an_agent_name_no_registry_knows_is_admitted() {
    let ceremony = Ceremony::new();
    let package = ceremony.skill("alpha");

    let record = ceremony
        .admit_scoped(
            &[(package, ReviewDepth::Read, vec!["kimi".to_owned()])],
            &ceremony.primary,
        )
        .expect("an Agent this host does not run is still a valid scope");

    assert_eq!(record.payload.members[0].agents, vec!["kimi".to_owned()]);
}

#[test]
fn a_generation_one_payload_is_refused() {
    let ceremony = Ceremony::new();
    let package = ceremony.skill("alpha");
    let store = ceremony.fixture.store();
    let mut record = ceremony
        .admit(&[(package, ReviewDepth::Read)], &ceremony.primary)
        .expect("admission succeeds");
    let trust = TrustStore::load(&store)
        .expect("trust loads")
        .expect("trust exists");

    record.payload.schema = "louiselm.skills.generation/1".to_owned();

    assert!(
        matches!(
            admission::verify_record(&store, &record, &trust),
            Err(AdmissionError::Chain(_)),
        ),
        "a /1 payload is refused as unsupported, never silently upgraded",
    );
}
