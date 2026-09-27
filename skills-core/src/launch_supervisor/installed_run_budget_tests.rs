//! Two installed Sessions spend one broker-owned Provider total, not separate child totals.

use super::*;
use crate::{
    broker::provider_credentials::ProviderCredentialStore,
    conformance::{
        ReportResult,
        admission::Enforcement,
        installed::{certify, measure},
    },
};
use std::{io::Read, net::SocketAddr, os::unix::net::UnixStream};

const BUDGET_WORKER: &str =
    "launch_supervisor::system::installed_tests::verification::budget::installed_budget_worker";

fn permission(now: u64) -> crate::provider_request::ApprovedProviderRequests {
    let mut permission = guard::approval(now);
    permission.upstream = "https://api.openai.com:1/v1/responses".into();
    permission.addresses = vec!["127.0.0.1".parse().unwrap()];
    permission
}

fn child_grant(launch: LaunchRequest, uid: u32, now: u64) -> GrantRequest {
    let mut grant = approval(launch, uid, None);
    grant.expires_at_ms = now + 120_000;
    grant.provider_requests = Some(permission(now));
    grant
}

fn drive_until(broker: &Arc<InstalledBroker>, session: &mut BrokerSession, done: &Path) {
    let deadline = Instant::now() + Duration::from_secs(25);
    while !done.exists() {
        assert!(
            Instant::now() < deadline,
            "Provider budget fixture timed out"
        );
        broker.drive_provider(session).unwrap();
        if session.provider_handoff_pending_for_test() {
            assert!(!broker.step(session).unwrap());
        } else {
            thread::sleep(Duration::from_millis(10));
        }
    }
}

fn dispose(broker: &InstalledBroker, session: &mut BrokerSession, uid: u32) {
    let caller = LifecycleCaller::Operator { uid };
    let park = lifecycle(session, LifecycleAction::Park, SessionState::Running, 1);
    broker.request_lifecycle(session, &caller, &park).unwrap();
    while broker
        .inspect(&session.authorization().session_id)
        .unwrap()
        .unwrap()
        .state
        != SessionState::Parked
    {
        assert!(!broker.step(session).unwrap());
    }
    let disposal = lifecycle(session, LifecycleAction::Disposal, SessionState::Parked, 2);
    broker
        .request_lifecycle(session, &caller, &disposal)
        .unwrap();
}

#[test]
fn installed_budget_worker() {
    let Some(root) = std::env::var_os("LOUISELM_RUN_BUDGET_FIXTURE") else {
        return;
    };
    let root = PathBuf::from(root);
    let broker = Arc::new(InstalledBroker::bind(&paths(&root), &root.join("state")).unwrap());
    let config = crate::launcher_install::public_runtime_config(&paths(&root)).unwrap();
    let now = clock_ms();
    let first = child_grant(request(), config.operator_uid, now);
    let second = child_grant(verifier_launch(0), config.operator_uid, now);
    let run = broker
        .authorize_run(&fixture_run_envelope(
            &first,
            Digest::of(b"budget-plan").to_string(),
            now,
        ))
        .unwrap();
    broker
        .authorize_child(&first, &run.envelope_digest)
        .unwrap();
    broker
        .authorize_child(&second, &run.envelope_digest)
        .unwrap();
    println!("BUDGET_FIRST_READY");
    let mut first_session = broker.serve_launch().unwrap();
    assert!(!broker.step(&mut first_session).unwrap());
    println!(
        "BUDGET_FIRST_ADDR {}",
        first_session.provider_address_for_test().unwrap()
    );
    drive_until(&broker, &mut first_session, &root.join("budget-first-done"));
    println!("BUDGET_SECOND_READY");
    let mut second_session = broker.serve_launch().unwrap();
    assert!(!broker.step(&mut second_session).unwrap());
    println!(
        "BUDGET_SECOND_ADDR {}",
        second_session.provider_address_for_test().unwrap()
    );
    drive_until(
        &broker,
        &mut second_session,
        &root.join("budget-second-done"),
    );
    dispose(&broker, &mut second_session, config.operator_uid);
    dispose(&broker, &mut first_session, config.operator_uid);
    println!("BUDGET_DONE");
}

struct Controller {
    input: UnixStream,
    output: UnixStream,
    relay: thread::JoinHandle<Result<i32, crate::launch_supervisor::SupervisorError>>,
}

impl Controller {
    fn new(session: crate::launch_supervisor::LaunchedSession) -> Self {
        let (input, controller_input) = UnixStream::pair().unwrap();
        let (controller_output, output) = UnixStream::pair().unwrap();
        output
            .set_read_timeout(Some(Duration::from_secs(20)))
            .unwrap();
        let relay = thread::spawn(move || {
            session.relay_stdio(
                RelayStdio::new(
                    BufReader::new(fs::File::from(OwnedFd::from(controller_input))),
                    fs::File::from(OwnedFd::from(controller_output)),
                )
                .unwrap(),
            )
        });
        Self {
            input,
            output,
            relay,
        }
    }

    fn request(&mut self, address: SocketAddr) -> String {
        writeln!(self.input, "\x1b{address} valid-once").unwrap();
        let mut marker = [0];
        self.output.read_exact(&mut marker).unwrap();
        assert_eq!(marker, [0x1b]);
        let mut length = [0; 4];
        self.output.read_exact(&mut length).unwrap();
        let length = u32::from_be_bytes(length) as usize;
        assert!(length <= 64 * 1024);
        let mut response = vec![0; length];
        self.output.read_exact(&mut response).unwrap();
        String::from_utf8(response).unwrap()
    }

    fn finish(self) {
        self.relay.join().unwrap().unwrap();
    }
}

fn spent(root: &Path) -> usize {
    let attempts = root
        .join("state/authorizations/provider-requests/runs")
        .join(Digest::of(b"run").hex());
    if attempts.exists() {
        fs::read_dir(attempts).unwrap().count()
    } else {
        0
    }
}

fn worker(root: &Path) -> (BrokerChild, mpsc::Receiver<String>) {
    let mut child = Command::new("/usr/bin/setpriv")
        .args([
            "--reuid",
            &BROKER_UID.to_string(),
            "--regid",
            &BROKER_UID.to_string(),
            "--clear-groups",
        ])
        .arg(std::env::current_exe().unwrap())
        .args([BUDGET_WORKER, "--exact", "--nocapture"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LOUISELM_RUN_BUDGET_FIXTURE", root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let output = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(output).lines() {
            if tx.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    (BrokerChild(child), rx)
}

#[test]
fn privileged_installed_run_budget() {
    if std::env::var_os("LOUISELM_REQUIRE_BROKER_GUARD").is_none() {
        eprintln!("skipping: shared Run budget requires the Debian launcher VM");
        return;
    }
    assert!(rustix::process::geteuid().is_root());
    let _account = BrokerAccount::create();
    let root = tempfile::Builder::new()
        .prefix("louiselm-run-budget-")
        .tempdir_in("/var/lib")
        .unwrap();
    let (paths, mut config, registry) = install_fixture_with_slots(root.path(), 3);
    provider_credentials::provision_empty_state(root.path());
    let custody = ProviderCredentialStore::root_in(&root.path().join("state"));
    fs::write(custody.join("openai"), b"fixture-secret").unwrap();
    chown(custody.join("openai"), Some(BROKER_UID), Some(BROKER_UID)).unwrap();
    fs::set_permissions(custody.join("openai"), fs::Permissions::from_mode(0o600)).unwrap();
    registry_record(
        &registry.join("envelopes.json"),
        &serde_json::json!([{"id":"envelope","network":"brokered","description":"one shared Provider attempt"}]),
    );
    config.conformance = Enforcement::Enforced;
    write_json(&paths.state_root.join("config.json"), &config);
    write_json(&paths.state_root.join("public-config.json"), &config);
    let parent = rustix::process::getppid()
        .unwrap()
        .as_raw_nonzero()
        .get()
        .cast_unsigned();
    measure(&paths, &config, Instant::now() + Duration::from_mins(3)).unwrap();
    let certificate = certify(&paths, Instant::now() + Duration::from_mins(3), parent).unwrap();
    assert_eq!(
        certificate.observations.result().unwrap(),
        ReportResult::Passed,
        "{:?}",
        certificate.observations
    );
    let sessions = root.path().join("sessions");
    fs::create_dir(&sessions).unwrap();
    fs::set_permissions(&sessions, fs::Permissions::from_mode(0o711)).unwrap();
    let (mut child, lines) = worker(root.path());
    marker(&lines, "BUDGET_FIRST_READY");
    let mut first = Controller::new(launch_real(
        &paths,
        &config,
        &registry,
        &sessions,
        request(),
    ));
    let address = marker(&lines, "BUDGET_FIRST_ADDR ")
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let first_answer = first.request(address);
    assert!(first_answer.starts_with("HTTP/1.1 "), "{first_answer}");
    assert_eq!(
        spent(root.path()),
        1,
        "first Session must spend the Run unit"
    );
    fs::write(root.path().join("budget-first-done"), b"").unwrap();
    marker(&lines, "BUDGET_SECOND_READY");
    let mut second = Controller::new(launch_real(
        &paths,
        &config,
        &registry,
        &sessions,
        verifier_launch(0),
    ));
    let address = marker(&lines, "BUDGET_SECOND_ADDR ")
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let denied = second.request(address);
    assert!(denied.contains("429 Too Many Requests"), "{denied}");
    assert!(denied.contains("capability_denied"), "{denied}");
    assert_eq!(
        spent(root.path()),
        1,
        "second Session cannot reset the Run total"
    );
    fs::write(root.path().join("budget-second-done"), b"").unwrap();
    marker(&lines, "BUDGET_DONE");
    assert!(child.0.wait().unwrap().success());
    first.finish();
    second.finish();
    for slot in 0..3 {
        crate::launcher_install::acquire_identity(&paths, slot)
            .unwrap()
            .release()
            .unwrap();
    }
}
