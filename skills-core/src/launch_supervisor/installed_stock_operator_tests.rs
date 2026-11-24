//! Stock runtime + installed operator + Enforced conformance in a private VM.
use super::*;
use crate::broker::provider_credentials::ProviderCredentialStore;
use crate::conformance::{
    ReportResult,
    installed::{certify, measure},
};
use crate::launch_transport::SeqpacketListener;
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

// Compile the actual dispatcher, including its query/lifecycle worker, rather
// than writing another broker loop. Only its HTTPS root differs in case 10.
#[expect(
    dead_code,
    reason = "The offline fixture invokes only the production server dispatcher, not its CLI front end."
)]
#[path = "../bin/louiselm-control.rs"]
mod control;

pub(super) const BROKER_ENTRY: &str =
    "launch_supervisor::system::installed_tests::daemon::bead_executor::stock::offline_broker";

#[test]
fn offline_broker() {
    let Some(root) = std::env::var_os("LOUISELM_OFFLINE_OPERATOR_BROKER") else {
        return;
    };
    let root = PathBuf::from(root);
    let listener =
        SeqpacketListener::adopt(rustix::io::fcntl_dupfd_cloexec(std::io::stdin(), 3).unwrap())
            .unwrap();
    rustix::stdio::dup2_stdin(fs::File::open("/dev/null").unwrap()).unwrap();
    let broker =
        InstalledBroker::over(&LauncherPaths::system(), Path::new(STATE), listener).unwrap();
    assert!(
        broker.provider_credential("openai").is_ok(),
        "offline fixture credential must be loaded before the first prompt"
    );
    let certificate =
        ureq::tls::Certificate::from_pem(&fs::read(root.join("guard-provider-root.pem")).unwrap())
            .unwrap();
    let provider_root: control::ProviderTestRoot = (
        certificate,
        crate::broker::BrokerSession::set_provider_test_root,
    );
    control::serve(broker, Some(&provider_root)).unwrap();
}

pub(super) fn install(
    root: &Path,
    paths: &LauncherPaths,
    config: &LauncherConfig,
    startup_only: bool,
) -> Option<Upstream> {
    assert!(
        std::env::var_os("LOUISELM_TEST_BR").is_some(),
        "stock operator gate requires native br"
    );
    assert!(
        std::env::var_os("LOUISELM_TEST_WORKSPACE_SNAPSHOT").is_some(),
        "stock operator gate requires full source"
    );
    assert!(
        std::env::var_os("LOUISELM_TEST_BEADS_JSONL").is_some(),
        "stock operator gate requires a full native tracker export"
    );
    let snapshot: serde_json::Value = serde_json::from_slice(&workspace::snapshot_bytes()).unwrap();
    assert!(snapshot["files"].as_array().unwrap().len() > 1000);
    let stock = std::env::var_os("LOUISELM_STOCK_CODEX_DIR")
        .expect("stock operator gate requires the pinned chain");
    let project = std::env::var_os("LOUISELM_TEST_LUA_ROOT").expect("explicit fixture source");
    assert!(
        Command::new("/usr/bin/python3")
            .arg(Path::new(&project).join("scripts/fetch-stock-codex-chain"))
            .arg("--check")
            .arg(&stock)
            .stdout(Stdio::null())
            .status()
            .unwrap()
            .success(),
        "stock operator gate rejects an unpinned or synthetic runtime"
    );
    let registry = root.join("registry");
    fs::write(root.join("stock-skills-off"), b"").unwrap();
    super::super::super::codex_chain::install(
        root,
        &registry,
        config,
        Some(Path::new(&stock)),
        None,
    );
    registry_record(
        &registry.join("envelopes.json"),
        &serde_json::json!([
            {"id":"envelope","network":"brokered","description":"offline stock operator"}
        ]),
    );
    workspace::stage_manifest(
        config,
        &workspace::fixture_manifest_for(
            super::super::super::codex_chain::runtime_directory(root).as_deref(),
            crate::registry::NetworkPolicy::Brokered,
        ),
    );
    assert_eq!(
        config.conformance,
        crate::conformance::admission::Enforcement::Enforced
    );
    let parent = rustix::process::getppid()
        .unwrap()
        .as_raw_nonzero()
        .get()
        .cast_unsigned();
    measure(paths, config, Instant::now() + Duration::from_mins(3)).unwrap();
    let certificate = certify(paths, Instant::now() + Duration::from_mins(3), parent).unwrap();
    assert_eq!(
        certificate.observations.result().unwrap(),
        ReportResult::Passed,
        "{:?}",
        certificate.observations
    );
    eprintln!("STOCK_OPERATOR pinned runtime, full source and Enforced certificate selected");
    (!startup_only).then(|| Upstream::start(root))
}

pub(super) fn provision_credential() {
    // Called between the first pristine startup and the serving startup.
    let custody = ProviderCredentialStore::root_in(Path::new(STATE));
    fs::create_dir_all(&custody).unwrap();
    chown(&custody, Some(BROKER_UID), Some(BROKER_UID)).unwrap();
    fs::set_permissions(&custody, fs::Permissions::from_mode(0o700)).unwrap();
    let credential = custody.join("openai");
    fs::write(&credential, b"fixture-secret").unwrap();
    chown(&credential, Some(BROKER_UID), Some(BROKER_UID)).unwrap();
    fs::set_permissions(&credential, fs::Permissions::from_mode(0o600)).unwrap();
}

pub(super) fn verify_rehearsal_refusal(operator_uid: u32) {
    let authorization_root = Path::new(STATE).join("authorizations");
    let before = tree_bytes(&authorization_root);
    let output = Command::new("/usr/bin/setpriv")
        .args([
            "--reuid",
            &operator_uid.to_string(),
            "--regid",
            &operator_uid.to_string(),
            "--clear-groups",
        ])
        .arg("/usr/local/lib/louiselm/current/bin/louiselm-control")
        .args(["run", "rehearse", "--json"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stdout, [] as [u8; 0]);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stderr).unwrap(),
        serde_json::json!({
            "schema": "louiselm.run-rehearsal/1",
            "status": "refused",
            "code": "terminal_cleanup_unproven",
            "blocker": "louiselm-rrfgt",
        })
    );
    assert_eq!(tree_bytes(&authorization_root), before);
    assert_eq!(
        fs::read_dir(Path::new(STATE).join("receipts/sessions"))
            .unwrap()
            .count(),
        0
    );
    eprintln!("REHEARSAL refused before authority reservation/consumption; no receipts");
}

fn tree_bytes(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    for entry in fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files.extend(tree_bytes(&path));
        } else {
            files.insert(path.clone(), fs::read(path).unwrap());
        }
    }
    files
}

pub(super) fn verify_run(
    root: &Path,
    paths: &LauncherPaths,
    config: &LauncherConfig,
    state: &Path,
    selection: &serde_json::Value,
    startup_only: bool,
) {
    use crate::launch_receipt::{ConformanceEvidence, ReceiptOutcome, SessionState, SignedReceipt};
    let heads: Vec<String> =
        serde_json::from_slice(&fs::read(state.join("stock-heads.json")).unwrap()).unwrap();
    assert_eq!(heads.len(), if startup_only { 1 } else { 4 });
    let run = selection["envelope"]["run_id"].as_str().unwrap();
    let measured = crate::registry::Registry::open_trusted(&root.join("registry"))
        .unwrap()
        .runtime("runtime")
        .unwrap()
        .measure()
        .unwrap();
    let runtime_digest = Digest::of(&serde_json::to_vec(&measured).unwrap()).to_string();
    let scratch = tempfile::tempdir_in(root).unwrap();
    let verifier = crate::launcher_install::LauncherVerifier::open(paths, scratch.path()).unwrap();
    let children = if startup_only { 1 } else { 3 };
    let mut chains = 0;
    for (index, expected_head) in heads.iter().take(children).enumerate() {
        for role in ["worker", "verifier"]
            .into_iter()
            .take(if startup_only { 1 } else { 2 })
        {
            let id = format!("{run}-{role}-{}", index + 1);
            let directory = Path::new(STATE).join("receipts/sessions").join(&id);
            let mut files: Vec<_> = fs::read_dir(directory)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .filter(|path| path.to_string_lossy().ends_with(".receipt.json"))
                .collect();
            files.sort();
            let chain: Vec<_> = files
                .iter()
                .map(|path| SignedReceipt::parse_canonical(&fs::read(path).unwrap()).unwrap())
                .collect();
            let anchor = verifier.receipt_anchor(&id).unwrap();
            let head =
                crate::launch_receipt::verify_chain(&chain, &anchor, |key, payload, signature| {
                    verifier.verify(key, payload, signature).is_ok()
                })
                .unwrap();
            assert_eq!(head.state(), SessionState::Terminal);
            let ReceiptOutcome::Launch { evidence, .. } = &chain[0].payload.outcome else {
                panic!("missing launch evidence")
            };
            assert!(
                matches!(evidence.conformance, ConformanceEvidence::Certified { .. }),
                "stock operator must not pass with PreCutover or a waiver"
            );
            assert!(matches!(
                evidence.skills,
                crate::session_manifest::SkillInputs::Off { .. }
            ));
            if role == "worker" {
                assert_eq!(evidence.runtime_measurement_digest, runtime_digest);
            }
            let digest = Digest::parse(&evidence.session_input_manifest_id).unwrap();
            let staged = config
                .broker_socket_path
                .parent()
                .unwrap()
                .join("workspace-inputs")
                .join(digest.hex());
            let inputs = crate::workspace::launch_inputs::inspect(&staged, &digest).unwrap();
            assert_eq!(&inputs.source.base_commit, expected_head);
            chains += 1;
        }
    }
    assert_eq!(chains, if startup_only { 1 } else { 6 });
    let requests = Path::new(STATE)
        .join("authorizations/provider-requests/runs")
        .join(Digest::of(run.as_bytes()).hex());
    let spent = if requests.exists() {
        fs::read_dir(requests).unwrap().count()
    } else {
        0
    };
    assert_eq!(spent, if startup_only { 0 } else { 6 });
    eprintln!(
        "STOCK_OPERATOR verified_terminal_chains={chains} certified=true current_head_bindings=true provider_attempts={spent}"
    );
}

pub(super) struct Upstream {
    pub(super) port: u16,
    stopped: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<usize>>,
}

impl Upstream {
    fn start(root: &Path) -> Self {
        let (listener, acceptor) = super::super::super::guard::provider_listener(root);
        let port = listener.local_addr().unwrap().port();
        let stopped = Arc::new(AtomicBool::new(false));
        let cancelled = Arc::clone(&stopped);
        let worker = thread::spawn(move || {
            let mut requests = 0;
            while !cancelled.load(Ordering::Acquire) {
                let stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    Err(error) => panic!("offline upstream accept: {error}"),
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                let mut tls = acceptor.accept(stream).unwrap();
                assert_eq!(
                    tls.ssl().servername(openssl::ssl::NameType::HOST_NAME),
                    Some("api.openai.com")
                );
                let body = request_body(&mut tls);
                assert_eq!(body["model"], "gpt-5.6-luna");
                assert_eq!(body["reasoning"]["effort"], "low");
                assert!(requests < 6, "unexpected Provider retry or excess request");
                let bead = requests / 2 + 1;
                let response = if requests % 2 == 0 {
                    let command = format!(
                        "printf 'accepted Bead output\\n' > accepted-{bead}.txt; printf TOOL_OK"
                    );
                    let input = format!(
                        "text((await tools.exec_command({{cmd: {}}})).output)",
                        serde_json::to_string(&command).unwrap()
                    );
                    super::super::super::guard::custom_tool_response_for(&input)
                } else {
                    let output = body["input"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|item| item["type"] == "custom_tool_call_output")
                        .expect("stock runtime did not return the Code Mode output");
                    assert!(output.to_string().contains("TOOL_OK"));
                    super::super::super::guard::stock_response(false)
                };
                write!(tls, "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", response.len()).unwrap();
                tls.write_all(&response).unwrap();
                tls.flush().unwrap();
                requests += 1;
                eprintln!("STOCK_OPERATOR upstream_request={requests} bead={bead}");
            }
            requests
        });
        Self {
            port,
            stopped,
            worker: Some(worker),
        }
    }

    pub(super) fn finish(mut self) {
        self.stopped.store(true, Ordering::Release);
        assert_eq!(self.worker.take().unwrap().join().unwrap(), 6);
        eprintln!(
            "STOCK_OPERATOR six local TLS requests, three Code Mode outputs and no retry proven"
        );
    }
}

impl Drop for Upstream {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            if thread::panicking() {
                eprintln!("offline upstream also failed during fixture cleanup");
            } else {
                panic!("offline upstream failed");
            }
        }
    }
}

fn request_body(tls: &mut openssl::ssl::SslStream<std::net::TcpStream>) -> serde_json::Value {
    let mut request = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        assert!(Instant::now() < deadline, "offline request body timed out");
        let mut chunk = [0; 4096];
        let count = tls.read(&mut chunk).unwrap();
        assert!(count > 0, "offline request body ended early");
        request.extend_from_slice(&chunk[..count]);
        assert!(
            request.len() <= 4 * 1024 * 1024,
            "offline request exceeds bound"
        );
        if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            let header = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
            let length = header
                .lines()
                .find_map(|line| line.strip_prefix("content-length: "))
                .unwrap()
                .parse::<usize>()
                .unwrap();
            if request.len() >= end + 4 + length {
                assert!(header.contains("authorization: bearer fixture-secret"));
                return serde_json::from_slice(&request[end + 4..end + 4 + length]).unwrap();
            }
        }
    }
}
