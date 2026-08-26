use std::{fs, process::Command};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use louiselm_capture::PairingRegistry;

#[test]
fn local_ingest_and_list_expose_the_durable_inbox() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let data = temporary.path().join("data");
    let state = temporary.path().join("state");
    let audio = temporary.path().join("idea.wav");
    fs::write(&audio, b"speech").expect("audio");
    let id = "11111111-1111-4111-8111-111111111111";

    let ingest = command(&data, &state)
        .args([
            "ingest-local",
            "--file",
            audio.to_str().expect("audio path"),
            "--id",
            id,
            "--recorded-at-ms",
            "1765000000000",
            "--duration-ms",
            "4200",
            "--mime",
            "audio/wav",
        ])
        .output()
        .expect("ingest command");
    assert!(ingest.status.success(), "{:?}", ingest.stderr);

    let listed = command(&data, &state)
        .arg("list")
        .output()
        .expect("list command");
    assert!(listed.status.success(), "{:?}", listed.stderr);
    let listed: serde_json::Value = serde_json::from_slice(&listed.stdout).expect("list JSON");
    assert_eq!(listed[0]["record"]["id"], id);
    assert_eq!(listed[0]["transcript"], serde_json::Value::Null);
    assert!(
        listed[0]["audio_path"]
            .as_str()
            .expect("audio path")
            .ends_with("audio.wav")
    );
}

#[test]
fn run_admission_exposes_the_approved_generated_work_ceiling() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let data = temporary.path().join("data");
    let state = temporary.path().join("state");
    let run_id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let arguments = [
        "run",
        "admit",
        "--id",
        run_id,
        "--session-id",
        "codex/session-123",
        "--agent",
        "codex",
        "--acp-session-id",
        "session-123",
        "--cwd",
        "/tmp/project",
        "--load-session",
        "true",
    ];

    let admitted = command(&data, &state)
        .args(arguments)
        .args(["--generated-work-max", "5"])
        .output()
        .expect("admit Run");
    assert!(admitted.status.success(), "{:?}", admitted.stderr);
    let response: serde_json::Value =
        serde_json::from_slice(&admitted.stdout).expect("admission JSON");
    assert_eq!(response["id"], run_id);
    assert_eq!(response["state"], "active");
    assert_eq!(response["generated_work"]["ceiling"], 5);
    assert_eq!(response["generated_work"]["consumed"], 0);
    assert_eq!(response["generated_work"]["reserved"], 0);

    let lowering = command(&data, &state)
        .args(arguments)
        .args(["--generated-work-max", "4"])
        .output()
        .expect("lower ceiling");
    assert!(!lowering.status.success());
}

#[test]
fn pairing_refuses_loopback_until_a_private_profile_is_configured() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let refused = command(
        &temporary.path().join("data"),
        &temporary.path().join("state"),
    )
    .arg("pair")
    .output()
    .expect("pair command");
    assert!(!refused.status.success());
    assert!(
        String::from_utf8(refused.stderr)
            .expect("stderr")
            .contains("configure-network")
    );

    let configured = command(
        &temporary.path().join("data"),
        &temporary.path().join("state"),
    )
    .args([
        "configure-network",
        "--profile",
        "lan",
        "--bind",
        "192.168.1.20:7391",
        "--url",
        "https://192.168.1.20:7391",
    ])
    .output()
    .expect("configure command");
    assert!(configured.status.success(), "{:?}", configured.stderr);

    let status = command(
        &temporary.path().join("data"),
        &temporary.path().join("state"),
    )
    .arg("status")
    .output()
    .expect("status command");
    assert!(status.status.success(), "{:?}", status.stderr);
    let status: serde_json::Value = serde_json::from_slice(&status.stdout).expect("status JSON");
    assert_eq!(status["network"]["profile"], "lan");
    assert_eq!(status["network"]["bind"], "192.168.1.20:7391");
    assert_eq!(
        status["network"]["receiver_url"],
        "https://192.168.1.20:7391"
    );
    assert_eq!(status["paired_device_count"], 0);
    assert_eq!(status["delivery"]["state"], "reachable");
    assert_eq!(status["delivery"]["warning"], serde_json::Value::Null);

    let output = command(
        &temporary.path().join("data"),
        &temporary.path().join("state"),
    )
    .arg("pair")
    .output()
    .expect("pair command");
    assert!(output.status.success(), "{:?}", output.stderr);
    let rendered = String::from_utf8(output.stdout).expect("terminal QR");
    assert!(!rendered.contains("receiver_url"));
    assert!(!rendered.contains("\"token\""));

    let lines = rendered.lines().collect::<Vec<_>>();
    let width = lines
        .iter()
        .map(|line| line.chars().count())
        .max()
        .expect("QR width");
    assert!(width <= lines.len() * 2 + 2);

    let svg_path = temporary.path().join("pairing.svg");
    let svg_output = command(
        &temporary.path().join("data"),
        &temporary.path().join("state"),
    )
    .args(["pair", "--svg", svg_path.to_str().expect("SVG path")])
    .output()
    .expect("SVG pair command");
    assert!(svg_output.status.success(), "{:?}", svg_output.stderr);
    assert_eq!(
        String::from_utf8(svg_output.stdout).expect("SVG command output"),
        format!("{}\n", svg_path.display())
    );

    let svg = fs::read_to_string(&svg_path).expect("pairing SVG");
    assert!(svg.contains("shape-rendering=\"crispEdges\""));
    assert!(svg.contains("fill=\"#fff\""));
    assert!(svg.contains("fill=\"#000\""));
    assert!(!svg.contains("receiver_url"));
    assert!(!svg.contains("\"token\""));

    #[cfg(unix)]
    assert_eq!(
        fs::metadata(&svg_path)
            .expect("SVG metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn status_warns_when_paired_devices_only_have_a_loopback_receiver() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let data = temporary.path().join("data");
    let state = temporary.path().join("state");
    let registry =
        PairingRegistry::open(state.join("louiselm/capture/pairing")).expect("pairing registry");
    let offer = registry
        .issue(
            "https://192.0.2.1:7391",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            1_000,
            5_000,
        )
        .expect("offer");
    registry
        .consume(&offer.token, "garden-phone", 2_000)
        .expect("paired device");

    let output = command(&data, &state)
        .arg("status")
        .output()
        .expect("status command");
    assert!(output.status.success(), "{:?}", output.stderr);
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).expect("status JSON");
    assert_eq!(status["paired_device_count"], 1);
    assert_eq!(status["delivery"]["state"], "degraded");
    assert_eq!(
        status["delivery"]["warning"],
        "paired devices cannot reach the loopback-only receiver"
    );
    assert_eq!(
        status["devices"][0]["last_delivery_at_ms"],
        serde_json::Value::Null
    );
}

fn command(data: &std::path::Path, state: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_louiselm-capture"));
    command
        .env("LOUISELM_CAPTURE_DATA_DIR", data)
        .env("LOUISELM_CAPTURE_STATE_DIR", state)
        .env("LOUISELM_CAPTURE_CONFIG_DIR", state);
    command
}
