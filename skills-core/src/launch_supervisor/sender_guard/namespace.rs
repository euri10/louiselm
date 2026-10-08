//! Private bpffs ownership; never explicitly unpin or unmount live enforcement.

use super::GuardError;
use nix::sched::{CloneFlags, unshare};
use rustix::mount::{MountFlags, MountPropagationFlags};
use std::{fs::File, os::unix::fs::MetadataExt};

pub(super) struct PinNamespace(File);

impl PinNamespace {
    pub(super) fn create() -> Result<Self, GuardError> {
        if !rustix::process::geteuid().is_root() {
            return Err(GuardError::Unavailable);
        }
        // NEWNS|FS isolates only this thread's mount/filesystem context. Never
        // add FILES: unsharing the descriptor table would invalidate
        // Owned/BorrowedFd values shared with other threads. Mount propagation
        // is made private before creating the thread-local bpffs.
        unshare(CloneFlags::CLONE_NEWNS | CloneFlags::CLONE_FS)
            .map_err(|_| GuardError::Unavailable)?;
        rustix::mount::mount_change(
            "/",
            MountPropagationFlags::PRIVATE | MountPropagationFlags::REC,
        )
        .map_err(|_| GuardError::Unavailable)?;
        rustix::mount::mount(
            "bpf",
            "/sys/fs/bpf",
            "bpf",
            MountFlags::empty(),
            Some(c"mode=0700"),
        )
        .map_err(|_| GuardError::Unavailable)?;
        File::open("/proc/thread-self/ns/mnt")
            .map(Self)
            .map_err(|_| GuardError::Unavailable)
    }

    pub(super) fn lease(&self) -> Result<File, GuardError> {
        self.0.try_clone().map_err(|_| GuardError::Unavailable)
    }

    pub(super) fn id(&self) -> Result<u64, GuardError> {
        self.0
            .metadata()
            .map(|value| value.ino())
            .map_err(|_| GuardError::Unavailable)
    }
}
