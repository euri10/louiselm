//! Private bpffs ownership; never explicitly unpin or unmount live enforcement.

use super::GuardError;
use rustix::{
    mount::{MountFlags, MountPropagationFlags},
    thread::{UnshareFlags, unshare_unsafe},
};
use std::{fs::File, os::unix::fs::MetadataExt};

pub(super) struct PinNamespace(File);

impl PinNamespace {
    #[expect(
        unsafe_code,
        reason = "Reviewed NEWNS|FS boundary; see docs/agent-rust.md."
    )]
    pub(super) fn create() -> Result<Self, GuardError> {
        if !rustix::process::geteuid().is_root() {
            return Err(GuardError::Unavailable);
        }
        // SAFETY: NEWNS|FS never requests FILES or changes the shared fd table.
        // Owned/BorrowedFd values therefore remain valid on every thread. Mount
        // propagation is made private before creating the thread-local bpffs.
        unsafe { unshare_unsafe(UnshareFlags::NEWNS | UnshareFlags::FS) }
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
