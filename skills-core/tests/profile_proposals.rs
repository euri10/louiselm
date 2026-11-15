//! Profile proposals are reviewed against active supply and do not change slots.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Fixture setup and behavioral expectations fail the test directly."
)]

mod support;

use std::{fs, os::unix::fs::PermissionsExt, process::Command};

use louiselm_skills::{
    Digest, Policy, ReviewDepth, SshKeygenSigner, TrustStore,
    admission::{self, AdmissionError, AdmissionMember, AdmissionRequest},
    dossier::{Dossier, DossierRequest},
    sshsig::SkPolicy,
    witness::GitWitness,
};
use serde_json::{Value, json};
use support::{Fixture, SshKey, write_file};

const OLD: &str =
    "---\ntype: louiselm/operator-profile\n---\n\nUse plain English.\nKeep answers short.\n";
const PROPOSED: &str = "---\ntype: louiselm/operator-profile\n---\n\nUse French.\nKeep answers short.\nPrefer local tools.\n";

fn profile(fixture: &Fixture, name: &str, text: &str) -> Digest {
    let path = fixture.candidate(name);
    write_file(&path.join("PROFILE.md"), text);
    fixture.capture(&path).unwrap().0.digest
}

fn cli(fixture: &Fixture, args: &[&str]) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_louiselm-skills"))
        .args(args)
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

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "One scenario follows the active profile through review, signing, and unchanged slots."
)]
fn a_profile_proposal_diffs_the_current_profile_and_binds_the_same_dossier() {
    let fixture = Fixture::new();
    let primary = SshKey::generate(&fixture, "primary");
    let release = SshKey::generate(&fixture, "release");
    let store = fixture.store();
    TrustStore::bootstrap(
        &store,
        "louiselm/skills",
        &primary.public_key(),
        &release.public_key(),
        SkPolicy::none(),
        1,
    )
    .unwrap();
    let old = profile(&fixture, "old", OLD);
    let signer = SshKeygenSigner::new(primary.private_key_path());
    let members = |digest: &Digest| {
        vec![AdmissionMember {
            package: digest.clone(),
            depth: ReviewDepth::Read,
            agents: vec!["codex".into()],
        }]
    };
    let admit = |digest: &Digest| AdmissionRequest {
        members: members(digest),
        signer: &signer,
        admitted_at_ms: 2,
        configured_agents: Some(vec!["codex".into()]),
    };
    let old_review = support::reviewed_profile(&store, &old, None);
    let initial =
        admission::admit(&store, &Policy::embedded(), &admit(&old), Some(&old_review)).unwrap();
    let witness = GitWitness::new(
        &fixture.witness_remote(),
        "generations",
        &fixture.path("witness"),
    );
    admission::witness(&store, &initial.digest(), &witness, 3).unwrap();
    admission::activate(&store, &initial.digest(), 4).unwrap();

    let slot = fixture.path("home/AGENTS.md");
    write_file(&fixture.path("registry/agents.json"), &json!({"schema":"louiselm.launch.registry/1", "entries":[{
        "id":"codex", "provider":"test", "runtime_id":"test", "arguments":[], "environment":{}, "always_on_path":slot
    }]}).to_string());
    cli(
        &fixture,
        &[
            "view",
            "materialize",
            "--registry",
            fixture.path("registry").to_str().unwrap(),
            "--robot-json",
        ],
    );
    assert_eq!(fs::read_to_string(&slot).unwrap(), OLD);

    let proposed = profile(&fixture, "proposed", PROPOSED);
    let value = cli(
        &fixture,
        &[
            "dossier",
            &proposed.to_string(),
            "--review-depth",
            "read",
            "--robot-json",
        ],
    );
    assert_eq!(value["inspection"]["package_kind"], "operator_profile");
    assert_eq!(value["diff"]["base_digest"], old.to_string());
    let lines = value["diff"]["entries"][0]["lines"].as_array().unwrap();
    assert!(
        lines
            .iter()
            .any(|line| line["kind"] == "removed" && line["text"] == "Use plain English.")
    );
    assert!(
        lines
            .iter()
            .any(|line| line["kind"] == "added" && line["text"] == "Use French.")
    );
    assert!(
        lines
            .iter()
            .any(|line| line["kind"] == "added" && line["text"] == "Prefer local tools.")
    );
    let wrong = profile(
        &fixture,
        "unrelated",
        "---\ntype: louiselm/operator-profile\n---\nUnrelated.\n",
    );
    let refused = Command::new(env!("CARGO_BIN_EXE_louiselm-skills"))
        .args([
            "dossier",
            &proposed.to_string(),
            "--against",
            &wrong.to_string(),
        ])
        .env("LOUISELM_SKILLS_STORE", fixture.store_root())
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr)
            .contains("base must be the current admitted profile")
    );
    assert_eq!(fs::read_to_string(&slot).unwrap(), OLD);

    let expected = Dossier::build(
        &store,
        &Policy::embedded(),
        &DossierRequest::new(&proposed)
            .against(&old)
            .with_review_depth(ReviewDepth::Read),
    )
    .unwrap();
    let next = admission::admit(
        &store,
        &Policy::embedded(),
        &admit(&proposed),
        Some(&expected.portable_digest()),
    )
    .unwrap();
    assert_eq!(
        next.payload.members[0].dossier_digest,
        expected.portable_digest().to_string()
    );
    assert_eq!(fs::read_to_string(&slot).unwrap(), OLD);

    let old_file = store
        .root()
        .join("packages")
        .join(old.directory_name())
        .join("files/PROFILE.md");
    fs::set_permissions(&old_file, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(&old_file, "changed after Admission").unwrap();
    let error = admission::admit(
        &store,
        &Policy::embedded(),
        &admit(&proposed),
        Some(&expected.portable_digest()),
    )
    .unwrap_err();
    assert!(
        matches!(error, admission::AdmissionError::Chain(message) if message == "profile review predecessor package is damaged")
    );
}

#[test]
fn the_first_profile_is_reviewed_as_added_content() {
    let fixture = Fixture::new();
    let proposed = profile(&fixture, "first", PROPOSED);
    let value = cli(
        &fixture,
        &["dossier", &proposed.to_string(), "--robot-json"],
    );
    assert!(value["diff"]["base_digest"].is_null());
    let entry = &value["diff"]["entries"][0];
    assert_eq!(entry["change"], "added");
    let lines = entry["lines"].as_array().unwrap();
    assert!(
        lines
            .iter()
            .any(|line| line["kind"] == "added" && line["text"] == "Use French.")
    );
    assert!(
        lines
            .iter()
            .any(|line| line["kind"] == "added" && line["text"] == "Prefer local tools.")
    );

    let hostile = profile(
        &fixture,
        "hostile",
        "---\ntype: louiselm/operator-profile\n---\nUnsafe \u{1b}[2J text.\n",
    );
    let value = cli(&fixture, &["dossier", &hostile.to_string(), "--robot-json"]);
    for line in value["diff"]["entries"][0]["lines"].as_array().unwrap() {
        assert!(!line["text"].as_str().unwrap().chars().any(char::is_control));
    }
}

struct NoSigning;
impl louiselm_skills::Signer for NoSigning {
    fn sign(&self, _: &str, _: &[u8]) -> Result<String, louiselm_skills::signer::SignerError> {
        Err(louiselm_skills::signer::SignerError::Failed(
            "retry must not sign".into(),
        ))
    }
}

#[test]
fn linked_profile_admission_can_be_recovered_after_it_becomes_current() {
    let fixture = Fixture::new();
    let primary = SshKey::generate(&fixture, "primary");
    let release = SshKey::generate(&fixture, "release");
    let store = fixture.store();
    TrustStore::bootstrap(
        &store,
        "louiselm/skills",
        &primary.public_key(),
        &release.public_key(),
        SkPolicy::none(),
        1,
    )
    .unwrap();
    let proposed = profile(&fixture, "first", PROPOSED);
    let reviewed = Dossier::build(
        &store,
        &Policy::embedded(),
        &DossierRequest::new(&proposed).with_review_depth(ReviewDepth::Read),
    )
    .unwrap()
    .portable_digest();
    let signer = SshKeygenSigner::new(primary.private_key_path());
    let request = AdmissionRequest {
        members: vec![AdmissionMember {
            package: proposed,
            depth: ReviewDepth::Read,
            agents: vec!["codex".into()],
        }],
        signer: &signer,
        admitted_at_ms: 2,
        configured_agents: Some(vec!["codex".into()]),
    };
    let operation = "12345678-1234-4234-8234-123456789abc";
    assert!(matches!(
        admission::admit_linked(&store, &Policy::embedded(), &request, operation, None),
        Err(admission::AdmissionError::ProfileReviewRequired)
    ));
    let record = admission::admit_linked(
        &store,
        &Policy::embedded(),
        &request,
        operation,
        Some(&reviewed),
    )
    .unwrap();
    let witness = GitWitness::new(
        &fixture.witness_remote(),
        "generations",
        &fixture.path("witness"),
    );
    admission::witness(&store, &record.digest(), &witness, 3).unwrap();
    admission::activate(&store, &record.digest(), 4).unwrap();
    let changed = AdmissionRequest {
        members: vec![AdmissionMember {
            package: request.members[0].package.clone(),
            depth: ReviewDepth::Skimmed,
            agents: vec!["codex".into()],
        }],
        signer: &NoSigning,
        ..request
    };
    let error = admission::admit_linked(&store, &Policy::embedded(), &changed, operation, None)
        .unwrap_err();
    assert!(
        matches!(error, admission::AdmissionError::Chain(message) if message == "linked Admission request changed")
    );
    let retry = admission::admit_linked(
        &store,
        &Policy::embedded(),
        &AdmissionRequest {
            signer: &NoSigning,
            members: vec![AdmissionMember {
                depth: ReviewDepth::Read,
                ..changed.members[0].clone()
            }],
            ..changed
        },
        operation,
        None,
    )
    .unwrap();
    assert_eq!(retry.generation, record.generation);
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "One scenario demonstrates the stale review and fresh-review signing boundary."
)]
fn profile_review_must_be_repeated_if_the_active_profile_changes_before_signing() {
    let fixture = Fixture::new();
    let primary = SshKey::generate(&fixture, "primary");
    let release = SshKey::generate(&fixture, "release");
    let store = fixture.store();
    TrustStore::bootstrap(
        &store,
        "louiselm/skills",
        &primary.public_key(),
        &release.public_key(),
        SkPolicy::none(),
        1,
    )
    .unwrap();
    let old = profile(&fixture, "old", OLD);
    let middle = profile(
        &fixture,
        "middle",
        "---\ntype: louiselm/operator-profile\n---\nA different active profile.\n",
    );
    let proposed = profile(&fixture, "proposal", PROPOSED);
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
    let old_review = support::reviewed_profile(&store, &old, None);
    let first = admission::admit(
        &store,
        &Policy::embedded(),
        &request(&old),
        Some(&old_review),
    )
    .unwrap();
    admission::witness(&store, &first.digest(), &witness, 3).unwrap();
    admission::activate(&store, &first.digest(), 4).unwrap();
    let reviewed = cli(
        &fixture,
        &[
            "dossier",
            &proposed.to_string(),
            "--review-depth",
            "read",
            "--robot-json",
        ],
    );
    assert_eq!(reviewed["diff"]["base_digest"], old.to_string());
    let reviewed_digest = Digest::parse(reviewed["review_digest"].as_str().unwrap()).unwrap();
    let middle_review = support::reviewed_profile(&store, &middle, Some(&old));
    let second = admission::admit(
        &store,
        &Policy::embedded(),
        &request(&middle),
        Some(&middle_review),
    )
    .unwrap();
    admission::witness(&store, &second.digest(), &witness, 5).unwrap();
    admission::activate(&store, &second.digest(), 6).unwrap();
    let operation = "12345678-1234-4234-8234-123456789abc";
    let error = admission::admit_linked(
        &store,
        &Policy::embedded(),
        &request(&proposed),
        operation,
        Some(&reviewed_digest),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        admission::AdmissionError::ProfileReviewChanged
    ));
    assert_eq!(
        admission::verify_linked(
            &store,
            &Policy::embedded(),
            "louiselm/skills",
            operation,
            &[proposed.to_string()],
            &["codex".into()],
        )
        .unwrap(),
        None,
    );
    assert_eq!(
        admission::current(&store).unwrap().unwrap().generation,
        second.generation
    );
    let updated = cli(
        &fixture,
        &[
            "dossier",
            &proposed.to_string(),
            "--review-depth",
            "read",
            "--robot-json",
        ],
    );
    assert_eq!(updated["diff"]["base_digest"], middle.to_string());
    let updated_digest = Digest::parse(updated["review_digest"].as_str().unwrap()).unwrap();
    let record = admission::admit_linked(
        &store,
        &Policy::embedded(),
        &request(&proposed),
        operation,
        Some(&updated_digest),
    )
    .unwrap();
    assert_eq!(
        record.payload.members[0].dossier_digest,
        updated_digest.to_string()
    );
}

#[test]
fn a_profile_batch_can_preserve_a_skill_with_the_same_scope() {
    let fixture = Fixture::new();
    let primary = SshKey::generate(&fixture, "primary");
    let release = SshKey::generate(&fixture, "release");
    let store = fixture.store();
    TrustStore::bootstrap(
        &store,
        "louiselm/skills",
        &primary.public_key(),
        &release.public_key(),
        SkPolicy::none(),
        1,
    )
    .unwrap();
    let candidate = fixture.candidate("skill");
    write_file(
        &candidate.join("SKILL.md"),
        "---\nname: stable\ndescription: Stable skill.\n---\nBody.\n",
    );
    let skill = fixture.capture(&candidate).unwrap().0.digest;
    let signer = SshKeygenSigner::new(primary.private_key_path());
    let member = |package: &Digest| AdmissionMember {
        package: package.clone(),
        depth: ReviewDepth::Read,
        agents: vec!["codex".into()],
    };
    let first = admission::admit(
        &store,
        &Policy::embedded(),
        &AdmissionRequest {
            members: vec![member(&skill)],
            signer: &signer,
            admitted_at_ms: 2,
            configured_agents: None,
        },
        None,
    )
    .unwrap();
    let witness = GitWitness::new(
        &fixture.witness_remote(),
        "generations",
        &fixture.path("witness"),
    );
    admission::witness(&store, &first.digest(), &witness, 3).unwrap();
    admission::activate(&store, &first.digest(), 4).unwrap();
    let proposed = profile(&fixture, "proposal", PROPOSED);
    let reviewed = Dossier::build(
        &store,
        &Policy::embedded(),
        &DossierRequest::new(&proposed).with_review_depth(ReviewDepth::Read),
    )
    .unwrap()
    .portable_digest();
    let request = AdmissionRequest {
        members: vec![member(&proposed), member(&skill)],
        signer: &signer,
        admitted_at_ms: 5,
        configured_agents: Some(vec!["codex".into()]),
    };
    let operation = "12345678-1234-4234-8234-123456789abc";
    let record = admission::admit_linked(
        &store,
        &Policy::embedded(),
        &request,
        operation,
        Some(&reviewed),
    )
    .unwrap();
    let mut packages = [proposed.to_string(), skill.to_string()];
    packages.sort();
    assert_eq!(
        record.payload.member_digests().as_slice(),
        packages.as_slice()
    );
    assert_eq!(
        admission::verify_linked(
            &store,
            &Policy::embedded(),
            "louiselm/skills",
            operation,
            &packages,
            &["codex".into()]
        )
        .unwrap(),
        Some(record.generation)
    );
}

#[test]
fn a_linked_profile_update_cannot_silently_drop_existing_skills() {
    let fixture = Fixture::new();
    let primary = SshKey::generate(&fixture, "primary");
    let release = SshKey::generate(&fixture, "release");
    let store = fixture.store();
    TrustStore::bootstrap(
        &store,
        "louiselm/skills",
        &primary.public_key(),
        &release.public_key(),
        SkPolicy::none(),
        1,
    )
    .unwrap();
    let old = profile(&fixture, "old-profile", OLD);
    let skill_dir = fixture.candidate("skill");
    write_file(
        &skill_dir.join("SKILL.md"),
        "---\nname: narrow\ndescription: Narrow skill.\n---\nSkill.\n",
    );
    let skill = fixture.capture(&skill_dir).unwrap().0.digest;
    let signer = SshKeygenSigner::new(primary.private_key_path());
    let agents = vec!["claude".into(), "codex".into()];
    let initial = AdmissionRequest {
        members: vec![
            AdmissionMember {
                package: old,
                depth: ReviewDepth::Read,
                agents: agents.clone(),
            },
            AdmissionMember {
                package: skill.clone(),
                depth: ReviewDepth::Read,
                agents: vec!["codex".into()],
            },
        ],
        signer: &signer,
        admitted_at_ms: 2,
        configured_agents: Some(agents.clone()),
    };
    let old_review = support::reviewed_profile(&store, &initial.members[0].package, None);
    let record =
        admission::admit(&store, &Policy::embedded(), &initial, Some(&old_review)).unwrap();
    let witness = GitWitness::new(
        &fixture.witness_remote(),
        "generations",
        &fixture.path("witness"),
    );
    admission::witness(&store, &record.digest(), &witness, 3).unwrap();
    admission::activate(&store, &record.digest(), 4).unwrap();
    let proposed = profile(&fixture, "proposal", PROPOSED);
    let request = AdmissionRequest {
        members: vec![AdmissionMember {
            package: proposed.clone(),
            depth: ReviewDepth::Read,
            agents: agents.clone(),
        }],
        signer: &signer,
        admitted_at_ms: 5,
        configured_agents: Some(agents),
    };
    let error = admission::admit_linked(
        &store,
        &Policy::embedded(),
        &request,
        "12345678-1234-4234-8234-123456789abc",
        None,
    )
    .unwrap_err();
    assert!(matches!(error, AdmissionError::ProfileWouldChangeSkills));
    let widened = AdmissionRequest {
        members: vec![
            AdmissionMember {
                package: proposed,
                depth: ReviewDepth::Read,
                agents: request.members[0].agents.clone(),
            },
            AdmissionMember {
                package: skill,
                depth: ReviewDepth::Read,
                agents: request.members[0].agents.clone(),
            },
        ],
        ..request
    };
    let error = admission::admit_linked(
        &store,
        &Policy::embedded(),
        &widened,
        "12345678-1234-4234-8234-123456789abc",
        None,
    )
    .unwrap_err();
    assert!(matches!(error, AdmissionError::ProfileWouldChangeSkills));
    assert_eq!(
        admission::current(&store).unwrap().unwrap().generation,
        record.generation
    );
}
