use std::{fs, process::Command};

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
}

fn command(data: &std::path::Path, state: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_louiselm-capture"));
    command
        .env("LOUISELM_CAPTURE_DATA_DIR", data)
        .env("LOUISELM_CAPTURE_STATE_DIR", state)
        .env("LOUISELM_CAPTURE_CONFIG_DIR", state);
    command
}
