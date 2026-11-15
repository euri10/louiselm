//! The installed preflight path emits only bounded, non-authoritative reports.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "Tests abort on fixture errors."
)]

#[path = "support/discovery.rs"]
#[allow(
    dead_code,
    reason = "Shared producing fixture also supports signed discovery tests."
)]
mod discovery_support;
mod support;

use discovery_support::DiscoveryFixture;
use louiselm_skills::{
    launch::REQUEST_SCHEMA,
    preflight::{DIRECT_LAUNCH_NOTICE, PREFLIGHT_SCHEMA},
    session_manifest::INPUT_MANIFEST_SCHEMA,
};
use std::process::{Command, Output};

fn run(fixture: &DiscoveryFixture, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_louiselm-skills"))
        .arg("preflight")
        .args(args)
        .env("LOUISELM_SKILLS_STORE", fixture.fixture.store_root())
        .output()
        .unwrap()
}

#[test]
fn neovim_and_cli_help_match_the_accepted_input_schemas() {
    let request = REQUEST_SCHEMA.strip_prefix("louiselm.launch.").unwrap();
    let manifest = INPUT_MANIFEST_SCHEMA
        .strip_prefix("louiselm.session.")
        .unwrap();
    let expected = format!("canonical launch {request} and Session {manifest}");
    let output = Command::new(env!("CARGO_BIN_EXE_louiselm-skills"))
        .args(["preflight", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let cli_help = String::from_utf8(output.stdout).unwrap();
    let vimdoc =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../doc/louiselm.txt"))
            .unwrap();
    for help in [vimdoc.as_str(), cli_help.as_str()] {
        let normalized = help.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(normalized.contains(&expected), "missing {expected} in help");
    }
}

#[test]
fn robot_and_human_preflight_name_the_same_exact_request_without_claiming_launch_success() {
    let fixture = DiscoveryFixture::new();
    let request_path = fixture.fixture.path("request.json");
    let manifest_path = fixture.fixture.path("input.json");
    std::fs::write(&request_path, fixture.request.canonical_bytes()).unwrap();
    std::fs::write(&manifest_path, fixture.manifest.canonical_bytes()).unwrap();
    let args = [
        "--request",
        request_path.to_str().unwrap(),
        "--manifest",
        manifest_path.to_str().unwrap(),
        "--robot-json",
    ];
    let robot = run(&fixture, &args);
    assert_eq!(
        robot.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&robot.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&robot.stdout).unwrap();
    assert_eq!(value["schema"], PREFLIGHT_SCHEMA);
    assert_eq!(value["manifest_state"], "matched");
    assert_eq!(value["posture"]["state"], "unverified");
    assert_eq!(
        value["request_digest"],
        fixture.request.digest().to_string()
    );
    let human = run(&fixture, &args[..4]);
    assert_eq!(human.status.code(), Some(2));
    let text = String::from_utf8(human.stdout).unwrap();
    assert!(text.contains(&fixture.request.digest().to_string()));
    assert!(text.contains("network_scope: denied"));
    assert!(text.contains("isolation_contract: louiselm.isolation/2"));
    assert!(text.contains("not authorization or live Session state"));
}

#[test]
fn closed_parser_and_bad_files_never_echo_sensitive_arguments_or_contents() {
    let fixture = DiscoveryFixture::new();
    let path = fixture.fixture.path("private-marker");
    std::fs::write(&path, "private-marker").unwrap();
    for args in [
        vec!["--private-marker", "--robot-json"],
        vec!["--request", path.to_str().unwrap(), "--robot-json"],
        vec![
            "--direct",
            "--request",
            path.to_str().unwrap(),
            "--robot-json",
        ],
        vec![
            "--request",
            path.to_str().unwrap(),
            "--previous-request",
            path.to_str().unwrap(),
            "--robot-json",
        ],
        vec!["--direct", "--direct", "--robot-json"],
    ] {
        let output = run(&fixture, &args);
        assert_eq!(output.status.code(), Some(1));
        assert!(!String::from_utf8_lossy(&output.stdout).contains("private-marker"));
        assert!(!String::from_utf8_lossy(&output.stderr).contains("private-marker"));
    }
}

#[test]
fn direct_launch_does_not_invent_a_session_or_verified_posture() {
    let fixture = DiscoveryFixture::new();
    let output = run(&fixture, &["--direct", "--robot-json"]);
    assert_eq!(output.status.code(), Some(2));
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["notice"], DIRECT_LAUNCH_NOTICE);
    assert!(value.get("posture").is_none());
    assert!(value.get("session_id").is_none());
}

fn shared_preflight() -> (
    louiselm_skills::preflight::Preview,
    louiselm_skills::launch::LaunchRequest,
) {
    let fixture = DiscoveryFixture::new();
    let mut manifest = fixture.manifest.clone();
    manifest.envelope.revision = 2;
    let mut request = fixture.request.clone();
    request.envelope_revision = 2;
    request.session_input_manifest_id = manifest.digest().to_string();
    let preview = louiselm_skills::preflight::inspect(
        &request,
        Some(&manifest),
        None,
        &louiselm_skills::Policy::embedded(),
        None,
        Some((&fixture.request, &fixture.manifest)),
    )
    .unwrap();
    (preview, request)
}

#[test]
#[ignore = "Explicit regeneration; the ordinary test below is the CI freshness gate."]
fn regenerate_preflight_fixture() {
    let (preview, request) = shared_preflight();
    std::fs::write(
        "../tests/fixtures/preflight_v1.json",
        format!("{}\n", serde_json::to_string_pretty(&preview).unwrap()),
    )
    .unwrap();
    std::fs::write(
        "../tests/fixtures/preflight_request_v2.json",
        request.canonical_bytes(),
    )
    .unwrap();
}

#[test]
fn shared_neovim_fixture_is_emitted_by_the_real_preflight_producer() {
    let (preview, request) = shared_preflight();
    let expected: serde_json::Value =
        serde_json::from_str(include_str!("../../tests/fixtures/preflight_v1.json")).unwrap();
    assert_eq!(serde_json::to_value(preview).unwrap(), expected);
    assert_eq!(
        request.canonical_bytes(),
        include_str!("../../tests/fixtures/preflight_request_v2.json")
            .trim_end()
            .as_bytes(),
    );
}
