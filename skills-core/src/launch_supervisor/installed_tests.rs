//! Real dedicated broker, installed software signer and measured supervisor in the VM.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Disposable privileged fixtures assert setup and exact observable outcomes."
)]

use super::*;
use crate::{
    broker::{ApprovedCommands, GrantRequest, InstalledBroker},
    install::{InstalledState, STATE_SCHEMA},
    launch::{PROTOCOL_VERSION, REQUEST_SCHEMA},
    launch_protocol::{
        CommandMessage, CommandOperation, CommandOutcome, TOOL_EXECUTION_SCHEMA,
        ToolExecutionRequest,
    },
    launch_supervisor::{InstalledLaunchSigner, LaunchSupervisor},
    launcher_install::{IdentityPool, InstallRequest, SystemCommandRunner},
    registry::Registry,
    release::Component,
};
use std::{
    io::{BufRead, BufReader, Write},
    os::{fd::OwnedFd, unix::fs::symlink},
    process::{Child, Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

const BROKER_UID: u32 = 4_019_000;
const AGENT_UID: u32 = 4_020_000;
const COMMAND: &str = "printf authorized > effect; printf done";
const WORKER: &str = "launch_supervisor::system::installed_tests::installed_broker_worker";
const ACCOUNT: &str = "louiselm-broker-gate";

#[path = "installed_failure_tests.rs"]
mod failures;

#[path = "installed_recovery_tests.rs"]
mod recovery;

#[path = "installed_verification_tests.rs"]
mod verification;

struct BrokerAccount;

impl BrokerAccount {
    fn create() -> Self {
        assert!(
            Command::new("/usr/bin/getent")
                .args(["passwd", &BROKER_UID.to_string()])
                .output()
                .unwrap()
                .stdout
                .is_empty(),
            "fixture UID must be unused"
        );
        assert!(
            Command::new("/usr/sbin/groupadd")
                .args(["--gid", &BROKER_UID.to_string(), ACCOUNT])
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("/usr/sbin/useradd")
                .args([
                    "--no-create-home",
                    "--no-log-init",
                    "--uid",
                    &BROKER_UID.to_string(),
                    "--gid",
                    &BROKER_UID.to_string(),
                    "--home-dir",
                    "/nonexistent",
                    "--shell",
                    "/usr/sbin/nologin",
                    ACCOUNT
                ])
                .status()
                .unwrap()
                .success()
        );
        Self
    }
}

impl Drop for BrokerAccount {
    fn drop(&mut self) {
        assert!(
            Command::new("/usr/sbin/userdel")
                .arg(ACCOUNT)
                .status()
                .unwrap()
                .success()
        );
        if Command::new("/usr/bin/getent")
            .args(["group", ACCOUNT])
            .status()
            .unwrap()
            .success()
        {
            assert!(
                Command::new("/usr/sbin/groupdel")
                    .arg(ACCOUNT)
                    .status()
                    .unwrap()
                    .success()
            );
        }
    }
}

struct BrokerChild(Child);
impl Drop for BrokerChild {
    fn drop(&mut self) {
        // Test teardown owns the child even when an assertion unwinds.
        if self.0.try_wait().unwrap().is_none() {
            self.0.kill().unwrap();
        }
        self.0.wait().unwrap();
    }
}

fn paths(root: &Path) -> LauncherPaths {
    LauncherPaths {
        release_prefix: root.join("release"),
        state_root: root.join("release/launcher"),
        sudoers: root.join("sudoers"),
        subuid: root.join("subuid"),
        subgid: root.join("subgid"),
        passwd: PathBuf::from("/etc/passwd"),
        group: PathBuf::from("/etc/group"),
        nsswitch: PathBuf::from("/etc/nsswitch.conf"),
        ssh_keygen: PathBuf::from("/usr/bin/ssh-keygen"),
        getent: PathBuf::from("/usr/bin/getent"),
        visudo: PathBuf::from("/usr/sbin/visudo"),
        bwrap: PathBuf::from("/usr/bin/bwrap"),
        broker_socket: root.join("rendezvous/control.sock"),
    }
}

fn request() -> LaunchRequest {
    LaunchRequest {
        schema: REQUEST_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        request_id: "launch".into(),
        authorization_id: "approval".into(),
        session_id: "session".into(),
        run_id: "run".into(),
        agent_id: "agent".into(),
        envelope_id: "envelope".into(),
        envelope_revision: 1,
        skill_generation_id: Digest::of(b"fixture-generation").to_string(),
        session_input_manifest_id: Digest::of(b"fixture-input").to_string(),
    }
}

fn write_json(path: &Path, value: &impl serde::Serialize) {
    fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
}

fn registry_record(path: &Path, entries: &serde_json::Value) {
    write_json(
        path,
        &serde_json::json!({"schema":crate::registry::REGISTRY_SCHEMA,"entries":entries}),
    );
}

fn install_fixture(root: &Path) -> (LauncherPaths, LauncherConfig, PathBuf) {
    install_fixture_with_slots(root, 1)
}

#[expect(
    clippy::too_many_lines,
    reason = "One fixture installs measured binaries, root authority and a dedicated broker without modifying host or system installation."
)]
fn install_fixture_with_slots(root: &Path, slots: u32) -> (LauncherPaths, LauncherConfig, PathBuf) {
    fs::set_permissions(root, fs::Permissions::from_mode(0o711)).unwrap();
    let binaries = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let fixture_paths = paths(root);
    let runtime = root.join("runtime");
    fs::create_dir(&runtime).unwrap();
    let agent = runtime.join("agent");
    fs::copy(binaries.join("louiselm-tool-test-agent"), &agent).unwrap();
    fs::set_permissions(&agent, fs::Permissions::from_mode(0o555)).unwrap();
    let mut manifest = super::tool_integration_tests::manifest(&agent);
    for name in ["louiselm-launch", "louiselm-tool-test-helper"] {
        let executable = binaries.join(name);
        manifest.components.push(Component {
            name: name.into(),
            path: format!("bin/{name}"),
            sha256: Digest::of(&fs::read(&executable).unwrap()).hex().into(),
            size: fs::metadata(executable).unwrap().len(),
            executable: true,
        });
    }
    manifest.release_id = manifest.digest().to_string();
    let release = fixture_paths
        .release_prefix
        .join("releases")
        .join(&manifest.release_id);
    fs::create_dir_all(release.join("bin")).unwrap();
    for component in &manifest.components {
        fs::copy(
            binaries.join(&component.name),
            release.join(&component.path),
        )
        .unwrap();
        fs::set_permissions(
            release.join(&component.path),
            fs::Permissions::from_mode(0o555),
        )
        .unwrap();
    }
    write_json(&release.join("manifest.json"), &manifest);
    symlink(
        Path::new("releases").join(&manifest.release_id),
        fixture_paths.release_prefix.join("current"),
    )
    .unwrap();
    write_json(
        &fixture_paths.release_prefix.join("state.json"),
        &InstalledState {
            schema: STATE_SCHEMA.into(),
            release_id: manifest.release_id.clone(),
            built_at_ms: 1,
            installed_at_ms: 2,
            source_commit: "fixture".into(),
            policy_version: "fixture".into(),
        },
    );
    for file in [&fixture_paths.subuid, &fixture_paths.subgid] {
        fs::write(file, b"").unwrap();
    }
    let operator = if fs::read_to_string("/etc/passwd")
        .unwrap()
        .lines()
        .any(|line| line.starts_with("vm:"))
    {
        "vm"
    } else {
        "runner"
    };
    let installed = crate::launcher_install::install(
        &fixture_paths,
        &SystemCommandRunner,
        &InstallRequest {
            operator: operator.into(),
            broker_uid: BROKER_UID,
            broker_gid: BROKER_UID,
            pool: IdentityPool {
                uid_start: AGENT_UID,
                gid_start: AGENT_UID,
                slots,
            },
        },
        3,
    )
    .unwrap();
    assert!(installed.failures.is_empty(), "{:?}", installed.failures);
    let config = crate::launcher_install::runtime_config(&fixture_paths).unwrap();
    for name in ["state", "rendezvous"] {
        let directory = root.join(name);
        fs::create_dir(&directory).unwrap();
        chown(&directory, Some(BROKER_UID), Some(BROKER_UID)).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let registry = root.join("registry");
    fs::create_dir(&registry).unwrap();
    registry_record(
        &registry.join("agents.json"),
        &serde_json::json!([{
            "id":"agent","provider":"fixture","runtime_id":"runtime","arguments":[],"environment":{},
            "tool_integration":super::super::tool_integration::CONTRACT
        }]),
    );
    registry_record(
        &registry.join("runtimes.json"),
        &serde_json::json!([{
            "id":"runtime","root":runtime,"executable":"agent","executable_sha256":manifest.components[0].sha256,
            "adapters":[],"version":"fixture","origin":"fixture"
        }]),
    );
    registry_record(
        &registry.join("envelopes.json"),
        &serde_json::json!([{
            "id":"envelope","network":"denied","description":"exact fixture command"
        }]),
    );
    (fixture_paths, config, registry)
}

#[test]
fn installed_broker_worker() {
    let Some(root) = std::env::var_os("LOUISELM_BROKER_FIXTURE") else {
        return;
    };
    let root = PathBuf::from(root);
    let broker = InstalledBroker::bind(&paths(&root), &root.join("state")).unwrap();
    assert!(fs::read(paths(&root).state_root.join("config.json")).is_err());
    assert!(fs::read_dir(paths(&root).state_root.join("private")).is_err());
    let now = u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    let config = crate::launcher_install::public_runtime_config(&paths(&root)).unwrap();
    broker
        .authorize(&GrantRequest {
            require_cold_recovery: true,
            request: request(),
            controller_uid: config.operator_uid,
            expires_at_ms: now + 30_000,
            broker_loss_grace_ms: 500,
            commands: Some(ApprovedCommands {
                command_digest: Digest::of(COMMAND.as_bytes()).to_string(),
                timeout_ms: 5000,
                uses: 3,
                allow_delegation: true,
                expires_at_ms: now + 60_000,
            }),
        })
        .unwrap();
    println!("BROKER_READY");
    let launched = broker.serve_launch();
    if std::env::var_os("LOUISELM_BROKER_FAULT").is_some() {
        assert!(
            launched.is_err(),
            "fault must not return a launched Session"
        );
        let inspection = broker.inspect("session").unwrap().unwrap();
        assert_eq!(
            inspection.state,
            crate::launch_receipt::SessionState::Starting
        );
        assert!(inspection.start_evidence.is_none());
        println!("BROKER_REJECTED");
        return;
    }
    let mut session = launched.unwrap();
    let inspection = broker.inspect_active(&session).unwrap();
    assert_eq!(inspection.broker_head.unwrap().sequence, 1);
    assert!(inspection.launch_evidence.is_some());
    assert_eq!(
        inspection.launch,
        crate::broker::LaunchObservation::Acknowledged { channel_open: true }
    );
    let proof = inspection.start_evidence.unwrap();
    assert_eq!(proof.assigned_uid, AGENT_UID);
    println!("BROKER_RUNNING {}", proof.agent_pid);
    recovery::controller_registers(&broker, &mut session, config.operator_uid);
    while !broker.step(&mut session).unwrap() {}
    assert_eq!(
        broker.inspect("session").unwrap().unwrap().state,
        crate::launch_receipt::SessionState::Terminal
    );
    println!("BROKER_TERMINAL");
}

fn broker_process(root: &Path, fault: Option<&str>) -> (BrokerChild, mpsc::Receiver<String>) {
    let mut command = Command::new("/usr/bin/setpriv");
    command
        .args([
            "--reuid",
            &BROKER_UID.to_string(),
            "--regid",
            &BROKER_UID.to_string(),
            "--clear-groups",
        ])
        .arg(std::env::current_exe().unwrap())
        .args([WORKER, "--exact", "--nocapture"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LOUISELM_BROKER_FIXTURE", root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    if let Some(fault) = fault {
        command.env("LOUISELM_BROKER_FAULT", fault);
    }
    let mut child = command.spawn().unwrap();
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

fn marker(lines: &mpsc::Receiver<String>, prefix: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let line = lines
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap();
        if line.starts_with(prefix) {
            return line;
        }
    }
}

#[test]
fn privileged_installed_broker_launch_and_effects() {
    installed_broker_effects(false);
}

#[test]
fn privileged_installed_controller_loss_settlement() {
    installed_broker_effects(true);
}

#[expect(
    clippy::too_many_lines,
    reason = "One end-to-end fixture owns the separate broker, signed launch, real request and terminal cleanup."
)]
fn installed_broker_effects(controller_loss: bool) {
    if std::env::var_os("LOUISELM_REQUIRE_BROKER_LAUNCH").is_none() {
        eprintln!("skipping: installed broker composition requires the disposable launcher VM");
        return;
    }
    assert!(rustix::process::geteuid().is_root());
    assert!(
        fs::read_to_string("/proc/self/uid_map")
            .unwrap()
            .split_whitespace()
            .eq(["0", "0", "4294967295"])
    );
    let _account = BrokerAccount::create();
    let root = tempfile::Builder::new()
        .prefix("louiselm-broker-")
        .tempdir_in("/var/lib")
        .unwrap();
    let (paths, config, registry_root) = install_fixture(root.path());
    assert!(matches!(
        InstalledBroker::bind(&paths, &root.path().join("state")),
        Err(crate::broker::BrokerError::Installation)
    ));
    let (mut broker_process, lines) = broker_process(root.path(), None);
    marker(&lines, "BROKER_READY");
    let mut platform =
        SystemLaunchPlatform::new(paths.clone(), config.clone(), Duration::from_secs(5)).unwrap();
    platform.registry_root = registry_root.clone();
    let sessions = root.path().join("sessions");
    fs::create_dir(&sessions).unwrap();
    fs::set_permissions(&sessions, fs::Permissions::from_mode(0o711)).unwrap();
    let supervisor = LaunchSupervisor::new(
        connect_control_broker(&config, Duration::from_secs(5)).unwrap(),
        Arc::new(InstalledLaunchSigner::open(&paths, Duration::from_secs(5)).unwrap()),
        Arc::new(platform),
        Arc::new(Registry::open_trusted(&registry_root).unwrap()),
        sessions.clone(),
        Duration::from_secs(5),
    );
    let (tx, rx) = mpsc::channel();
    let now = u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    supervisor
        .launch(
            request(),
            config.operator_uid,
            now,
            Box::new(move |result| {
                tx.send(result).unwrap();
            }),
        )
        .unwrap();
    let session = rx.recv_timeout(Duration::from_secs(20)).unwrap().unwrap();
    let agent_pid: u32 = marker(&lines, "BROKER_RUNNING ")
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let (mut input, controller_input) = std::os::unix::net::UnixStream::pair().unwrap();
    let (controller_output, output) = std::os::unix::net::UnixStream::pair().unwrap();
    output
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let (relay_tx, relay_rx) = mpsc::channel();
    thread::spawn(move || {
        relay_tx
            .send(
                session.relay_stdio(
                    RelayStdio::new(
                        BufReader::new(fs::File::from(OwnedFd::from(controller_input))),
                        fs::File::from(OwnedFd::from(controller_output)),
                    )
                    .unwrap(),
                ),
            )
            .unwrap();
    });
    let mut output = BufReader::new(output);
    input.write_all(b"positive-control\n").unwrap();
    let mut echo = String::new();
    output.read_line(&mut echo).unwrap();
    assert_eq!(echo, "positive-control\n");
    let command = ToolExecutionRequest {
        schema: TOOL_EXECUTION_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        request_id: "effect".into(),
        session_id: "session".into(),
        run_id: "run".into(),
        envelope_revision: 1,
        sequence: 1,
        command: COMMAND.into(),
        timeout_ms: 5000,
    };
    recovery::initialize_checkpoint(
        &mut input,
        &mut output,
        &command,
        &mut broker_process.0,
        &lines,
    );
    input.write_all(&[0x1e]).unwrap();
    input.write_all(&command.canonical_bytes()).unwrap();
    input.write_all(b"\n").unwrap();
    let mut reply = Vec::new();
    output.read_until(b'\n', &mut reply).unwrap();
    assert_eq!(reply.remove(0), 0x1e);
    let response: CommandMessage = serde_json::from_slice(&reply).unwrap();
    let CommandOperation::Result {
        outcome: CommandOutcome::Completed { output: result },
    } = response.operation
    else {
        panic!("command must complete");
    };
    assert_eq!(result.exit_code, 0, "{result:?}");
    assert_eq!(result.stdout, "done");
    assert!(result.stderr.is_empty());
    assert_eq!(
        fs::read(sessions.join("session/workspace/effect")).unwrap(),
        b"authorized"
    );
    assert_denial_and_helper(
        &mut input,
        &mut output,
        &sessions.join("session/workspace"),
        &command,
    );
    if controller_loss {
        drop(input);
    } else {
        rustix::process::kill_process(
            rustix::process::Pid::from_raw(i32::try_from(agent_pid).unwrap()).unwrap(),
            rustix::process::Signal::TERM,
        )
        .unwrap();
    }
    marker(&lines, "BROKER_TERMINAL");
    assert!(broker_process.0.wait().unwrap().success());
    if controller_loss {
        recovery::assert_loss_settled(root.path());
    }
    relay_rx
        .recv_timeout(Duration::from_secs(10))
        .unwrap()
        .unwrap();
    crate::launcher_install::acquire_identity(&paths, 0)
        .unwrap()
        .release()
        .unwrap();
}

fn exchange(input: &mut impl Write, output: &mut impl BufRead, bytes: &[u8]) -> CommandMessage {
    input.write_all(&[0x1e]).unwrap();
    input.write_all(bytes).unwrap();
    input.write_all(b"\n").unwrap();
    input.flush().unwrap();
    let mut reply = Vec::new();
    output.read_until(b'\n', &mut reply).unwrap();
    assert_eq!(reply.remove(0), 0x1e);
    serde_json::from_slice(&reply).unwrap()
}

fn assert_denial_and_helper(
    input: &mut impl Write,
    output: &mut impl BufRead,
    workspace: &Path,
    command: &ToolExecutionRequest,
) {
    let mut denied = command.clone();
    denied.sequence = 2;
    denied.request_id = "denied".into();
    denied.command = "printf escaped > escaped".into();
    let reply = exchange(input, output, &denied.canonical_bytes());
    assert!(matches!(
        reply.operation,
        CommandOperation::Result {
            outcome: CommandOutcome::NotStarted { .. }
        }
    ));
    assert!(!workspace.join("escaped").exists());
    fs::remove_file(workspace.join("effect")).unwrap();
    let mut initial = command.clone();
    initial.request_id = "helper-work".into();
    let delegate = CommandMessage {
        schema: crate::launch_protocol::COMMAND_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        request_id: "delegate".into(),
        session_id: command.session_id.clone(),
        run_id: command.run_id.clone(),
        envelope_revision: command.envelope_revision,
        operation: CommandOperation::Delegate {
            grant: crate::launch_protocol::GrantRequest {
                sequence: 1,
                command_digest: Digest::of(COMMAND.as_bytes()).to_string(),
                timeout_ms: 5000,
                uses: 1,
                valid_for_ms: 5000,
            },
            command: initial,
        },
    };
    let reply = exchange(input, output, &delegate.canonical_bytes());
    assert!(
        matches!(reply.operation, CommandOperation::Granted { grant: 1, .. }),
        "{reply:?}"
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while !workspace.join("effect").exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(fs::read(workspace.join("effect")).unwrap(), b"authorized");
}
