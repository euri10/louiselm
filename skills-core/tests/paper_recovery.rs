//! Paper recovery uses fixture secrets only; never a maintainer's phrase.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Test fixtures abort on setup failure and assert failures directly."
)]

mod support;

use louiselm_skills::{
    sshsig::SkPolicy,
    trust::{
        Role, TrustStore,
        paper::{
            self, PAPER_NAMESPACE, POSSESSION_NAMESPACE, PaperAuthorization, PaperChange,
            PaperError, PaperPhrase, ReplacementKey, ReplacementProof,
        },
    },
};
use support::{Fixture, SshKey};

#[test]
fn cli_refuses_development_authority_before_opening_a_secret_channel() {
    let fixture = Fixture::new();
    let absent = fixture.path("must-not-create");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_louiselm-skills"))
        .args(["recovery", "paper-recover", "--store"])
        .arg(&absent)
        .output()
        .expect("CLI runs");
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("trusted installed tool"));
    assert!(!absent.exists());
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_louiselm-skills"))
        .args([
            "recovery",
            "paper-recover",
            "--phrase",
            "never-echo-this-fixture",
        ])
        .output()
        .expect("CLI runs");
    assert_eq!(output.status.code(), Some(1));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("never-echo"));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("never-echo"));
}

fn phrase(byte: u8) -> PaperPhrase {
    // Fixed public test vectors, not real credentials.
    PaperPhrase::parse(
        &bip39::Mnemonic::from_entropy(&[byte; 32])
            .expect("fixture entropy")
            .to_string(),
    )
    .expect("fixture phrase")
}

fn enrolled(fixture: &Fixture) -> (TrustStore, SshKey) {
    let primary = SshKey::generate(fixture, "primary");
    let recovery = SshKey::generate(fixture, "recovery");
    let trust = TrustStore::bootstrap(
        &fixture.store(),
        "test/paper",
        &primary.public_key(),
        &recovery.public_key(),
        SkPolicy::none(),
        1,
    )
    .expect("bootstrap");
    let next = phrase(0);
    let change = PaperChange::new(&trust, vec![], &next).expect("enrollment plan");
    let signature = primary.sign(PAPER_NAMESPACE, &change.canonical_bytes());
    let trust = paper::apply(
        &fixture.store(),
        &change,
        PaperAuthorization::SigningKey {
            role: Role::Primary,
            signature: &signature,
        },
        &[],
        &next,
        2,
    )
    .expect("enroll paper recovery");
    (trust, primary)
}

#[test]
fn a_phrase_replaces_the_primary_and_is_consumed_in_the_same_change() {
    let fixture = Fixture::new();
    let (trust, _) = enrolled(&fixture);
    let replacement = SshKey::generate(&fixture, "replacement");
    let next = phrase(1);
    let change = PaperChange::new(
        &trust,
        vec![ReplacementKey {
            role: Role::Primary,
            public_key: replacement.public_key(),
        }],
        &next,
    )
    .expect("replacement plan");
    let proofs = [ReplacementProof {
        role: Role::Primary,
        signature: replacement.sign(POSSESSION_NAMESPACE, &change.canonical_bytes()),
    }];
    let recovered = paper::apply(
        &fixture.store(),
        &change,
        PaperAuthorization::Phrase(&phrase(0)),
        &proofs,
        &next,
        3,
    )
    .expect("recover with paper");
    assert_eq!(
        recovered.admission_key().expect("primary").public_key,
        replacement.public_key()
    );
    assert_eq!(recovered.sequence, trust.sequence + 1);
    assert!(
        paper::apply(
            &fixture.store(),
            &change,
            PaperAuthorization::Phrase(&phrase(0)),
            &proofs,
            &next,
            4
        )
        .is_err(),
        "no replay"
    );
    let rotate_phrase = PaperChange::new(&recovered, vec![], &phrase(2)).expect("fresh plan");
    assert!(
        paper::apply(
            &fixture.store(),
            &rotate_phrase,
            PaperAuthorization::Phrase(&phrase(0)),
            &[],
            &phrase(2),
            4
        )
        .is_err(),
        "consumed phrase fails even for a fresh plan"
    );
    paper::apply(
        &fixture.store(),
        &rotate_phrase,
        PaperAuthorization::Phrase(&next),
        &[],
        &phrase(2),
        4,
    )
    .expect("replacement phrase works");
}

#[test]
fn wrong_phrase_confirmation_and_missing_possession_leave_every_byte_unchanged() {
    let fixture = Fixture::new();
    let (trust, _) = enrolled(&fixture);
    let replacement = SshKey::generate(&fixture, "replacement");
    let next = phrase(1);
    let change = PaperChange::new(
        &trust,
        vec![ReplacementKey {
            role: Role::Primary,
            public_key: replacement.public_key(),
        }],
        &next,
    )
    .expect("replacement plan");
    let proofs = [ReplacementProof {
        role: Role::Primary,
        signature: replacement.sign(POSSESSION_NAMESPACE, &change.canonical_bytes()),
    }];
    let before = std::fs::read(fixture.path("store/trust/roles.json")).expect("state");
    assert!(
        paper::apply(
            &fixture.store(),
            &change,
            PaperAuthorization::Phrase(&phrase(7)),
            &proofs,
            &next,
            3
        )
        .is_err()
    );
    assert!(
        paper::apply(
            &fixture.store(),
            &change,
            PaperAuthorization::Phrase(&phrase(0)),
            &proofs,
            &phrase(2),
            3
        )
        .is_err()
    );
    assert!(
        paper::apply(
            &fixture.store(),
            &change,
            PaperAuthorization::Phrase(&phrase(0)),
            &[],
            &next,
            3
        )
        .is_err()
    );
    assert_eq!(
        std::fs::read(fixture.path("store/trust/roles.json")).expect("state"),
        before
    );
}

#[test]
fn phrases_are_checksummed_domain_scoped_and_redacted() {
    let secret = phrase(0);
    assert_ne!(secret.verifier("one"), secret.verifier("two"));
    assert_eq!(format!("{secret:?}"), "PaperPhrase([REDACTED])");
    assert!(matches!(
        PaperPhrase::parse("secret text never echoed"),
        Err(PaperError::InvalidPhrase)
    ));
    assert!(
        PaperPhrase::parse(&"abandon ".repeat(24)).is_err(),
        "invalid checksum"
    );
    let short = bip39::Mnemonic::from_entropy(&[0; 16])
        .expect("12 word fixture")
        .to_string();
    assert!(
        PaperPhrase::parse(&short).is_err(),
        "require full 256-bit entropy"
    );
    let generated = PaperPhrase::generate().expect("OS randomness");
    let exposed = generated.expose_secret();
    assert_eq!(exposed.split_whitespace().count(), 24);
    assert_eq!(
        PaperPhrase::parse(&exposed)
            .expect("round trip")
            .verifier("one"),
        generated.verifier("one")
    );
}

#[test]
fn stale_wrong_domain_and_confused_roles_never_change_authority() {
    let fixture = Fixture::new();
    let (trust, primary) = enrolled(&fixture);
    let next = phrase(1);
    let change = PaperChange::new(&trust, vec![], &next).expect("plan");
    let mut wrong_domain = change.clone();
    wrong_domain.trust_domain = "elsewhere".to_owned();
    let mut wrong_schema = change.clone();
    wrong_schema.schema = "ordinary approval".to_owned();
    let mut stale = change.clone();
    stale.predecessor = "other snapshot".to_owned();
    for invalid in [wrong_domain, wrong_schema, stale] {
        assert!(
            paper::apply(
                &fixture.store(),
                &invalid,
                PaperAuthorization::Phrase(&phrase(0)),
                &[],
                &next,
                3
            )
            .is_err()
        );
    }
    let wrong_namespace = primary.sign(POSSESSION_NAMESPACE, &change.canonical_bytes());
    assert!(
        paper::apply(
            &fixture.store(),
            &change,
            PaperAuthorization::SigningKey {
                role: Role::Primary,
                signature: &wrong_namespace
            },
            &[],
            &next,
            3
        )
        .is_err()
    );
    let signature = primary.sign(PAPER_NAMESPACE, &change.canonical_bytes());
    assert!(
        paper::apply(
            &fixture.store(),
            &change,
            PaperAuthorization::SigningKey {
                role: Role::Recovery,
                signature: &signature
            },
            &[],
            &next,
            3
        )
        .is_err()
    );
    assert!(
        PaperChange::new(
            &trust,
            vec![ReplacementKey {
                role: Role::Recovery,
                public_key: "replacement".to_owned()
            }],
            &next
        )
        .is_err()
    );
    assert_eq!(
        TrustStore::load(&fixture.store()).expect("state"),
        Some(trust)
    );
}

#[test]
fn consumed_phrases_and_retired_keys_cannot_be_reenrolled() {
    let fixture = Fixture::new();
    let (trust, primary) = enrolled(&fixture);
    let replacement = SshKey::generate(&fixture, "replacement");
    let next = phrase(1);
    let change = PaperChange::new(
        &trust,
        vec![ReplacementKey {
            role: Role::Primary,
            public_key: replacement.public_key(),
        }],
        &next,
    )
    .expect("plan");
    let proof = ReplacementProof {
        role: Role::Primary,
        signature: replacement.sign(POSSESSION_NAMESPACE, &change.canonical_bytes()),
    };
    let recovered = paper::apply(
        &fixture.store(),
        &change,
        PaperAuthorization::Phrase(&phrase(0)),
        &[proof],
        &next,
        3,
    )
    .expect("recovery");
    assert!(PaperChange::new(&recovered, vec![], &phrase(0)).is_err());
    assert!(PaperChange::new(&recovered, vec![], &next).is_err());
    assert!(
        PaperChange::new(
            &recovered,
            vec![ReplacementKey {
                role: Role::Primary,
                public_key: primary.public_key()
            }],
            &phrase(2)
        )
        .is_err()
    );
    let encoded = serde_json::to_string(&recovered).expect("public state");
    assert!(!encoded.contains(&*phrase(0).expose_secret()));
    assert!(!encoded.contains(&*next.expose_secret()));
}

#[test]
fn hardware_policy_is_inherited_and_cannot_be_downgraded_by_paper() {
    let fixture = Fixture::new();
    let (mut trust, _) = enrolled(&fixture);
    trust
        .keys
        .iter_mut()
        .find(|key| key.role == Role::Primary)
        .expect("primary")
        .sk_policy = SkPolicy::require_presence_and_verification();
    std::fs::write(
        fixture.path("store/trust/roles.json"),
        serde_json::to_vec(&trust).expect("JSON"),
    )
    .expect("hardware-policy fixture");
    let software = SshKey::generate(&fixture, "software");
    let change = PaperChange::new(
        &trust,
        vec![ReplacementKey {
            role: Role::Primary,
            public_key: software.public_key(),
        }],
        &phrase(1),
    )
    .expect("plan");
    let proof = ReplacementProof {
        role: Role::Primary,
        signature: software.sign(POSSESSION_NAMESPACE, &change.canonical_bytes()),
    };
    assert!(matches!(
        paper::apply(
            &fixture.store(),
            &change,
            PaperAuthorization::Phrase(&phrase(0)),
            &[proof],
            &phrase(1),
            3
        ),
        Err(PaperError::Signature(
            louiselm_skills::sshsig::SignatureError::NotHardwareBacked { .. }
        ))
    ));
    assert_eq!(
        TrustStore::load(&fixture.store()).expect("state"),
        Some(trust)
    );
}

#[test]
fn competing_paper_changes_apply_at_most_once() {
    let fixture = Fixture::new();
    let (trust, _) = enrolled(&fixture);
    let first = PaperChange::new(&trust, vec![], &phrase(1)).expect("first plan");
    let second = PaperChange::new(&trust, vec![], &phrase(2)).expect("competing plan");
    let barrier = std::sync::Barrier::new(2);
    let store = fixture.store();
    let results = std::thread::scope(|scope| {
        let handles = [(first, 1), (second, 2)].map(|(change, byte)| {
            let barrier = &barrier;
            let store = &store;
            scope.spawn(move || {
                barrier.wait();
                paper::apply(
                    store,
                    &change,
                    PaperAuthorization::Phrase(&phrase(0)),
                    &[],
                    &phrase(byte),
                    3,
                )
            })
        });
        handles.map(|handle| handle.join().expect("thread"))
    });
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        TrustStore::load(&store)
            .expect("state")
            .expect("enrolled")
            .sequence,
        trust.sequence + 1
    );
}

#[test]
fn malformed_recovery_state_fails_closed() {
    let fixture = Fixture::new();
    let (trust, _) = enrolled(&fixture);
    let value = serde_json::to_value(&trust).unwrap();
    for (field, bad) in [
        ("schema", serde_json::json!("louiselm.skills.trust/999")),
        ("paper_verifier", serde_json::json!("not-a-digest")),
        (
            "retired_paper_verifiers",
            serde_json::json!([trust.paper_verifier]),
        ),
        ("unknown_authority", serde_json::json!(true)),
    ] {
        let mut invalid = value.clone();
        invalid[field] = bad;
        std::fs::write(
            fixture.path("store/trust/roles.json"),
            serde_json::to_vec(&invalid).unwrap(),
        )
        .unwrap();
        assert!(
            TrustStore::load(&fixture.store()).is_err(),
            "refuse {field}"
        );
    }
}

#[test]
fn paper_recovery_preserves_history_but_old_keys_cannot_make_new_admissions() {
    use louiselm_skills::{
        Policy,
        admission::{self, AdmissionRequest},
        signer::SshKeygenSigner,
    };
    let fixture = Fixture::new();
    let (_, primary) = enrolled(&fixture);
    let signer = SshKeygenSigner::new(primary.private_key_path());
    let request = AdmissionRequest {
        members: vec![],
        view_roots: std::collections::BTreeMap::default(),
        signer: &signer,
        admitted_at_ms: 3,
    };
    let record = admission::admit(&fixture.store(), &Policy::embedded(), &request).unwrap();
    let trust = TrustStore::load(&fixture.store()).unwrap().unwrap();
    let replacement = SshKey::generate(&fixture, "replacement");
    let next = phrase(1);
    let change = PaperChange::new(
        &trust,
        vec![ReplacementKey {
            role: Role::Primary,
            public_key: replacement.public_key(),
        }],
        &next,
    )
    .unwrap();
    let proof = ReplacementProof {
        role: Role::Primary,
        signature: replacement.sign(POSSESSION_NAMESPACE, &change.canonical_bytes()),
    };
    let trust = paper::apply(
        &fixture.store(),
        &change,
        PaperAuthorization::Phrase(&phrase(0)),
        &[proof],
        &next,
        4,
    )
    .unwrap();
    admission::verify_record(&fixture.store(), &record, &trust).unwrap();
    assert!(admission::admit(&fixture.store(), &Policy::embedded(), &request).is_err());
    let replacement_signer = SshKeygenSigner::new(replacement.private_key_path());
    let request = AdmissionRequest {
        signer: &replacement_signer,
        ..request
    };
    admission::admit(&fixture.store(), &Policy::embedded(), &request).unwrap();
}

#[test]
fn both_signing_roles_require_exact_distinct_possession_proofs() {
    let fixture = Fixture::new();
    let (mut trust, _) = enrolled(&fixture);
    // The development fixture already has a software release role. Production
    // has strict assertion policy and is tested separately for refusal.
    let mut release = trust.admission_key().unwrap().clone();
    release.role = Role::Release;
    release.public_key = SshKey::generate(&fixture, "old-release").public_key();
    trust.keys.push(release);
    std::fs::write(
        fixture.path("store/trust/roles.json"),
        serde_json::to_vec(&trust).unwrap(),
    )
    .unwrap();
    let primary = SshKey::generate(&fixture, "new-primary");
    let release = SshKey::generate(&fixture, "new-release");
    let keys = vec![
        ReplacementKey {
            role: Role::Primary,
            public_key: primary.public_key(),
        },
        ReplacementKey {
            role: Role::Release,
            public_key: release.public_key(),
        },
    ];
    let next = phrase(1);
    let change = PaperChange::new(&trust, keys, &next).unwrap();
    let proofs = vec![
        ReplacementProof {
            role: Role::Primary,
            signature: primary.sign(POSSESSION_NAMESPACE, &change.canonical_bytes()),
        },
        ReplacementProof {
            role: Role::Release,
            signature: release.sign(POSSESSION_NAMESPACE, &change.canonical_bytes()),
        },
    ];
    let mut duplicate = proofs.clone();
    duplicate[1].role = Role::Primary;
    let mut wrong_namespace = proofs.clone();
    wrong_namespace[1].signature = release.sign(PAPER_NAMESPACE, &change.canonical_bytes());
    let mut changed_plan = change.clone();
    changed_plan.next_verifier = phrase(2).verifier(&trust.trust_domain);
    for (plan, proof, confirmed) in [
        (&change, &duplicate, &next),
        (&change, &wrong_namespace, &next),
        (&changed_plan, &proofs, &phrase(2)),
    ] {
        assert!(
            paper::apply(
                &fixture.store(),
                plan,
                PaperAuthorization::Phrase(&phrase(0)),
                proof,
                confirmed,
                4
            )
            .is_err()
        );
        assert_eq!(
            TrustStore::load(&fixture.store()).unwrap(),
            Some(trust.clone())
        );
    }
    let recovered = paper::apply(
        &fixture.store(),
        &change,
        PaperAuthorization::Phrase(&phrase(0)),
        &proofs,
        &next,
        4,
    )
    .unwrap();
    assert_eq!(
        recovered.admission_key().unwrap().public_key,
        primary.public_key()
    );
    assert_eq!(
        recovered.key_for(Role::Release).unwrap().public_key,
        release.public_key()
    );
    assert_eq!(
        recovered.key_for(Role::Recovery),
        trust.key_for(Role::Recovery)
    );
}

#[cfg(target_os = "linux")]
#[test]
fn failed_paper_publication_preserves_the_old_phrase_and_all_state() {
    const MARKER: &str = "LOUISELM_TEST_PAPER_WRITE_LIMIT";
    if let Some(path) = std::env::var_os(MARKER) {
        let store = louiselm_skills::Store::open(std::path::Path::new(&path)).unwrap();
        let trust = TrustStore::load(&store).unwrap().unwrap();
        let next = phrase(1);
        let change = PaperChange::new(&trust, vec![], &next).unwrap();
        assert!(matches!(
            paper::apply(
                &store,
                &change,
                PaperAuthorization::Phrase(&phrase(0)),
                &[],
                &next,
                4
            ),
            Err(PaperError::Trust(
                louiselm_skills::trust::TrustError::Io { .. }
            ))
        ));
        assert_eq!(TrustStore::load(&store).unwrap(), Some(trust));
        return;
    }
    let fixture = Fixture::new();
    let (mut trust, _) = enrolled(&fixture);
    trust.retired = vec![trust.admission_key().unwrap().clone(); 100];
    std::fs::write(
        fixture.path("store/trust/roles.json"),
        serde_json::to_vec(&trust).unwrap(),
    )
    .unwrap();
    let before = std::fs::read(fixture.path("store/trust/roles.json")).unwrap();
    let child = std::process::Command::new("/bin/sh")
        .args([
            "-c",
            "trap '' XFSZ; ulimit -f 4; exec \"$@\"",
            "paper-write-limit",
        ])
        .arg(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "failed_paper_publication_preserves_the_old_phrase_and_all_state",
            "--nocapture",
        ])
        .env(MARKER, fixture.store_root())
        .output()
        .unwrap();
    assert!(
        child.status.success(),
        "{}",
        String::from_utf8_lossy(&child.stdout)
    );
    assert_eq!(
        std::fs::read(fixture.path("store/trust/roles.json")).unwrap(),
        before
    );
    let next = phrase(1);
    let change = PaperChange::new(&trust, vec![], &next).unwrap();
    paper::apply(
        &fixture.store(),
        &change,
        PaperAuthorization::Phrase(&phrase(0)),
        &[],
        &next,
        4,
    )
    .expect("failed write did not consume the old phrase");
}

#[test]
fn exhaustion_refuses_paper_planning_and_application_without_consuming_a_phrase() {
    let fixture = Fixture::new();
    let (mut trust, _) = enrolled(&fixture);
    let next = phrase(1);
    let change = PaperChange::new(&trust, vec![], &next).unwrap();
    trust.sequence = u64::MAX;
    std::fs::write(
        fixture.path("store/trust/roles.json"),
        serde_json::to_vec(&trust).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        PaperChange::new(&trust, vec![], &next),
        Err(PaperError::Trust(
            louiselm_skills::trust::TrustError::SequenceExhausted
        ))
    ));
    assert!(matches!(
        paper::apply(
            &fixture.store(),
            &change,
            PaperAuthorization::Phrase(&phrase(0)),
            &[],
            &next,
            4
        ),
        Err(PaperError::Trust(
            louiselm_skills::trust::TrustError::SequenceExhausted
        ))
    ));
    assert_eq!(TrustStore::load(&fixture.store()).unwrap(), Some(trust));
}
