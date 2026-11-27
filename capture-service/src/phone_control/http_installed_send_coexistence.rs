//! Installed coexistence of one real Send and the broker's pending result.
use super::*;
use std::{
    io::{BufRead, BufReader},
    net::TcpStream,
    path::PathBuf,
    process::Child,
    sync::mpsc,
    time::{Duration, Instant},
};

use aws_lc_rs::{
    rand::SystemRandom,
    signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair},
};

use super::super::super::{
    ActionEvidence, ActionEvidenceKind, ActionEvidenceStore, ActionOutcome, fixed, prefix,
};
use super::super::tests::signed_unlock;

const TEXT: &str = "A reviewed prompt";
const OPERATION: &str = "11223344556677889900aabbccddeeff";
const URL: &str = "https://localhost:7391";

pub(super) fn enabled() -> bool {
    std::env::var("LOUISELM_TEST_SEND_COEXIST").as_deref() == Ok("1")
}

pub(super) fn phone_root(root: &Path) -> PathBuf {
    root.join(if enabled() {
        "louiselm/capture/phone-control"
    } else {
        "phone"
    })
}

pub(super) fn runs_root(root: &Path) -> PathBuf {
    root.join(if enabled() {
        "louiselm/workflow/runs"
    } else {
        "runs"
    })
}

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Editor(Child);
impl Drop for Editor {
    fn drop(&mut self) {
        drop(self.0.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            match self.0.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(25));
                }
                _ => break,
            }
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn command(root: &Path, program: &Path) -> Command {
    let mut command = Command::new(program);
    command
        .env_clear()
        .env("PATH", "/usr/local/lib/louiselm/current/bin:/usr/bin:/bin")
        .env("LANG", "C.UTF-8")
        .env("HOME", root)
        .env("LOUISELM_CAPTURE_CONFIG_DIR", root)
        .env("LOUISELM_CAPTURE_DATA_DIR", root)
        .env("LOUISELM_CAPTURE_STATE_DIR", root);
    command
}

fn capture(root: &Path, args: &[&str]) -> Value {
    let binary = std::env::var_os("LOUISELM_TEST_CAPTURE").unwrap();
    let output = command(root, Path::new(&binary))
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "capture command {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn receiver(root: &Path, project: &Path) -> OwnedChild {
    let binary = std::env::var_os("LOUISELM_TEST_CAPTURE").unwrap();
    let mut child = OwnedChild(
        command(root, Path::new(&binary))
            .arg("serve")
            .env("LOUISELM_RECEIVER_ENABLED", "true")
            .env("LOUISELM_PHONE_CONTROL_ENABLED", "true")
            .env("LOUISELM_RUNS_ENABLED", "true")
            .env("LOUISELM_ATTENTION_ENABLED", "true")
            .env("LOUISELM_BEADS_WORKSPACE", project)
            .env("LOUISELM_REAL_BR", "/bin/true")
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let socket = root.join("louiselm/workflow/phone-control-admin.sock");
    let deadline = Instant::now() + Duration::from_secs(30);
    while std::os::unix::net::UnixStream::connect(&socket).is_err()
        || TcpStream::connect("127.0.0.1:7391").is_err()
    {
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "capture receiver exited"
        );
        assert!(Instant::now() < deadline, "capture receiver did not start");
        std::thread::sleep(Duration::from_millis(20));
    }
    child
}

fn https_client(tls: &crate::TlsIdentity) -> reqwest::Client {
    let cert =
        reqwest::Certificate::from_pem(&std::fs::read(tls.certificate_path()).unwrap()).unwrap();
    reqwest::Client::builder()
        .no_proxy()
        .tls_certs_only([cert])
        .build()
        .unwrap()
}

async fn post(client: &reqwest::Client, credential: &str, endpoint: &str, body: &Value) -> Value {
    client
        .post(format!("{URL}/v1/phone-control/{endpoint}"))
        .bearer_auth(credential)
        .json(body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

struct Phone {
    credential: String,
    grant: String,
    revision: u64,
    receiver: String,
    period: String,
}

impl Phone {
    async fn unlock(&mut self, client: &reqwest::Client, fixture: &Value) {
        let challenge = post(
            client,
            &self.credential,
            "challenge",
            &json!({
                "v":1,"type":"challenge","grant_id":self.grant,"grant_revision":self.revision
            }),
        )
        .await;
        let result = post(
            client,
            &self.credential,
            "unlock",
            &signed_unlock(fixture, &challenge),
        )
        .await;
        assert_eq!(result["type"], "unlocked", "{result}");
        self.period = result["period_id"].as_str().unwrap().into();
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "the fixture signs the complete phone request"
    )]
    fn signed(
        &self,
        fixture: &Value,
        controller: &str,
        subject: &str,
        action: u8,
        payload: &[u8],
        operation: &str,
        revision: u64,
        interaction: u8,
    ) -> Value {
        let hash = lower_hex(&Sha256::digest(payload));
        let mut bytes = prefix(2);
        bytes.extend(fixed::<16>(&self.grant).unwrap());
        bytes.extend(self.revision.to_be_bytes());
        bytes.extend(fixed::<16>(&self.period).unwrap());
        bytes.extend(fixed::<32>(&self.receiver).unwrap());
        bytes.extend(fixed::<16>(controller).unwrap());
        bytes.extend(fixed::<16>(subject).unwrap());
        bytes.push(action);
        bytes.extend(fixed::<32>(&hash).unwrap());
        bytes.extend(fixed::<16>(operation).unwrap());
        bytes.extend(revision.to_be_bytes());
        bytes.push(interaction);
        let key = EcdsaKeyPair::from_pkcs8(
            &ECDSA_P256_SHA256_ASN1_SIGNING,
            &unhex(fixture["keys"]["working"]["pkcs8"].as_str().unwrap()),
        )
        .unwrap();
        let signature = key.sign(&SystemRandom::new(), &bytes).unwrap();
        json!({"v":1,"type":"request","request":{
            "grant_id":self.grant,"grant_revision":self.revision,"period_id":self.period,
            "receiver_identity":self.receiver,"controller_id":controller,"subject_id":subject,
            "action":action,"payload":lower_hex(payload),"payload_hash":hash,"operation_id":operation,
            "expected_revision":revision,"interaction":interaction,"signature":lower_hex(signature.as_ref())
        }})
    }
}

async fn checkpoint(root: &Path, name: &str) {
    std::fs::write(root.join(format!("coexist-{name}")), []).unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    while !root.join(format!("coexist-{name}.ack")).is_file() {
        assert!(
            Instant::now() < deadline,
            "broker checkpoint {name} did not complete"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn owner(root: &Path, receiver: &str) -> ActionEvidence {
    let tls = root.join("louiselm/capture/tls");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let record = {
            let store = ActionEvidenceStore::open_bound(
                root.join("louiselm/workflow/phone-action-owner"),
                tls.join("phone-action-owner.json"),
                receiver,
                ActionEvidenceKind::Owner,
            )
            .unwrap();
            store.lookup(OPERATION).unwrap()
        };
        if matches!(record.outcome, ActionOutcome::Completed { .. }) {
            assert!(matches!(
                record.phases.as_slice(),
                [
                    ActionOutcome::Accepted { .. },
                    ActionOutcome::Dispatched { .. },
                    ActionOutcome::Completed { .. }
                ]
            ));
            return record;
        }
        assert!(Instant::now() < deadline, "Send did not durably complete");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "one installed transaction observes both product states through restart"
)]
pub(super) async fn review(root: &Path) {
    let result_id = std::env::var("LOUISELM_TEST_REVIEW_RESULT").unwrap();
    let project =
        std::fs::canonicalize(std::env::var_os("LOUISELM_TEST_LUA_ROOT").unwrap()).unwrap();
    let run = RunStore::new(runs_root(root)).unwrap();
    let attention =
        crate::AttentionStore::new(root.join("louiselm/workflow/attention"), Some(run)).unwrap();
    let projections: Vec<crate::BrokerProjection> =
        serde_json::from_slice(&std::fs::read(root.join("coexist-projection.json")).unwrap())
            .unwrap();
    for projection in projections {
        attention.project(&projection).unwrap();
    }
    let mut server = receiver(root, &project);
    let tls = crate::TlsIdentity::load_existing(&root.join("louiselm/capture/tls")).unwrap();
    let client = https_client(&tls);
    let pairing = PairingRegistry::open(root.join("louiselm/capture/pairing")).unwrap();
    let now = crate::time::now_ms();
    let offer = pairing
        .issue(URL, tls.public_key_sha256(), now, 60_000)
        .unwrap();
    let paired: Value = client
        .post(format!("{URL}/v1/pair"))
        .json(&json!({"token":offer.token,"device_name":"Disposable coexistence phone"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let device = paired["device_id"].as_str().unwrap();
    let credential = paired["credential"].as_str().unwrap().to_owned();
    let started = capture(root, &["phone-control", "start", "--device-id", device]);
    let enrollment = started["enrollment_id"].as_str().unwrap();
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../android/app/src/test/resources/phone-control-v1.json"
    ))
    .unwrap();
    let key = fixture["keys"]["enrolled"]["sec1"].as_str().unwrap();
    let submitted = post(&client, &credential, "enrollment/submit", &json!({"v":1,"type":"enrollment_submit","enrollment_id":enrollment,"matching_code":started["matching_code"],"enrolled_key":key})).await;
    assert_eq!(submitted["type"], "enrollment_pending");
    let source = PhoneSourceStore::open(phone_root(root).join("run-sources"))
        .unwrap()
        .read(RUN)
        .unwrap();
    let roots = capture(root, &["phone-control", "project-roots"]);
    let saved_project = root.join("project");
    let saved_root = roots["roots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["path"] == saved_project.to_str().unwrap())
        .unwrap()["id"]
        .as_str()
        .unwrap();
    let send_root = capture(
        root,
        &[
            "phone-control",
            "register-root",
            "--path",
            project.to_str().unwrap(),
        ],
    );
    let approved = capture(
        root,
        &[
            "phone-control",
            "approve",
            "--device-id",
            device,
            "--enrollment-id",
            enrollment,
            "--enrolled-key",
            key,
            "--project",
            saved_project.to_str().unwrap(),
            "--project",
            project.to_str().unwrap(),
            "--project-root-id",
            saved_root,
            "--project-root-id",
            send_root["root_id"].as_str().unwrap(),
            "--agent",
            "agent",
            "--agent",
            "codex",
            "--group",
            "read",
            "--group",
            "operate_sessions",
        ],
    );
    let granted = post(
        &client,
        &credential,
        "enrollment/status",
        &json!({"v":1,"type":"enrollment_status","enrollment_id":enrollment}),
    )
    .await;
    assert_eq!(approved["grant_id"], granted["grant_id"]);
    let mut phone = Phone {
        credential,
        grant: granted["grant_id"].as_str().unwrap().into(),
        revision: granted["grant_revision"].as_u64().unwrap(),
        receiver: tls.public_key_sha256().into(),
        period: String::new(),
    };
    phone.unlock(&client, &fixture).await;
    let receipts = root.join("send-receipts");
    std::fs::create_dir(&receipts).unwrap();
    let nvim = std::env::var_os("LOUISELM_TEST_NVIM").unwrap();
    let binary = std::env::var_os("LOUISELM_TEST_CAPTURE").unwrap();
    let mut editor = Editor(
        command(root, Path::new(&nvim))
            .args(["--headless", "--noplugin", "-u", "NONE", "-l"])
            .arg(project.join("tests/fixtures/phone_send_headless.lua"))
            .env("LOUISELM_TEST_CAPTURE", binary)
            .env("LOUISELM_TEST_PROJECT", &project)
            .env("LOUISELM_TEST_RECEIPTS", &receipts)
            .env("LOUISELM_TEST_SEND_TEXT", TEXT)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let stdout = editor.0.stdout.take().unwrap();
    let (sender, reports) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    let report = || -> Value {
        serde_json::from_str(&reports.recv_timeout(Duration::from_secs(30)).unwrap()).unwrap()
    };
    let targets = report();
    let controller = targets["controller_id"].as_str().unwrap();
    let subject = targets["subject_id"].as_str().unwrap();
    let page = phone.signed(
        &fixture,
        controller,
        subject,
        1,
        &[0, 0, 0, 0, 0, 0, 0, 0, 0, 50],
        "00000000000000000000000000000001",
        0,
        1,
    );
    let page = post(&client, &phone.credential, "request", &page).await;
    assert_eq!(page["type"], "session_page", "{page}");
    let revision = page["revision"].as_u64().unwrap();
    let mut text = vec![1];
    text.extend(TEXT.as_bytes());
    let send = phone.signed(
        &fixture, controller, subject, 3, &text, OPERATION, revision, 1,
    );
    let accepted = post(&client, &phone.credential, "request", &send).await;
    assert_eq!(accepted["record"]["state"], "accepted", "{accepted}");
    let native_owner = owner(root, &phone.receiver);
    let mut lookup = vec![0];
    lookup.extend(unhex(OPERATION));
    let history = |phone: &Phone, n: u128| {
        phone.signed(
            &fixture,
            controller,
            subject,
            2,
            &lookup,
            &format!("{n:032x}"),
            0,
            0,
        )
    };
    let before = post(&client, &phone.credential, "request", &history(&phone, 2)).await;
    assert_eq!(before["record"]["operation_id"], OPERATION);
    assert_eq!(before["record"]["state"], "completed", "{before}");
    let zero = "00".repeat(16);
    let subject_run = RUN.replace('-', "");
    let read_saved = |phone: &Phone, n: u128| {
        phone.signed(
            &fixture,
            &zero,
            &subject_run,
            80,
            &payload(&result_id, None, None),
            &format!("{n:032x}"),
            0,
            1,
        )
    };
    let saved = post(
        &client,
        &phone.credential,
        "request",
        &read_saved(&phone, 3),
    )
    .await;
    assert_eq!(saved["type"], "saved_review", "{saved}");
    assert_eq!(saved["result_id"], result_id);
    assert_eq!(saved["run_id"], RUN);
    assert_eq!(saved["producer_agent"], "agent");
    assert_eq!(saved["file"]["path"], "review-pages.txt");
    let inspected = desktop_promotion("inspect", &json!({"result_id":result_id}));
    assert_eq!(inspected["result"]["request"]["request_id"], result_id);
    assert_eq!(inspected["result"]["discarded"], false);
    let initial_attention = capture(root, &["attention", "list"]);
    assert!(
        initial_attention["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["kind"] == "promotion_pending" && item["result_id"] == result_id)
    );
    checkpoint(root, "before").await;
    println!("SEND_SAVED_BEFORE_PASSED");
    let cleaned = capture(
        root,
        &[
            "phone-control",
            "remove-action-details",
            "--operation-id",
            OPERATION,
        ],
    );
    assert_eq!(cleaned["type"], "action_details_removed");
    assert_eq!(cleaned["operation_id"], OPERATION);
    let removed = post(&client, &phone.credential, "request", &history(&phone, 4)).await;
    assert_eq!(removed["record"]["operation_id"], OPERATION);
    assert_eq!(removed["record"]["state"], "details_removed", "{removed}");
    let saved_after = post(
        &client,
        &phone.credential,
        "request",
        &read_saved(&phone, 5),
    )
    .await;
    assert_eq!(saved_after["file"], saved["file"]);
    assert_eq!(saved_after["revision"], saved["revision"]);
    assert_eq!(
        desktop_promotion("inspect", &json!({"result_id":result_id})),
        inspected
    );
    assert_eq!(capture(root, &["attention", "list"]), initial_attention);
    assert_eq!(owner(root, &phone.receiver), native_owner);
    checkpoint(root, "after").await;
    println!("SEND_SAVED_AFTER_PASSED");
    editor.0.stdin.take().unwrap().write_all(b"stop\n").unwrap();
    let final_report = report();
    assert_eq!(final_report["selected_prompts"], 1);
    assert_eq!(final_report["other_prompts"], 0);
    assert_eq!(final_report["draft_preserved"], true);
    assert!(editor.0.wait().unwrap().success());
    server.0.kill().unwrap();
    server.0.wait().unwrap();
    server = receiver(root, &project);
    let client = https_client(&tls);
    phone.unlock(&client, &fixture).await;
    let repeated = capture(
        root,
        &[
            "phone-control",
            "remove-action-details",
            "--operation-id",
            OPERATION,
        ],
    );
    assert_eq!(repeated["type"], "action_details_removed");
    let history_after = post(&client, &phone.credential, "request", &history(&phone, 6)).await;
    assert_eq!(history_after["record"]["operation_id"], OPERATION);
    assert_eq!(
        history_after["record"]["state"], "details_removed",
        "{history_after}"
    );
    let saved_restart = post(
        &client,
        &phone.credential,
        "request",
        &read_saved(&phone, 7),
    )
    .await;
    assert_eq!(saved_restart["file"], saved["file"]);
    assert_eq!(saved_restart["revision"], saved["revision"]);
    assert_eq!(
        desktop_promotion("inspect", &json!({"result_id":result_id})),
        inspected
    );
    assert_eq!(capture(root, &["attention", "list"]), initial_attention);
    assert_eq!(owner(root, &phone.receiver), native_owner);
    assert_eq!(
        PhoneSourceStore::open(phone_root(root).join("run-sources"))
            .unwrap()
            .read(RUN)
            .unwrap(),
        source
    );
    checkpoint(root, "restart").await;
    println!("SEND_SAVED_RESTART_PASSED");
    drop(server);
    println!("SEND_SAVED_COEXISTENCE_PASSED");
    println!("SAVED_REVIEW_INSTALLED_HTTP_PASSED");
}
