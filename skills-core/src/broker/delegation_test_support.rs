//! Trusted fixture pins and real credential-carrying sockets; no launch-proof claim.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "Tests assert bounded fixture setup."
)]

use crate::{
    Digest,
    broker::{AuditLog, delegation::*},
    launch_protocol::{
        ProtocolMessage, TOOL_EXECUTION_SCHEMA, ToolExecutionRequest, ToolExecutionResult,
    },
    launch_supervisor::CapabilityBinding,
    launch_transport::{
        AuthenticatedPacket, CredentialPin, KernelCredentials, KernelProcess, LauncherPacket,
        SeqpacketChannel, SeqpacketListener,
    },
};
use rustix::{
    net::{
        AddressFamily, SendFlags, SocketAddrUnix, SocketFlags, SocketType, connect, send,
        socket_with,
    },
    process::{Pid, PidfdFlags, getgid, getuid, pidfd_open},
};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};

const WAIT: Duration = Duration::from_secs(5);

pub(super) fn request() -> ToolExecutionRequest {
    ToolExecutionRequest {
        schema: TOOL_EXECUTION_SCHEMA.to_owned(),
        protocol_version: 1,
        request_id: "effect-1".to_owned(),
        session_id: "session-1".to_owned(),
        run_id: "run-1".to_owned(),
        envelope_revision: 0,
        sequence: 1,
        command: "printf delegated-sensitive".to_owned(),
        timeout_ms: 500,
    }
}

pub(super) struct Peer {
    child: Child,
    pub process: Arc<KernelProcess>,
    pub channel: SeqpacketChannel,
}

impl Peer {
    pub fn new(path: &Path) -> Self {
        let listener = SeqpacketListener::bind(path).unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "broker::delegation_tests::support::socket_peer",
                "--nocapture",
            ])
            .env("LOUISELM_DELEGATION_TEST_SOCKET", path)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut ready = [0];
        child
            .stderr
            .as_mut()
            .unwrap()
            .read_exact(&mut ready)
            .unwrap();
        assert_eq!(ready, *b"r");
        let credentials = KernelCredentials {
            pid: child.id(),
            uid: getuid().as_raw(),
            gid: getgid().as_raw(),
        };
        // The child is our known current-executable fixture and is blocked on
        // stdin. Production obtains this pin only from the existing exec proof.
        let process = Arc::new(
            KernelProcess::from_exec_stop(
                credentials,
                pidfd_open(
                    Pid::from_raw(i32::try_from(child.id()).unwrap()).unwrap(),
                    PidfdFlags::empty(),
                )
                .unwrap(),
                &fs::File::open(std::env::current_exe().unwrap()).unwrap(),
            )
            .unwrap(),
        );
        let (tx, rx) = mpsc::channel();
        listener
            .accept(
                CredentialPin::LiveProcess(Arc::clone(&process)),
                Box::new(move |result| {
                    tx.send(result).unwrap();
                }),
            )
            .unwrap();
        child.stdin.as_mut().unwrap().write_all(b"c").unwrap();
        let channel = rx.recv_timeout(WAIT).unwrap().unwrap();
        Self {
            child,
            process,
            channel,
        }
    }
    pub fn bound(&self) -> BoundProcess {
        BoundProcess {
            process: Arc::clone(&self.process),
            channel: self.channel.clone(),
        }
    }
    pub fn credentials(&self) -> KernelCredentials {
        self.process.credentials()
    }
    pub fn packet(&self) -> AuthenticatedPacket {
        let request = request();
        AuthenticatedPacket {
            bytes: request.canonical_bytes(),
            packet: LauncherPacket::Request(ProtocolMessage::ToolExecution(request)),
            peer_credentials: self.credentials(),
            message_credentials: self.credentials(),
        }
    }
    pub fn receive_real_packet(&mut self) -> AuthenticatedPacket {
        let (tx, rx) = mpsc::channel();
        self.channel
            .receive(Box::new(move |result| {
                tx.send(result).unwrap();
            }))
            .unwrap();
        self.child.stdin.as_mut().unwrap().write_all(b"p").unwrap();
        rx.recv_timeout(WAIT).unwrap().unwrap()
    }
    pub fn kill(&mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.channel.close();
    }
}

#[test]
fn socket_peer() {
    let Some(path) = std::env::var_os("LOUISELM_DELEGATION_TEST_SOCKET") else {
        return;
    };
    std::io::stderr().write_all(b"r").unwrap();
    let mut byte = [0];
    std::io::stdin().read_exact(&mut byte).unwrap();
    assert_eq!(byte, *b"c");
    let socket = socket_with(
        AddressFamily::UNIX,
        SocketType::SEQPACKET,
        SocketFlags::CLOEXEC,
        None,
    )
    .unwrap();
    connect(&socket, &SocketAddrUnix::new(Path::new(&path)).unwrap()).unwrap();
    while std::io::stdin().read_exact(&mut byte).is_ok() {
        if byte == *b"p" {
            send(&socket, &request().canonical_bytes(), SendFlags::NOSIGNAL).unwrap();
        }
    }
}

pub(super) struct Fixture {
    pub root: tempfile::TempDir,
    pub owner: ToolDelegation,
    pub agent: Peer,
    pub tool: Peer,
    pub audit: Arc<AuditLog>,
    pub grant: ToolGrantRequest,
}
impl Fixture {
    pub fn new(allow_delegation: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let agent = Peer::new(&root.path().join("agent.sock"));
        let tool = Peer::new(&root.path().join("tool.sock"));
        let credentials = agent.credentials();
        let audit = Arc::new(AuditLog::open(root.path()).unwrap());
        let scope = CommandScope {
            command_digest: Digest::of(request().command.as_bytes()),
            timeout_ms: 1000,
            uses: 4,
        };
        let expires_at = Instant::now() + Duration::from_secs(30);
        let owner = ToolDelegation::new(
            CapabilityBinding {
                session_id: "session-1".to_owned(),
                run_id: "run-1".to_owned(),
                channel_id: "agent-capability".to_owned(),
                envelope_revision: 0,
                identity_slot: 1,
                assigned_uid: credentials.uid,
                assigned_gid: credentials.gid,
                agent_pid: credentials.pid,
            },
            DelegationPolicy {
                authorization_id: "authorization-1".to_owned(),
                scope: scope.clone(),
                allow_delegation,
                expires_at,
            },
            agent.bound(),
            Arc::clone(&audit),
        )
        .unwrap();
        let grant = ToolGrantRequest {
            session_id: "session-1".to_owned(),
            run_id: "run-1".to_owned(),
            envelope_revision: 0,
            sequence: 1,
            scope: CommandScope { uses: 2, ..scope },
            expires_at,
        };
        Self {
            root,
            owner,
            agent,
            tool,
            audit,
            grant,
        }
    }
    pub fn delegate(&self) -> Result<DelegatedTool, DelegationError> {
        self.owner
            .delegate(self.agent.credentials(), &self.grant, self.tool.bound())
    }
}

#[derive(Default)]
pub(super) struct QueuedEffect(Mutex<Option<(PendingEffect, EffectCompletion)>>);
impl ToolEffect for QueuedEffect {
    fn execute(
        &self,
        effect: PendingEffect,
        complete: EffectCompletion,
    ) -> Result<(), DelegationError> {
        let mut pending = self.0.lock().unwrap();
        if pending.is_some() {
            return Err(DelegationError::EffectFailed);
        }
        *pending = Some((effect, complete));
        Ok(())
    }
}
impl QueuedEffect {
    pub fn take(&self) -> (PendingEffect, EffectCompletion) {
        self.0.lock().unwrap().take().unwrap()
    }
    pub fn finish(&self, effects: Arc<std::sync::atomic::AtomicUsize>) {
        let (pending, complete) = self.take();
        std::thread::spawn(move || {
            let result = pending.commit(|request| {
                effects.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(ToolExecutionResult {
                    exit_code: 0,
                    stdout: request.command.clone(),
                    stderr: String::new(),
                    truncated: false,
                    timed_out: false,
                })
            });
            complete(result.and_then(|(output, committed)| committed.finish(Ok(output))));
        })
        .join()
        .unwrap();
    }
}
pub(super) fn completion() -> (
    EffectCompletion,
    mpsc::Receiver<Result<ToolExecutionResult, DelegationError>>,
) {
    let (tx, rx) = mpsc::channel();
    (
        Box::new(move |result| {
            tx.send(result).unwrap();
        }),
        rx,
    )
}
pub(super) fn outcome(
    rx: &mpsc::Receiver<Result<ToolExecutionResult, DelegationError>>,
) -> Result<ToolExecutionResult, DelegationError> {
    rx.recv_timeout(WAIT).unwrap()
}
