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

fn command(data: &std::path::Path, state: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_louiselm-capture"));
    command
        .env("LOUISELM_CAPTURE_DATA_DIR", data)
        .env("LOUISELM_CAPTURE_STATE_DIR", state);
    command
}
