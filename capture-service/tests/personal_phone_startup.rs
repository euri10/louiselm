//! Personal receiver startup exercised through the shipped CLI.
#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "disposable process fixtures and assertions"
)]

use std::{
    fs,
    io::Write,
    net::TcpStream,
    os::unix::{fs::PermissionsExt, net::UnixStream},
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use louiselm_capture::{ActionIdentity, PairingRegistry, TlsIdentity};
use serde_json::{Value, json};

fn command(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_louiselm-capture"));
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LOUISELM_CAPTURE_CONFIG_DIR", root)
        .env("LOUISELM_CAPTURE_DATA_DIR", root)
        .env("LOUISELM_CAPTURE_STATE_DIR", root);
    command
}

struct Service(Child);

impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start(root: &Path) -> Service {
    let child = command(root)
        .arg("serve")
        .env("LOUISELM_RECEIVER_ENABLED", "true")
        .env("LOUISELM_PHONE_CONTROL_ENABLED", "true")
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut service = Service(child);
    let socket = root.join("louiselm/workflow/phone-control-admin.sock");
    let deadline = Instant::now() + Duration::from_secs(15);
    while UnixStream::connect(&socket).is_err() {
        assert!(service.0.try_wait().unwrap().is_none(), "service stopped");
        assert!(Instant::now() < deadline, "service did not start");
        thread::sleep(Duration::from_millis(20));
    }
    service
}

fn start_capture_only(root: &Path) -> Service {
    let child = command(root)
        .arg("serve")
        .env("LOUISELM_RECEIVER_ENABLED", "true")
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut service = Service(child);
    let deadline = Instant::now() + Duration::from_secs(15);
    while TcpStream::connect("127.0.0.1:7391").is_err() {
        assert!(service.0.try_wait().unwrap().is_none(), "service stopped");
        assert!(Instant::now() < deadline, "capture service did not start");
        thread::sleep(Duration::from_millis(20));
    }
    service
}

fn cli(root: &Path, args: &[&str]) -> Value {
    let output = command(root).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn journal(root: &Path, request: &Value) -> Value {
    let mut child = command(root)
        .args(["phone-control", "owner-journal"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(request.to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    serde_json::from_slice(&output.stdout).unwrap()
}

fn unavailable_disable(root: &Path, receiver: &str) {
    let pairing = PairingRegistry::open(root.join("louiselm/capture/pairing")).unwrap();
    let offer = pairing
        .issue("https://localhost:7391", receiver, 1_000, 5_000)
        .unwrap();
    let phone = pairing.consume(&offer.token, "test phone", 1_001).unwrap();
    let output = command(root)
        .args(["phone-control", "disable", "--device-id", &phone.device_id])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("phone-control request refused: unavailable"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn refuse_missing_receiver(root: &Path) {
    let mut child = command(root)
        .arg("serve")
        .env("LOUISELM_RECEIVER_ENABLED", "true")
        .env("LOUISELM_PHONE_CONTROL_ENABLED", "true")
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("receiver started after losing its identity");
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert!(!child.wait_with_output().unwrap().status.success());
    let tls = root.join("louiselm/capture/tls");
    assert!(!tls.join("receiver-key.pem").exists());
    assert!(!tls.join("receiver-cert.pem").exists());
}

#[test]
#[ignore = "requires private mount and network namespaces, isolated /run and loopback"]
#[expect(
    clippy::too_many_lines,
    reason = "one lifecycle retains the approved grant and action across legacy migration and restart"
)]
fn approved_phone_and_native_journal_survive_restart_but_missing_evidence_does_not_reset() {
    let root = tempfile::tempdir().unwrap();
    let service = start(root.path());
    let tls = root.path().join("louiselm/capture/tls");
    let identity = TlsIdentity::load_or_create(&tls).unwrap();
    let receiver = identity.public_key_sha256().to_owned();
    assert!(tls.join("phone-action-evidence.json").is_file());

    let pairing = PairingRegistry::open(root.path().join("louiselm/capture/pairing")).unwrap();
    let now = u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    let offer = pairing
        .issue("https://localhost:7391", &receiver, now, 60_000)
        .unwrap();
    let phone = pairing
        .consume(&offer.token, "test phone", now + 1)
        .unwrap();
    let enrollment = cli(
        root.path(),
        &["phone-control", "start", "--device-id", &phone.device_id],
    );
    let enrollment_id = enrollment["enrollment_id"].as_str().unwrap();
    let code = enrollment["matching_code"].as_str().unwrap();
    let keys: Value = serde_json::from_str(include_str!(
        "../../android/app/src/test/resources/phone-control-v1.json"
    ))
    .unwrap();
    let key = keys["keys"]["enrolled"]["sec1"].as_str().unwrap();
    let cert =
        reqwest::Certificate::from_pem(&fs::read(identity.certificate_path()).unwrap()).unwrap();
    let client = reqwest::blocking::Client::builder()
        .tls_certs_only([cert])
        .build()
        .unwrap();
    let submitted: Value = client.post("https://localhost:7391/v1/phone-control/enrollment/submit")
        .bearer_auth(&phone.credential)
        .json(&json!({"v":1,"type":"enrollment_submit","enrollment_id":enrollment_id,"matching_code":code,"enrolled_key":key}))
        .send().unwrap().json().unwrap();
    assert_eq!(submitted["type"], "enrollment_pending");
    let project = fs::canonicalize(root.path()).unwrap();
    let approved = cli(
        root.path(),
        &[
            "phone-control",
            "approve",
            "--device-id",
            &phone.device_id,
            "--enrollment-id",
            enrollment_id,
            "--enrolled-key",
            key,
            "--project",
            project.to_str().unwrap(),
            "--agent",
            "codex",
            "--group",
            "read",
            "--group",
            "operate_sessions",
        ],
    );
    assert_eq!(approved["type"], "enrollment_approved");

    // Reproduce an already-approved v3 installation from before Send existed.
    // Its freshly initialized stores have never admitted an action.
    drop(service);
    let owner = root.path().join("louiselm/workflow/phone-action-owner");
    let archive = root
        .path()
        .join("louiselm/capture/phone-control/action-evidence");
    fs::remove_dir_all(&owner).unwrap();
    fs::remove_file(owner.with_extension("lock")).unwrap();
    fs::remove_dir_all(&archive).unwrap();
    fs::remove_file(archive.with_extension("lock")).unwrap();
    for name in [
        "phone-action-owner.json",
        "phone-action-owner.json.lock",
        "phone-action-evidence.json",
        "phone-action-evidence.json.lock",
        "phone-admission-barrier.json",
        "phone-admission-barrier.lock",
        "phone-admission-registrar.lock",
        "phone-admission-cutoff.json",
    ] {
        fs::remove_file(tls.join(name)).unwrap();
    }
    let pairing_file = root.path().join("louiselm/capture/pairing/pairing.json");
    let mut legacy: Value = serde_json::from_slice(&fs::read(&pairing_file).unwrap()).unwrap();
    legacy["schema_version"] = json!(3);
    legacy.as_object_mut().unwrap().remove("personal_send");
    fs::write(&pairing_file, serde_json::to_vec(&legacy).unwrap()).unwrap();
    let service = start(root.path());
    let guard: Value =
        serde_json::from_slice(&fs::read(tls.join("phone-action-evidence.json")).unwrap()).unwrap();
    let generation = guard["generation"].as_str().unwrap();
    let grant = cli(
        root.path(),
        &[
            "phone-control",
            "inspect-grant",
            "--device-id",
            &phone.device_id,
        ],
    );
    assert_eq!(grant["grant_id"], approved["grant_id"]);

    let action = ActionIdentity {
        operation_id: "01".repeat(16),
        receiver_identity: receiver,
        controller_id: "03".repeat(16),
        subject_id: "04".repeat(16),
        action: 3,
        payload_hash: "05".repeat(32),
        grant_id: approved["grant_id"].as_str().unwrap().into(),
        grant_revision: approved["grant_revision"].as_u64().unwrap(),
        period_id: "07".repeat(16),
        expected_revision: 2,
        project: project.to_str().unwrap().into(),
        agent: "codex".into(),
        recorded_at_ms: now,
    };
    let begin = json!({"v":2,"type":"begin","generation":generation,"identity":action});
    assert_eq!(journal(root.path(), &begin)["type"], "begun");
    drop(service);
    let service = start(root.path());
    let grant = cli(
        root.path(),
        &[
            "phone-control",
            "inspect-grant",
            "--device-id",
            &phone.device_id,
        ],
    );
    assert_eq!(grant["grant_id"], approved["grant_id"]);
    assert_eq!(journal(root.path(), &begin)["type"], "existing");
    drop(service);

    fs::remove_dir_all(&owner).unwrap();
    fs::remove_file(owner.with_extension("lock")).unwrap();
    let _service = start(root.path());
    assert_eq!(journal(root.path(), &begin)["code"], "unavailable");
    assert!(!owner.exists());
}

#[test]
#[ignore = "requires private mount and network namespaces, isolated /run and loopback"]
#[expect(
    clippy::too_many_lines,
    reason = "the same real-service sequence compares each independently missing authority artifact"
)]
fn missing_or_incomplete_state_never_reopens_write_registration() {
    for missing in [
        "archive",
        "archive_guard",
        "owner",
        "owner_guard",
        "owner_guard_lock",
        "barrier",
        "barrier_and_lock",
        "starting",
        "pairing",
        "pairing_and_stores",
    ] {
        let root = tempfile::tempdir().unwrap();
        let service = start(root.path());
        let tls = root.path().join("louiselm/capture/tls");
        let identity = TlsIdentity::load_or_create(&tls).unwrap();
        let archive = root
            .path()
            .join("louiselm/capture/phone-control/action-evidence");
        let owner = root.path().join("louiselm/workflow/phone-action-owner");
        let guard: Value =
            serde_json::from_slice(&fs::read(tls.join("phone-action-evidence.json")).unwrap())
                .unwrap();
        let generation = guard["generation"].as_str().unwrap();
        let action = ActionIdentity {
            operation_id: "01".repeat(16),
            receiver_identity: identity.public_key_sha256().into(),
            controller_id: "03".repeat(16),
            subject_id: "04".repeat(16),
            action: 3,
            payload_hash: "05".repeat(32),
            grant_id: "06".repeat(16),
            grant_revision: 1,
            period_id: "07".repeat(16),
            expected_revision: 2,
            project: fs::canonicalize(root.path())
                .unwrap()
                .to_str()
                .unwrap()
                .into(),
            agent: "codex".into(),
            recorded_at_ms: 1_000,
        };
        let begin = json!({"v":2,"type":"begin","generation":generation,"identity":action});
        let inspect = json!({"v":1,"type":"inspect","identity":action});
        assert_eq!(journal(root.path(), &begin)["type"], "begun");
        drop(service);
        match missing {
            "archive" => {
                fs::remove_dir_all(&archive).unwrap();
                fs::remove_file(archive.with_extension("lock")).unwrap();
            }
            "archive_guard" => fs::remove_file(tls.join("phone-action-evidence.json")).unwrap(),
            "owner" => {
                fs::remove_dir_all(&owner).unwrap();
                fs::remove_file(owner.with_extension("lock")).unwrap();
            }
            "owner_guard" => fs::remove_file(tls.join("phone-action-owner.json")).unwrap(),
            "owner_guard_lock" => {
                fs::remove_file(tls.join("phone-action-owner.json.lock")).unwrap();
            }
            "barrier" => fs::remove_file(tls.join("phone-admission-barrier.json")).unwrap(),
            "barrier_and_lock" => {
                fs::remove_file(tls.join("phone-admission-barrier.json")).unwrap();
                fs::remove_file(tls.join("phone-admission-barrier.lock")).unwrap();
            }
            "starting" => {
                let path = root.path().join("louiselm/capture/pairing/pairing.json");
                let mut state: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                state["personal_send"]["phase"] = json!("starting");
                fs::write(path, serde_json::to_vec(&state).unwrap()).unwrap();
            }
            "pairing_and_stores" => {
                fs::remove_dir_all(&archive).unwrap();
                fs::remove_file(archive.with_extension("lock")).unwrap();
                fs::remove_dir_all(&owner).unwrap();
                fs::remove_file(owner.with_extension("lock")).unwrap();
                for name in [
                    "phone-action-owner.json",
                    "phone-action-owner.json.lock",
                    "phone-action-evidence.json",
                    "phone-action-evidence.json.lock",
                    "phone-admission-barrier.json",
                    "phone-admission-barrier.lock",
                    "phone-admission-registrar.lock",
                    "phone-admission-cutoff.json",
                ] {
                    fs::remove_file(tls.join(name)).unwrap();
                }
                fs::remove_file(root.path().join("louiselm/capture/pairing/pairing.json")).unwrap();
            }
            "pairing" => {
                fs::remove_file(root.path().join("louiselm/capture/pairing/pairing.json")).unwrap();
            }
            _ => unreachable!(),
        }
        for _ in 0..2 {
            let service = start(root.path());
            unavailable_disable(root.path(), identity.public_key_sha256());
            if matches!(missing, "archive" | "archive_guard") {
                // Owner evidence remains readable even while capture history is down.
                assert_eq!(journal(root.path(), &inspect)["type"], "existing");
                if missing == "archive" {
                    assert!(!archive.exists());
                }
            } else if matches!(
                missing,
                "owner"
                    | "owner_guard"
                    | "owner_guard_lock"
                    | "barrier"
                    | "barrier_and_lock"
                    | "pairing_and_stores"
            ) {
                assert_eq!(journal(root.path(), &begin)["code"], "unavailable");
            }
            drop(service);
        }
    }
}

#[test]
#[ignore = "requires private mount and network namespaces, isolated /run and loopback"]
fn existing_pairings_never_silently_replace_a_lost_receiver_key() {
    let root = tempfile::tempdir().unwrap();
    let service = start(root.path());
    let tls = root.path().join("louiselm/capture/tls");
    let receiver = TlsIdentity::load_or_create(&tls)
        .unwrap()
        .public_key_sha256()
        .to_owned();
    let pairing = PairingRegistry::open(root.path().join("louiselm/capture/pairing")).unwrap();
    let offer = pairing
        .issue("https://localhost:7391", &receiver, 1_000, 5_000)
        .unwrap();
    let phone = pairing.consume(&offer.token, "test phone", 1_001).unwrap();
    drop(service);
    fs::remove_file(tls.join("receiver-key.pem")).unwrap();
    fs::remove_file(tls.join("receiver-cert.pem")).unwrap();

    refuse_missing_receiver(root.path());
    assert_eq!(
        pairing.status().unwrap().devices[0].device_id,
        phone.device_id
    );
}

#[test]
#[ignore = "requires private mount and network namespaces, isolated /run and loopback"]
fn lost_key_and_pairing_do_not_discard_retained_phone_state() {
    for retained in ["evidence", "grant_history"] {
        let root = tempfile::tempdir().unwrap();
        let service = start(root.path());
        let capture = root.path().join("louiselm/capture");
        let tls = capture.join("tls");
        drop(service);
        fs::remove_file(tls.join("receiver-key.pem")).unwrap();
        fs::remove_file(tls.join("receiver-cert.pem")).unwrap();
        fs::remove_file(capture.join("pairing/pairing.json")).unwrap();
        let grants = capture.join("phone-control/grants.json");
        let grant_history =
            br#"{"grants":{},"revisions":{"11111111-1111-4111-8111-111111111111":1}}"#;
        fs::write(&grants, grant_history).unwrap();
        if retained == "grant_history" {
            let owner = root.path().join("louiselm/workflow/phone-action-owner");
            let archive = capture.join("phone-control/action-evidence");
            fs::remove_dir_all(&owner).unwrap();
            fs::remove_file(owner.with_extension("lock")).unwrap();
            fs::remove_dir_all(&archive).unwrap();
            fs::remove_file(archive.with_extension("lock")).unwrap();
            for name in [
                "phone-action-owner.json",
                "phone-action-owner.json.lock",
                "phone-action-evidence.json",
                "phone-action-evidence.json.lock",
                "phone-admission-barrier.json",
                "phone-admission-barrier.lock",
                "phone-admission-registrar.lock",
                "phone-admission-cutoff.json",
            ] {
                fs::remove_file(tls.join(name)).unwrap();
            }
        }
        refuse_missing_receiver(root.path());
        assert_eq!(fs::read(&grants).unwrap(), grant_history);
        if retained == "evidence" {
            assert!(
                capture
                    .join("phone-control/action-evidence/actions.json")
                    .is_file()
            );
        } else {
            assert!(!capture.join("phone-control/action-evidence").exists());
        }
    }
}

#[test]
#[ignore = "requires private mount and network namespaces, isolated /run and loopback"]
fn interrupted_first_initialization_is_not_retried_as_an_empty_store() {
    let root = tempfile::tempdir().unwrap();
    let service = start_capture_only(root.path());
    let parent = root.path().join("louiselm");
    let capture = parent.join("capture");
    let tls = capture.join("tls");
    let receiver = TlsIdentity::load_or_create(&tls)
        .unwrap()
        .public_key_sha256()
        .to_owned();
    fs::create_dir_all(parent.join("workflow")).unwrap();
    drop(service);

    // Pairing remains writable but the private staging directory cannot be
    // created, interrupting the transaction after its durable Starting marker.
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o500)).unwrap();
    let service = start(root.path());
    let pairing_path = capture.join("pairing/pairing.json");
    let record: Value = serde_json::from_slice(&fs::read(&pairing_path).unwrap()).unwrap();
    assert_eq!(record["personal_send"]["phase"], "starting");
    assert!(!capture.join("phone-control/action-evidence").exists());
    drop(service);
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).unwrap();

    let service = start(root.path());
    unavailable_disable(root.path(), &receiver);
    let record: Value = serde_json::from_slice(&fs::read(pairing_path).unwrap()).unwrap();
    assert_eq!(record["personal_send"]["phase"], "starting");
    assert!(!capture.join("phone-control/action-evidence").exists());
    drop(service);
}

#[test]
#[ignore = "requires private mount and network namespaces, isolated /run and loopback"]
fn concurrent_first_services_converge_on_one_durable_generation() {
    let root = tempfile::tempdir().unwrap();
    let spawn = || {
        Service(
            command(root.path())
                .arg("serve")
                .env("LOUISELM_RECEIVER_ENABLED", "true")
                .env("LOUISELM_PHONE_CONTROL_ENABLED", "true")
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        )
    };
    let mut first = spawn();
    let mut second = spawn();
    let socket = root
        .path()
        .join("louiselm/workflow/phone-control-admin.sock");
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let first_running = first.0.try_wait().unwrap().is_none();
        let second_running = second.0.try_wait().unwrap().is_none();
        assert!(first_running || second_running, "both services failed");
        if first_running != second_running && UnixStream::connect(&socket).is_ok() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "concurrent services did not settle"
        );
        thread::sleep(Duration::from_millis(20));
    }
    drop(first);
    drop(second);
    let _service = start(root.path());
    let tls = root.path().join("louiselm/capture/tls");
    let identity = TlsIdentity::load_or_create(&tls).unwrap();
    let guard: Value =
        serde_json::from_slice(&fs::read(tls.join("phone-action-evidence.json")).unwrap()).unwrap();
    let generation = guard["generation"].as_str().unwrap();
    let pairing: Value = serde_json::from_slice(
        &fs::read(root.path().join("louiselm/capture/pairing/pairing.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(pairing["personal_send"]["phase"], "ready");
    assert_eq!(pairing["personal_send"]["generation"], generation);
    assert_eq!(
        pairing["personal_send"]["receiver"],
        identity.public_key_sha256()
    );
    let action = ActionIdentity {
        operation_id: "01".repeat(16),
        receiver_identity: identity.public_key_sha256().into(),
        controller_id: "03".repeat(16),
        subject_id: "04".repeat(16),
        action: 3,
        payload_hash: "05".repeat(32),
        grant_id: "06".repeat(16),
        grant_revision: 1,
        period_id: "07".repeat(16),
        expected_revision: 2,
        project: fs::canonicalize(root.path())
            .unwrap()
            .to_str()
            .unwrap()
            .into(),
        agent: "codex".into(),
        recorded_at_ms: 1_000,
    };
    let begin = json!({"v":2,"type":"begin","generation":generation,"identity":action});
    assert_eq!(journal(root.path(), &begin)["type"], "begun");
    assert_eq!(journal(root.path(), &begin)["type"], "existing");
}
