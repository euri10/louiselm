//! Kernel lifetime pins for process-bound capability transports.

#[cfg(test)]
#[path = "process_tests.rs"]
mod tests;

use std::{
    fs, io,
    os::fd::{AsRawFd, OwnedFd},
    os::unix::fs::MetadataExt,
    sync::atomic::{AtomicBool, Ordering},
};

use rustix::event::{PollFd, PollFlags, Timespec, poll};

use super::KernelCredentials;

/// A kernel-pinned process whose executable was checked while stopped at exec.
///
/// Construction belongs to the trusted launcher, not an incoming socket peer.
/// A numeric PID, inherited descriptor, or replacement process cannot recreate it.
#[derive(Debug)]
pub struct KernelProcess {
    credentials: KernelCredentials,
    pidfd: OwnedFd,
    executable_device: u64,
    executable_inode: u64,
    // Keep the measured inode allocated even after a later exec replaces it.
    _executable: fs::File,
    revoked: AtomicBool,
}

impl PartialEq for KernelProcess {
    fn eq(&self, other: &Self) -> bool {
        self.pidfd.as_raw_fd() == other.pidfd.as_raw_fd() && self.credentials == other.credentials
    }
}

impl Eq for KernelProcess {}

impl KernelProcess {
    pub(crate) fn from_exec_stop(
        credentials: KernelCredentials,
        pidfd: OwnedFd,
        expected: &fs::File,
    ) -> io::Result<Self> {
        let executable = expected.try_clone()?;
        let expected = executable.metadata()?;
        let process = Self {
            credentials,
            pidfd,
            executable_device: expected.dev(),
            executable_inode: expected.ino(),
            _executable: executable,
            revoked: AtomicBool::new(false),
        };
        if let Some(reason) = process.observe()? {
            return Err(io::Error::other(reason));
        }
        Ok(process)
    }

    /// Returns credentials established by the trusted launcher.
    #[must_use]
    pub fn credentials(&self) -> KernelCredentials {
        self.credentials
    }

    /// Checks the pinned lifetime and executable without waiting for process exit.
    ///
    /// Call on an I/O worker: executable metadata reads may perform filesystem I/O.
    /// Both lifetime observations must remain live around the metadata read.
    ///
    /// # Errors
    /// Returns kernel observation errors; callers must deny authority on failure.
    pub fn valid(&self) -> io::Result<bool> {
        if self.revoked.load(Ordering::Acquire) {
            return Ok(false);
        }
        match self.observe() {
            Ok(None) => Ok(!self.revoked.load(Ordering::Acquire)),
            result => {
                self.revoked.store(true, Ordering::Release);
                result.map(|_| false)
            }
        }
    }

    // None proves the observation; a reason describes permanent invalidation.
    fn observe(&self) -> io::Result<Option<String>> {
        let pid = self.credentials.pid;
        if !self.alive()? {
            return Ok(Some(format!(
                "Agent process {pid} exited before executable observation"
            )));
        }
        let executable = match fs::metadata(format!("/proc/{pid}/exe")) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Some(format!(
                    "Agent executable unavailable for process {pid}"
                )));
            }
            Err(error) => return Err(error),
        };
        if executable.dev() != self.executable_device || executable.ino() != self.executable_inode {
            return Ok(Some(format!(
                "Agent executable changed for process {pid}: expected device {} inode {}, observed device {} inode {}",
                self.executable_device,
                self.executable_inode,
                executable.dev(),
                executable.ino()
            )));
        }
        if !self.alive()? {
            return Ok(Some(format!(
                "Agent process {pid} exited during executable observation"
            )));
        }
        Ok(None)
    }

    fn alive(&self) -> io::Result<bool> {
        let mut fds = [PollFd::new(&self.pidfd, PollFlags::IN)];
        poll(&mut fds, Some(&Timespec::default()))?;
        Ok(fds[0].revents().is_empty())
    }
}
