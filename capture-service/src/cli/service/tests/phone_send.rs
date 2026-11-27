//! Signed Android Send through production receiver startup and two real ACP transports.

use super::{Features, Paths, paths, prepare};
use crate::{
    PairingRegistry, TlsIdentity,
    phone_control::{ActionEvidenceKind, ActionEvidenceStore, ActionOutcome},
    time::now_ms,
};
use serde_json::{Value, json};
use std::{
    env, fs,
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::Duration,
};

const TEST_CLASS: &str = "dev.louiselm.capture.PhoneSendEndToEndTest";
const UPGRADE_TEST_CLASS: &str = "dev.louiselm.capture.PhoneSendUpgradeEndToEndTest";
const DRAFT_TEST_CLASS: &str = "dev.louiselm.capture.PhoneDraftSendEndToEndTest";
const FIRST_TEXT: &str = "First Send from the phone — café";
const NEXT_TEXT: &str = "Second Send after restarting — café";

struct Desktop {
    project: PathBuf,
    binary: PathBuf,
    paths: Paths,
}

impl Desktop {
    fn isolated(&self, program: &Path) -> Command {
        let mut cmd = Command::new(program);
        cmd.env_clear()
            .env("HOME", &self.paths.config)
            .env("PATH", "/usr/bin:/bin")
            .env("LANG", "C.UTF-8")
            .env("LOUISELM_CAPTURE_CONFIG_DIR", &self.paths.config)
            .env("LOUISELM_CAPTURE_DATA_DIR", &self.paths.data)
            .env("LOUISELM_CAPTURE_STATE_DIR", &self.paths.state);
        cmd
    }

    fn admin(&self, args: &[&str]) -> Value {
        let output = self
            .isolated(&self.binary)
            .arg("phone-control")
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "desktop {} failed: {}",
            args[0],
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    fn start_receiver(
        &self,
        listener: &TcpListener,
        directory: &Path,
        read_only: bool,
    ) -> ReceiverProcess {
        fs::create_dir(directory).unwrap();
        let fd = std::os::fd::OwnedFd::from(listener.try_clone().unwrap());
        let child = self
            .isolated(&env::current_exe().unwrap())
            .args([
                "--exact",
                concat!(module_path!(), "::serve_disposable_receiver")
                    .split_once("::")
                    .unwrap()
                    .1,
                "--ignored",
            ])
            .env("LOUISELM_TEST_RECEIVER_DIR", directory)
            .env(
                "LOUISELM_TEST_RECEIVER_READ_ONLY",
                if read_only { "1" } else { "0" },
            )
            .stdin(Stdio::from(fd))
            .spawn()
            .unwrap();
        let mut process = ReceiverProcess {
            child,
            directory: directory.to_owned(),
        };
        wait_for_file(
            &directory.join("ready"),
            &mut process.child,
            Duration::from_secs(30),
        );
        process
    }

    fn enroll(&self, url: &str) -> Phone {
        let tls = TlsIdentity::load_existing(&self.paths.tls()).unwrap();
        let cert =
            reqwest::Certificate::from_pem(&fs::read(tls.certificate_path()).unwrap()).unwrap();
        let https = reqwest::blocking::Client::builder()
            .no_proxy()
            .tls_certs_only([cert])
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap();
        let pairing = PairingRegistry::open(self.paths.pairing()).unwrap();
        let offer = pairing
            .issue(url, tls.public_key_sha256(), now_ms(), 60_000)
            .unwrap();
        let paired: Value = https
            .post(format!("{url}/v1/pair"))
            .json(&json!({"token":offer.token, "device_name":"Disposable Send phone"}))
            .send()
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .unwrap();
        let device = paired["device_id"].as_str().unwrap();
        let credential = paired["credential"].as_str().unwrap();
        let started = self.admin(&["start", "--device-id", device]);
        assert_eq!(started["type"], "enrollment_started");
        let enrollment = started["enrollment_id"].as_str().unwrap();
        let keys: Value = serde_json::from_str(include_str!(
            "../../../../../android/app/src/test/resources/phone-control-v1.json"
        ))
        .unwrap();
        let enrolled_key = keys["keys"]["enrolled"]["sec1"].as_str().unwrap();
        let post = |path: &str, message: &Value| -> Value {
            https
                .post(format!("{url}/v1/phone-control/enrollment/{path}"))
                .bearer_auth(credential)
                .json(message)
                .send()
                .unwrap()
                .error_for_status()
                .unwrap()
                .json()
                .unwrap()
        };
        let submitted = post(
            "submit",
            &json!({"v":1,"type":"enrollment_submit","enrollment_id":enrollment,
            "matching_code":started["matching_code"], "enrolled_key":enrolled_key}),
        );
        assert_eq!(submitted["type"], "enrollment_pending");
        let approved = self.admin(&[
            "approve",
            "--device-id",
            device,
            "--enrollment-id",
            enrollment,
            "--enrolled-key",
            enrolled_key,
            "--project",
            self.project.to_str().unwrap(),
            "--agent",
            "codex",
            "--group",
            "read",
            "--group",
            "operate_sessions",
        ]);
        let granted = post(
            "status",
            &json!({"v":1,"type":"enrollment_status","enrollment_id":enrollment}),
        );
        assert_eq!(granted["type"], "enrollment_granted");
        assert_eq!(approved["grant_id"], granted["grant_id"]);
        Phone {
            url: url.to_owned(),
            identity: tls.public_key_sha256().to_owned(),
            device: device.to_owned(),
            credential: credential.to_owned(),
            grant: granted["grant_id"].as_str().unwrap().to_owned(),
            revision: granted["grant_revision"].as_u64().unwrap(),
        }
    }
}

// Keep the same ephemeral loopback endpoint across real process restarts. The
// child inherits the reserved listener instead of binding a workstation port.
#[test]
#[ignore = "receiver subprocess of the Android Send acceptance"]
fn serve_disposable_receiver() {
    use std::os::fd::AsFd;
    let Some(directory) = env::var_os("LOUISELM_TEST_RECEIVER_DIR").map(PathBuf::from) else {
        return;
    };
    let paths = paths(directory.parent().unwrap());
    let listener = TcpListener::from(std::io::stdin().as_fd().try_clone_to_owned().unwrap());
    let features = Features {
        receiver: true,
        phone_control: true,
        attention: false,
        runs: false,
        transcription: false,
        push: false,
    };
    let prepared = prepare(paths.clone(), features).unwrap();
    let (receiver, _, identity) = prepared.receiver.unwrap();
    let service = prepared.phone_control.unwrap();
    // Exercise the production read-only startup branch without damaging the
    // disposable evidence store needed by the later normal receiver.
    let (service, receiver) = if env::var("LOUISELM_TEST_RECEIVER_READ_ONLY").as_deref() == Ok("1")
    {
        drop(service);
        let service = crate::phone_control::Service::open_without_registrar(
            &paths.phone_control_store(),
            identity.public_key_sha256(),
            prepared.pairing.unwrap(),
        )
        .unwrap();
        let receiver = receiver.with_phone_control(service.router());
        (service, receiver)
    } else {
        (service, receiver)
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let sockets = service
            .bind(
                &paths.phone_control_admin_socket(),
                &paths.phone_control_socket(),
                &paths.operator_capability(),
            )
            .await
            .unwrap();
        tokio::spawn(sockets.serve());
        let config = axum_server::tls_rustls::RustlsConfig::from_pem_file(
            identity.certificate_path(),
            identity.private_key_path(),
        )
        .await
        .unwrap();
        let server = axum_server::from_tcp_rustls(listener, config).unwrap();
        tokio::spawn(server.serve(receiver.router().into_make_service()));
    });

    fs::write(directory.join("ready"), []).unwrap();
    let started = std::time::Instant::now();
    while !directory.join("stop").is_file() {
        assert!(
            started.elapsed() < Duration::from_mins(15),
            "fixture owner disappeared"
        );
        thread::sleep(Duration::from_millis(25));
    }
}

struct ReceiverProcess {
    child: Child,
    directory: PathBuf,
}

impl ReceiverProcess {
    fn stop(mut self) {
        fs::write(self.directory.join("stop"), []).unwrap();
        assert!(self.child.wait().unwrap().success());
    }
}

impl Drop for ReceiverProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn wait_for_file(path: &Path, child: &mut Child, timeout: Duration) {
    let start = std::time::Instant::now();
    while !path.is_file() {
        assert!(
            child.try_wait().unwrap().is_none(),
            "fixture stopped before {}",
            path.display()
        );
        assert!(
            start.elapsed() < timeout,
            "fixture did not reach {}",
            path.display()
        );
        thread::sleep(Duration::from_millis(25));
    }
}

fn completed_owner_evidence(desktop: &Desktop, phone: &Phone, operation: &str) -> Value {
    let owner = ActionEvidenceStore::open_bound(
        desktop
            .paths
            .state
            .join("louiselm/workflow/phone-action-owner"),
        desktop.paths.tls().join("phone-action-owner.json"),
        &phone.identity,
        ActionEvidenceKind::Owner,
    )
    .unwrap();
    let started = std::time::Instant::now();
    loop {
        let record = owner.lookup(operation).unwrap();
        if let ActionOutcome::Completed { evidence } = &record.outcome {
            let [
                ActionOutcome::Accepted { evidence: accepted },
                ActionOutcome::Dispatched {
                    evidence: dispatched,
                },
                ActionOutcome::Completed {
                    evidence: completed,
                },
            ] = record.phases.as_slice()
            else {
                panic!("completed owner record lacks its exact ordered phases");
            };
            assert_eq!(evidence, completed);
            return json!({"operation_id":record.identity.operation_id,
                "controller_id":record.identity.controller_id,"subject_id":record.identity.subject_id,
                "accepted":accepted,"dispatched":dispatched,"completed":completed});
        }
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "owner completion not durable"
        );
        thread::sleep(Duration::from_millis(25));
    }
}

struct Editor {
    child: Child,
    reports: mpsc::Receiver<String>,
    targets: Value,
}

impl Editor {
    fn start(desktop: &Desktop, receipts: &Path, text: &str) -> Self {
        Self::start_with_drafts(desktop, receipts, text, None, false)
    }

    fn start_with_drafts(
        desktop: &Desktop,
        receipts: &Path,
        text: &str,
        sync: Option<&Path>,
        restarted: bool,
    ) -> Self {
        fs::create_dir(receipts).unwrap();
        let nvim = PathBuf::from(env::var_os("LOUISELM_TEST_NVIM").unwrap());
        let mut command = desktop.isolated(&nvim);
        command
            .args(["--headless", "--noplugin", "-u", "NONE", "-l"])
            .arg(
                desktop
                    .project
                    .join("tests/fixtures/phone_send_headless.lua"),
            )
            .env("LOUISELM_TEST_CAPTURE", &desktop.binary)
            .env("LOUISELM_TEST_PROJECT", &desktop.project)
            .env("LOUISELM_TEST_RECEIPTS", receipts)
            .env("LOUISELM_TEST_SEND_TEXT", text)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        if let Some(directory) = sync {
            command.env("LOUISELM_TEST_DRAFT_SYNC", directory);
            if restarted {
                command.env("LOUISELM_TEST_DRAFT_RESTARTED", "1");
            }
        }
        let mut child = command.spawn().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (writer, reports) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if writer.send(line).is_err() {
                    return;
                }
            }
        });
        let mut editor = Self {
            child,
            reports,
            targets: Value::Null,
        };
        editor.targets = editor.report();
        assert_ne!(
            editor.targets["subject_id"],
            editor.targets["other_subject_id"]
        );
        editor
    }

    fn desktop_turn(&mut self, step: &str) {
        writeln!(self.child.stdin.as_mut().unwrap(), "{step}").unwrap();
        assert_eq!(self.report()["step"], step);
    }

    fn report(&self) -> Value {
        let line = self
            .reports
            .recv_timeout(Duration::from_secs(30))
            .expect("no Neovim receipt");
        serde_json::from_str(&line).unwrap()
    }

    fn finish(mut self, expected_prompts: u64) {
        self.child
            .stdin
            .take()
            .unwrap()
            .write_all(b"stop\n")
            .unwrap();
        let result = self.report();
        assert_eq!(
            result["selected_prompts"], expected_prompts,
            "ACP prompt count for the selected Agent"
        );
        assert_eq!(result["other_prompts"], 0);
        assert_eq!(result["selected_turns"], expected_prompts);
        assert_eq!(result["other_turns"], 0);
        if expected_prompts > 0 {
            assert!(
                result["transcript_mentions"].as_u64().unwrap() >= 2,
                "desktop transcript lacks the prompt or reply"
            );
        }
        assert_eq!(result["draft_preserved"], true);
        assert!(self.child.wait().unwrap().success());
    }
}

impl Drop for Editor {
    fn drop(&mut self) {
        // Give the fixture a chance to dispose and reap its mock Agents even
        // after an Android assertion fails; kill only if that fails or stalls.
        drop(self.child.stdin.take());
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) if std::time::Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(25));
                }
                _ => break,
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Phone {
    url: String,
    identity: String,
    device: String,
    credential: String,
    grant: String,
    revision: u64,
}

impl Phone {
    fn start_android(
        &self,
        desktop: &Desktop,
        directory: &Path,
        first: &Value,
        test_class: &str,
    ) -> MobileRun {
        let android = desktop.project.join("android");
        let report = android
            .join("app/build/test-results/testDebugUnitTest")
            .join(format!("TEST-{test_class}.xml"));
        let child = Command::new(android.join("gradlew"))
            .current_dir(&android)
            .args([
                "--no-daemon",
                "--max-workers=1",
                "-PlouiselmFirebase=false",
                "testDebugUnitTest",
                "--rerun",
                "--tests",
                test_class,
            ])
            .env("LOUISELM_E2E_URL", &self.url)
            .env("LOUISELM_E2E_RECEIVER", &self.identity)
            .env("LOUISELM_E2E_CREDENTIAL", &self.credential)
            .env("LOUISELM_E2E_DEVICE", &self.device)
            .env("LOUISELM_E2E_GRANT", &self.grant)
            .env("LOUISELM_E2E_GRANT_REVISION", self.revision.to_string())
            .env(
                "LOUISELM_E2E_OLD_CONTROLLER",
                first["controller_id"].as_str().unwrap(),
            )
            .env(
                "LOUISELM_E2E_OLD_SUBJECT",
                first["subject_id"].as_str().unwrap(),
            )
            .env(
                "LOUISELM_E2E_OLD_OTHER_SUBJECT",
                first["other_subject_id"].as_str().unwrap(),
            )
            .env("LOUISELM_E2E_SEND_SYNC", directory)
            .env("LOUISELM_E2E_SEND_TEXT", FIRST_TEXT)
            .env("LOUISELM_E2E_NEXT_TEXT", NEXT_TEXT)
            .spawn()
            .unwrap();
        MobileRun {
            child,
            report,
            directory: directory.to_owned(),
        }
    }
}

struct MobileRun {
    child: Child,
    report: PathBuf,
    directory: PathBuf,
}

impl MobileRun {
    fn wait_for(&mut self, step: &str) {
        wait_for_file(
            &self.directory.join(step),
            &mut self.child,
            Duration::from_mins(10),
        );
    }

    fn complete(&self, step: &str) {
        fs::write(self.directory.join(format!("{step}.done")), []).unwrap();
    }

    fn finish(mut self) {
        assert!(
            self.child.wait().unwrap().success(),
            "Android Send test failed"
        );
        let xml = fs::read_to_string(&self.report).unwrap();
        for count in [
            "tests=\"1\"",
            "failures=\"0\"",
            "errors=\"0\"",
            "skipped=\"0\"",
        ] {
            assert!(
                xml.contains(count),
                "missing Android execution receipt: {count}"
            );
        }
    }
}

impl Drop for MobileRun {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
#[ignore = "requires the Android SDK, Gradle, stock Neovim, and built capture executable"]
fn one_enrollment_sends_to_selected_agents_across_restarts() {
    let root = tempfile::tempdir_in("/tmp").unwrap(); // Short Unix socket paths.
    let desktop = Desktop {
        paths: paths(root.path()),
        project: fs::canonicalize(concat!(env!("CARGO_MANIFEST_DIR"), "/..")).unwrap(),
        binary: PathBuf::from(env::var_os("LOUISELM_TEST_CAPTURE").unwrap()),
    };
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!(
        "https://localhost:{}",
        listener.local_addr().unwrap().port()
    );
    let first = desktop.start_receiver(&listener, &root.path().join("receiver-one"), false);
    let phone = desktop.enroll(&url);
    let editor = Editor::start(&desktop, &root.path().join("first"), FIRST_TEXT);
    let old_targets = editor.targets.clone();
    let mut android = phone.start_android(&desktop, root.path(), &old_targets, TEST_CLASS);
    android.wait_for("restart");
    let operation = fs::read_to_string(root.path().join("first-operation")).unwrap();
    let proof = completed_owner_evidence(&desktop, &phone, operation.trim());
    fs::write(root.path().join("first-proof.json"), proof.to_string()).unwrap();
    editor.finish(1);
    first.stop();

    let second = desktop.start_receiver(&listener, &root.path().join("receiver-two"), false);
    android.complete("restart");
    android.wait_for("start-editor");
    let editor = Editor::start(&desktop, &root.path().join("second"), NEXT_TEXT);
    assert_ne!(
        old_targets["controller_id"],
        editor.targets["controller_id"]
    );
    assert_ne!(old_targets["subject_id"], editor.targets["subject_id"]);
    android.complete("start-editor");
    android.wait_for("disable");
    let disabled = desktop.admin(&["disable", "--device-id", &phone.device]);
    assert_eq!(disabled["type"], "disabled");
    android.complete("disable");
    android.finish();
    editor.finish(1);
    second.stop();
}

#[test]
#[ignore = "requires the Android SDK, Gradle, stock Neovim, and built capture executable"]
fn unavailable_send_then_reviewed_send_after_enablement() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let desktop = Desktop {
        paths: paths(root.path()),
        project: fs::canonicalize(concat!(env!("CARGO_MANIFEST_DIR"), "/..")).unwrap(),
        binary: PathBuf::from(env::var_os("LOUISELM_TEST_CAPTURE").unwrap()),
    };
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!(
        "https://localhost:{}",
        listener.local_addr().unwrap().port()
    );
    let first = desktop.start_receiver(&listener, &root.path().join("receiver-read-only"), true);
    let phone = desktop.enroll(&url);
    let editor = Editor::start(&desktop, &root.path().join("before"), FIRST_TEXT);
    let old_targets = editor.targets.clone();
    let mut android = phone.start_android(&desktop, root.path(), &old_targets, UPGRADE_TEST_CLASS);
    android.wait_for("enable-send");
    editor.finish(0);
    first.stop();

    let second = desktop.start_receiver(&listener, &root.path().join("receiver-enabled"), false);
    let editor = Editor::start(&desktop, &root.path().join("after"), FIRST_TEXT);
    assert_ne!(
        old_targets["controller_id"],
        editor.targets["controller_id"]
    );
    assert_ne!(old_targets["subject_id"], editor.targets["subject_id"]);
    android.complete("enable-send");
    android.finish();
    editor.finish(1);
    second.stop();
}

#[test]
#[ignore = "requires the Android SDK, Gradle, stock Neovim, and built capture executable"]
fn reviewed_drafts_survive_send_orderings_and_reconcile() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let desktop = Desktop {
        paths: paths(root.path()),
        project: fs::canonicalize(concat!(env!("CARGO_MANIFEST_DIR"), "/..")).unwrap(),
        binary: PathBuf::from(env::var_os("LOUISELM_TEST_CAPTURE").unwrap()),
    };
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!(
        "https://localhost:{}",
        listener.local_addr().unwrap().port()
    );
    let receiver = desktop.start_receiver(&listener, &root.path().join("receiver"), false);
    let phone = desktop.enroll(&url);
    let mut editor = Editor::start_with_drafts(
        &desktop,
        &root.path().join("editor"),
        FIRST_TEXT,
        Some(root.path()),
        false,
    );
    let mut android = phone.start_android(&desktop, root.path(), &editor.targets, DRAFT_TEST_CLASS);
    android.wait_for("desktop-before");
    editor.desktop_turn("desktop-before");
    android.complete("desktop-before");
    android.wait_for("first-complete");
    let operation = fs::read_to_string(root.path().join("first-operation")).unwrap();
    completed_owner_evidence(&desktop, &phone, operation.trim());
    android.complete("first-complete");
    android.wait_for("desktop-after");
    editor.desktop_turn("desktop-after");
    android.complete("desktop-after");
    android.wait_for("second-complete");
    let operation = fs::read_to_string(root.path().join("second-operation")).unwrap();
    completed_owner_evidence(&desktop, &phone, operation.trim());
    let old_targets = editor.targets.clone();
    editor.finish(5);
    receiver.stop();
    let receiver =
        desktop.start_receiver(&listener, &root.path().join("receiver-restarted"), false);
    let mut editor = Editor::start_with_drafts(
        &desktop,
        &root.path().join("editor-restarted"),
        FIRST_TEXT,
        Some(root.path()),
        true,
    );
    assert_ne!(
        old_targets["controller_id"],
        editor.targets["controller_id"]
    );
    assert_ne!(old_targets["subject_id"], editor.targets["subject_id"]);
    android.complete("second-complete");
    android.wait_for("desktop-conflict");
    editor.desktop_turn("desktop-conflict");
    android.complete("desktop-conflict");
    android.finish();
    editor.finish(1);
    receiver.stop();
}
