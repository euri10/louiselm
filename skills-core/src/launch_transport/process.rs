//! Kernel lifetime pins for process-bound capability transports.

#[cfg(test)]
#[path = "process_tests.rs"]
mod tests;

use std::{
    fs, io,
    os::fd::{AsRawFd, OwnedFd},
    os::unix::fs::MetadataExt,
    sync::{
        OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

use libbpf_rs::{MapCore, MapFlags, MapHandle};
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
    guard: OnceLock<GuardLifetime>,
}

#[derive(Debug)]
struct GuardLifetime {
    tasks: MapHandle,
    lost: MapHandle,
}

impl PartialEq for KernelProcess {
    fn eq(&self, other: &Self) -> bool {
        self.pidfd.as_raw_fd() == other.pidfd.as_raw_fd() && self.credentials == other.credentials
    }
}

impl Eq for KernelProcess {}

impl KernelProcess {
    pub(crate) fn pidfd(&self) -> std::os::fd::BorrowedFd<'_> {
        std::os::fd::AsFd::as_fd(&self.pidfd)
    }

    // Called only at the measured exec stop, after enrollment has been frozen.
    // The LSM denies subsequent procfs executable reads, including by root.
    // Its task storage instead permanently revokes on exec, and its loss latch
    // covers all owners. Never fall back to procfs after this binding.
    pub(crate) fn bind_sender_guard(&self, tasks: &MapHandle, lost: &MapHandle) -> io::Result<()> {
        let guard = GuardLifetime {
            tasks: MapHandle::try_from(tasks).map_err(io::Error::other)?,
            lost: MapHandle::try_from(lost).map_err(io::Error::other)?,
        };
        self.guard
            .set(guard)
            .map_err(|_| io::Error::other("Sender guard already bound"))?;
        if !self.valid()? {
            return Err(io::Error::other("Sender guard runtime lost"));
        }
        Ok(())
    }
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
            guard: OnceLock::new(),
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

    /// Device/inode measured at the exec stop and pinned for this process life.
    /// `valid()` must also hold when this identity grants continuing authority.
    pub(crate) fn executable_identity(&self) -> (u64, u64) {
        (self.executable_device, self.executable_inode)
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
        if let Some(guard) = self.guard.get() {
            let expected: Vec<_> = [1_u64, 1, 1, 0]
                .into_iter()
                .flat_map(u64::to_ne_bytes)
                .collect();
            let grant = guard
                .tasks
                .lookup(&self.pidfd.as_raw_fd().to_ne_bytes(), MapFlags::ANY)
                .map_err(io::Error::other)?;
            let lost = guard
                .lost
                .lookup(&0_u32.to_ne_bytes(), MapFlags::ANY)
                .map_err(io::Error::other)?;
            return Ok((grant.as_deref() != Some(expected.as_slice())
                || lost.as_deref() != Some(&0_u32.to_ne_bytes())
                || !self.alive()?)
            .then(|| "Sender guard runtime authority lost".into()));
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
