//! Private process bootstrap for confinement admission and descriptor transfer.
//!
//! The parent starts this entrypoint with a private socket on stdin, admits the
//! blocked process to its cgroup, then transfers the three declared channels.
//! This process never forks: its only execution transition replaces itself
//! with Bubblewrap after the transfer is complete.

use std::{
    ffi::OsString,
    io::{self, IoSlice, IoSliceMut, Read, Write},
    mem::MaybeUninit,
    net::Shutdown,
    os::{
        fd::{AsFd, AsRawFd, OwnedFd},
        unix::{net::UnixStream, process::CommandExt},
    },
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use rustix::{
    io::{Errno, FdFlags, fcntl_dupfd_cloexec, fcntl_setfd},
    net::{
        RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, ReturnFlags, SendAncillaryBuffer,
        SendAncillaryMessage, SendFlags, recvmsg, sendmsg,
    },
};

/// The exact private verb recognized by the launcher executable.
pub const ARGUMENT: &str = "__sandbox_bootstrap";

const VERSION: u8 = 1;
const READY: u8 = 1;
const ERROR: u8 = 2;
const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);

/// Receives the declared channels and replaces this process with Bubblewrap.
///
/// `arguments` contains the Bubblewrap path followed by its arguments. Stdin
/// must be the private startup socket; stdout and stderr remain the workload's
/// output pipes. Call only from the executable's fresh bootstrap entrypoint:
/// success replaces the process and never returns.
///
/// # Errors
/// Returns malformed or incomplete startup input, timeout, descriptor setup,
/// or execution errors. No target program runs after a startup error.
pub fn run(arguments: &[OsString]) -> io::Result<()> {
    // Keep the error channel separate from fd0: exec restores workload stdin
    // before it can report an execution failure. CLOEXEC closes this duplicate
    // only when execution succeeds.
    let fd = fcntl_dupfd_cloexec(io::stdin(), 3).map_err(io::Error::from)?;
    let mut channel = UnixStream::from(fd);
    let deadline = deadline(STARTUP_TIMEOUT)?;
    let result = receive_and_exec(&mut channel, arguments, deadline);
    if let Err(error) = &result {
        let errno = error.raw_os_error().unwrap_or(Errno::INVAL.raw_os_error());
        let mut reply = [ERROR, 0, 0, 0, 0];
        reply[1..].copy_from_slice(&errno.to_be_bytes());
        // Error delivery cannot recover a disconnected parent. Its own
        // handshake deadline and process owner remain responsible for cleanup.
        let _ = write_all(&mut channel, &reply, deadline);
    }
    result
}

pub(super) fn handoff(
    mut channel: UnixStream,
    descriptors: [OwnedFd; 3],
    timeout: Duration,
) -> io::Result<()> {
    let deadline = deadline(timeout)?;
    send_descriptors(&channel, &descriptors, deadline)?;
    // EOF ends the request, making trailing payload or ancillary transfers
    // invalid rather than leaving a second protocol message unread at exec.
    channel.shutdown(Shutdown::Write)?;
    drop(descriptors);
    match read_byte(&mut channel, deadline)? {
        Some(READY) => {}
        Some(ERROR) => return Err(read_error(&mut channel, deadline)?),
        Some(_) => return Err(invalid("unexpected bootstrap readiness message")),
        None => return Err(disconnected()),
    }
    match read_byte(&mut channel, deadline)? {
        None => Ok(()),
        Some(ERROR) => Err(read_error(&mut channel, deadline)?),
        Some(_) => Err(invalid("unexpected bootstrap execution message")),
    }
}

fn receive_and_exec(
    channel: &mut UnixStream,
    arguments: &[OsString],
    deadline: Instant,
) -> io::Result<()> {
    let (program, arguments) = arguments
        .split_first()
        .filter(|(program, _)| !program.is_empty())
        .ok_or_else(|| invalid("missing bootstrap executable"))?;
    let [input, status, block] = receive_descriptors(channel, deadline)?;
    if read_byte(channel, deadline)?.is_some() {
        return Err(invalid("unexpected trailing bootstrap input"));
    }

    let mut command = Command::new(program);
    command
        .arg("--json-status-fd")
        .arg(status.as_raw_fd().to_string())
        .arg("--block-fd")
        .arg(block.as_raw_fd().to_string())
        .args(arguments)
        .env_clear()
        .stdin(Stdio::from(input))
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    // Unlike the multithreaded parent, this fresh process cannot race another
    // spawn. Both owned descriptors remain open until exec succeeds or returns.
    fcntl_setfd(&status, FdFlags::empty()).map_err(io::Error::from)?;
    fcntl_setfd(&block, FdFlags::empty()).map_err(io::Error::from)?;
    write_all(channel, &[READY], deadline)?;
    let error = command.exec();
    drop((status, block));
    Err(error)
}

fn send_descriptors(
    channel: &UnixStream,
    descriptors: &[OwnedFd; 3],
    deadline: Instant,
) -> io::Result<()> {
    let borrowed = descriptors.each_ref().map(AsFd::as_fd);
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(3))];
    let mut ancillary = SendAncillaryBuffer::new(&mut space);
    if !ancillary.push(SendAncillaryMessage::ScmRights(&borrowed)) {
        return Err(invalid("bootstrap descriptor message exceeded its buffer"));
    }
    loop {
        channel.set_write_timeout(Some(remaining(deadline)?))?;
        match sendmsg(
            channel,
            &[IoSlice::new(&[VERSION])],
            &mut ancillary,
            SendFlags::NOSIGNAL,
        ) {
            Ok(1) => return Ok(()),
            Ok(_) => return Err(io::Error::from(io::ErrorKind::WriteZero)),
            Err(Errno::INTR) => {}
            Err(error) => return Err(error.into()),
        }
    }
}

fn receive_descriptors(channel: &UnixStream, deadline: Instant) -> io::Result<[OwnedFd; 3]> {
    let mut bytes = [0_u8; 2];
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(3))];
    let mut ancillary = RecvAncillaryBuffer::new(&mut space);
    let received = loop {
        channel.set_read_timeout(Some(remaining(deadline)?))?;
        match recvmsg(
            channel,
            &mut [IoSliceMut::new(&mut bytes)],
            &mut ancillary,
            RecvFlags::CMSG_CLOEXEC,
        ) {
            Ok(received) => break received,
            Err(Errno::INTR) => {}
            Err(error) => return Err(error.into()),
        }
    };
    let mut descriptors = Vec::new();
    let mut unexpected = false;
    for message in ancillary.drain() {
        match message {
            RecvAncillaryMessage::ScmRights(rights) => descriptors.extend(rights),
            _ => unexpected = true,
        }
    }
    if received.bytes == 0 {
        return Err(disconnected());
    }
    if received.bytes != 1
        || bytes[0] != VERSION
        || unexpected
        || received
            .flags
            .intersects(ReturnFlags::CTRUNC | ReturnFlags::TRUNC)
    {
        return Err(invalid("malformed bootstrap descriptor message"));
    }
    descriptors
        .try_into()
        .map_err(|_| invalid("bootstrap requires exactly three descriptors"))
}

fn read_error(channel: &mut UnixStream, deadline: Instant) -> io::Result<io::Error> {
    let mut bytes = [0; 4];
    for byte in &mut bytes {
        *byte = read_byte(channel, deadline)?.ok_or_else(disconnected)?;
    }
    let errno = i32::from_be_bytes(bytes);
    if errno <= 0 {
        return Err(invalid("invalid bootstrap error number"));
    }
    Ok(io::Error::from_raw_os_error(errno))
}

fn read_byte(channel: &mut UnixStream, deadline: Instant) -> io::Result<Option<u8>> {
    let mut byte = [0];
    loop {
        channel.set_read_timeout(Some(remaining(deadline)?))?;
        match channel.read(&mut byte) {
            Ok(0) => return Ok(None),
            Ok(_) => return Ok(Some(byte[0])),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

fn write_all(channel: &mut UnixStream, mut bytes: &[u8], deadline: Instant) -> io::Result<()> {
    while !bytes.is_empty() {
        channel.set_write_timeout(Some(remaining(deadline)?))?;
        match channel.write(bytes) {
            Ok(0) => return Err(io::Error::from(io::ErrorKind::WriteZero)),
            Ok(written) => bytes = &bytes[written..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn deadline(timeout: Duration) -> io::Result<Instant> {
    Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| invalid("bootstrap timeout is too large"))
}

fn remaining(deadline: Instant) -> io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(|| io::Error::from(io::ErrorKind::TimedOut))
}

fn disconnected() -> io::Error {
    io::Error::new(
        io::ErrorKind::UnexpectedEof,
        "bootstrap disconnected before readiness",
    )
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Test fixtures abort on setup failure and assert failures directly."
)]
mod tests {
    use super::*;

    fn channels() -> ([OwnedFd; 3], UnixStream) {
        let (input, _writer) = io::pipe().unwrap();
        let (status, status_child) = UnixStream::pair().unwrap();
        status
            .set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        let (_unblock, block_child) = std::os::unix::net::UnixDatagram::pair().unwrap();
        (
            [input.into(), status_child.into(), block_child.into()],
            status,
        )
    }

    #[test]
    fn handoff_timeout_closes_its_descriptors_and_socket() {
        let (channel, peer) = UnixStream::pair().unwrap();
        let (descriptors, mut status) = channels();
        let started = Instant::now();
        let error = handoff(channel, descriptors, Duration::from_millis(20)).unwrap_err();
        assert!(matches!(
            error.kind(),
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
        ));
        assert!(started.elapsed() < Duration::from_secs(1));
        // The unread SCM_RIGHTS message owns queued copies until its receiver closes.
        drop(peer);
        assert_eq!(status.read(&mut [0]).unwrap(), 0);
    }

    #[test]
    fn handoff_requires_ready_and_propagates_exec_errors() {
        for response in [Vec::new(), vec![READY, ERROR, 0, 0, 0, 2]] {
            let (channel, mut peer) = UnixStream::pair().unwrap();
            let (descriptors, mut status) = channels();
            let has_error = !response.is_empty();
            let worker = std::thread::spawn(move || {
                let deadline = deadline(Duration::from_secs(1)).unwrap();
                drop(receive_descriptors(&peer, deadline).unwrap());
                assert_eq!(read_byte(&mut peer, deadline).unwrap(), None);
                peer.write_all(&response).unwrap();
            });
            let error = handoff(channel, descriptors, Duration::from_secs(1)).unwrap_err();
            worker.join().unwrap();
            if has_error {
                assert_eq!(error.raw_os_error(), Some(2));
            } else {
                assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
            }
            assert_eq!(status.read(&mut [0]).unwrap(), 0);
        }
    }

    #[test]
    fn concurrent_spawn_cannot_keep_transferred_status_endpoint_open() {
        let (channel, mut peer) = UnixStream::pair().unwrap();
        let (descriptors, mut status) = channels();
        let worker = std::thread::spawn(move || {
            let deadline = deadline(Duration::from_secs(1)).unwrap();
            let received = receive_descriptors(&peer, deadline).unwrap();
            assert_eq!(read_byte(&mut peer, deadline).unwrap(), None);
            let unrelated = Command::new("/bin/cat")
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            drop(received);
            peer.write_all(&[READY]).unwrap();
            unrelated
        });
        handoff(channel, descriptors, Duration::from_secs(1)).unwrap();
        let mut unrelated = worker.join().unwrap();
        let still_running = unrelated.try_wait().unwrap().is_none();
        let eof = status.read(&mut [0]);
        let _ = unrelated.kill();
        unrelated.wait().unwrap();
        assert!(still_running);
        assert_eq!(
            eof.unwrap(),
            0,
            "an unrelated live process must not own the status endpoint"
        );
    }

    #[test]
    fn rejected_ancillary_messages_close_every_received_descriptor() {
        for (version, count) in [(9, 3), (VERSION, 4), (VERSION, 8)] {
            let (channel, peer) = UnixStream::pair().unwrap();
            let (mut observer, child) = UnixStream::pair().unwrap();
            observer
                .set_read_timeout(Some(Duration::from_millis(200)))
                .unwrap();
            let borrowed = vec![child.as_fd(); count];
            let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(8))];
            let mut ancillary = SendAncillaryBuffer::new(&mut space);
            assert!(ancillary.push(SendAncillaryMessage::ScmRights(&borrowed)));
            sendmsg(
                &channel,
                &[IoSlice::new(&[version])],
                &mut ancillary,
                SendFlags::NOSIGNAL,
            )
            .unwrap();
            drop(child);
            assert!(receive_descriptors(&peer, deadline(Duration::from_secs(1)).unwrap()).is_err());
            assert_eq!(
                observer.read(&mut [0]).unwrap(),
                0,
                "version={version}, rights={count}"
            );
        }
    }
}
