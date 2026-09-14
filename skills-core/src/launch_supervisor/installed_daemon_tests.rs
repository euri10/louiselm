//! The actual activated executable, inside a disposable private mount namespace.
use super::*;
use crate::launch_supervisor::LaunchedSession;
use crate::launch_transport::{SeqpacketConnector, SeqpacketListener};
use rustix::net::{
    AddressFamily, SocketAddrUnix, SocketFlags, SocketType, bind, listen, socket_with,
};
use std::os::unix::process::ExitStatusExt;

const STATE: &str = "/var/lib/louiselm/broker";
const SEED: &str = "launch_supervisor::system::installed_tests::daemon::seed_authorizations";

#[path = "installed_daemon_attention_tests.rs"]
mod attention;

fn completed<T: Send + 'static>(queue: impl FnOnce(Box<dyn FnOnce(T) + Send>)) -> T {
    let (tx, rx) = mpsc::channel();
    queue(Box::new(move |value| tx.send(value).unwrap()));
    rx.recv_timeout(Duration::from_mins(1)).unwrap()
}

fn named_request(name: &str) -> LaunchRequest {
    let mut request = request();
    request.session_id = name.into();
    request.request_id = format!("launch-{name}");
    request.authorization_id = format!("approval-{name}");
    request
}

#[test]
fn seed_authorizations() {
    if std::env::var_os("LOUISELM_DAEMON_SEED").is_none() {
        return;
    }
    let listener =
        SeqpacketListener::adopt(rustix::io::fcntl_dupfd_cloexec(std::io::stdin(), 3).unwrap())
            .unwrap();
    let paths = LauncherPaths::system();
    let broker = InstalledBroker::over(&paths, Path::new(STATE), listener).unwrap();
    let config = crate::launcher_install::public_runtime_config(&paths).unwrap();
    let now = u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    for name in ["session", "sibling", "failure"] {
        broker
            .authorize(&GrantRequest {
                request: named_request(name),
                controller_uid: config.operator_uid,
                require_cold_recovery: false,
                expires_at_ms: now + 180_000,
                broker_loss_grace_ms: crate::launch::MAX_BROKER_LOSS_GRACE_MS,
                commands: None,
            })
            .unwrap();
    }
    attention::enqueue("before-start");
}

fn mounts(root: &Path) {
    assert_ne!(
        fs::read_link("/proc/self/ns/mnt").unwrap(),
        fs::read_link("/proc/1/ns/mnt").unwrap(),
        "run under unshare --mount --propagation private"
    );
    // Atomic subordinate-ID replacement needs a directory mount, not an
    // individual file mount whose rename would fail with EBUSY.
    assert!(
        Command::new("/bin/cp")
            .args(["-a", "/etc"])
            .arg(root.join("etc"))
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("/usr/bin/mount")
            .arg("--bind")
            .arg(root.join("etc"))
            .arg("/etc")
            .status()
            .unwrap()
            .success()
    );
    for target in [
        "/usr/local/lib",
        "/run",
        "/etc/sudoers.d",
        "/var/lib/louiselm",
    ] {
        fs::create_dir_all(target).unwrap();
        assert!(
            Command::new("/usr/bin/mount")
                .args(["-t", "tmpfs", "-o", "mode=0755", "tmpfs", target])
                .status()
                .unwrap()
                .success()
        );
    }
}

fn process(manager: &OwnedFd, uid: u32, seed: bool) -> BrokerChild {
    process_with_groups(manager, uid, seed, &uid.to_string())
}

fn process_with_groups(manager: &OwnedFd, uid: u32, seed: bool, groups: &str) -> BrokerChild {
    let mut command = Command::new("/usr/bin/setpriv");
    command
        .args([
            "--reuid",
            &uid.to_string(),
            "--regid",
            &uid.to_string(),
            // systemd 257 initializes the supplementary list with the primary
            // GID even for an account with no additional group memberships.
            "--groups",
            groups,
        ])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::from(manager.try_clone().unwrap()))
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    if seed {
        command
            .arg(std::env::current_exe().unwrap())
            .args([SEED, "--exact", "--nocapture"])
            .env("LOUISELM_DAEMON_SEED", "1");
    } else {
        command.args(["/bin/sh", "-c", "export LISTEN_PID=$$ LISTEN_FDS=1; exec 3<&0; exec 0</dev/null; exec /usr/local/lib/louiselm/current/bin/louiselm-control serve"]);
    }
    BrokerChild(command.spawn().unwrap())
}

fn terminate(child: &mut BrokerChild) {
    let pid = rustix::process::Pid::from_child(&child.0);
    let start = Instant::now();
    rustix::process::kill_process(pid, rustix::process::Signal::TERM).unwrap();
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            assert_eq!(status.signal(), Some(15));
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "SIGTERM must not drain workers"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn connect(config: &LauncherConfig) -> crate::launch_transport::SeqpacketChannel {
    let connector = SeqpacketConnector::new().unwrap();
    completed(|done| {
        connector
            .connect_via_manager(
                &config.broker_socket_path,
                CredentialPin::Identity {
                    uid: BROKER_UID,
                    gid: BROKER_UID,
                },
                CredentialPin::Identity { uid: 0, gid: 0 },
                done,
            )
            .unwrap();
    })
    .unwrap()
}

fn ready(config: &LauncherConfig) {
    // Socket activation queues this before startup validation completes. A
    // refusal of unapproved work proves the real accept loop is now serving.
    let peer = connect(config);
    completed(|done| {
        peer.send(named_request("readiness").canonical_bytes(), done)
            .unwrap();
    })
    .unwrap();
    let packet = completed(|done| peer.receive(done).unwrap()).unwrap();
    assert!(
        matches!(packet.packet, crate::launch_transport::LauncherPacket::Response(response)
        if matches!(response.result, ResponseResult::Error { .. }))
    );
    peer.close();
}

fn launch(
    paths: &LauncherPaths,
    config: &LauncherConfig,
    root: &Path,
    name: &str,
) -> Result<LaunchedSession, SupervisorError> {
    let registry = root.join("registry");
    let mut platform =
        SystemLaunchPlatform::new(paths.clone(), config.clone(), Duration::from_secs(5)).unwrap();
    platform.registry_root = registry.clone();
    let supervisor = LaunchSupervisor::new(
        connect_control_broker(config, Duration::from_secs(5)).unwrap(),
        Arc::new(InstalledLaunchSigner::open(paths, Duration::from_secs(5)).unwrap()),
        Arc::new(platform),
        Arc::new(Registry::open_trusted(&registry).unwrap()),
        root.join("sessions"),
        Duration::from_secs(5),
    );
    let now = u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    completed(|done| {
        supervisor
            .launch(named_request(name), config.operator_uid, now, done)
            .unwrap();
    })
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "One process lifecycle checks startup refusals, concurrent launches, storage failure, SIGTERM and exact-socket restart under one isolated installation."
)]
fn privileged_activated_daemon_serves_launches_and_restart() {
    if std::env::var_os("LOUISELM_REQUIRE_CONTROL_DAEMON").is_none() {
        eprintln!("skipping: requires disposable VM and private mount namespace");
        return;
    }
    assert!(rustix::process::geteuid().is_root());
    let root = tempfile::Builder::new()
        .prefix("louiselm-daemon-")
        .tempdir_in("/var/lib")
        .unwrap();
    mounts(root.path());
    let _account = BrokerAccount::create();
    let (paths, config, _) = install_fixture_at(root.path(), 3, LauncherPaths::system());
    fs::create_dir(STATE).unwrap();
    chown(STATE, Some(BROKER_UID), Some(BROKER_UID)).unwrap();
    fs::set_permissions(STATE, fs::Permissions::from_mode(0o700)).unwrap();
    fs::create_dir(root.path().join("sessions")).unwrap();
    fs::set_permissions(
        root.path().join("sessions"),
        fs::Permissions::from_mode(0o711),
    )
    .unwrap();
    let manager = socket_with(
        AddressFamily::UNIX,
        SocketType::SEQPACKET,
        SocketFlags::CLOEXEC,
        None,
    )
    .unwrap();
    rustix::net::sockopt::set_socket_passcred(&manager, true).unwrap();
    bind(
        &manager,
        &SocketAddrUnix::new(&config.broker_socket_path).unwrap(),
    )
    .unwrap();
    listen(&manager, 8).unwrap();
    assert!(
        process(&manager, BROKER_UID, true)
            .0
            .wait()
            .unwrap()
            .success()
    );
    assert!(
        !process(&manager, 0, false).0.wait().unwrap().success(),
        "root is not the installed broker"
    );
    assert!(
        !process_with_groups(&manager, BROKER_UID, false, "0")
            .0
            .wait()
            .unwrap()
            .success(),
        "an additional group must never enter the broker"
    );
    let mut daemon = process(&manager, BROKER_UID, false);
    ready(&config);
    // Missing endpoint configuration must not prevent startup. Provision it
    // after startup, without restarting the daemon or connecting a Session.
    attention::configure();
    eprintln!("daemon: ready");
    let silent = connect(&config);
    let session = launch(&paths, &config, root.path(), "session").unwrap();
    eprintln!("daemon: first launch");
    assert_eq!(session.receipt().payload.sequence, 1);
    let sibling = launch(&paths, &config, root.path(), "sibling").unwrap();
    eprintln!("daemon: concurrent sibling launch");
    assert_eq!(sibling.receipt().payload.sequence, 1);
    sibling.dispose().unwrap();
    let receipts = Path::new(STATE).join("receipts/sessions");
    fs::set_permissions(&receipts, fs::Permissions::from_mode(0o500)).unwrap();
    assert!(matches!(
        launch(&paths, &config, root.path(), "failure"),
        Err(SupervisorError::DurabilityUnavailable)
    ));
    fs::set_permissions(&receipts, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        !receipts
            .join("failure/00000000000000000000.receipt.json")
            .exists()
    );
    assert!(daemon.0.try_wait().unwrap().is_none());
    // The receiver was absent throughout both launches: delivery cannot block
    // either the accept loop or an existing Session's worker.
    let listener = attention::listen();
    let (first, request) = attention::receive(&listener);
    assert_eq!(
        request["projection"]["change"]["subject_id"],
        "before-start"
    );
    attention::reply(first, &request, false);
    let (retry, repeated) = attention::receive(&listener);
    assert_eq!(
        request, repeated,
        "bad ACK must retry the exact oldest entry"
    );
    attention::reply(retry, &repeated, true);
    attention::wait_ack(1);
    let sequence = attention::enqueue("while-running");
    // Disposal above also produces real lifecycle Attention. Preserve its
    // ordering rather than assuming our second fixture is the second entry.
    let (mut interrupted, pending) = (2..=sequence)
        .find_map(|expected| {
            let (stream, entry) = attention::receive(&listener);
            assert_eq!(entry["projection"]["sequence"], expected);
            if expected == sequence {
                Some((stream, entry))
            } else {
                attention::reply(stream, &entry, true);
                attention::wait_ack(expected);
                None
            }
        })
        .unwrap();
    assert_eq!(
        pending["projection"]["change"]["subject_id"],
        "while-running"
    );
    terminate(&mut daemon);
    attention::assert_stopped(&mut interrupted);
    attention::assert_pending(sequence);
    assert!(
        completed(|done| silent.receive(done).unwrap()).is_err(),
        "shutdown closes stalled handshake"
    );
    let mut restarted = process(&manager, BROKER_UID, false);
    let (retry, repeated) = attention::receive(&listener);
    assert_eq!(
        pending, repeated,
        "stop must not acknowledge undelivered work"
    );
    attention::reply(retry, &repeated, true);
    attention::wait_ack(sequence);
    // The retained supervisor reattaches to the same manager-owned socket and
    // then completes the ordinary controller-loss/terminal receipt path.
    session.dispose().unwrap();
    assert!(restarted.0.try_wait().unwrap().is_none());
    terminate(&mut restarted);
    let terminal = fs::read_dir(Path::new(STATE).join("receipts/sessions/session"))
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .ends_with("receipt.json")
        })
        .count();
    assert!(
        terminal >= 4,
        "launch/start/park/terminal remain durable across restart"
    );
    fs::set_permissions(STATE, fs::Permissions::from_mode(0o750)).unwrap();
    assert!(
        !process(&manager, BROKER_UID, false)
            .0
            .wait()
            .unwrap()
            .success()
    );
    fs::set_permissions(STATE, fs::Permissions::from_mode(0o700)).unwrap();
    let marker = Path::new(STATE).join("identity.json");
    let mut identity: serde_json::Value =
        serde_json::from_slice(&fs::read(&marker).unwrap()).unwrap();
    identity["uid"] = (BROKER_UID + 1).into();
    write_json(&marker, &identity);
    assert!(
        !process(&manager, BROKER_UID, false)
            .0
            .wait()
            .unwrap()
            .success()
    );
}
