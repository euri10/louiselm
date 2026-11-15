//! Standalone profile Admission binds the Dossier the operator reviewed.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Fixture setup and behavioral expectations fail the test directly."
)]

mod support;

use std::process::{Command, Output};

use louiselm_skills::{
    Digest, Policy, ReviewDepth, SshKeygenSigner, TrustStore,
    admission::{self, AdmissionError, AdmissionMember, AdmissionRequest},
    sshsig::SkPolicy,
    witness::GitWitness,
};
use serde_json::{Value, json};
use support::{Fixture, SshKey, write_file};

fn profile(fixture: &Fixture, name: &str) -> Digest {
    let path = fixture.candidate(name);
    write_file(
        &path.join("PROFILE.md"),
        &format!("---\ntype: louiselm/operator-profile\n---\n{name}\n"),
    );
    fixture.capture(&path).unwrap().0.digest
}

fn review(fixture: &Fixture, digest: &Digest) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_louiselm-skills"))
        .args([
            "dossier",
            &digest.to_string(),
            "--review-depth",
            "read",
            "--robot-json",
        ])
        .env("LOUISELM_SKILLS_STORE", fixture.store_root())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn review_digest(value: &Value) -> Digest {
    Digest::parse(value["review_digest"].as_str().unwrap()).unwrap()
}

fn admit_cli(fixture: &Fixture, key: &SshKey, profile: &Digest, review: Option<&Digest>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_louiselm-skills"));
    command
        .args([
            "generation",
            "admit",
            "--member",
            &format!("{profile}:read=codex"),
            "--key",
        ])
        .arg(key.private_key_path())
        .arg("--registry")
        .arg(fixture.path("registry"))
        .arg("--robot-json")
        .env("LOUISELM_SKILLS_STORE", fixture.store_root());
    if let Some(review) = review {
        command.args(["--reviewed-profile-dossier", &review.to_string()]);
    }
    command.output().unwrap()
}

struct NoSigning;

impl louiselm_skills::Signer for NoSigning {
    fn sign(&self, _: &str, _: &[u8]) -> Result<String, louiselm_skills::signer::SignerError> {
        Err(louiselm_skills::signer::SignerError::Failed(
            "signer was called".into(),
        ))
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "The scenario follows one review across activation, refusal, and fresh signing."
)]
fn standalone_refuses_stale_profile_review_and_accepts_fresh_review() {
    let fixture = Fixture::new();
    let primary = SshKey::generate(&fixture, "primary");
    let release = SshKey::generate(&fixture, "release");
    let store = fixture.store();
    let policy = Policy::embedded();
    TrustStore::bootstrap(
        &store,
        "louiselm/skills",
        &primary.public_key(),
        &release.public_key(),
        SkPolicy::none(),
        1,
    )
    .unwrap();
    let a = profile(&fixture, "profile-a");
    let b = profile(&fixture, "profile-b");
    let c = profile(&fixture, "profile-c");
    let signer = SshKeygenSigner::new(primary.private_key_path());
    let request = |digest: &Digest| AdmissionRequest {
        members: vec![AdmissionMember {
            package: digest.clone(),
            depth: ReviewDepth::Read,
            agents: vec!["codex".into()],
        }],
        signer: &signer,
        admitted_at_ms: 2,
        configured_agents: Some(vec!["codex".into()]),
    };
    let witness = GitWitness::new(
        &fixture.witness_remote(),
        "generations",
        &fixture.path("witness"),
    );
    let first_review = review_digest(&review(&fixture, &a));
    let first = admission::admit(&store, &policy, &request(&a), Some(&first_review)).unwrap();
    admission::witness(&store, &first.digest(), &witness, 3).unwrap();
    admission::activate(&store, &first.digest(), 4).unwrap();
    let stale = review(&fixture, &c);
    assert_eq!(stale["diff"]["base_digest"], a.to_string());
    let b_review = review_digest(&review(&fixture, &b));
    let second = admission::admit(&store, &policy, &request(&b), Some(&b_review)).unwrap();
    admission::witness(&store, &second.digest(), &witness, 5).unwrap();
    admission::activate(&store, &second.digest(), 6).unwrap();

    let guarded = AdmissionRequest {
        signer: &NoSigning,
        ..request(&c)
    };
    assert!(matches!(
        admission::admit(&store, &policy, &guarded, None),
        Err(AdmissionError::ProfileReviewRequired)
    ));
    let stale_digest = review_digest(&stale);
    let error = admission::admit(&store, &policy, &guarded, Some(&stale_digest)).unwrap_err();
    assert!(matches!(error, AdmissionError::ProfileReviewChanged));
    assert_eq!(
        admission::current(&store).unwrap().unwrap().generation,
        second.generation
    );
    write_file(
        &fixture.path("registry/agents.json"),
        &json!({"schema":"louiselm.launch.registry/1", "entries":[{
            "id":"codex", "provider":"test", "runtime_id":"test", "arguments":[], "environment":{}
        }]})
        .to_string(),
    );
    for (review, expected) in [
        (None, "requires the reviewed profile Dossier digest"),
        (Some(&stale_digest), "profile Dossier changed since review"),
    ] {
        let output = admit_cli(&fixture, &primary, &c, review);
        assert!(!output.status.success());
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains(expected), "{stderr}");
        assert!(!stderr.contains("profile-c"));
        assert!(!stderr.contains(fixture.path("profile-c").to_str().unwrap()));
        assert_eq!(output.stdout, Vec::<u8>::new());
    }
    let fresh = review(&fixture, &c);
    assert_eq!(fresh["diff"]["base_digest"], b.to_string());
    let fresh_digest = review_digest(&fresh);
    let mut different_depth = request(&c);
    different_depth.signer = &NoSigning;
    different_depth.members[0].depth = ReviewDepth::Skimmed;
    assert!(matches!(
        admission::admit(&store, &policy, &different_depth, Some(&fresh_digest)),
        Err(AdmissionError::ProfileReviewChanged)
    ));
    let mut changed_policy = Policy::embedded_bytes().to_vec();
    changed_policy.push(b'\n');
    let changed_policy = Policy::from_bytes(&changed_policy).unwrap();
    assert!(matches!(
        admission::admit(&store, &changed_policy, &guarded, Some(&fresh_digest)),
        Err(AdmissionError::ProfileReviewChanged)
    ));
    let output = admit_cli(&fixture, &primary, &c, Some(&fresh_digest));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("profile-c"));
    let cli_record: louiselm_skills::GenerationRecord =
        serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        cli_record.payload.members[0].dossier_digest,
        fresh_digest.to_string()
    );
    let record = admission::admit(&store, &policy, &request(&c), Some(&fresh_digest)).unwrap();
    assert_eq!(
        record.payload.members[0].dossier_digest,
        fresh["review_digest"].as_str().unwrap()
    );
}
