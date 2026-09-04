//! The command line: the surface an operator and an Agent both drive.

mod support;

use std::{path::Path, process::Command};

use support::{Fixture, write_file};

const BINARY: &str = env!("CARGO_BIN_EXE_louiselm-skills");

struct Output {
    status: i32,
    stdout: String,
    stderr: String,
}

fn run(fixture: &Fixture, arguments: &[&str]) -> Output {
    let output = Command::new(BINARY)
        .args(arguments)
        .env("LOUISELM_SKILLS_STORE", fixture.store_root())
        .output()
        .expect("the tool runs");
    Output {
        status: output.status.code().expect("the tool exits normally"),
        stdout: String::from_utf8(output.stdout).expect("stdout is UTF-8"),
        stderr: String::from_utf8(output.stderr).expect("stderr is UTF-8"),
    }
}

fn skill_candidate(fixture: &Fixture, name: &str) -> std::path::PathBuf {
    let candidate = fixture.candidate(name);
    write_file(
        &candidate.join("SKILL.md"),
        "---\nname: demo\ndescription: A demonstration skill.\n---\n\nBody.\n",
    );
    candidate
}

fn package(fixture: &Fixture, candidate: &Path) -> String {
    let output = run(
        fixture,
        &[
            "package",
            candidate.to_str().expect("path is UTF-8"),
            "--captured-at",
            "1756800000000",
            "--robot-json",
        ],
    );
    assert_eq!(output.status, 0, "packaging failed: {}", output.stderr);
    let parsed: serde_json::Value =
        serde_json::from_str(&output.stdout).expect("robot output is JSON");
    parsed["digest"]
        .as_str()
        .expect("the digest is reported")
        .to_owned()
}

#[test]
fn packaging_reports_the_digest_and_whether_it_was_already_stored() {
    let fixture = Fixture::new();
    let candidate = skill_candidate(&fixture, "candidate");

    let first = run(
        &fixture,
        &[
            "package",
            candidate.to_str().expect("path is UTF-8"),
            "--captured-at",
            "1756800000000",
            "--robot-json",
        ],
    );
    let second = run(
        &fixture,
        &[
            "package",
            candidate.to_str().expect("path is UTF-8"),
            "--captured-at",
            "1756800000001",
            "--robot-json",
        ],
    );

    assert_eq!(first.status, 0);
    let first: serde_json::Value = serde_json::from_str(&first.stdout).expect("JSON");
    let second: serde_json::Value = serde_json::from_str(&second.stdout).expect("JSON");
    assert_eq!(first["outcome"], "created");
    assert_eq!(second["outcome"], "existing");
    assert_eq!(first["digest"], second["digest"]);
    assert_eq!(first["schema"], "louiselm.skills.package-result/1");
}

#[test]
fn a_refused_candidate_exits_nonzero_and_says_why() {
    let fixture = Fixture::new();
    let candidate = skill_candidate(&fixture, "candidate");
    std::fs::hard_link(candidate.join("SKILL.md"), candidate.join("alias.md"))
        .expect("hard link is creatable");

    let output = run(
        &fixture,
        &["package", candidate.to_str().expect("path is UTF-8")],
    );

    assert_eq!(output.status, 1);
    assert!(
        output.stderr.contains("hardlinked"),
        "stderr was: {}",
        output.stderr,
    );
    assert!(output.stdout.is_empty());
}

#[test]
fn a_dossier_is_available_to_a_human_and_to_an_agent() {
    let fixture = Fixture::new();
    let candidate = skill_candidate(&fixture, "candidate");
    let digest = package(&fixture, &candidate);

    let human = run(&fixture, &["dossier", &digest]);
    let machine = run(&fixture, &["dossier", &digest, "--robot-json"]);

    assert_eq!(human.status, 0);
    assert_eq!(machine.status, 0);
    assert!(human.stdout.contains(&digest));
    assert!(human.stdout.contains("Next:"));
    let parsed: serde_json::Value =
        serde_json::from_str(&machine.stdout).expect("robot output is JSON");
    assert_eq!(parsed["package"]["digest"], digest);
    assert_eq!(parsed["schema"], "louiselm.skills.dossier/1");
}

#[test]
fn an_unreviewable_package_exits_with_its_own_status() {
    let fixture = Fixture::new();
    let candidate = fixture.candidate("candidate");
    write_file(&candidate.join("notes.md"), "no skill file here\n");
    let digest = package(&fixture, &candidate);

    let output = run(&fixture, &["dossier", &digest]);

    assert_eq!(
        output.status, 2,
        "an unreviewable package must be distinguishable from a clean one",
    );
    assert!(output.stdout.contains("Fatal:"));
}

#[test]
fn verification_and_listing_answer_from_stored_bytes() {
    let fixture = Fixture::new();
    let candidate = skill_candidate(&fixture, "candidate");
    let digest = package(&fixture, &candidate);

    let verified = run(&fixture, &["verify", &digest, "--robot-json"]);
    assert_eq!(verified.status, 0);
    let parsed: serde_json::Value = serde_json::from_str(&verified.stdout).expect("JSON");
    assert_eq!(parsed["intact"], true);

    let listed = run(&fixture, &["list"]);
    assert_eq!(listed.status, 0);
    assert!(listed.stdout.contains(&digest));

    let stored = fixture
        .store()
        .open_package(
            &louiselm_skills::Digest::parse(&digest).expect("digest parses"),
            &louiselm_skills::Policy::embedded(),
        )
        .expect("package opens")
        .file_path("SKILL.md");
    std::fs::set_permissions(&stored, std::os::unix::fs::PermissionsExt::from_mode(0o644))
        .expect("mode is settable");
    std::fs::write(&stored, "swapped\n").expect("file is writable");

    let tampered = run(&fixture, &["verify", &digest]);
    assert_eq!(tampered.status, 2);
    assert!(tampered.stdout.contains("FAILED") || tampered.stderr.contains("FAILED"));
}

#[test]
fn the_policy_in_force_can_be_printed_and_pinned() {
    let fixture = Fixture::new();

    let shown = run(&fixture, &["policy"]);
    assert_eq!(shown.status, 0);
    let parsed: serde_json::Value = serde_json::from_str(&shown.stdout).expect("policy is JSON");
    assert_eq!(parsed["schema"], "louiselm.skills.policy/1");

    let digest = run(&fixture, &["policy", "--digest"]);
    assert_eq!(digest.status, 0);
    assert_eq!(
        digest.stdout.trim(),
        louiselm_skills::Policy::embedded().digest().to_string(),
    );
}

#[test]
fn a_replacement_policy_is_refused_unless_the_caller_pinned_it() {
    let fixture = Fixture::new();
    let candidate = skill_candidate(&fixture, "candidate");
    let widened = fixture.path("widened-policy.json");
    let text = String::from_utf8(louiselm_skills::Policy::embedded_bytes().to_vec())
        .expect("policy is UTF-8")
        .replace("\"allow_non_ascii\": false", "\"allow_non_ascii\": true");
    write_file(&widened, &text);

    let unpinned = run(
        &fixture,
        &[
            "package",
            candidate.to_str().expect("path is UTF-8"),
            "--policy",
            widened.to_str().expect("path is UTF-8"),
        ],
    );
    assert_eq!(unpinned.status, 1);
    assert!(
        unpinned.stderr.contains("--policy-digest"),
        "stderr was: {}",
        unpinned.stderr,
    );

    let wrong = run(
        &fixture,
        &[
            "package",
            candidate.to_str().expect("path is UTF-8"),
            "--policy",
            widened.to_str().expect("path is UTF-8"),
            "--policy-digest",
            &louiselm_skills::Policy::embedded().digest().to_string(),
        ],
    );
    assert_eq!(wrong.status, 1);
    assert!(
        wrong.stderr.contains("digest mismatch"),
        "stderr was: {}",
        wrong.stderr,
    );

    let pinned_digest = louiselm_skills::Digest::of(text.as_bytes()).to_string();
    let pinned = run(
        &fixture,
        &[
            "package",
            candidate.to_str().expect("path is UTF-8"),
            "--policy",
            widened.to_str().expect("path is UTF-8"),
            "--policy-digest",
            &pinned_digest,
            "--robot-json",
        ],
    );
    assert_eq!(pinned.status, 0, "stderr was: {}", pinned.stderr);
    let parsed: serde_json::Value = serde_json::from_str(&pinned.stdout).expect("JSON");
    assert_eq!(parsed["policy_digest"], pinned_digest);
}

#[test]
fn an_unknown_command_is_refused_rather_than_guessed() {
    let fixture = Fixture::new();

    let output = run(&fixture, &["admit"]);

    assert_eq!(output.status, 1);
    assert!(output.stderr.contains("unknown command"));
}

#[test]
fn launcher_status_has_a_robot_view() {
    let fixture = Fixture::new();

    let output = run(&fixture, &["launcher", "status", "--robot-json"]);
    let human = run(&fixture, &["launcher", "status"]);

    assert!(
        matches!(output.status, 0 | 2),
        "stderr was: {}",
        output.stderr
    );
    let status: serde_json::Value =
        serde_json::from_str(&output.stdout).expect("launcher status is JSON");
    assert_eq!(status["schema"], "louiselm.launch.install.status/1");
    assert!(status["trusted"].is_boolean());
    assert!(human.stdout.contains("Launcher authority:"));
    assert!(human.stdout.contains("active key"));
}

#[test]
fn a_development_build_cannot_provision_launcher_authority() {
    let fixture = Fixture::new();

    let output = run(
        &fixture,
        &[
            "launcher",
            "install",
            "--operator",
            "louise",
            "--broker-uid",
            "1500",
            "--broker-gid",
            "1500",
            "--uid-start",
            "200000",
            "--gid-start",
            "300000",
            "--slots",
            "4",
        ],
    );

    assert_eq!(output.status, 1);
    assert!(
        output.stderr.contains("current verified release"),
        "stderr was: {}",
        output.stderr
    );
}

#[test]
fn launcher_paths_cannot_be_redirected() {
    let fixture = Fixture::new();
    let output = run(
        &fixture,
        &["launcher", "status", "--prefix", "/tmp/launcher"],
    );

    assert_eq!(output.status, 1);
    assert!(output.stderr.contains("paths are fixed"));
}
