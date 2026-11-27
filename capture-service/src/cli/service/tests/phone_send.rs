//! Signed Android Send through production receiver startup and two real ACP transports.

use super::{Features, Paths, paths, prepare};
use crate::{PairingRegistry, TlsIdentity, time::now_ms};
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

    // Each runtime owns all receiver tasks, including accepted connections. Dropping it
    // between phases forces the next startup to rebuild solely from the same private stores.
    fn receiver(&self, listener: &TcpListener) -> tokio::runtime::Runtime {
        let features = Features {
            receiver: true,
            phone_control: true,
            attention: false,
            runs: false,
            transcription: false,
            push: false,
        };
        let prepared = prepare(self.paths.clone(), features).unwrap();
        let service = prepared.phone_control.unwrap();
        let (receiver, _, identity) = prepared.receiver.unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let sockets = service
                .bind(
                    &self.paths.phone_control_admin_socket(),
                    &self.paths.phone_control_socket(),
                    &self.paths.operator_capability(),
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
            let server =
                axum_server::from_tcp_rustls(listener.try_clone().unwrap(), config).unwrap();
            tokio::spawn(server.serve(receiver.router().into_make_service()));
        });
        runtime
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

struct Editor {
    child: Child,
    reports: mpsc::Receiver<String>,
    targets: Value,
}

impl Editor {
    fn start(desktop: &Desktop, receipts: &Path, text: &str) -> Self {
        fs::create_dir(receipts).unwrap();
        let nvim = PathBuf::from(env::var_os("LOUISELM_TEST_NVIM").unwrap());
        let mut child = desktop
            .isolated(&nvim)
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
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
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

    fn report(&self) -> Value {
        let line = self
            .reports
            .recv_timeout(Duration::from_secs(30))
            .expect("no Neovim receipt");
        serde_json::from_str(&line).unwrap()
    }

    fn finish(mut self) {
        self.child
            .stdin
            .take()
            .unwrap()
            .write_all(b"stop\n")
            .unwrap();
        let result = self.report();
        assert_eq!(
            result["selected_prompts"], 1,
            "exactly one ACP prompt to the selected Agent"
        );
        assert_eq!(result["other_prompts"], 0);
        assert_eq!(result["selected_turns"], 1);
        assert_eq!(result["other_turns"], 0);
        assert!(
            result["transcript_mentions"].as_u64().unwrap() >= 2,
            "desktop transcript lacks the prompt or reply"
        );
        assert_eq!(result["draft_preserved"], true);
        assert!(self.child.wait().unwrap().success());
    }
}

impl Drop for Editor {
    fn drop(&mut self) {
        // A failing Android assertion must also release the disposable editor and mock Agents.
        drop(self.child.stdin.take());
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
    fn screen(
        &self,
        desktop: &Desktop,
        editor: &Editor,
        phase: &str,
        text: &str,
        old: Option<&Value>,
    ) {
        let android = desktop.project.join("android");
        let report = android
            .join("app/build/test-results/testDebugUnitTest")
            .join(format!("TEST-{TEST_CLASS}.xml"));
        let mut gradle = Command::new(android.join("gradlew"));
        if let Some(targets) = old {
            gradle
                .env(
                    "LOUISELM_E2E_OLD_CONTROLLER",
                    targets["controller_id"].as_str().unwrap(),
                )
                .env(
                    "LOUISELM_E2E_OLD_SUBJECT",
                    targets["subject_id"].as_str().unwrap(),
                );
        }
        let status = gradle
            .current_dir(&android)
            .args([
                "--no-daemon",
                "--max-workers=1",
                "-PlouiselmFirebase=false",
                "testDebugUnitTest",
                "--rerun",
                "--tests",
                TEST_CLASS,
            ])
            .env("LOUISELM_E2E_URL", &self.url)
            .env("LOUISELM_E2E_RECEIVER", &self.identity)
            .env("LOUISELM_E2E_CREDENTIAL", &self.credential)
            .env("LOUISELM_E2E_DEVICE", &self.device)
            .env("LOUISELM_E2E_GRANT", &self.grant)
            .env("LOUISELM_E2E_GRANT_REVISION", self.revision.to_string())
            .env(
                "LOUISELM_E2E_CONTROLLER",
                editor.targets["controller_id"].as_str().unwrap(),
            )
            .env(
                "LOUISELM_E2E_SUBJECT",
                editor.targets["subject_id"].as_str().unwrap(),
            )
            .env(
                "LOUISELM_E2E_OTHER_SUBJECT",
                editor.targets["other_subject_id"].as_str().unwrap(),
            )
            .env("LOUISELM_E2E_SEND_PHASE", phase)
            .env("LOUISELM_E2E_SEND_TEXT", text)
            .status()
            .unwrap();
        assert!(status.success(), "Android Send failed during {phase}");
        let xml = fs::read_to_string(report).unwrap();
        for count in [
            "tests=\"1\"",
            "failures=\"0\"",
            "errors=\"0\"",
            "skipped=\"0\"",
        ] {
            assert!(
                xml.contains(count),
                "missing Android execution receipt for {phase}: {count}"
            );
        }
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
    let first = desktop.receiver(&listener);
    let phone = desktop.enroll(&url);
    let first_text = "First Send from the phone — café";
    let editor = Editor::start(&desktop, &root.path().join("first"), first_text);
    let old_targets = editor.targets.clone();
    phone.screen(&desktop, &editor, "first", first_text, None);
    editor.finish();
    drop(first);

    let _second = desktop.receiver(&listener);
    let next_text = "Second Send after restarting — café";
    let editor = Editor::start(&desktop, &root.path().join("second"), next_text);
    assert_ne!(
        old_targets["controller_id"],
        editor.targets["controller_id"]
    );
    assert_ne!(old_targets["subject_id"], editor.targets["subject_id"]);
    phone.screen(&desktop, &editor, "lost_ack", next_text, Some(&old_targets));
    let disabled = desktop.admin(&["disable", "--device-id", &phone.device]);
    assert_eq!(disabled["type"], "disabled");
    phone.screen(&desktop, &editor, "disabled", next_text, None);
    editor.finish();
}
