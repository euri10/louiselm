//! Scoped ownership of the CLI's foreground terminal during bounded signing.

use rustix::{
    process::{self, Pid},
    termios,
};
use std::{fs::File, io};

pub(super) struct Foreground<'a> {
    terminal: &'a File,
    original: Pid,
    restored: bool,
}

impl<'a> Foreground<'a> {
    pub(super) fn enter(terminal: &'a File, child: Pid) -> io::Result<Self> {
        if !cfg!(target_os = "linux") {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "interactive bounded signing requires Linux",
            ));
        }
        let original = termios::tcgetpgrp(terminal)?;
        if original != process::getpgrp() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "signer does not own the foreground terminal",
            ));
        }
        let mut owner = Self {
            terminal,
            original,
            restored: true,
        };
        termios::tcsetpgrp(terminal, child)?;
        owner.restored = false;
        // The child may have tried reading before the handoff and stopped on
        // SIGTTIN. Resume only this owned, waitable process group.
        process::kill_process_group(child, process::Signal::CONT)?;
        Ok(owner)
    }

    pub(super) fn restore(&mut self) -> io::Result<()> {
        restore_foreground(self.terminal, self.original)?;
        self.restored = true;
        Ok(())
    }
}

impl Drop for Foreground<'_> {
    fn drop(&mut self) {
        if !self.restored {
            // Unwinding/refusal only. The normal return path checks restoration
            // before a signature can be used to mutate authority.
            let _ = self.restore();
        }
    }
}

#[cfg(target_os = "linux")]
#[expect(
    unsafe_code,
    reason = "Reviewed SIGTTOU-only mask around foreground restoration; louiselm-qnow."
)]
fn restore_foreground(terminal: &File, original: Pid) -> io::Result<()> {
    use rustix::runtime::{self, How, KernelSigSet};
    let mut mask = KernelSigSet::empty();
    mask.insert(process::Signal::TTOU);
    // SAFETY: Only the calling thread's SIGTTOU bit changes, for an intentional
    // foreground handoff. SIGTTOU is not libc-reserved and no memory/lifecycle
    // invariant relies on its delivery here. No threads or children are spawned
    // in this scope, and no other signal is blocked, unblocked or overwritten.
    let previous = unsafe { runtime::kernel_sigprocmask(How::BLOCK, Some(&mask)) }?;
    let result = termios::tcsetpgrp(terminal, original).map_err(io::Error::from);
    if !previous.contains(process::Signal::TTOU) {
        // SAFETY: Undo only the SIGTTOU bit this call added, on this same thread.
        // Restoring the complete old kernel mask could touch libc-reserved bits;
        // using the singleton mask preserves every unrelated bit instead.
        unsafe { runtime::kernel_sigprocmask(How::UNBLOCK, Some(&mask)) }?;
    }
    result
}

#[cfg(not(target_os = "linux"))]
fn restore_foreground(_terminal: &File, _original: Pid) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "interactive bounded signing requires Linux",
    ))
}
