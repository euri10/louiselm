//! Deadline-bound ownership of fixed, childless sentinel processes.

use super::peer::{Reply, Request};
use rustix::{
    event::{PollFd, PollFlags, poll},
    process::{Pid, Signal, WaitOptions, kill_process, waitpid},
};
use std::{
    io::{self, Read, Write},
    os::unix::process::CommandExt,
    path::Path,
    process::{Child, Command, Stdio},
    time::Instant,
};

pub(super) struct Peer {
    child: Child,
    disposed: bool,
}

impl Peer {
    pub(super) fn spawn(executable: &Path, uid: u32) -> io::Result<Self> {
        let child = Command::new(executable)
            .arg("__conformance-guard-probe")
            .env_clear()
            .uid(uid)
            .gid(uid)
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        Ok(Self {
            child,
            disposed: false,
        })
    }
    pub(super) fn pid(&self) -> Pid {
        Pid::from_child(&self.child)
    }
    pub(super) fn send(&mut self, request: &Request) -> io::Result<()> {
        let input = self.child.stdin.as_mut().ok_or(io::ErrorKind::BrokenPipe)?;
        serde_json::to_writer(&mut *input, request)?;
        input.write_all(b"\n")?;
        input.flush()
    }
    pub(super) fn receive(&mut self, deadline: Instant) -> io::Result<Reply> {
        let mut line = Vec::new();
        let output = self
            .child
            .stdout
            .as_mut()
            .ok_or(io::ErrorKind::BrokenPipe)?;
        loop {
            let timeout = deadline
                .checked_duration_since(Instant::now())
                .ok_or(io::ErrorKind::TimedOut)?;
            let mut fds = [PollFd::new(&*output, PollFlags::IN)];
            if poll(
                &mut fds,
                Some(
                    &timeout
                        .try_into()
                        .map_err(|_| io::ErrorKind::InvalidInput)?,
                ),
            )? == 0
            {
                return Err(io::ErrorKind::TimedOut.into());
            }
            let mut byte = [0];
            output.read_exact(&mut byte)?;
            if byte[0] == b'\n' {
                return serde_json::from_slice(&line).map_err(io::Error::other);
            }
            line.push(byte[0]);
            if line.len() >= 8192 {
                return Err(io::ErrorKind::InvalidData.into());
            }
        }
    }
    pub(super) fn request(&mut self, request: &Request, deadline: Instant) -> io::Result<Reply> {
        self.send(request)?;
        self.receive(deadline)
    }
    pub(super) fn freeze(&mut self, deadline: Instant) -> io::Result<()> {
        kill_process(self.pid(), Signal::STOP)?;
        loop {
            if let Some((_, status)) = waitpid(
                Some(self.pid()),
                WaitOptions::NOHANG | WaitOptions::UNTRACED,
            )? {
                return if status.stopped() {
                    Ok(())
                } else {
                    Err(io::ErrorKind::InvalidData.into())
                };
            }
            if Instant::now() >= deadline {
                return Err(io::ErrorKind::TimedOut.into());
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }
    pub(super) fn resume(&self) -> io::Result<()> {
        kill_process(self.pid(), Signal::CONT).map_err(Into::into)
    }
    pub(super) fn dispose(&mut self) -> io::Result<()> {
        if !self.disposed {
            match self.child.kill() {
                Ok(()) => (),
                Err(error) if error.kind() == io::ErrorKind::InvalidInput => (),
                Err(error) => return Err(error),
            }
            self.child.wait()?;
            self.disposed = true;
        }
        Ok(())
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        // Explicit disposal supplies the verdict; fallback never certifies cleanup.
        let _ = self.dispose();
    }
}
