//! Pending Send and offline Forget through the personal desktop entrypoints.
#![cfg(target_os = "linux")]
#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "disposable process fixtures and assertions"
)]

use std::{
    env,
    fmt::Write as _,
    fs,
    io::{BufRead, BufReader, Write},
    net::TcpStream,
    os::unix::{net::UnixStream, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use aws_lc_rs::{
    rand::SystemRandom,
    signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair},
};
use louiselm_capture::{ActionEvidenceKind, ActionEvidenceStore, PairingRegistry, TlsIdentity};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const URL: &str = "https://localhost:7391";
const TEXT: &str = "Disposable pending Send";
const OPERATION: &str = "11223344556677889900aabbccddeeff";

struct Process(Child);

impl Drop for Process {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

struct Desktop {
    root: PathBuf,
    project: PathBuf,
}

impl Desktop {
    fn command(&self, program: &Path) -> Command {
        let mut command = Command::new(program);
        command
            .env_clear()
            .env("HOME", &self.root)
            .env("PATH", "/usr/bin:/bin")
            .env("LANG", "C.UTF-8")
            .env("LOUISELM_CAPTURE_CONFIG_DIR", &self.root)
            .env("LOUISELM_CAPTURE_DATA_DIR", &self.root)
            .env("LOUISELM_CAPTURE_STATE_DIR", &self.root);
        command
    }

    fn capture(&self) -> Command {
        self.command(Path::new(env!("CARGO_BIN_EXE_louiselm-capture")))
    }

    fn cli(&self, args: &[&str]) -> String {
        let output = self.capture().args(args).output().unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    fn admin(&self, args: &[&str]) -> Value {
        let mut all = vec!["phone-control"];
        all.extend_from_slice(args);
        serde_json::from_str(&self.cli(&all)).unwrap()
    }

    fn start_receiver(&self) -> Process {
        let mut receiver = Process(
            self.capture()
                .arg("serve")
                .env("LOUISELM_RECEIVER_ENABLED", "true")
                .env("LOUISELM_PHONE_CONTROL_ENABLED", "true")
                .stdout(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let socket = self.root.join("louiselm/workflow/phone-control-admin.sock");
        let deadline = Instant::now() + Duration::from_secs(20);
        while UnixStream::connect(&socket).is_err() || TcpStream::connect("127.0.0.1:7391").is_err()
        {
            assert!(receiver.0.try_wait().unwrap().is_none(), "receiver exited");
            assert!(Instant::now() < deadline, "receiver did not start");
            thread::sleep(Duration::from_millis(20));
        }
        receiver
    }

    fn owner(&self, request: &Value) -> Value {
        let mut child = self
            .capture()
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

    fn enroll(&self, client: &reqwest::blocking::Client, fixture: &Value) -> Phone {
        let tls = TlsIdentity::load_or_create(self.root.join("louiselm/capture/tls")).unwrap();
        let receiver = tls.public_key_sha256().to_owned();
        let registry = PairingRegistry::open(self.root.join("louiselm/capture/pairing")).unwrap();
        let now = u64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis(),
        )
        .unwrap();
        let offer = registry.issue(URL, &receiver, now, 60_000).unwrap();
        let paired: Value = client
            .post(format!("{URL}/v1/pair"))
            .json(&json!({"token":offer.token,"device_name":"disposable phone"}))
            .send()
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .unwrap();
        let device = paired["device_id"].as_str().unwrap().to_owned();
        let credential = paired["credential"].as_str().unwrap().to_owned();
        let started = self.admin(&["start", "--device-id", &device]);
        let enrollment = started["enrollment_id"].as_str().unwrap();
        let enrolled_key = fixture["keys"]["enrolled"]["sec1"].as_str().unwrap();
        let pending = post(
            client,
            &credential,
            "enrollment/submit",
            &json!({
                "v":1,"type":"enrollment_submit","enrollment_id":enrollment,
                "matching_code":started["matching_code"],"enrolled_key":enrolled_key
            }),
        );
        assert_eq!(pending["type"], "enrollment_pending");
        let approved = self.admin(&[
            "approve",
            "--device-id",
            &device,
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
        Phone {
            device,
            credential,
            receiver,
            grant: approved["grant_id"].as_str().unwrap().into(),
            revision: approved["grant_revision"].as_u64().unwrap(),
        }
    }
}

struct Phone {
    device: String,
    credential: String,
    receiver: String,
    grant: String,
    revision: u64,
}

fn post(
    client: &reqwest::blocking::Client,
    credential: &str,
    endpoint: &str,
    value: &Value,
) -> Value {
    client
        .post(format!("{URL}/v1/phone-control/{endpoint}"))
        .bearer_auth(credential)
        .json(value)
        .send()
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .unwrap()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(
        String::with_capacity(bytes.len() * 2),
        |mut output, byte| {
            write!(output, "{byte:02x}").unwrap();
            output
        },
    )
}

fn unhex(value: &str) -> Vec<u8> {
    value
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

fn sign(fixture: &Value, key: &str, bytes: &[u8]) -> String {
    let key = EcdsaKeyPair::from_pkcs8(
        &ECDSA_P256_SHA256_ASN1_SIGNING,
        &unhex(fixture["keys"][key]["pkcs8"].as_str().unwrap()),
    )
    .unwrap();
    hex(key.sign(&SystemRandom::new(), bytes).unwrap().as_ref())
}

fn unlock(client: &reqwest::blocking::Client, fixture: &Value, phone: &Phone) -> String {
    let challenge = post(
        client,
        &phone.credential,
        "challenge",
        &json!({
            "v":1,"type":"challenge","grant_id":phone.grant,"grant_revision":phone.revision
        }),
    );
    let field = |name: &str| challenge[name].as_str().unwrap();
    let enrolled_hash = fixture["proof"]["enrolled_key_hash"].as_str().unwrap();
    let working = fixture["keys"]["working"]["sec1"].as_str().unwrap();
    let mut bytes = b"louiselm-phone-control\0\x01\x01".to_vec();
    bytes.extend(unhex(&phone.grant));
    bytes.extend(phone.revision.to_be_bytes());
    for value in [
        enrolled_hash,
        &phone.receiver,
        field("challenge_id"),
        field("challenge"),
        working,
    ] {
        bytes.extend(unhex(value));
    }
    let proof = json!({"v":1,"type":"unlock","proof":{
        "grant_id":phone.grant,"grant_revision":phone.revision,"enrolled_key_hash":enrolled_hash,
        "receiver_identity":phone.receiver,"challenge_id":field("challenge_id"),
        "challenge":field("challenge"),"working_key":working,"signature":sign(fixture,"enrolled",&bytes)
    }});
    let result = post(client, &phone.credential, "unlock", &proof);
    assert_eq!(result["type"], "unlocked");
    result["period_id"].as_str().unwrap().to_owned()
}

#[allow(
    clippy::too_many_arguments,
    reason = "wire fields are signed in their protocol order"
)]
fn request(
    fixture: &Value,
    phone: &Phone,
    period: &str,
    controller: &str,
    subject: &str,
    action: u8,
    payload: &[u8],
    operation: &str,
    revision: u64,
    interaction: u8,
) -> Value {
    let hash = hex(&Sha256::digest(payload));
    let mut bytes = b"louiselm-phone-control\0\x01\x02".to_vec();
    bytes.extend(unhex(&phone.grant));
    bytes.extend(phone.revision.to_be_bytes());
    for value in [period, &phone.receiver, controller, subject] {
        bytes.extend(unhex(value));
    }
    bytes.push(action);
    bytes.extend(unhex(&hash));
    bytes.extend(unhex(operation));
    bytes.extend(revision.to_be_bytes());
    bytes.push(interaction);
    json!({"v":1,"type":"request","request":{
        "grant_id":phone.grant,"grant_revision":phone.revision,"period_id":period,
        "receiver_identity":phone.receiver,"controller_id":controller,"subject_id":subject,
        "action":action,"payload":hex(payload),"payload_hash":hash,"operation_id":operation,
        "expected_revision":revision,"interaction":interaction,"signature":sign(fixture,"working",&bytes)
    }})
}

struct Editor {
    process: Process,
    reports: mpsc::Receiver<String>,
    group_active: bool,
}

impl Editor {
    fn start(desktop: &Desktop, directory: &Path) -> (Self, Value) {
        fs::create_dir(directory).unwrap();
        let nvim = env::var("LOUISELM_TEST_NVIM").unwrap();
        // Allow only this disposable editor to be traced when Yama restricts
        // sibling processes; exec still starts the unmodified Neovim binary.
        let mut child = desktop
            .command(Path::new("/usr/bin/python3"))
            .args([
                "-c",
                "import ctypes,os,sys; assert ctypes.CDLL(None).prctl(0x59616d61,ctypes.c_ulong(-1),0,0,0)==0; os.execv(sys.argv[1],sys.argv[1:])",
                &nvim,
                "--headless",
                "--noplugin",
                "-u",
                "NONE",
                "-l",
            ])
            .arg(
                desktop
                    .project
                    .join("tests/fixtures/phone_send_headless.lua"),
            )
            .env(
                "LOUISELM_TEST_CAPTURE",
                env!("CARGO_BIN_EXE_louiselm-capture"),
            )
            .env("LOUISELM_TEST_PROJECT", &desktop.project)
            .env("LOUISELM_TEST_RECEIPTS", directory)
            .env("LOUISELM_TEST_SEND_TEXT", TEXT)
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let (tx, reports) = mpsc::channel();
        let stdout = child.stdout.take().unwrap();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    return;
                }
            }
        });
        let editor = Self {
            process: Process(child),
            reports,
            group_active: true,
        };
        let targets = editor.report();
        assert!(
            live_group_members(editor.process.0.id()).len() >= 3,
            "echo Agents must share the owned editor group"
        );
        (editor, targets)
    }

    fn report(&self) -> Value {
        serde_json::from_str(
            &self
                .reports
                .recv_timeout(Duration::from_secs(30))
                .expect("editor receipt"),
        )
        .unwrap()
    }

    fn finish(mut self) {
        self.process
            .0
            .stdin
            .take()
            .unwrap()
            .write_all(b"stop\n")
            .unwrap();
        let report = self.report();
        assert_eq!(report["selected_prompts"], 0);
        assert_eq!(report["other_prompts"], 0);
        assert_eq!(report["selected_turns"], 0);
        assert_eq!(report["other_turns"], 0);
        assert_eq!(report["transcript_mentions"], 0);
        assert_eq!(report["draft_preserved"], true);
        let pid = self.process.0.id();
        let deadline = Instant::now() + Duration::from_secs(10);
        while process_state(pid) != Some('Z') {
            assert!(
                Instant::now() < deadline,
                "editor did not exit after disposing its Sessions"
            );
            thread::sleep(Duration::from_millis(10));
        }
        assert!(self.cleanup().unwrap().success());
    }

    fn cleanup(&mut self) -> Result<std::process::ExitStatus, String> {
        let pid = self.process.0.id();
        let result = Command::new("/bin/kill")
            .args(["-KILL", "--", &format!("-{pid}")])
            .status()
            .map_err(|error| error.to_string())?;
        if !result.success() {
            return Err("could not stop the disposable editor process group".into());
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while !live_group_members(pid).is_empty() {
            if Instant::now() >= deadline {
                return Err("disposable editor process group still has live members".into());
            }
            thread::sleep(Duration::from_millis(10));
        }
        let status = self.process.0.wait().map_err(|error| error.to_string())?;
        self.group_active = false;
        Ok(status)
    }
}

impl Drop for Editor {
    fn drop(&mut self) {
        if self.group_active {
            // The unreaped group leader pins this ID until all fixture children
            // are stopped, including on a failed assertion.
            if let Err(error) = self.cleanup() {
                eprintln!("fixture cleanup failed: {error}");
            }
        }
    }
}

fn process_stat(pid: u32) -> Option<(char, u32)> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let fields = stat
        .rsplit_once(") ")?
        .1
        .split_whitespace()
        .collect::<Vec<_>>();
    Some((
        fields.first()?.chars().next()?,
        fields.get(2)?.parse().ok()?,
    ))
}

fn process_state(pid: u32) -> Option<char> {
    process_stat(pid).map(|(state, _)| state)
}

fn live_group_members(group: u32) -> Vec<u32> {
    fs::read_dir("/proc")
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let pid = entry.file_name().to_str()?.parse::<u32>().ok()?;
            let (state, process_group) = process_stat(pid)?;
            (process_group == group && state != 'Z').then_some(pid)
        })
        .collect()
}

fn signal(pid: u32, name: &str) {
    assert!(
        Command::new("/bin/kill")
            .args([name, &pid.to_string()])
            .status()
            .unwrap()
            .success()
    );
}

// strace stops the disposable editor on every read. Resume all earlier reads;
// leave it stopped after the real queued commit has entered its read buffer.
fn hold_commit(
    editor: &Editor,
    trace: &Path,
    send: impl FnOnce() + Send + 'static,
) -> (Process, thread::JoinHandle<()>) {
    let pid = editor.process.0.id();
    let mut tracer = Process(
        Command::new("strace")
            .args(["-qq", "-s", "16384", "-o"])
            .arg(trace)
            .args([
                "-e",
                "trace=read,write",
                "-e",
                "inject=read:signal=STOP",
                "-p",
                &pid.to_string(),
            ])
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let status = fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
        if status.lines().any(|line| {
            line.starts_with("TracerPid:") && line.split_whitespace().nth(1) != Some("0")
        }) {
            break;
        }
        assert!(
            tracer.0.try_wait().unwrap().is_none(),
            "strace failed to attach"
        );
        assert!(Instant::now() < deadline, "strace did not attach");
        thread::sleep(Duration::from_millis(5));
    }
    let sender = thread::spawn(send);
    let mut stops = 0;
    let mut offset = 0;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let log = fs::read_to_string(trace).unwrap_or_default();
        let count = log.matches("--- stopped by SIGSTOP ---").count();
        if count > stops {
            let new = &log[offset..];
            if new.contains("action_commit") && new.contains(OPERATION) {
                return (tracer, sender);
            }
            stops = count;
            offset = log.len();
            signal(pid, "-CONT");
        }
        assert!(
            tracer.0.try_wait().unwrap().is_none(),
            "strace exited before commit"
        );
        assert!(
            Instant::now() < deadline,
            "commit was not observed in the owner read: {log}"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
#[ignore = "requires private mount and network namespaces, isolated /run, loopback, strace and stable Neovim"]
#[allow(
    clippy::too_many_lines,
    reason = "one real receiver, owner, Forget and restart lifecycle"
)]
fn pending_commit_is_fenced_after_offline_forget_and_survives_restart() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let desktop = Desktop {
        root: root.path().into(),
        project: fs::canonicalize(concat!(env!("CARGO_MANIFEST_DIR"), "/..")).unwrap(),
    };
    let mut receiver = desktop.start_receiver();
    let tls = desktop.root.join("louiselm/capture/tls");
    let certificate =
        reqwest::Certificate::from_pem(&fs::read(tls.join("receiver-cert.pem")).unwrap()).unwrap();
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .tls_certs_only([certificate])
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap();
    let fixture: Value = serde_json::from_str(include_str!(
        "../../android/app/src/test/resources/phone-control-v1.json"
    ))
    .unwrap();
    let phone = desktop.enroll(&client, &fixture);
    let observer = desktop.enroll(&client, &fixture);
    let (editor, targets) = Editor::start(&desktop, &desktop.root.join("editor"));
    let controller = targets["controller_id"].as_str().unwrap();
    let subject = targets["subject_id"].as_str().unwrap();
    let period = unlock(&client, &fixture, &phone);
    let page = request(
        &fixture,
        &phone,
        &period,
        controller,
        subject,
        1,
        &[0, 0, 0, 0, 0, 0, 0, 0, 0, 50],
        "01010101010101010101010101010101",
        0,
        1,
    );
    let page = post(&client, &phone.credential, "request", &page);
    assert_eq!(page["type"], "session_page");
    let revision = page["revision"].as_u64().unwrap();
    let mut payload = vec![1];
    payload.extend(TEXT.as_bytes());
    let pending = request(
        &fixture, &phone, &period, controller, subject, 3, &payload, OPERATION, revision, 1,
    );
    let client_for_send = client.clone();
    let credential = phone.credential.clone();
    let pending_for_send = pending.clone();
    let (mut tracer, sender) = hold_commit(&editor, &desktop.root.join("trace"), move || {
        let reply = client_for_send
            .post(format!("{URL}/v1/phone-control/request"))
            .bearer_auth(credential)
            .json(&pending_for_send)
            .send();
        assert!(
            reply.is_err(),
            "receiver death must lose the pending response"
        );
    });
    let editor_status =
        || fs::read_to_string(format!("/proc/{}/status", editor.process.0.id())).unwrap();
    assert!(
        editor_status()
            .lines()
            .any(|line| line.starts_with("State:") && line.contains("stop"))
    );
    assert!(
        !fs::read_to_string(desktop.root.join("trace"))
            .unwrap()
            .contains("gate_use")
    );
    receiver.0.kill().unwrap();
    receiver.0.wait().unwrap();
    sender.join().unwrap();
    let archive = ActionEvidenceStore::open_bound(
        desktop
            .root
            .join("louiselm/capture/phone-control/action-evidence"),
        tls.join("phone-action-evidence.json"),
        &phone.receiver,
        ActionEvidenceKind::Archive,
    )
    .unwrap();
    let original = archive.lookup(OPERATION).unwrap();
    drop(archive);
    assert_eq!(original.outcome, louiselm_capture::ActionOutcome::Unknown);
    let inspect = json!({"v":1,"type":"inspect","identity":original.identity});
    assert_eq!(desktop.owner(&inspect)["outcome"], "unknown");
    assert_eq!(
        desktop.cli(&["revoke-device", &phone.device]).trim(),
        phone.device
    );
    assert!(
        editor_status()
            .lines()
            .any(|line| line.starts_with("State:") && line.contains("stop"))
    );
    let registry = PairingRegistry::open(desktop.root.join("louiselm/capture/pairing")).unwrap();
    assert_eq!(
        registry.authenticate_device(&phone.credential).unwrap(),
        None
    );
    assert_eq!(
        registry.authenticate_device(&observer.credential).unwrap(),
        Some(observer.device.clone())
    );
    assert_eq!(desktop.owner(&inspect)["outcome"], "unknown");
    tracer.0.kill().unwrap();
    tracer.0.wait().unwrap();
    signal(editor.process.0.id(), "-CONT");
    let deadline = Instant::now() + Duration::from_secs(20);
    let evidence = loop {
        let result = desktop.owner(&inspect);
        if let Some(evidence) = result["outcome"]["fenced_no_effect"]["evidence"].as_str() {
            break evidence.to_owned();
        }
        assert!(
            Instant::now() < deadline,
            "native owner did not fence: {result}"
        );
        thread::sleep(Duration::from_millis(25));
    };
    assert_eq!(evidence.len(), 64);
    editor.finish();
    let _receiver = desktop.start_receiver();
    assert_eq!(
        desktop.owner(&inspect)["outcome"]["fenced_no_effect"]["evidence"],
        evidence
    );
    let guard: Value =
        serde_json::from_slice(&fs::read(tls.join("phone-action-owner.json")).unwrap()).unwrap();
    let begin =
        json!({"v":2,"type":"begin","generation":guard["generation"],"identity":original.identity});
    let repeated = desktop.owner(&begin);
    assert_eq!(repeated["type"], "existing");
    assert_eq!(
        repeated["outcome"]["fenced_no_effect"]["evidence"],
        evidence
    );
    let period = unlock(&client, &fixture, &observer);
    let mut history_payload = vec![0];
    history_payload.extend(unhex(OPERATION));
    let history = request(
        &fixture,
        &observer,
        &period,
        controller,
        subject,
        2,
        &history_payload,
        "02020202020202020202020202020202",
        0,
        0,
    );
    let history = post(&client, &observer.credential, "request", &history);
    assert_eq!(history["type"], "action_history");
    assert_eq!(history["record"]["operation_id"], OPERATION);
    assert_eq!(
        history["record"]["receiver_identity"],
        original.identity.receiver_identity
    );
    assert_eq!(history["record"]["controller_id"], controller);
    assert_eq!(history["record"]["subject_id"], subject);
    assert_eq!(history["record"]["grant_id"], phone.grant);
    assert_eq!(history["record"]["action"], 3);
    assert_eq!(
        history["record"]["payload_hash"],
        original.identity.payload_hash
    );
    assert_eq!(history["record"]["expected_revision"], revision);
    assert_eq!(history["record"]["state"], "fenced_no_effect");
    assert_eq!(history["record"]["evidence"], evidence);
    assert!(history["record"].get("payload").is_none());
    assert_eq!(history["record"]["accepted_evidence"], "");
    assert_eq!(history["record"]["dispatched_evidence"], "");
    assert_eq!(history["record"]["completed_evidence"], "");
    let old = client
        .post(format!("{URL}/v1/phone-control/request"))
        .bearer_auth(&phone.credential)
        .json(&pending)
        .send()
        .unwrap();
    assert_eq!(old.status(), reqwest::StatusCode::UNAUTHORIZED);
    let old: Value = old.json().unwrap();
    assert_eq!(old["code"], "unauthenticated");
}
