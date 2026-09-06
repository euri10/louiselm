//! Real-process coverage for the private sandbox bootstrap boundary.
#![cfg(target_os = "linux")]
#![forbid(unsafe_code)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Test fixtures abort on setup failure and assert failures directly."
)]

use std::{
    fs,
    io::{self, IoSlice, Read, Write},
    mem::MaybeUninit,
    os::{
        fd::{AsFd, BorrowedFd, OwnedFd},
        unix::{fs::PermissionsExt, net::UnixStream},
    },
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use rustix::net::{SendAncillaryBuffer, SendAncillaryMessage, SendFlags, sendmsg};

const TIMEOUT: Duration = Duration::from_secs(3);

#[test]
fn bootstrap_refuses_ambient_descriptors_before_target_exec() {
    if std::env::var_os("LOUISELM_BOOTSTRAP_FD_CHILD").is_none() {
        // No non-CLOEXEC window in the parallel test runner (louiselm-xhgy).
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "bootstrap_refuses_ambient_descriptors_before_target_exec",
                "--nocapture",
            ])
            .env("LOUISELM_BOOTSTRAP_FD_CHILD", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        return;
    }
    // Deliberate non-CLOEXEC authority, unlike the ordinary declared transfers.
    let file = fs::File::open("/dev/null").unwrap();
    let inherited = rustix::io::fcntl_dupfd_cloexec(&file, 100).unwrap();
    rustix::io::fcntl_setfd(&inherited, rustix::io::FdFlags::empty()).unwrap();
    let mut bootstrap = Bootstrap::start(Path::new("/bin/true"));
    // Drop our copy before the handshake. Only the freshly exec'd bootstrap
    // still owns an inherited copy; nothing is altered in the parent's table.
    drop(inherited);
    let (_writer, _status) = valid_channels(&bootstrap);
    bootstrap
        .channel
        .shutdown(std::net::Shutdown::Write)
        .unwrap();
    let mut reply = Vec::new();
    bootstrap.channel.read_to_end(&mut reply).unwrap();
    assert_eq!(
        reply,
        [2, 0, 0, 0, 22],
        "undeclared FD rejected before READY"
    );
    assert!(!bootstrap.wait().success());
}

struct Bootstrap {
    child: Child,
    channel: UnixStream,
}

impl Bootstrap {
    fn start(program: &Path) -> Self {
        let (channel, child_channel) = UnixStream::pair().unwrap();
        channel.set_read_timeout(Some(TIMEOUT)).unwrap();
        channel.set_write_timeout(Some(TIMEOUT)).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_louiselm-launch"))
            .arg("__sandbox_bootstrap")
            .arg(program)
            .env_clear()
            .stdin(Stdio::from(OwnedFd::from(child_channel)))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        Self { child, channel }
    }

    fn wait(&mut self) -> std::process::ExitStatus {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(Instant::now() < deadline, "bootstrap did not settle");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for Bootstrap {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn send_rights(channel: &UnixStream, payload: &[u8], descriptors: &[BorrowedFd<'_>]) {
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(8))];
    let mut ancillary = SendAncillaryBuffer::new(&mut space);
    assert!(ancillary.push(SendAncillaryMessage::ScmRights(descriptors)));
    assert_eq!(
        sendmsg(
            channel,
            &[IoSlice::new(payload)],
            &mut ancillary,
            SendFlags::NOSIGNAL,
        )
        .unwrap(),
        payload.len()
    );
}

fn valid_channels(bootstrap: &Bootstrap) -> (io::PipeWriter, UnixStream) {
    let (input, writer) = io::pipe().unwrap();
    let (status, status_child) = UnixStream::pair().unwrap();
    status.set_read_timeout(Some(TIMEOUT)).unwrap();
    let (_unblock, block_child) = std::os::unix::net::UnixDatagram::pair().unwrap();
    send_rights(
        &bootstrap.channel,
        &[1],
        &[input.as_fd(), status_child.as_fd(), block_child.as_fd()],
    );
    (writer, status)
}

#[test]
fn bootstrap_preserves_stdio_and_gate_descriptors_across_exec() {
    let fixture = tempfile::tempdir().unwrap();
    let program = fixture.path().join("bwrap");
    fs::write(
        &program,
        "#!/bin/sh\n\
         test \"$1\" = --json-status-fd && test \"$3\" = --block-fd || exit 20\n\
         test -S /proc/self/fd/$2 && test -S /proc/self/fd/$4 || exit 21\n\
         eval \"printf 'status\\\\n' >&$2\"\n\
         printf 'error-channel\\n' >&2\n\
         printf 'out:'\n\
         exec /bin/cat\n",
    )
    .unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
    let mut bootstrap = Bootstrap::start(&program);
    let (mut writer, mut status) = valid_channels(&bootstrap);
    bootstrap
        .channel
        .shutdown(std::net::Shutdown::Write)
        .unwrap();
    let mut readiness = Vec::new();
    bootstrap.channel.read_to_end(&mut readiness).unwrap();
    assert_eq!(readiness, [1], "Ready must precede exec's channel close");
    writer.write_all(b"payload\n").unwrap();
    drop(writer);
    assert!(bootstrap.wait().success());
    let mut output = String::new();
    bootstrap
        .child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut output)
        .unwrap();
    assert_eq!(output, "out:payload\n");
    let mut error = String::new();
    bootstrap
        .child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut error)
        .unwrap();
    assert_eq!(error, "error-channel\n");
    let mut status_text = String::new();
    status.read_to_string(&mut status_text).unwrap();
    assert_eq!(status_text, "status\n");
}

#[test]
fn bootstrap_waits_for_complete_parent_release_before_exec() {
    let mut bootstrap = Bootstrap::start(Path::new("/bin/true"));
    let (_writer, _status) = valid_channels(&bootstrap);
    bootstrap
        .channel
        .set_read_timeout(Some(Duration::from_millis(50)))
        .unwrap();
    let mut byte = [0];
    let error = bootstrap.channel.read(&mut byte).unwrap_err();
    assert!(matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    ));
    assert!(bootstrap.child.try_wait().unwrap().is_none());

    bootstrap.channel.set_read_timeout(Some(TIMEOUT)).unwrap();
    bootstrap
        .channel
        .shutdown(std::net::Shutdown::Write)
        .unwrap();
    let mut response = Vec::new();
    bootstrap.channel.read_to_end(&mut response).unwrap();
    assert_eq!(response, [1]);
    assert!(bootstrap.wait().success());
}

#[test]
fn parent_disconnect_cannot_release_bootstrap() {
    for transfer_first in [false, true] {
        let mut bootstrap = Bootstrap::start(Path::new("/bin/true"));
        let channels = transfer_first.then(|| valid_channels(&bootstrap));
        bootstrap
            .channel
            .shutdown(std::net::Shutdown::Both)
            .unwrap();
        assert!(!bootstrap.wait().success());
        drop(channels);
    }
}

#[test]
fn bootstrap_reports_exec_failure_and_closes_transferred_channels() {
    let mut bootstrap = Bootstrap::start(Path::new("/missing/louiselm-bootstrap-target"));
    let (_writer, mut status) = valid_channels(&bootstrap);
    bootstrap
        .channel
        .shutdown(std::net::Shutdown::Write)
        .unwrap();
    let mut response = Vec::new();
    bootstrap.channel.read_to_end(&mut response).unwrap();
    assert_eq!(response, [1, 2, 0, 0, 0, 2]); // Ready, Error, ENOENT.
    assert!(!bootstrap.wait().success());
    assert_eq!(status.read(&mut [0]).unwrap(), 0);
}

#[test]
fn bootstrap_rejects_wrong_versions_trailing_bytes_and_missing_or_extra_rights() {
    let files: Vec<_> = (0..8)
        .map(|_| fs::File::open("/dev/null").unwrap())
        .collect();
    let descriptors: Vec<_> = files.iter().map(AsFd::as_fd).collect();
    for (payload, count) in [
        (&[9][..], 3),
        (&[1, 9][..], 3),
        (&[1][..], 0),
        (&[1][..], 2),
        (&[1][..], 4),
        (&[1][..], 8),
    ] {
        let mut bootstrap = Bootstrap::start(Path::new("/bin/true"));
        send_rights(&bootstrap.channel, payload, &descriptors[..count]);
        bootstrap
            .channel
            .shutdown(std::net::Shutdown::Write)
            .unwrap();
        let mut response = Vec::new();
        bootstrap.channel.read_to_end(&mut response).unwrap();
        assert_eq!(
            response.first(),
            Some(&2),
            "payload={payload:?}, rights={count}"
        );
        assert_eq!(response.len(), 5);
        assert!(!bootstrap.wait().success());
    }
}

#[test]
fn bootstrap_rejects_a_later_payload_after_the_descriptor_message() {
    let mut bootstrap = Bootstrap::start(Path::new("/bin/true"));
    let (_writer, _status) = valid_channels(&bootstrap);
    bootstrap.channel.write_all(b"trailing").unwrap();
    bootstrap
        .channel
        .shutdown(std::net::Shutdown::Write)
        .unwrap();
    let mut response = Vec::new();
    // A rejecting receiver may close while the invalid suffix is still unread.
    let read = bootstrap.channel.read_to_end(&mut response);
    assert!(read.is_ok() || read.unwrap_err().kind() == io::ErrorKind::ConnectionReset);
    assert!(!response.contains(&1), "invalid input must not reach Ready");
    assert!(!bootstrap.wait().success());
}

#[test]
fn bootstrap_rejects_a_second_descriptor_message() {
    let mut bootstrap = Bootstrap::start(Path::new("/bin/true"));
    let (_writer, mut status) = valid_channels(&bootstrap);
    let extra = fs::File::open("/dev/null").unwrap();
    send_rights(&bootstrap.channel, &[9], &[extra.as_fd()]);
    bootstrap
        .channel
        .shutdown(std::net::Shutdown::Write)
        .unwrap();
    let mut response = Vec::new();
    let read = bootstrap.channel.read_to_end(&mut response);
    assert!(read.is_ok() || read.unwrap_err().kind() == io::ErrorKind::ConnectionReset);
    assert!(!response.contains(&1));
    assert!(!bootstrap.wait().success());
    assert_eq!(status.read(&mut [0]).unwrap(), 0);
}
