use std::{fs, process::Command};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use louiselm_capture::PairingRegistry;

#[cfg(unix)]
#[test]
fn run_br_shim_brokers_generation_and_passes_reads_through() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let recorded = temporary.path().join("arguments");
    let fake = temporary.path().join("fake-command");
    fs::write(
        &fake,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n",
            recorded.display()
        ),
    )
    .expect("fake command");
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o700)).expect("fake mode");
    let shim = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../scripts/run-tools/br");

    let generated = Command::new(&shim)
        .args(["q", "captured", "idea"])
        .env("LOUISELM_CAPTURE", &fake)
        .env("LOUISELM_REAL_BR", &fake)
        .output()
        .expect("shim generation");
    assert!(generated.status.success());
    assert_eq!(
        fs::read_to_string(&recorded).expect("generated arguments"),
        "run\ngenerate\n--command\nq\n--\ncaptured\nidea\n"
    );

    let read = Command::new(&shim)
        .args(["ready", "--json"])
        .env("LOUISELM_CAPTURE", &fake)
        .env("LOUISELM_REAL_BR", &fake)
        .output()
        .expect("shim read");
    assert!(read.status.success());
    assert_eq!(
        fs::read_to_string(&recorded).expect("read arguments"),
        "ready\n--json\n"
    );
}

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
    let arguments = ["run", "admit", "--id", run_id];

    let admitted = command(&data, &state)
        .args(arguments)
        .args(["--generated-work-max", "5", "--park-ttl-ms", "3600000"])
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
    assert!(
        response["token"]
            .as_str()
            .is_some_and(|token| !token.is_empty())
    );

    let lowering = command(&data, &state)
        .args(arguments)
        .args(["--generated-work-max", "4", "--park-ttl-ms", "3600000"])
        .output()
        .expect("lower ceiling");
    assert!(!lowering.status.success());
}

#[cfg(unix)]
#[test]
fn ambiguous_cli_retry_reuses_the_supplied_mutation_identity() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let data = temporary.path().join("data");
    let state = temporary.path().join("state");
    let run_id = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
    let mutation_id = "11111111-2222-4333-8444-555555555555";
    let admitted = command(&data, &state)
        .args([
            "run",
            "admit",
            "--id",
            run_id,
            "--generated-work-max",
            "1",
            "--park-ttl-ms",
            "3600000",
        ])
        .output()
        .expect("admit Run");
    assert!(admitted.status.success(), "{:?}", admitted.stderr);
    let admission: serde_json::Value =
        serde_json::from_slice(&admitted.stdout).expect("admission JSON");
    let token = admission["token"].as_str().expect("generate token");
    let attached = command(&data, &state)
        .args([
            "run",
            "attach",
            "--id",
            run_id,
            "--session-id",
            "codex/session-1",
            "--agent",
            "codex",
            "--acp-session-id",
            "session-1",
            "--cwd",
            "/tmp/project",
            "--load-session",
            "true",
        ])
        .output()
        .expect("attach Run");
    assert!(attached.status.success(), "{:?}", attached.stderr);

    let counter = temporary.path().join("create-count");
    let issue = temporary.path().join("issue.json");
    let fake_br = temporary.path().join("fake-br");
    fs::write(
        &fake_br,
        format!(
            "#!/bin/sh\nset -eu\ncase \"$1\" in\n  create)\n    external_ref=\n    shift\n    while [ \"$#\" -gt 0 ]; do\n      if [ \"$1\" = --external-ref ]; then external_ref=$2; break; fi\n      shift\n    done\n    count=0\n    if [ -f '{counter}' ]; then count=$(cat '{counter}'); fi\n    count=$((count + 1))\n    printf '%s' \"$count\" > '{counter}'\n    printf '[{{\"id\":\"qa-generated\",\"external_ref\":\"%s\"}}]\\n' \"$external_ref\" > '{issue}'\n    exit 1\n    ;;\n  list) cat '{issue}' ;;\n  *) exit 2 ;;\nesac\n",
            counter = counter.display(),
            issue = issue.display(),
        ),
    )
    .expect("fake br");
    fs::set_permissions(&fake_br, fs::Permissions::from_mode(0o700)).expect("fake br mode");
    let database = temporary.path().join("beads.db");
    let invoke = || {
        let mut process = command(&data, &state);
        process
            .args([
                "run",
                "generate",
                "--command",
                "create",
                "--",
                "--title",
                "Generated",
            ])
            .env("LOUISELM_RUN_ID", run_id)
            .env("LOUISELM_RUN_TOKEN", token)
            .env("LOUISELM_MUTATION_ID", mutation_id)
            .env("LOUISELM_REAL_BR", &fake_br)
            .env("BEADS_DB", &database);
        process
    };

    let first = invoke().output().expect("ambiguous first attempt");
    assert!(!first.status.success());
    assert!(
        String::from_utf8(first.stderr)
            .expect("first stderr")
            .contains(mutation_id)
    );
    let second = invoke().output().expect("reconciled retry");
    assert!(second.status.success(), "{:?}", second.stderr);
    assert_eq!(fs::read_to_string(counter).expect("create count"), "1");
}

#[cfg(unix)]
#[test]
fn distinct_logical_mutations_receive_distinct_identities_and_both_create() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let data = temporary.path().join("data");
    let state = temporary.path().join("state");
    let run_id = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
    let first_mutation_id = "11111111-2222-4333-8444-555555555555";
    let second_mutation_id = "22222222-3333-4444-8555-666666666666";
    let admitted = command(&data, &state)
        .args([
            "run",
            "admit",
            "--id",
            run_id,
            "--generated-work-max",
            "2",
            "--park-ttl-ms",
            "3600000",
        ])
        .output()
        .expect("admit Run");
    assert!(admitted.status.success(), "{:?}", admitted.stderr);
    let admission: serde_json::Value =
        serde_json::from_slice(&admitted.stdout).expect("admission JSON");
    let token = admission["token"].as_str().expect("generate token");
    let attached = command(&data, &state)
        .args([
            "run",
            "attach",
            "--id",
            run_id,
            "--session-id",
            "codex/session-1",
            "--agent",
            "codex",
            "--acp-session-id",
            "session-1",
            "--cwd",
            "/tmp/project",
            "--load-session",
            "true",
        ])
        .output()
        .expect("attach Run");
    assert!(attached.status.success(), "{:?}", attached.stderr);

    let counter = temporary.path().join("create-count");
    let issue = temporary.path().join("issue.json");
    let fake_br = temporary.path().join("fake-br");
    fs::write(
        &fake_br,
        format!(
            "#!/bin/sh\nset -eu\ncase \"$1\" in\n  create)\n    external_ref=\n    shift\n    while [ \"$#\" -gt 0 ]; do\n      if [ \"$1\" = --external-ref ]; then external_ref=$2; break; fi\n      shift\n    done\n    count=0\n    if [ -f '{counter}' ]; then count=$(cat '{counter}'); fi\n    count=$((count + 1))\n    printf '%s' \"$count\" > '{counter}'\n    printf '[{{\"id\":\"qa-generated\",\"external_ref\":\"%s\"}}]\\n' \"$external_ref\" > '{issue}'\n    exit 1\n    ;;\n  list) cat '{issue}' ;;\n  *) exit 2 ;;\nesac\n",
            counter = counter.display(),
            issue = issue.display(),
        ),
    )
    .expect("fake br");
    fs::set_permissions(&fake_br, fs::Permissions::from_mode(0o700)).expect("fake br mode");
    let database = temporary.path().join("beads.db");
    let invoke = |mutation_id: &str| {
        let mut process = command(&data, &state);
        process
            .args([
                "run",
                "generate",
                "--command",
                "create",
                "--",
                "--title",
                "Generated",
            ])
            .env("LOUISELM_RUN_ID", run_id)
            .env("LOUISELM_RUN_TOKEN", token)
            .env("LOUISELM_MUTATION_ID", mutation_id)
            .env("LOUISELM_REAL_BR", &fake_br)
            .env("BEADS_DB", &database);
        process.output().expect("ambiguous attempt")
    };

    let first = invoke(first_mutation_id);
    assert!(!first.status.success());
    assert!(
        String::from_utf8(first.stderr)
            .expect("first stderr")
            .contains(first_mutation_id)
    );
    let second = invoke(second_mutation_id);
    assert!(!second.status.success());
    let second_stderr = String::from_utf8(second.stderr).expect("second stderr");
    assert!(second_stderr.contains(second_mutation_id));
    assert!(!second_stderr.contains(first_mutation_id));
    assert_eq!(fs::read_to_string(counter).expect("create count"), "2");
}

/// Admit and attach a Run, returning its generate token.
fn admitted_run(
    data: &std::path::Path,
    state: &std::path::Path,
    run_id: &str,
    max: &str,
) -> String {
    let admitted = command(data, state)
        .args([
            "run",
            "admit",
            "--id",
            run_id,
            "--generated-work-max",
            max,
            "--park-ttl-ms",
            "3600000",
        ])
        .output()
        .expect("admit Run");
    assert!(admitted.status.success(), "{:?}", admitted.stderr);
    let admission: serde_json::Value =
        serde_json::from_slice(&admitted.stdout).expect("admission JSON");
    let token = admission["token"]
        .as_str()
        .expect("generate token")
        .to_owned();
    let attached = command(data, state)
        .args([
            "run",
            "attach",
            "--id",
            run_id,
            "--session-id",
            "codex/session-1",
            "--agent",
            "codex",
            "--acp-session-id",
            "session-1",
            "--cwd",
            "/tmp/project",
            "--load-session",
            "true",
        ])
        .output()
        .expect("attach Run");
    assert!(attached.status.success(), "{:?}", attached.stderr);
    token
}

#[test]
fn generator_reservations_round_trip_through_the_cli_and_replay_safely() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let data = temporary.path().join("data");
    let state = temporary.path().join("state");
    let run_id = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
    let mutation_id = "11111111-2222-4333-8444-555555555555";
    let token = admitted_run(&data, &state, run_id, "5");
    // The Run id and generate token arrive through the environment, matching
    // `run generate`. Passing the token as an argument would expose it in the
    // process table to every other user on the host.
    let ledger = |arguments: &[&str]| {
        let output = command(&data, &state)
            .args(arguments)
            .env("LOUISELM_RUN_ID", run_id)
            .env("LOUISELM_RUN_TOKEN", &token)
            .output()
            .expect("ledger command");
        assert!(output.status.success(), "{:?}", output);
        serde_json::from_slice::<serde_json::Value>(&output.stdout).expect("ledger JSON")
    };

    let reserved = ledger(&[
        "run",
        "reserve",
        "--mutation-id",
        mutation_id,
        "--kind",
        "skill_generator",
        "--units",
        "3",
    ]);
    assert_eq!(reserved["state"], "reserved");
    assert_eq!(reserved["generated_work"]["reserved"], 3);
    assert_eq!(reserved["generated_work"]["consumed"], 0);
    assert_eq!(reserved["generated_work"]["ceiling"], 5);

    // Replaying the same reservation is idempotent rather than additive.
    let replayed = ledger(&[
        "run",
        "reserve",
        "--mutation-id",
        mutation_id,
        "--kind",
        "skill_generator",
        "--units",
        "3",
    ]);
    assert_eq!(replayed["state"], "pending");
    assert_eq!(replayed["generated_work"]["reserved"], 3);

    let confirmed = ledger(&[
        "run",
        "confirm",
        "--mutation-id",
        mutation_id,
        "--issue-id",
        "louiselm-generated-1",
    ]);
    assert_eq!(confirmed["state"], "confirmed");
    assert_eq!(confirmed["generated_work"]["consumed"], 1);
    assert_eq!(confirmed["generated_work"]["reserved"], 2);

    // Replaying one output does not consume a second unit.
    let reconfirmed = ledger(&[
        "run",
        "confirm",
        "--mutation-id",
        mutation_id,
        "--issue-id",
        "louiselm-generated-1",
    ]);
    assert_eq!(reconfirmed["generated_work"]["consumed"], 1);
    assert_eq!(reconfirmed["generated_work"]["reserved"], 2);

    let released = ledger(&["run", "release", "--mutation-id", mutation_id]);
    assert_eq!(released["state"], "released");
    assert_eq!(released["generated_work"]["consumed"], 1);
    assert_eq!(released["generated_work"]["reserved"], 0);

    // Releasing an unknown reservation is a no-op, not a failure.
    let repeated = ledger(&["run", "release", "--mutation-id", mutation_id]);
    assert_eq!(repeated["generated_work"]["reserved"], 0);
}

#[test]
fn a_cli_reservation_beyond_the_ceiling_reports_exhausted_and_parks() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let data = temporary.path().join("data");
    let state = temporary.path().join("state");
    let run_id = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
    let token = admitted_run(&data, &state, run_id, "2");
    let reserve = |mutation_id: &str, units: &str| {
        let output = command(&data, &state)
            .args([
                "run",
                "reserve",
                "--mutation-id",
                mutation_id,
                "--kind",
                "skill_generator",
                "--units",
                units,
            ])
            .env("LOUISELM_RUN_ID", run_id)
            .env("LOUISELM_RUN_TOKEN", &token)
            .output()
            .expect("reserve");
        assert!(output.status.success(), "{:?}", output);
        serde_json::from_slice::<serde_json::Value>(&output.stdout).expect("reserve JSON")
    };

    // Three units against a ceiling of two never starts, and the refusal is a
    // Park rather than an error, so an operator can decide to raise or stop.
    let exhausted = reserve("11111111-2222-4333-8444-555555555555", "3");
    assert_eq!(exhausted["state"], "exhausted");
    assert_eq!(exhausted["generated_work"]["reserved"], 0);
    assert_eq!(exhausted["generated_work"]["consumed"], 0);

    // The Run is parked, so a subsequent reservation is refused too.
    let after_park = reserve("22222222-3333-4444-8555-666666666666", "1");
    assert_eq!(after_park["state"], "exhausted");
}

#[test]
fn cli_reservations_refuse_a_wrong_token_and_malformed_arguments() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let data = temporary.path().join("data");
    let state = temporary.path().join("state");
    let run_id = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
    let token = admitted_run(&data, &state, run_id, "5");
    let attempt = |arguments: &[&str], run_token: &str| {
        command(&data, &state)
            .args(arguments)
            .env("LOUISELM_RUN_ID", run_id)
            .env("LOUISELM_RUN_TOKEN", run_token)
            .output()
            .expect("reservation attempt")
    };
    let reserve_arguments = [
        "run",
        "reserve",
        "--mutation-id",
        "11111111-2222-4333-8444-555555555555",
        "--kind",
        "skill_generator",
        "--units",
        "1",
    ];

    let forged = attempt(&reserve_arguments, "an-entirely-wrong-token");
    assert!(!forged.status.success());
    let zero_units = attempt(
        &[
            "run",
            "reserve",
            "--mutation-id",
            "11111111-2222-4333-8444-555555555555",
            "--kind",
            "skill_generator",
            "--units",
            "0",
        ],
        &token,
    );
    assert!(!zero_units.status.success());
    let unknown_mutation = attempt(
        &[
            "run",
            "confirm",
            "--mutation-id",
            "not-a-uuid",
            "--issue-id",
            "x",
        ],
        &token,
    );
    assert!(!unknown_mutation.status.success());

    // None of the refusals touched the ledger.
    let survivor = attempt(&reserve_arguments, &token);
    assert!(survivor.status.success(), "{survivor:?}");
    let state_json: serde_json::Value =
        serde_json::from_slice(&survivor.stdout).expect("reserve JSON");
    assert_eq!(state_json["state"], "reserved");
    assert_eq!(state_json["generated_work"]["reserved"], 1);
    assert_eq!(state_json["generated_work"]["consumed"], 0);
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
