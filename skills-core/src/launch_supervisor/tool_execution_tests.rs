//! Real namespace probes; privileged Session composition is checked separately.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Test fixtures assert setup and observed kernel outcomes."
)]

use super::*;
use crate::{
    launch_protocol::{STATUS_REQUEST_SCHEMA, StatusRequest},
    launch_transport::{CredentialPin, KernelCredentials, SeqpacketListener, TransportError},
    sandbox::IdentityPlan,
};
use std::{fs, io::IoSlice, mem::MaybeUninit, os::unix::net::UnixListener, path::Path, sync::mpsc};

fn fixture() -> (tempfile::TempDir, ToolExecutor) {
    let root = tempfile::tempdir().unwrap();
    for dir in ["runtime", "home", "workspace"] {
        fs::create_dir(root.path().join(dir)).unwrap();
    }
    fs::write(root.path().join("runtime/agent"), b"measured runtime").unwrap();
    fs::write(root.path().join("home/authority"), b"private authority").unwrap();
    let bootstrap = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("louiselm-launch");
    assert!(
        bootstrap.exists(),
        "build louiselm-launch before running the library test alone"
    );
    let backend = BubblewrapBackend::at(Path::new("/usr/bin/bwrap"))
        .with_bootstrap(&bootstrap)
        .without_cgroup();
    let plan = ConfinementPlan {
        session_id: "test-agent".to_owned(),
        runtime_root: root.path().join("runtime"),
        executable: root.path().join("runtime/agent"),
        arguments: vec![],
        environment: BTreeMap::new(),
        home: root.path().join("home"),
        workspace: root.path().join("workspace"),
        cache: None,
        beads_replica: None,
        system_roots: default_system_roots(),
        network: NetworkPolicy::Denied,
        identity: IdentityPlan::NamespaceOnly,
        channels: vec![],
    };
    (root, ToolExecutor::new(backend, &plan).unwrap())
}

fn command(
    executor: &ToolExecutor,
    command: &str,
    timeout: Duration,
) -> Result<ToolExecutionResult, SupervisorError> {
    let mut plan = executor.plan.clone();
    plan.session_id = "tool-test".to_owned();
    plan.arguments = vec!["-c".to_owned(), command.to_owned()];
    run(
        &executor.backend,
        &plan,
        timeout,
        &AtomicBool::new(false),
        || Ok(true),
        |prepared| prepared.start().map_err(super::super::system::map_sandbox),
    )
}

#[test]
fn tool_tracker_environment_comes_only_from_the_launcher_replica() {
    let (root, executor) = fixture();
    assert!(!executor.plan.environment.contains_key("BEADS_DIR"));
    let mut agent = executor.plan.clone();
    agent.runtime_root = root.path().join("runtime");
    agent.home = root.path().join("home");
    let replica = root.path().join("beads-replica");
    agent.beads_replica = Some(replica.clone());
    agent
        .environment
        .insert("BEADS_DIR".into(), "/untrusted".into());
    let tool = ToolExecutor::new(executor.backend.clone(), &agent).unwrap();
    assert_eq!(tool.plan.beads_replica, Some(replica.clone()));
    assert_eq!(
        tool.plan.environment["BEADS_DIR"],
        replica.join("current/.beads").display().to_string()
    );
}

#[test]
fn hostile_tool_cannot_reach_agent_files_memory_or_private_socket() {
    let (root, executor) = fixture();
    let private = root.path().join("home/private.sock");
    let _socket = UnixListener::bind(&private).unwrap();
    assert!(fs::read(root.path().join("home/authority")).is_ok());
    assert!(fs::read_link(format!("/proc/{}/exe", std::process::id())).is_ok());
    fs::write(
        root.path().join("workspace/probe.py"),
        format!(
            r"
import ctypes, os, socket
private = {private:?}
for path in [{home:?}, {runtime:?}, '/proc/{pid}/mem', '/proc/{pid}/root']:
    for flags in [os.O_RDONLY, os.O_WRONLY]:
        try:
            fd = os.open(path, flags)
        except OSError:
            pass
        else:
            os.close(fd)
            raise AssertionError('Agent state was exposed')
try:
    os.kill({pid}, 0)
except OSError:
    pass
else:
    raise AssertionError('Agent is visible in tool PID namespace')
libc = ctypes.CDLL(None, use_errno=True)
assert libc.ptrace(16, {pid}, 0, 0) == -1
s = socket.socket(socket.AF_UNIX)
try:
    s.connect(private)
except OSError:
    pass
else:
    raise AssertionError('private socket was reachable')
assert not os.path.exists('/tmp/louiselm-capability.sock')
open('permitted', 'w').write('workspace-only')
print('denied')
",
            private = private.to_string_lossy(),
            home = root.path().join("home/authority").to_string_lossy(),
            runtime = root.path().join("runtime/agent").to_string_lossy(),
            pid = std::process::id()
        ),
    )
    .unwrap();
    let output = command(&executor, "python3 probe.py", Duration::from_secs(5)).unwrap();
    assert_eq!(output.exit_code, 0, "{}", output.stderr);
    assert_eq!(output.stdout, "denied\n");
    assert_eq!(
        fs::read_to_string(root.path().join("workspace/permitted")).unwrap(),
        "workspace-only"
    );
    assert_eq!(
        fs::read(root.path().join("home/authority")).unwrap(),
        b"private authority"
    );
    assert_eq!(
        fs::read(root.path().join("runtime/agent")).unwrap(),
        b"measured runtime"
    );
}

#[test]
fn tool_output_and_descendant_lifetime_are_bounded() {
    let (root, executor) = fixture();
    let output = command(&executor, "yes x", Duration::from_millis(400)).unwrap();
    assert!(output.timed_out && output.truncated);
    assert!(output.stdout.len() <= MAX_TOOL_OUTPUT_BYTES);
    let output = command(
        &executor,
        "(sleep 1; touch escaped) & sleep 10",
        Duration::from_millis(300),
    )
    .unwrap();
    assert!(output.timed_out);
    thread::sleep(Duration::from_millis(1_100));
    assert!(!root.path().join("workspace/escaped").exists());
}

#[test]
fn cancelled_start_never_executes_repository_code() {
    let (root, executor) = fixture();
    let mut plan = executor.plan.clone();
    plan.session_id = "cancelled-tool".to_owned();
    plan.arguments = vec!["-c".to_owned(), "touch should-not-exist".to_owned()];
    assert_eq!(
        run(
            &executor.backend,
            &plan,
            Duration::from_secs(5),
            &AtomicBool::new(true),
            || Ok(true),
            |prepared| prepared.start().map_err(super::super::system::map_sandbox)
        ),
        Err(SupervisorError::AgentIdentityRejected)
    );
    assert!(!root.path().join("workspace/should-not-exist").exists());
}

#[test]
fn failed_tool_cleanup_remains_failed_on_repeated_disposal() {
    let (_root, mut executor) = fixture();
    executor.worker = Some(thread::spawn(|| Err(SupervisorError::CleanupUnproven)));
    assert_eq!(executor.cancel(), Err(SupervisorError::CleanupUnproven));
    assert_eq!(executor.dispose(), Err(SupervisorError::CleanupUnproven));
    assert_eq!(executor.dispose(), Err(SupervisorError::CleanupUnproven));
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "One descriptor-authority scenario preserves the causal positive control, SCM_RIGHTS transfer, descendant denial and direct-connection denial."
)]
fn passed_agent_descriptor_and_direct_tool_connection_have_no_authority() {
    use rustix::net::{
        AddressFamily, SendAncillaryBuffer, SendAncillaryMessage, SendFlags, SocketAddrUnix,
        SocketFlags, SocketType, connect, send, sendmsg, socket_with,
    };
    for descendant in [false, true] {
        let (root, executor) = fixture();
        let path = root.path().join("workspace/capability.sock");
        let listener = SeqpacketListener::bind(&path).unwrap();
        let credentials = KernelCredentials {
            pid: std::process::id(),
            uid: rustix::process::getuid().as_raw(),
            gid: rustix::process::getgid().as_raw(),
        };
        let (accepted_tx, accepted_rx) = mpsc::channel();
        listener
            .accept(
                CredentialPin::Process(credentials),
                Box::new(move |result| {
                    accepted_tx.send(result).unwrap();
                }),
            )
            .unwrap();
        let peer = socket_with(
            AddressFamily::UNIX,
            SocketType::SEQPACKET,
            SocketFlags::CLOEXEC,
            None,
        )
        .unwrap();
        connect(&peer, &SocketAddrUnix::new(&path).unwrap()).unwrap();
        let server = accepted_rx
            .recv_timeout(Duration::from_secs(3))
            .unwrap()
            .unwrap();
        let bytes = StatusRequest {
            schema: STATUS_REQUEST_SCHEMA.to_owned(),
            protocol_version: 1,
            request_id: "probe".to_owned(),
            session_id: "session".to_owned(),
            run_id: "run".to_owned(),
        }
        .canonical_bytes();
        // Positive control: the actual owning process may send on this descriptor.
        let (received_tx, received_rx) = mpsc::channel();
        server
            .receive(Box::new(move |result| {
                received_tx.send(result).unwrap();
            }))
            .unwrap();
        send(&peer, &bytes, SendFlags::NOSIGNAL).unwrap();
        assert_eq!(
            received_rx
                .recv_timeout(Duration::from_secs(3))
                .unwrap()
                .unwrap()
                .bytes,
            bytes
        );

        let transfer = UnixListener::bind(root.path().join("workspace/transfer.sock")).unwrap();
        transfer.set_nonblocking(true).unwrap();
        let rights_worker = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            let stream = loop {
                match transfer.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error)
                        if error.kind() == io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        thread::sleep(Duration::from_millis(5));
                    }
                    result => panic!("tool never requested passed descriptor: {result:?}"),
                }
            };
            let descriptors = [peer.as_fd()];
            let mut storage = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
            let mut ancillary = SendAncillaryBuffer::new(&mut storage);
            assert!(ancillary.push(SendAncillaryMessage::ScmRights(&descriptors)));
            sendmsg(
                &stream,
                &[IoSlice::new(b"fd")],
                &mut ancillary,
                SendFlags::NOSIGNAL,
            )
            .unwrap();
        });
        fs::write(root.path().join("workspace/packet"), &bytes).unwrap();
        fs::write(
            root.path().join("workspace/send.py"),
            format!(
                r"
import array, os, socket
s = socket.socket(socket.AF_UNIX)
s.connect('transfer.sock')
_, control, _, _ = s.recvmsg(2, socket.CMSG_SPACE(4))
fds = array.array('i')
fds.frombytes(control[0][2][:4])
cap = socket.socket(fileno=fds[0])
packet = open('packet', 'rb').read()
if {descendant}:
    child = os.fork()
    if child:
        assert os.waitpid(child, 0)[1] == 0
    else:
        cap.send(packet)
        os._exit(0)
else:
    cap.send(packet)
",
                descendant = if descendant { "True" } else { "False" }
            ),
        )
        .unwrap();
        let (denied_tx, denied_rx) = mpsc::channel();
        server
            .receive(Box::new(move |result| {
                denied_tx.send(result).unwrap();
            }))
            .unwrap();
        let output = command(&executor, "python3 send.py", Duration::from_secs(5)).unwrap();
        rights_worker.join().unwrap();
        assert_eq!(output.exit_code, 0, "{}", output.stderr);
        assert!(matches!(
            denied_rx.recv_timeout(Duration::from_secs(3)).unwrap(),
            Err(TransportError::MessageCredentialsMismatch { .. })
        ));

        let (direct_tx, direct_rx) = mpsc::channel();
        listener
            .accept(
                CredentialPin::Process(credentials),
                Box::new(move |result| {
                    direct_tx.send(result).unwrap();
                }),
            )
            .unwrap();
        let output = command(&executor, "python3 -c 'import socket; s=socket.socket(socket.AF_UNIX,socket.SOCK_SEQPACKET); s.connect(\"capability.sock\")'", Duration::from_secs(5)).unwrap();
        assert_eq!(output.exit_code, 0, "{}", output.stderr);
        assert!(matches!(
            direct_rx.recv_timeout(Duration::from_secs(3)).unwrap(),
            Err(TransportError::PeerCredentialsMismatch { .. })
        ));
        listener.close();
    }
}
