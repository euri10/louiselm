//! Real kernel credential and lifetime checks, without privileged host identity.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "Local process/socket fixtures abort on failed setup or observations."
)]

use super::KernelProcess;
use crate::{
    launch_protocol::{PROTOCOL_VERSION, STATUS_REQUEST_SCHEMA, StatusRequest},
    launch_transport::{
        CredentialPin, KernelCredentials, SeqpacketChannel, SeqpacketListener, TransportError,
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
    os::fd::OwnedFd,
    path::Path,
    process::{Child, Command, Stdio},
    sync::{Arc, mpsc},
    time::Duration,
};

const WAIT: Duration = Duration::from_secs(2);
const MODE: &str = "LOUISELM_KERNEL_PIN_TEST_MODE";
const SOCKET: &str = "LOUISELM_KERNEL_PIN_TEST_SOCKET";

fn pin(pid: u32) -> Arc<KernelProcess> {
    let kernel_pid = Pid::from_raw(i32::try_from(pid).unwrap()).unwrap();
    Arc::new(
        KernelProcess::from_exec_stop(
            KernelCredentials {
                pid,
                uid: getuid().as_raw(),
                gid: getgid().as_raw(),
            },
            pidfd_open(kernel_pid, PidfdFlags::empty()).unwrap(),
            &fs::File::open(std::env::current_exe().unwrap()).unwrap(),
        )
        .unwrap(),
    )
}

fn packet() -> Vec<u8> {
    StatusRequest {
        schema: STATUS_REQUEST_SCHEMA.to_owned(),
        protocol_version: PROTOCOL_VERSION,
        request_id: "pin-test".to_owned(),
        session_id: "session-1".to_owned(),
        run_id: "run-1".to_owned(),
    }
    .canonical_bytes()
}

fn raw_connect(path: &Path) -> OwnedFd {
    let socket = socket_with(
        AddressFamily::UNIX,
        SocketType::SEQPACKET,
        SocketFlags::CLOEXEC,
        None,
    )
    .unwrap();
    connect(&socket, &SocketAddrUnix::new(path).unwrap()).unwrap();
    socket
}

fn accept(
    listener: &SeqpacketListener,
    process: Arc<KernelProcess>,
) -> mpsc::Receiver<Result<SeqpacketChannel, TransportError>> {
    let (sender, receiver) = mpsc::sync_channel(1);
    listener
        .accept(
            CredentialPin::LiveProcess(process),
            Box::new(move |result| {
                sender.send(result).unwrap();
            }),
        )
        .unwrap();
    receiver
}

fn receive(
    channel: &SeqpacketChannel,
) -> Result<crate::launch_transport::AuthenticatedPacket, TransportError> {
    let (sender, receiver) = mpsc::sync_channel(1);
    channel
        .receive(Box::new(move |result| {
            sender.send(result).unwrap();
        }))
        .unwrap();
    receiver.recv_timeout(WAIT).unwrap()
}

struct Helper(Child);
impl Helper {
    fn command(mode: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "launch_transport::process::tests::process_helper",
                "--nocapture",
            ])
            .env(MODE, mode)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        command
    }
    fn release(&mut self) {
        self.0.stdin.as_mut().unwrap().write_all(b"g").unwrap();
    }
    fn ready(&mut self) {
        let mut byte = [0];
        self.0
            .stderr
            .as_mut()
            .unwrap()
            .read_exact(&mut byte)
            .unwrap();
        assert_eq!(byte, *b"r");
    }
    fn kill(&mut self) {
        self.0.kill().unwrap();
        self.0.wait().unwrap();
    }
}
impl Drop for Helper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn inherited_descriptor_cannot_send_as_the_authenticated_agent() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("channel.sock");
    let listener = SeqpacketListener::bind(&path).unwrap();
    let accepted = accept(&listener, pin(std::process::id()));
    let socket = raw_connect(&path);
    let channel = accepted.recv_timeout(WAIT).unwrap().unwrap();
    send(&socket, &packet(), SendFlags::NOSIGNAL).unwrap();
    assert_eq!(
        receive(&channel).unwrap().message_credentials.pid,
        std::process::id()
    );
    let mut helper = Helper(
        Helper::command("inherit")
            .stderr(Stdio::from(socket))
            .spawn()
            .unwrap(),
    );
    helper.release();
    assert!(matches!(
        receive(&channel),
        Err(TransportError::MessageCredentialsMismatch { .. })
    ));
    assert!(channel.is_closed());
    assert!(helper.0.wait().unwrap().success());
}

#[test]
fn agent_exit_rejects_its_queued_packet_and_cannot_revive_the_channel() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("channel.sock");
    let listener = SeqpacketListener::bind(&path).unwrap();
    let mut helper = Helper(
        Helper::command("connect")
            .env(SOCKET, &path)
            .spawn()
            .unwrap(),
    );
    helper.ready();
    let process = pin(helper.0.id());
    let accepted = accept(&listener, Arc::clone(&process));
    helper.release();
    helper.ready();
    let channel = accepted.recv_timeout(WAIT).unwrap().unwrap();
    assert_eq!(
        receive(&channel).unwrap().message_credentials,
        process.credentials()
    );
    helper.release();
    helper.ready();
    helper.kill();
    assert!(!process.valid().unwrap());
    assert!(matches!(
        receive(&channel),
        Err(TransportError::ProcessIdentityUnavailable)
    ));
    assert!(channel.is_closed());
    assert!(!process.valid().unwrap());
}

#[test]
fn process_helper() {
    let Ok(mode) = std::env::var(MODE) else {
        return;
    };
    let release = || {
        std::io::stdin().read_exact(&mut [0]).unwrap();
    };
    if mode == "inherit" {
        release();
        send(std::io::stderr(), &packet(), SendFlags::NOSIGNAL).unwrap();
        return;
    }
    std::io::stderr().write_all(b"r").unwrap();
    release();
    let socket = raw_connect(Path::new(&std::env::var(SOCKET).unwrap()));
    for _ in 0..2 {
        send(&socket, &packet(), SendFlags::NOSIGNAL).unwrap();
        std::io::stderr().write_all(b"r").unwrap();
        release();
    }
}
