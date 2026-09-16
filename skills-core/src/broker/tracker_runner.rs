//! Narrow subprocess seam for invoking the pinned `br` binary from the broker.
//!
//! Scoped only to canonical Beads mutation mediation, not a general command
//! runner: no shell, no caller-selected program, explicit arguments/
//! environment/working directory only. Reuses the launcher's waitable-child
//! and process-group cleanup boundary; no descendant may outlive the attempt.

use std::{
    ffi::OsString,
    io,
    os::unix::process::CommandExt,
    path::PathBuf,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use super::BrokerError;

/// One exact subprocess invocation. No shell, no ambient environment.
#[derive(Clone, Debug)]
pub(super) struct TrackerInvocation {
    /// The pinned `br` binary, an absolute path chosen by the broker.
    pub(super) program: PathBuf,
    /// Exact trusted executable bytes, checked again before each spawn.
    pub(super) program_digest: crate::Digest,
    /// Exact argument vector; never shell-interpreted.
    pub(super) arguments: Vec<OsString>,
    /// Exact environment; the ambient environment is cleared first.
    pub(super) environment: Vec<(OsString, OsString)>,
    /// Working directory `br` resolves its canonical database from.
    pub(super) current_dir: PathBuf,
}

/// Bounded, typed process result. No raw stdout/stderr is retained.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct TrackerOutput {
    /// The process exit code, when the process could be observed to exit.
    pub(super) exit_code: Option<i32>,
}

/// Narrow process seam used only to invoke the pinned `br` binary.
pub(super) trait TrackerRunner {
    /// Runs one invocation to completion or until its deadline expires.
    ///
    /// # Errors
    /// Returns [`BrokerError::TrackerInvocation`] when the process cannot be
    /// spawned, observed, or does not exit within its deadline.
    fn run(&self, invocation: &TrackerInvocation) -> Result<TrackerOutput, BrokerError>;
}

/// Spawns the real `br` process, bounded by a fixed deadline.
pub(super) struct SystemTrackerRunner {
    deadline: Duration,
}

impl SystemTrackerRunner {
    /// Builds a runner that kills and reports timeout past `deadline`.
    pub(super) const fn new(deadline: Duration) -> Self {
        Self { deadline }
    }
}

impl TrackerRunner for SystemTrackerRunner {
    fn run(&self, invocation: &TrackerInvocation) -> Result<TrackerOutput, BrokerError> {
        let bytes = std::fs::read(&invocation.program).map_err(BrokerError::TrackerInvocation)?;
        if crate::Digest::of(&bytes) != invocation.program_digest {
            return Err(BrokerError::InvalidGrant);
        }
        let mut command = Command::new(&invocation.program);
        command
            .args(&invocation.arguments)
            .env_clear()
            .current_dir(&invocation.current_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command.process_group(0);
        for (key, value) in &invocation.environment {
            command.env(key, value);
        }
        let mut child = command.spawn().map_err(BrokerError::TrackerInvocation)?;
        let group = rustix::process::Pid::from_child(&child);
        let exited =
            crate::launcher_install::wait_until_exit(group, Instant::now() + self.deadline);
        let status = crate::launcher_install::terminate_child(
            &mut child,
            Some(group),
            Instant::now() + Duration::from_secs(1),
        )
        .map_err(BrokerError::TrackerInvocation)?;
        if !exited.map_err(BrokerError::TrackerInvocation)? {
            return Err(BrokerError::TrackerInvocation(io::Error::new(
                io::ErrorKind::TimedOut,
                "br exceeded its deadline",
            )));
        }
        Ok(TrackerOutput {
            exit_code: status.code(),
        })
    }
}

#[cfg(test)]
#[path = "tracker_runner_tests.rs"]
mod tests;
