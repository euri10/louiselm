//! One cancellable owner for ACP input/output, discarded stderr and exit polling.

#[cfg(test)]
#[path = "relay_tests.rs"]
mod tests;

use std::{
    fs::File,
    io::{self, Read, Write},
    os::fd::OwnedFd,
    process::{ChildStderr, ChildStdin, ChildStdout},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use super::{
    RelayStdio, RunningAgentEvent, RunningAgentEvents, SupervisorError, stdio::NonblockingFile,
};

const IDLE_POLL: Duration = Duration::from_millis(10);
// Once the Agent has ended, inherited tool descriptors cannot keep its
// Session alive. Drain available ACP output, with a bound for active writers
// or a stalled controller; authority has already expired at the process pin.
const EXIT_DRAIN_TIMEOUT: Duration = Duration::from_millis(100);

pub(super) struct RelayWorker {
    stopped: Arc<AtomicBool>,
    worker: Option<JoinHandle<Result<(), SupervisorError>>>,
    result: Result<(), SupervisorError>,
}

impl RelayWorker {
    pub(super) fn start(
        controller: mpsc::Receiver<RelayStdio>,
        input: ChildStdin,
        output: ChildStdout,
        error: ChildStderr,
        try_wait: impl FnMut() -> Result<Option<i32>, SupervisorError> + Send + 'static,
        events: RunningAgentEvents,
    ) -> Result<Self, SupervisorError> {
        Self::start_with(controller, input, output, error, try_wait, events, |work| {
            thread::Builder::new()
                .name("louiselm-launch-relay".to_owned())
                .spawn(work)
        })
    }

    fn start_with(
        controller: mpsc::Receiver<RelayStdio>,
        input: ChildStdin,
        output: ChildStdout,
        error: ChildStderr,
        try_wait: impl FnMut() -> Result<Option<i32>, SupervisorError> + Send + 'static,
        events: RunningAgentEvents,
        spawn: impl FnOnce(
            Box<dyn FnOnce() -> Result<(), SupervisorError> + Send>,
        ) -> io::Result<JoinHandle<Result<(), SupervisorError>>>,
    ) -> Result<Self, SupervisorError> {
        let input = NonblockingFile::new(File::from(OwnedFd::from(input)))?;
        let output = NonblockingFile::new(File::from(OwnedFd::from(output)))?;
        let error = NonblockingFile::new(File::from(OwnedFd::from(error)))?;
        let stopped = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stopped);
        let worker = spawn(Box::new(move || {
            let mut relay = RelayLoop {
                controller: None,
                attachment: controller,
                detached: false,
                loss_reported: false,
                input: Some(input),
                output,
                error,
                to_agent: CopyBuffer::default(),
                to_controller: CopyBuffer::default(),
                stderr_eof: false,
            };
            let result = relay.run(&worker_stop, &events, try_wait);
            if let Err(error) = result {
                let event = if error == SupervisorError::AgentIdentityRejected {
                    RunningAgentEvent::AgentIdentityLost
                } else {
                    RunningAgentEvent::RelayFailed
                };
                emit(&worker_stop, &events, event);
            }
            relay.close().map_err(|_| SupervisorError::CleanupUnproven)
        }))
        .map_err(|_| SupervisorError::WorkerUnavailable)?;
        Ok(Self {
            stopped,
            worker: Some(worker),
            result: Ok(()),
        })
    }

    pub(super) fn stop(&mut self) -> Result<(), SupervisorError> {
        self.stopped.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            self.result = worker
                .join()
                .unwrap_or(Err(SupervisorError::CleanupUnproven));
        }
        self.result.clone()
    }
}

impl Drop for RelayWorker {
    fn drop(&mut self) {
        // dispose/explicit quiescence propagate errors before identity reuse.
        // The fallback still joins on failed setup or unwinding; never detach I/O.
        let _ = self.stop();
    }
}

fn emit(stopped: &AtomicBool, events: &RunningAgentEvents, event: RunningAgentEvent) {
    while !stopped.load(Ordering::Acquire) {
        if events(event) {
            return;
        }
        thread::park_timeout(IDLE_POLL);
    }
}

#[derive(Default)]
struct CopyBuffer {
    bytes: Vec<u8>,
    written: usize,
    eof: bool,
}

impl CopyBuffer {
    fn read(&mut self, input: &mut impl Read) -> io::Result<bool> {
        if self.eof || !self.bytes.is_empty() {
            return Ok(false);
        }
        let mut buffer = [0; 8 * 1024];
        match input.read(&mut buffer) {
            Ok(0) => {
                self.eof = true;
                Ok(true)
            }
            Ok(read) => {
                self.bytes.extend_from_slice(&buffer[..read]);
                Ok(true)
            }
            Err(error) if retryable(&error) => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn write(&mut self, output: &mut impl Write) -> io::Result<bool> {
        if self.bytes.is_empty() {
            return Ok(false);
        }
        match output.write(&self.bytes[self.written..]) {
            Ok(0) => Err(io::ErrorKind::WriteZero.into()),
            Ok(written) => {
                self.written += written;
                if self.written == self.bytes.len() {
                    self.bytes.clear();
                    self.written = 0;
                }
                Ok(true)
            }
            Err(error) if retryable(&error) => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn done(&self) -> bool {
        self.eof && self.bytes.is_empty()
    }
}

fn retryable(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
    )
}

struct RelayLoop {
    controller: Option<RelayStdio>,
    attachment: mpsc::Receiver<RelayStdio>,
    detached: bool,
    loss_reported: bool,
    input: Option<NonblockingFile>,
    output: NonblockingFile,
    error: NonblockingFile,
    to_agent: CopyBuffer,
    to_controller: CopyBuffer,
    stderr_eof: bool,
}

impl RelayLoop {
    fn run(
        &mut self,
        stopped: &AtomicBool,
        events: &RunningAgentEvents,
        mut try_wait: impl FnMut() -> Result<Option<i32>, SupervisorError>,
    ) -> Result<(), SupervisorError> {
        let mut exit = None;
        let mut exit_deadline = None;
        while !stopped.load(Ordering::Acquire) {
            self.attach();
            if exit.is_none() {
                exit = try_wait()?;
            }
            if exit.is_some() {
                self.input = None;
                exit_deadline.get_or_insert_with(|| Instant::now() + EXIT_DRAIN_TIMEOUT);
            }
            let mut progress = false;
            if let Some(input) = self.input.as_mut() {
                if let Some(controller) = self.controller.as_mut() {
                    progress |= self.to_agent.read(controller)?;
                } else if self.detached {
                    self.to_agent.eof = true;
                }
                progress |= self.to_agent.write(input)?;
                if self.to_agent.done() && !self.loss_reported {
                    // EOF is a lifecycle cause, not permission to end the Agent.
                    // Keep stdin owned until freeze/settlement/disposal; closing
                    // it here races EOF-exiting Agents ahead of the Park owner.
                    self.loss_reported = true;
                    emit(stopped, events, RunningAgentEvent::ControllerEof);
                }
            }
            progress |= self.to_controller.read(&mut self.output)?;
            if let Some(controller) = self.controller.as_mut() {
                progress |= self.to_controller.write(controller)?;
            } else if self.detached {
                progress |= self.to_controller.write(&mut io::sink())?;
            }
            if !self.stderr_eof {
                let mut discarded = [0; 8 * 1024];
                match self.error.read(&mut discarded) {
                    Ok(0) => {
                        self.stderr_eof = true;
                        progress = true;
                    }
                    Ok(_) => progress = true,
                    Err(error) if retryable(&error) => {}
                    Err(error) => return Err(error.into()),
                }
            }
            if let Some(code) = exit
                && ((self.to_controller.done() && self.stderr_eof)
                    || (!progress && self.to_controller.bytes.is_empty())
                    || exit_deadline.is_some_and(|deadline| Instant::now() >= deadline))
            {
                emit(
                    stopped,
                    events,
                    RunningAgentEvent::ProcessExited(super::system::classify_exit(code)),
                );
                return Ok(());
            }
            if !progress {
                thread::park_timeout(IDLE_POLL);
            }
        }
        Ok(())
    }

    fn attach(&mut self) {
        if self.controller.is_some() || self.detached {
            return;
        }
        match self.attachment.try_recv() {
            Ok(controller) => self.controller = Some(controller),
            Err(mpsc::TryRecvError::Disconnected) => self.detached = true,
            Err(mpsc::TryRecvError::Empty) => {}
        }
    }

    fn close(mut self) -> io::Result<()> {
        let controller = self.controller.take().map_or(Ok(()), RelayStdio::close);
        // An attachment racing cancellation must also be closed before the join
        // acknowledges quiescence; dropping the receiver closes queued values.
        let pending = self.attachment.try_recv().map_or(Ok(()), RelayStdio::close);
        let input = self.input.as_mut().map_or(Ok(()), NonblockingFile::restore);
        let output = self.output.restore();
        let error = self.error.restore();
        controller.and(pending).and(input).and(output).and(error)
    }
}
