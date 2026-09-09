//! Behavioral coverage for cli admission.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Test fixtures abort on setup failure and assert failures directly."
)]

//! The ceremony as an operator drives it, from the command line.

mod support;

use std::process::Command;

use support::{Fixture, SshKey, write_file};

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

fn package(fixture: &Fixture, name: &str) -> String {
    let candidate = fixture.candidate(name);
    write_file(
        &candidate.join("SKILL.md"),
        &format!("---\nname: {name}\ndescription: The {name} skill.\n---\n\nBody.\n"),
    );
    let output = run(
        fixture,
        &[
            "package",
            candidate.to_str().expect("path is UTF-8"),
            "--robot-json",
        ],
    );
    assert_eq!(output.status, 0, "packaging failed: {}", output.stderr);
    let parsed: serde_json::Value = serde_json::from_str(&output.stdout).expect("JSON");
    parsed["digest"].as_str().expect("a digest").to_owned()
}

fn bootstrap(fixture: &Fixture) -> (SshKey, SshKey) {
    let primary = SshKey::generate(fixture, "primary");
    let recovery = SshKey::generate(fixture, "recovery");
    let output = run(
        fixture,
        &[
            "trust",
            "bootstrap",
            "--primary",
            &primary.public_key(),
            "--release",
            &recovery.public_key(),
        ],
    );
    assert_eq!(output.status, 0, "bootstrap failed: {}", output.stderr);
    (primary, recovery)
}

fn key_argument(key: &SshKey) -> String {
    key.private_key_path()
        .to_str()
        .expect("path is UTF-8")
        .to_owned()
}

#[test]
fn the_whole_ceremony_runs_from_the_command_line() {
    let fixture = Fixture::new();
    let (primary, _recovery) = bootstrap(&fixture);
    let digest = package(&fixture, "alpha");
    let remote = fixture.witness_remote();

    let before = run(&fixture, &["generation", "status", "--robot-json"]);
    assert_eq!(before.status, 2, "no Generation in force is not admissible");
    let parsed: serde_json::Value = serde_json::from_str(&before.stdout).expect("JSON");
    assert_eq!(parsed["next_action"]["id"], "admit_first_generation");

    let admitted = run(
        &fixture,
        &[
            "generation",
            "admit",
            "--member",
            &format!("{digest}:read=claude"),
            "--key",
            &key_argument(&primary),
            "--robot-json",
        ],
    );
    assert_eq!(admitted.status, 0, "admit failed: {}", admitted.stderr);
    let record: serde_json::Value = serde_json::from_str(&admitted.stdout).expect("JSON");
    assert_eq!(record["state"], "pending_witness");
    let generation = record["generation"].as_str().expect("a digest").to_owned();

    let witnessed = run(
        &fixture,
        &[
            "generation",
            "witness",
            &generation,
            "--remote",
            remote.to_str().expect("path is UTF-8"),
        ],
    );
    assert_eq!(witnessed.status, 0, "witness failed: {}", witnessed.stderr);

    let activated = run(&fixture, &["generation", "activate", &generation]);
    assert_eq!(activated.status, 0, "activate failed: {}", activated.stderr);

    let after = run(&fixture, &["generation", "status"]);
    assert_eq!(after.status, 0);
    assert!(after.stdout.contains("current"), "{}", after.stdout);
    assert!(after.stdout.contains("witnessed"), "{}", after.stdout);
}

#[test]
fn admitting_without_trust_is_refused_before_anything_is_signed() {
    let fixture = Fixture::new();
    let digest = package(&fixture, "alpha");
    let key = SshKey::generate(&fixture, "stranger");

    let output = run(
        &fixture,
        &[
            "generation",
            "admit",
            "--member",
            &format!("{digest}=claude"),
            "--key",
            &key_argument(&key),
        ],
    );

    assert_eq!(output.status, 1);
    assert!(
        output.stderr.contains("not bootstrapped"),
        "stderr was: {}",
        output.stderr,
    );
}

#[test]
fn quarantine_narrows_from_the_command_line_and_refuses_to_widen() {
    let fixture = Fixture::new();
    let (primary, _recovery) = bootstrap(&fixture);
    let alpha = package(&fixture, "alpha");
    let beta = package(&fixture, "beta");
    let remote = fixture.witness_remote();

    let admitted = run(
        &fixture,
        &[
            "generation",
            "admit",
            "--member",
            &format!("{alpha}:read=claude"),
            "--member",
            &format!("{beta}:read=claude,codex"),
            "--key",
            &key_argument(&primary),
            "--robot-json",
        ],
    );
    assert_eq!(admitted.status, 0, "admit failed: {}", admitted.stderr);
    let record: serde_json::Value = serde_json::from_str(&admitted.stdout).expect("JSON");
    let generation = record["generation"].as_str().expect("a digest").to_owned();
    run(
        &fixture,
        &[
            "generation",
            "witness",
            &generation,
            "--remote",
            remote.to_str().expect("path is UTF-8"),
        ],
    );
    run(&fixture, &["generation", "activate", &generation]);

    let excluded = run(
        &fixture,
        &[
            "quarantine",
            "exclude",
            &beta,
            "--reason",
            "suspected prompt injection",
        ],
    );
    assert_eq!(excluded.status, 0, "quarantine failed: {}", excluded.stderr);

    let status = run(&fixture, &["generation", "status", "--robot-json"]);
    assert_eq!(
        status.status, 2,
        "a quarantined Generation is not fully admissible"
    );
    let parsed: serde_json::Value = serde_json::from_str(&status.stdout).expect("JSON");
    assert_eq!(parsed["state"], "quarantined");
    assert_eq!(
        parsed["effective_members"].as_array().expect("array").len(),
        1
    );
    assert_eq!(parsed["next_action"]["id"], "admit_after_quarantine");

    let cleared = run(&fixture, &["quarantine", "clear"]);
    assert_eq!(cleared.status, 1);
    assert!(
        cleared.stderr.contains("Skill Generation"),
        "stderr was: {}",
        cleared.stderr,
    );
}

#[test]
fn a_quarantine_without_a_reason_is_refused() {
    let fixture = Fixture::new();
    let digest = package(&fixture, "alpha");

    let output = run(&fixture, &["quarantine", "exclude", &digest]);

    assert_eq!(output.status, 1);
    assert!(
        output.stderr.contains("--reason"),
        "stderr was: {}",
        output.stderr
    );
}

#[test]
fn obsolete_recovery_role_and_rotation_commands_are_refused_without_mutation() {
    let fixture = Fixture::new();
    bootstrap(&fixture);
    let before = std::fs::read(fixture.path("store/trust/roles.json")).unwrap();
    for command in ["rotation-payload", "rotate"] {
        let output = run(&fixture, &["trust", command]);
        assert_eq!(output.status, 1);
        assert!(output.stdout.is_empty());
        assert_eq!(
            std::fs::read(fixture.path("store/trust/roles.json")).unwrap(),
            before
        );
    }
}

#[test]
fn a_directory_that_is_not_a_candidate_cannot_reach_the_ceremony() {
    let fixture = Fixture::new();
    let (primary, _recovery) = bootstrap(&fixture);
    let candidate = fixture.candidate("no-skill");
    write_file(&candidate.join("notes.md"), "no skill file here\n");
    let output = run(
        &fixture,
        &[
            "package",
            candidate.to_str().expect("path is UTF-8"),
            "--robot-json",
        ],
    );
    let parsed: serde_json::Value = serde_json::from_str(&output.stdout).expect("JSON");
    let digest = parsed["digest"].as_str().expect("a digest").to_owned();

    let admitted = run(
        &fixture,
        &[
            "generation",
            "admit",
            "--member",
            &format!("{digest}=claude"),
            "--key",
            &key_argument(&primary),
        ],
    );

    assert_eq!(admitted.status, 1);
    assert!(
        admitted.stderr.contains("cannot be admitted"),
        "stderr was: {}",
        admitted.stderr,
    );
}

// louiselm-d6fv.3.5: the ceremony records literal Agent scope.

#[test]
fn all_agents_expands_to_the_registered_agent_names() {
    let fixture = Fixture::new();
    let (primary, _) = bootstrap(&fixture);
    let digest = package(&fixture, "alpha");
    let registry_root = fixture.path("registry");
    let runtime_root = fixture.path("runtime");
    write_file(&runtime_root.join("bin/agent"), "#!/bin/sh\nexec cat\n");
    write_file(&runtime_root.join("lib/adapter.js"), "// adapter\n");
    support::write_registry(&registry_root, &runtime_root);
    // Two Agents on one runtime: expansion must record both, not the runtime.
    let agents = std::fs::read_to_string(registry_root.join("agents.json")).expect("agents.json");
    write_file(
        &registry_root.join("agents.json"),
        &agents.replace(
            r#""id":"demo","#,
            r#""id":"zeta","#,
        ).replace(
            "}]}",
            r#"},{"id":"alpha-agent","provider":"demo-provider","runtime_id":"demo-runtime","arguments":["--acp"],"environment":{}}]}"#,
        ),
    );

    let admitted = run(
        &fixture,
        &[
            "generation",
            "admit",
            "--member",
            &format!("{digest}:read"),
            "--all-agents",
            "--registry",
            registry_root.to_str().expect("path is UTF-8"),
            "--key",
            &key_argument(&primary),
            "--robot-json",
        ],
    );

    assert_eq!(admitted.status, 0, "admit failed: {}", admitted.stderr);
    let record: serde_json::Value = serde_json::from_str(&admitted.stdout).expect("JSON");
    let scope = &record["payload"]["members"][0]["agents"];
    assert_eq!(
        scope,
        &serde_json::json!(["alpha-agent", "zeta"]),
        "--all-agents must sign the literal names, sorted, never a wildcard",
    );
}

#[test]
fn all_agents_without_a_registry_is_refused() {
    let fixture = Fixture::new();
    let (primary, _) = bootstrap(&fixture);
    let digest = package(&fixture, "alpha");

    let admitted = run(
        &fixture,
        &[
            "generation",
            "admit",
            "--member",
            &format!("{digest}:read"),
            "--all-agents",
            "--key",
            &key_argument(&primary),
        ],
    );

    assert_eq!(admitted.status, 1);
    assert!(
        admitted.stderr.contains("--registry"),
        "stderr was: {}",
        admitted.stderr,
    );
}

#[test]
fn a_member_without_a_scope_is_refused() {
    let fixture = Fixture::new();
    let (primary, _) = bootstrap(&fixture);
    let digest = package(&fixture, "alpha");

    let admitted = run(
        &fixture,
        &[
            "generation",
            "admit",
            "--member",
            &format!("{digest}:read"),
            "--key",
            &key_argument(&primary),
        ],
    );

    assert_eq!(admitted.status, 1);
    assert!(
        admitted.stderr.contains("names no Agent"),
        "stderr was: {}",
        admitted.stderr,
    );
}

#[test]
fn the_view_flag_no_longer_exists() {
    let fixture = Fixture::new();
    let (primary, _) = bootstrap(&fixture);
    let digest = package(&fixture, "alpha");

    let admitted = run(
        &fixture,
        &[
            "generation",
            "admit",
            "--member",
            &format!("{digest}:read=claude"),
            "--view",
            "claude=/home/operator/.claude/skills",
            "--key",
            &key_argument(&primary),
        ],
    );

    assert_eq!(
        admitted.status, 1,
        "a host path must not be accepted into a portable signed record",
    );
}
