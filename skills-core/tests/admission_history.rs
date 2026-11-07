//! Explicit succession from historical generation/2, never legacy active supply.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Isolated software-key fixtures assert their exact failures."
)]

mod support;

use std::{fs, process::Command};

use louiselm_skills::{
    Digest, GenerationRecord, GenerationState, Policy, ReviewDepth, TrustStore,
    admission::{self, AdmissionError, AdmissionMember, AdmissionRequest},
    dossier::{Dossier, DossierRequest},
    generation::GenerationPayload,
    instruction_view,
    registry::Registry,
    session_manifest::{SessionInputs, SkillsMode},
    signer::{Signer, SignerError},
    sshsig::{ADMISSION_NAMESPACE, SkPolicy},
    witness::{GitWitness, Witness},
};
use support::{Fixture, SshKey, write_file};

struct History {
    fixture: Fixture,
    primary: SshKey,
    package: Digest,
    generation: Digest,
    payload: String,
    witnessed: Vec<u8>,
}

impl History {
    fn new() -> Self {
        let fixture = Fixture::new();
        let primary = SshKey::generate(&fixture, "primary");
        let release = SshKey::generate(&fixture, "release");
        let store = fixture.store();
        TrustStore::bootstrap(
            &store,
            "history-test",
            &primary.public_key(),
            &release.public_key(),
            SkPolicy::none(),
            1,
        )
        .unwrap();
        let candidate = fixture.candidate("legacy-skill");
        write_file(
            &candidate.join("SKILL.md"),
            "---\nname: legacy-skill\ndescription: Historical fixture.\n---\n\nBody.\n",
        );
        let package = fixture.capture(&candidate).unwrap().0.digest;
        let policy = Policy::embedded();
        // Exact field order and shape from generation.rs at installed2351d0a.
        // Do NOT construct this with current types then merely change the schema:
        // real historical members contain no relied_on field, even an empty one.
        let members = format!(
            r#"[{{"package_digest":"{package}","dossier_digest":"{}","review_depth":"read","agents":["codex"]}}]"#,
            Digest::of(b"a historical Dossier, not the current review")
        );
        let payload = format!(
            r#"{{"schema":"louiselm.skills.generation/2","trust_domain":"history-test","sequence":1,"predecessor":null,"signer_role":"primary","policy_digest":"{}","member_root":"{}","members":{members}}}"#,
            policy.digest(),
            Digest::of(members.as_bytes())
        );
        let generation = Digest::of(payload.as_bytes());
        let signature =
            serde_json::to_string(&primary.sign(ADMISSION_NAMESPACE, payload.as_bytes())).unwrap();
        let witnessed = format!(r#"{{"payload":{payload},"signature":{signature}}}"#).into_bytes();
        let witness = GitWitness::new(
            &fixture.witness_remote(),
            "history",
            &fixture.path("witness-work"),
        );
        witness.publish(&generation, &witnessed).unwrap();
        let (read_back, evidence) = witness.fetch(&generation).unwrap().unwrap();
        assert_eq!(read_back, witnessed);
        let record = format!(
            r#"{{"schema":"louiselm.skills.generation-record/1","generation":"{generation}","payload":{payload},"signature":{signature},"state":"current","witness":{},"admitted_at_ms":1,"invalid_reason":null}}"#,
            serde_json::to_string(&evidence).unwrap()
        );
        write_file(&admission::record_path(&store, &generation), &record);
        Self {
            fixture,
            primary,
            package,
            generation,
            payload,
            witnessed,
        }
    }

    fn cli(&self, arguments: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_louiselm-skills"))
            .args(arguments)
            .arg("--store")
            .arg(self.fixture.store_root())
            .output()
            .unwrap()
    }

    fn successor(&self) -> GenerationRecord {
        let output = self.cli(&[
            "generation",
            "admit",
            "--transition-from",
            &self.generation.to_string(),
            "--member",
            &format!("{}:read=codex", self.package),
            "--key",
            self.primary.private_key_path().to_str().unwrap(),
            "--robot-json",
        ]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    fn request<'a>(&self, signer: &'a dyn Signer) -> AdmissionRequest<'a> {
        AdmissionRequest {
            members: vec![AdmissionMember {
                package: self.package.clone(),
                depth: ReviewDepth::Read,
                agents: vec!["codex".into()],
            }],
            signer,
            admitted_at_ms: 2,
            configured_agents: None,
        }
    }
}

struct NeverSign;
impl Signer for NeverSign {
    fn sign(&self, _: &str, _: &[u8]) -> Result<String, SignerError> {
        panic!("a refused predecessor must not reach the signer");
    }
}

#[test]
fn historical_bytes_round_trip_but_cannot_become_active_supply() {
    let history = History::new();
    let store = history.fixture.store();
    let record = admission::load(&store, &history.generation).unwrap();
    assert_eq!(record.payload.canonical_bytes(), history.payload.as_bytes());
    assert_eq!(record.digest(), history.generation);
    assert_eq!(record.witness_bytes(), history.witnessed);
    let trust = TrustStore::load(&store).unwrap().unwrap();
    for error in [
        admission::verify_record(&store, &record, &trust).unwrap_err(),
        admission::activate(&store, &history.generation, 10).unwrap_err(),
        admission::witness(
            &store,
            &history.generation,
            &GitWitness::new(
                &history.fixture.witness_remote(),
                "history",
                &history.fixture.path("refused-witness"),
            ),
            10,
        )
        .unwrap_err(),
    ] {
        assert!(
            matches!(error, AdmissionError::Chain(ref reason) if reason.contains("generation/2")),
            "{error}"
        );
    }
    let status = admission::status(&store).unwrap();
    assert_eq!(status.state, Some(GenerationState::Invalid));
    assert_eq!(status.effective_members, Vec::<String>::new());
    assert_eq!(status.next_action.id, "admit_history_successor");
    let output = history.cli(&["generation", "status", "--robot-json"]);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let runtime = history.fixture.path("runtime");
    write_file(&runtime.join("bin/agent"), "fixture runtime");
    write_file(&runtime.join("lib/adapter.js"), "fixture adapter");
    let registry_root = history.fixture.path("registry");
    support::write_registry(&registry_root, &runtime);
    let registry = Registry::open(&registry_root).unwrap();
    let error = instruction_view::materialize(&store, &Policy::embedded(), &registry).unwrap_err();
    assert!(error.to_string().contains("generation/2"), "{error}");
    let error = SessionInputs::resolve(
        &store,
        &Policy::embedded(),
        &registry,
        "demo",
        SkillsMode::Enabled,
    )
    .unwrap_err();
    assert_eq!(
        error.dimension(),
        louiselm_skills::posture::DimensionName::ManagedSupply
    );
}

#[test]
fn explicit_successor_preserves_old_signature_and_witness_history() {
    let history = History::new();
    let store = history.fixture.store();
    let old_path = admission::record_path(&store, &history.generation);
    let old_bytes = fs::read(&old_path).unwrap();
    let successor = history.successor();
    assert_eq!(successor.payload.schema, "louiselm.skills.generation/3");
    assert_eq!(successor.payload.sequence, 2);
    assert_eq!(
        successor.payload.predecessor,
        Some(history.generation.to_string())
    );
    assert_eq!(successor.payload.members[0].agents, ["codex"]);
    assert_eq!(
        successor.payload.members[0].dossier_digest,
        Dossier::build(
            &store,
            &Policy::embedded(),
            &DossierRequest::new(&history.package).with_review_depth(ReviewDepth::Read)
        )
        .unwrap()
        .portable_digest()
        .to_string()
    );
    assert_eq!(successor.payload.members[0].relied_on, Vec::<String>::new());
    assert_eq!(fs::read(&old_path).unwrap(), old_bytes);
    assert!(matches!(
        admission::activate(&store, &successor.digest(), 19),
        Err(AdmissionError::NotWitnessed { .. })
    ));
    let witness = GitWitness::new(
        &history.fixture.witness_remote(),
        "history",
        &history.fixture.path("successor-witness-work"),
    );
    admission::witness(&store, &successor.digest(), &witness, 20).unwrap();
    admission::activate(&store, &successor.digest(), 21).unwrap();
    let old = admission::load(&store, &history.generation).unwrap();
    assert_eq!(old.state, GenerationState::Superseded);
    assert_eq!(old.payload.canonical_bytes(), history.payload.as_bytes());
    assert_eq!(old.witness_bytes(), history.witnessed);
    assert_eq!(
        witness.fetch(&history.generation).unwrap().unwrap().0,
        history.witnessed
    );
    assert_eq!(
        admission::current(&store).unwrap().unwrap().digest(),
        successor.digest()
    );
    admission::activate(&store, &successor.digest(), 22).unwrap();
    // The explicit transition cannot be reused after the successor is current.
    assert!(
        admission::admit_transition(
            &store,
            &Policy::embedded(),
            &history.request(&NeverSign),
            &history.generation
        )
        .is_err()
    );
    assert!(admission::activate(&store, &history.generation, 23).is_err());
    // Normal Admission continues the mixed-version history, without a flag.
    let signer = louiselm_skills::SshKeygenSigner::new(history.primary.private_key_path());
    let third = admission::admit(&store, &Policy::embedded(), &history.request(&signer)).unwrap();
    assert_eq!(third.payload.sequence, 3);
    assert_eq!(third.payload.predecessor, Some(successor.generation));
    admission::verify_record(&store, &third, &TrustStore::load(&store).unwrap().unwrap()).unwrap();
}

#[test]
fn closed_versioned_decoding_does_not_invent_fields_or_accept_ambiguity() {
    let history = History::new();
    for malformed in [
        history.payload.replace("generation/2", "generation/99"),
        history.payload.replace("generation/2", "generation/3"), // missing required reliance
        history
            .payload
            .replacen('{', r#"{"schema":"louiselm.skills.generation/2","#, 1),
        history
            .payload
            .replacen('{', r#"{"trust_domain":"duplicate","#, 1),
        history.payload.replacen('{', r#"{"unknown":true,"#, 1),
        history.payload.replace(
            r#""agents":["codex"]"#,
            r#""agents":["codex"],"relied_on":[]"#,
        ),
    ] {
        assert!(
            serde_json::from_str::<GenerationPayload>(&malformed).is_err(),
            "{malformed}"
        );
    }
    let linked = history.payload.strip_suffix('}').unwrap().to_owned()
        + r#", "approval_operation":"12345678-1234-4234-8234-123456789abc"}"#;
    let parsed: GenerationPayload = serde_json::from_str(&linked).unwrap();
    assert_eq!(
        String::from_utf8(parsed.canonical_bytes()).unwrap(),
        linked.replace(", \"approval", ",\"approval")
    );
    let current = history
        .payload
        .replace("generation/2", "generation/3")
        .replace(
            r#""agents":["codex"]"#,
            r#""agents":["codex"],"relied_on":[]"#,
        );
    let parsed: GenerationPayload = serde_json::from_str(&current).unwrap();
    assert_eq!(
        parsed.canonical_bytes(),
        current.as_bytes(),
        "v3 signed encoding must not change"
    );
}

#[test]
fn transition_requires_exact_legacy_current_and_refuses_ordinary_or_linked_admission() {
    let history = History::new();
    let store = history.fixture.store();
    let policy = Policy::embedded();
    let request = history.request(&NeverSign);
    assert!(
        admission::admit(&store, &policy, &request)
            .unwrap_err()
            .to_string()
            .contains("--transition-from")
    );
    assert!(
        admission::admit_linked(
            &store,
            &policy,
            &request,
            "12345678-1234-4234-8234-123456789abc"
        )
        .is_err()
    );
    assert!(admission::admit_transition(&store, &policy, &request, &Digest::of(b"stale")).is_err());
    assert!(
        admission::admit_transition(
            &Fixture::new().store(),
            &policy,
            &request,
            &history.generation
        )
        .is_err()
    );
    let path = admission::record_path(&store, &history.generation);
    let original = fs::read(&path).unwrap();
    let expected = history.generation.to_string();
    for args in [
        vec![
            "generation",
            "status",
            "--transition-from",
            expected.as_str(),
        ],
        vec![
            "generation",
            "admit",
            "--transition-from",
            expected.as_str(),
            "--skill-request",
            "12345678-1234-4234-8234-123456789abc",
            "--key",
            "must-not-read",
            "--member",
            "must-not-read",
        ],
    ] {
        let output = history.cli(&args);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("--transition-from"));
    }
    assert_eq!(fs::read(&path).unwrap(), original);
    assert_eq!(admission::list(&store).unwrap().len(), 1);
}

#[test]
fn legacy_history_without_a_current_pointer_cannot_silently_start_a_new_chain() {
    let history = History::new();
    let store = history.fixture.store();
    let mut record = admission::load(&store, &history.generation).unwrap();
    for state in [GenerationState::Superseded, GenerationState::PendingWitness] {
        record.state = state;
        fs::write(
            admission::record_path(&store, &history.generation),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
        assert!(
            admission::admit(&store, &Policy::embedded(), &history.request(&NeverSign)).is_err()
        );
        let status = admission::status(&store).unwrap();
        assert_eq!(status.next_action.id, "inspect_generation_history");
        assert_eq!(status.effective_members, Vec::<String>::new());
    }
}

#[test]
fn corrupted_or_unwitnessed_history_fails_before_signing_and_without_rewriting() {
    let history = History::new();
    let store = history.fixture.store();
    let path = admission::record_path(&store, &history.generation);
    let original = admission::load(&store, &history.generation).unwrap();
    for mutation in 0..8 {
        let mut record = original.clone();
        match mutation {
            0 => record.payload.trust_domain = "foreign".into(),
            1 => record.signature = "tampered".into(),
            2 => record.schema = "unknown".into(),
            3 => record.payload.member_root = Digest::of(b"wrong root").to_string(),
            4 => record.payload.sequence = 0,
            5 => {
                record.payload.sequence = 2;
                record.payload.predecessor = Some(Digest::of(b"missing").to_string());
            }
            6 => record.witness = None,
            7 => record.payload.sequence = u64::MAX,
            _ => unreachable!(),
        }
        record.generation = record.digest().to_string();
        // Keep an otherwise valid signature for structural/chain refusals.
        if mutation != 1 {
            record.signature = history
                .primary
                .sign(ADMISSION_NAMESPACE, &record.payload.canonical_bytes());
        }
        let bytes = serde_json::to_vec(&record).unwrap();
        fs::write(&path, &bytes).unwrap();
        assert!(
            admission::admit_transition(
                &store,
                &Policy::embedded(),
                &history.request(&NeverSign),
                &record.digest()
            )
            .is_err(),
            "mutation {mutation}"
        );
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_eq!(admission::list(&store).unwrap().len(), 1);
    }
}

#[test]
fn duplicate_current_and_changed_predecessor_never_get_hidden_by_history_loading() {
    let history = History::new();
    let store = history.fixture.store();
    let original_path = admission::record_path(&store, &history.generation);
    let original = fs::read(&original_path).unwrap();
    let successor = history.successor();
    // A valid successor still depends on the original signed history.
    let mut old: GenerationRecord = serde_json::from_slice(&original).unwrap();
    old.signature = "tampered after successor Admission".into();
    fs::write(&original_path, serde_json::to_vec(&old).unwrap()).unwrap();
    assert!(admission::load_verified(&store, &successor.digest()).is_err());
    fs::write(&original_path, &original).unwrap();
    let duplicate = store.root().join("generations/duplicate.json");
    fs::write(&duplicate, &original).unwrap();
    assert!(admission::current(&store).is_err());
    assert!(admission::status(&store).is_err());
    assert!(
        admission::admit_transition(
            &store,
            &Policy::embedded(),
            &history.request(&NeverSign),
            &history.generation
        )
        .is_err()
    );
    assert_eq!(fs::read(original_path).unwrap(), original);
}

#[test]
fn transition_recomputes_reliance_instead_of_inventing_it_for_old_members() {
    use louiselm_skills::{
        SshKeygenSigner,
        endorsement::{
            self, Batch, EndorsedPackage, EndorserRole, Signing, import, local,
            trust::{EndorserEntry, MAX_LIFETIME_MS},
        },
    };
    let history = History::new();
    let store = history.fixture.store();
    let publisher = SshKey::generate(&history.fixture, "publisher");
    let signer = SshKeygenSigner::new(history.primary.private_key_path());
    local::add(
        &store,
        &EndorserEntry {
            label: "fixture publisher".into(),
            max_lifetime_ms: MAX_LIFETIME_MS,
            public_key: publisher.public_key(),
            roles: vec![EndorserRole::Publisher],
            sk_policy: SkPolicy::none(),
        },
        &signer,
        1,
    )
    .unwrap();
    let bytes = endorsement::sign_batch(
        &Signing {
            signer_role: EndorserRole::Publisher,
            signer_key: publisher.public_key(),
            issued_at_ms: 1,
            expires_at_ms: 100,
        },
        &Batch::Endorsements(vec![EndorsedPackage {
            package_digest: history.package.clone(),
            policy_digest: Policy::embedded().digest().clone(),
            dossier_digest: None,
            assessment: None,
            review_depth: ReviewDepth::Read,
            assumptions: Vec::new(),
        }]),
        &SshKeygenSigner::new(publisher.private_key_path()),
    )
    .unwrap()
    .remove(0);
    let evidence = import::import_bytes(&store, &bytes).unwrap().digest;
    let successor = admission::admit_transition(
        &store,
        &Policy::embedded(),
        &history.request(&signer),
        &history.generation,
    )
    .unwrap();
    assert_eq!(
        successor.payload.members[0].relied_on,
        [evidence.to_string()]
    );
    assert_eq!(
        admission::load(&store, &history.generation)
            .unwrap()
            .witness_bytes(),
        history.witnessed
    );
}

#[test]
fn historical_verification_preserves_retired_key_approval_rules() {
    use louiselm_skills::trust::Role;
    let history = History::new();
    let store = history.fixture.store();
    let mut successor = history.successor();
    let replacement = SshKey::generate(&history.fixture, "replacement");
    let mut trust = TrustStore::load(&store).unwrap().unwrap();
    let old = trust.key_for(Role::Primary).unwrap().clone();
    trust.retired.push(old);
    trust
        .keys
        .iter_mut()
        .find(|key| key.role == Role::Primary)
        .unwrap()
        .public_key = replacement.public_key();
    successor.signature =
        replacement.sign(ADMISSION_NAMESPACE, &successor.payload.canonical_bytes());
    assert!(
        admission::verify_record(&store, &successor, &trust).is_err(),
        "retired key cannot authorize unregistered old history"
    );
    trust
        .approved_admissions
        .insert(history.generation.to_string());
    admission::verify_record(&store, &successor, &trust).unwrap();
}

#[test]
fn legacy_history_may_not_follow_a_newer_schema() {
    let history = History::new();
    let store = history.fixture.store();
    let successor = history.successor();
    let mut old = admission::load(&store, &history.generation).unwrap();
    old.payload.sequence = 3;
    old.payload.predecessor = Some(successor.generation);
    old.generation = old.digest().to_string();
    old.signature = history
        .primary
        .sign(ADMISSION_NAMESPACE, &old.payload.canonical_bytes());
    // Replace only the private fixture's local current pointer; the real API
    // cannot create or activate this unsupported downgrade.
    fs::write(
        admission::record_path(&store, &history.generation),
        serde_json::to_vec(&old).unwrap(),
    )
    .unwrap();
    let error = admission::admit_transition(
        &store,
        &Policy::embedded(),
        &history.request(&NeverSign),
        &old.digest(),
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("generation/2 history cannot follow generation/3"),
        "{error}"
    );
}
