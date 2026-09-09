//! Real pipe/socket behavior without root, credentials, network or an ACP peer.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Test fixtures abort on setup failure and assert observed outcomes."
)]

use std::{
    io::BufReader,
    net::Shutdown,
    os::unix::net::UnixStream,
    process::{Child, Command, Stdio},
    sync::{Mutex, atomic::AtomicUsize},
    time::Instant,
};

use super::*;

struct ChildProbe(Arc<Mutex<Child>>);

impl ChildProbe {
    fn spawn() -> Self {
        Self(Arc::new(Mutex::new(
            Command::new("/bin/cat")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        )))
    }

    fn pipes(&self) -> (ChildStdin, ChildStdout, ChildStderr) {
        let mut child = self.0.lock().unwrap();
        (
            child.stdin.take().unwrap(),
            child.stdout.take().unwrap(),
            child.stderr.take().unwrap(),
        )
    }

    fn wait_callback(
        &self,
    ) -> impl FnMut() -> Result<Option<i32>, SupervisorError> + Send + 'static {
        let child = Arc::clone(&self.0);
        move || {
            Ok(child
                .lock()
                .unwrap()
                .try_wait()?
                .map(|status| status.code().unwrap_or(-1)))
        }
    }
}

impl Drop for ChildProbe {
    fn drop(&mut self) {
        let mut child = self.0.lock().unwrap();
        // The child may have exited already; always reap our owned fixture.
        let _ = child.kill();
        child.wait().unwrap();
    }
}

fn controller() -> (RelayStdio, UnixStream, UnixStream) {
    let (input_peer, input) = UnixStream::pair().unwrap();
    let (output_peer, output) = UnixStream::pair().unwrap();
    output_peer
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    (
        RelayStdio::new(
            BufReader::new(File::from(OwnedFd::from(input))),
            File::from(OwnedFd::from(output)),
        )
        .unwrap(),
        input_peer,
        output_peer,
    )
}

#[test]
fn process_exit_does_not_wait_for_a_survivors_inherited_stdio() {
    let child = ChildProbe(Arc::new(Mutex::new(
        Command::new("/bin/sh")
            .args(["-c", "sleep 2 & printf ready"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    )));
    let (attachment, receiver) = mpsc::sync_channel(1);
    let (stdio, _input, mut output) = controller();
    attachment.send(stdio).unwrap();
    let (sender, events) = mpsc::channel();
    let (input, stdout, stderr) = child.pipes();
    let mut relay = RelayWorker::start(
        receiver,
        input,
        stdout,
        stderr,
        child.wait_callback(),
        Arc::new(move |event| sender.send(event).is_ok()),
    )
    .unwrap();
    assert_eq!(
        events.recv_timeout(Duration::from_millis(500)).unwrap(),
        RunningAgentEvent::ProcessExited(super::super::ProcessExitClassification::Success)
    );
    relay.stop().unwrap();
    let mut bytes = Vec::new();
    output.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"ready");
}

#[test]
fn cancellation_before_attachment_joins_the_worker_and_closes_child_pipes() {
    let child = ChildProbe::spawn();
    let (_attachment, receiver) = mpsc::sync_channel(1);
    let (input, output, error) = child.pipes();
    let mut relay = RelayWorker::start(
        receiver,
        input,
        output,
        error,
        child.wait_callback(),
        Arc::new(|_| true),
    )
    .unwrap();
    assert_eq!(relay.stop(), Ok(()));
    assert_eq!(relay.stop(), Ok(()), "quiescence is idempotent");
    assert_eq!(
        Arc::strong_count(&child.0),
        1,
        "the worker released process ownership before acknowledging"
    );
}

#[test]
fn opaque_roundtrip_drains_output_before_process_exit_and_closes_controller_io() {
    let child = ChildProbe::spawn();
    let (stdio, mut input_peer, mut output_peer) = controller();
    let (attachment, receiver) = mpsc::sync_channel(1);
    attachment.send(stdio).unwrap();
    let (input, output, error) = child.pipes();
    let (sent, events) = mpsc::channel();
    let mut relay = RelayWorker::start(
        receiver,
        input,
        output,
        error,
        child.wait_callback(),
        Arc::new(move |event| sent.send(event).is_ok()),
    )
    .unwrap();
    let bytes = [0xff, 0, b'\n', 0x80, b'a'].repeat(128 * 1024);
    let sent_bytes = bytes.clone();
    let writer = thread::spawn(move || {
        input_peer.write_all(&sent_bytes).unwrap();
        input_peer.shutdown(Shutdown::Write).unwrap();
    });
    let mut echo = vec![0; bytes.len()];
    output_peer.read_exact(&mut echo).unwrap();
    assert_eq!(echo, bytes);
    writer.join().unwrap();
    assert_eq!(
        events.recv_timeout(Duration::from_secs(2)).unwrap(),
        RunningAgentEvent::ControllerEof
    );
    assert_eq!(
        events.recv_timeout(Duration::from_secs(2)).unwrap(),
        RunningAgentEvent::ProcessExited(super::super::ProcessExitClassification::Success)
    );
    relay.stop().unwrap();
    assert_eq!(output_peer.read(&mut [0]).unwrap(), 0);
    assert_eq!(Arc::strong_count(&child.0), 1);
}

#[test]
fn cancellation_interrupts_a_backpressured_event_callback() {
    let child = ChildProbe::spawn();
    let (stdio, input_peer, _output_peer) = controller();
    let (attachment, receiver) = mpsc::sync_channel(1);
    attachment.send(stdio).unwrap();
    let (input, output, error) = child.pipes();
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&calls);
    let (sent, blocked) = mpsc::sync_channel(1);
    let mut relay = RelayWorker::start(
        receiver,
        input,
        output,
        error,
        child.wait_callback(),
        Arc::new(move |_| {
            observed.fetch_add(1, Ordering::SeqCst);
            let _ = sent.try_send(());
            false
        }),
    )
    .unwrap();
    input_peer.shutdown(Shutdown::Write).unwrap();
    blocked.recv_timeout(Duration::from_secs(2)).unwrap();
    relay.stop().unwrap();
    assert_eq!(Arc::strong_count(&child.0), 1);
    let after = calls.load(Ordering::SeqCst);
    assert!(after > 0);
    thread::sleep(Duration::from_millis(30));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        after,
        "no callbacks after the join"
    );
}

#[test]
fn cancellation_closes_a_full_controller_output_without_a_reader() {
    let child = ChildProbe::spawn();
    let (stdio, mut input_peer, mut output_peer) = controller();
    let (attachment, receiver) = mpsc::sync_channel(1);
    attachment.send(stdio).unwrap();
    let (input, output, error) = child.pipes();
    let mut relay = RelayWorker::start(
        receiver,
        input,
        output,
        error,
        child.wait_callback(),
        Arc::new(|_| true),
    )
    .unwrap();
    input_peer.write_all(b"ready").unwrap();
    output_peer.read_exact(&mut [0; 5]).unwrap();
    input_peer.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    let bytes = [0x5a; 8192];
    let mut written = 0;
    // A peer that never drains output eventually backpressures the complete
    // cat pipeline. Retry until it has stopped progressing, not just one EAGAIN.
    let mut stalled = 0;
    while stalled < 20 {
        match input_peer.write(&bytes) {
            Ok(count) => {
                written += count;
                stalled = 0;
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                stalled += 1;
                thread::sleep(Duration::from_millis(5));
            }
            result => panic!("fixture write: {result:?}"),
        }
        assert!(
            Instant::now() < deadline,
            "the pipeline never backpressured"
        );
    }
    assert!(
        written > 8192,
        "positive control filled more than the relay buffer"
    );
    let began = Instant::now();
    relay.stop().unwrap();
    assert!(began.elapsed() < Duration::from_secs(1));
    assert_eq!(Arc::strong_count(&child.0), 1);
    let mut drained = Vec::new();
    output_peer.read_to_end(&mut drained).unwrap();
    assert!(!drained.is_empty());
    assert!(drained.iter().all(|byte| *byte == 0x5a));
}

#[test]
fn worker_spawn_failure_drops_all_captured_pipes_and_controller_descriptors() {
    let child = ChildProbe::spawn();
    let (stdio, mut input_peer, _output_peer) = controller();
    input_peer
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let (attachment, receiver) = mpsc::sync_channel(1);
    attachment.send(stdio).unwrap();
    let (input, output, error) = child.pipes();
    let result = RelayWorker::start_with(
        receiver,
        input,
        output,
        error,
        child.wait_callback(),
        Arc::new(|_| true),
        |_work| Err(io::ErrorKind::OutOfMemory.into()),
    );
    assert!(matches!(result, Err(SupervisorError::WorkerUnavailable)));
    assert_eq!(Arc::strong_count(&child.0), 1);
    assert_eq!(input_peer.read(&mut [0]).unwrap(), 0);
}

#[test]
fn worker_panic_fails_quiescence_after_joining_and_closing_io() {
    let child = ChildProbe::spawn();
    let (stdio, mut input_peer, _output_peer) = controller();
    input_peer
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let (attachment, receiver) = mpsc::sync_channel(1);
    attachment.send(stdio).unwrap();
    let (input, output, error) = child.pipes();
    let (sent, panicking) = mpsc::sync_channel(1);
    let mut relay = RelayWorker::start(
        receiver,
        input,
        output,
        error,
        child.wait_callback(),
        Arc::new(move |_| {
            sent.send(()).unwrap();
            panic!("injected event callback panic")
        }),
    )
    .unwrap();
    input_peer.shutdown(Shutdown::Write).unwrap();
    panicking.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(relay.stop(), Err(SupervisorError::CleanupUnproven));
    assert_eq!(relay.stop(), Err(SupervisorError::CleanupUnproven));
    assert_eq!(Arc::strong_count(&child.0), 1);
    assert_eq!(input_peer.read(&mut [0]).unwrap(), 0);
}
