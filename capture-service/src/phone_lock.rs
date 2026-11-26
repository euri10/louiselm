//! Process-owned locks used by phone authority and evidence.

use std::{
    collections::BTreeMap,
    fs::{self, File, Metadata},
    io,
    os::unix::fs::{FileExt, MetadataExt},
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex},
    thread,
    time::{Duration, Instant},
};

use rustix::fs::{FlockOperation, Mode, OFlags, fcntl_lock, open};
use serde::{Deserialize, Serialize};

// F_SETLK is owned by the process, not by a Rust object or thread. Every
// phone-lock open and close is serialized here; opening an inode already held
// by this process and then closing it would silently release its kernel lock.
pub(crate) static PHONE_LOCKS: LazyLock<PhoneLocks> = LazyLock::new(|| PhoneLocks {
    registry: Mutex::new(LockRegistry::default()),
});

/// The process-wide owner of phone lock descriptors and their close ordering.
pub(crate) struct PhoneLocks {
    registry: Mutex<LockRegistry>,
}

#[derive(Default)]
struct LockRegistry {
    held: BTreeMap<FileIdentity, Vec<File>>,
    opened: BTreeMap<FileIdentity, usize>,
    unidentified: Vec<File>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FileIdentity {
    pub(crate) device: u64,
    pub(crate) inode: u64,
}

impl FileIdentity {
    pub(crate) fn at(path: &Path) -> io::Result<Self> {
        Self::from_metadata(&fs::symlink_metadata(path)?)
    }

    pub(crate) fn from_metadata(metadata: &Metadata) -> io::Result<Self> {
        if !metadata.is_file() || metadata.nlink() != 1 {
            return Err(io::Error::other("phone lock is not a single-linked file"));
        }
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
}

/// An exclusive record lock on the exact pinned inode.
pub(crate) struct PhoneLock {
    owner: &'static PhoneLocks,
    path: PathBuf,
    identity: FileIdentity,
    file: Option<File>,
}

/// An open file whose close is coordinated with process-owned phone locks.
pub(crate) struct PhoneFile {
    owner: &'static PhoneLocks,
    identity: FileIdentity,
    file: Option<File>,
}

impl PhoneLocks {
    pub(crate) fn open_read(&'static self, path: &Path) -> io::Result<PhoneFile> {
        self.open_file(path, OFlags::RDONLY)
    }

    pub(crate) fn open_rw(&'static self, path: &Path, create_new: bool) -> io::Result<PhoneFile> {
        let flags = if create_new {
            OFlags::RDWR | OFlags::CREATE | OFlags::EXCL
        } else {
            OFlags::RDWR
        };
        self.open_file(path, flags)
    }

    fn open_file(&'static self, path: &Path, flags: OFlags) -> io::Result<PhoneFile> {
        let mut held = self
            .registry
            .lock()
            .map_err(|_| io::Error::other("phone lock registry is poisoned"))?;
        let fd = open(
            path,
            // Reject nonregular files after opening without allowing a FIFO
            // to stall while the process-wide registry mutex is held.
            flags | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(io::Error::from)?;
        let file = File::from(fd);
        let metadata = match file.metadata() {
            Ok(metadata) => metadata,
            Err(error) => {
                if !held.held.is_empty() {
                    held.unidentified.push(file);
                }
                return Err(error);
            }
        };
        let actual = FileIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        };
        if let Some(deferred) = held.held.get_mut(&actual) {
            // Even closing an unexpected alias would unlock its owner's lock.
            deferred.push(file);
            return Err(io::ErrorKind::WouldBlock.into());
        }
        FileIdentity::from_metadata(&metadata)?;
        let count = held.opened.entry(actual).or_default();
        *count = count
            .checked_add(1)
            .ok_or_else(|| io::Error::other("phone file count overflow"))?;
        Ok(PhoneFile {
            owner: self,
            identity: actual,
            file: Some(file),
        })
    }

    pub(crate) fn acquire(
        &'static self,
        path: &Path,
        expected: FileIdentity,
        timeout: Duration,
    ) -> io::Result<PhoneLock> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| io::Error::other("phone lock deadline overflow"))?;
        loop {
            match self.try_acquire(path, expected) {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Err(error);
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                result => return result,
            }
        }
    }

    fn try_acquire(&'static self, path: &Path, expected: FileIdentity) -> io::Result<PhoneLock> {
        let mut held = self
            .registry
            .lock()
            .map_err(|_| io::Error::other("phone lock registry is poisoned"))?;
        if FileIdentity::at(path)? != expected {
            return Err(io::Error::other("phone lock identity changed"));
        }
        if held.held.contains_key(&expected) || held.opened.contains_key(&expected) {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let fd = open(
            path,
            OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map_err(io::Error::from)?;
        let file = File::from(fd);
        let metadata = match file.metadata() {
            Ok(metadata) => metadata,
            Err(error) => {
                if !held.held.is_empty() {
                    // Its inode is unknown, so closing it could unlock any
                    // locally held inode. Keep it until all owners release.
                    held.unidentified.push(file);
                }
                return Err(error);
            }
        };
        let actual = FileIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        };
        if let Some(deferred) = held.held.get_mut(&actual) {
            // A pathname race can open another locally held inode. Closing
            // this descriptor would release that owner's process lock, so
            // retain it until the owner explicitly unlocks.
            deferred.push(file);
            return Err(io::ErrorKind::WouldBlock.into());
        }
        if held.opened.contains_key(&actual) {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        if FileIdentity::from_metadata(&metadata)? != expected {
            return Err(io::Error::other("phone lock identity changed"));
        }
        fcntl_lock(&file, FlockOperation::NonBlockingLockExclusive).map_err(|error| {
            if error == rustix::io::Errno::AGAIN || error == rustix::io::Errno::ACCESS {
                io::ErrorKind::WouldBlock.into()
            } else {
                io::Error::from(error)
            }
        })?;
        held.held.insert(actual, Vec::new());
        let lock = PhoneLock {
            owner: self,
            path: path.to_owned(),
            identity: actual,
            file: Some(file),
        };
        drop(held);
        lock.validate()?;
        Ok(lock)
    }
}

impl PhoneFile {
    pub(crate) fn file(&self) -> io::Result<&File> {
        self.file
            .as_ref()
            .ok_or_else(|| io::Error::other("phone file was closed"))
    }

    pub(crate) fn file_mut(&mut self) -> io::Result<&mut File> {
        self.file
            .as_mut()
            .ok_or_else(|| io::Error::other("phone file was closed"))
    }
}

impl Drop for PhoneFile {
    fn drop(&mut self) {
        let Some(file) = self.file.take() else {
            return;
        };
        let mut held = match self.owner.registry.lock() {
            Ok(held) => held,
            Err(error) => error.into_inner(),
        };
        if let Some(deferred) = held.held.get_mut(&self.identity) {
            // Fail closed even if a bookkeeping invariant was violated.
            deferred.push(file);
            eprintln!("phone file unexpectedly aliases a held lock");
            return;
        }
        if let Some(count) = held.opened.get_mut(&self.identity) {
            if let Some(next) = count.checked_sub(1) {
                *count = next;
            } else {
                eprintln!("phone file registry count underflow");
            }
            if *count == 0 {
                held.opened.remove(&self.identity);
            }
        } else {
            eprintln!("phone file registry lost its owner");
        }
        drop(file);
    }
}

impl PhoneLock {
    #[cfg(test)]
    pub(crate) fn file(&self) -> io::Result<&File> {
        self.file
            .as_ref()
            .ok_or_else(|| io::Error::other("phone lock was released"))
    }

    pub(crate) fn validate(&self) -> io::Result<()> {
        let file = self
            .file
            .as_ref()
            .ok_or_else(|| io::Error::other("phone lock was released"))?;
        if FileIdentity::from_metadata(&file.metadata()?)? != self.identity
            || FileIdentity::at(&self.path)? != self.identity
        {
            return Err(io::Error::other("phone lock identity changed"));
        }
        Ok(())
    }

    pub(crate) fn validate_contents(&self, expected: &[u8]) -> io::Result<()> {
        self.validate()?;
        let file = self
            .file
            .as_ref()
            .ok_or_else(|| io::Error::other("phone lock was released"))?;
        if file.metadata()?.len() != expected.len() as u64 {
            return Err(io::Error::other("phone lock contents changed"));
        }
        let mut actual = vec![0; expected.len()];
        file.read_exact_at(&mut actual, 0)?;
        if actual != expected {
            return Err(io::Error::other("phone lock contents changed"));
        }
        self.validate()
    }

    pub(crate) fn release(mut self) -> io::Result<()> {
        self.release_inner()
    }

    fn release_inner(&mut self) -> io::Result<()> {
        let Some(file) = self.file.take() else {
            return Ok(());
        };
        let (mut held, poisoned) = match self.owner.registry.lock() {
            Ok(held) => (held, false),
            Err(error) => (error.into_inner(), true),
        };
        let result = fcntl_lock(&file, FlockOperation::NonBlockingUnlock).map_err(io::Error::from);
        drop(file);
        let deferred = held.held.remove(&self.identity);
        let owner_was_present = deferred.is_some();
        drop(deferred);
        if held.held.is_empty() {
            held.unidentified.clear();
        }
        if poisoned {
            return Err(io::Error::other("phone lock registry is poisoned"));
        }
        if !owner_was_present {
            return Err(io::Error::other("phone lock registry lost its owner"));
        }
        result
    }
}

impl Drop for PhoneLock {
    fn drop(&mut self) {
        if let Err(error) = self.release_inner() {
            eprintln!("phone lock release failed: {error}");
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test fixtures and assertions")]
mod tests {
    use std::{
        fs, io,
        os::unix::fs::symlink,
        process::{Command, Stdio},
        sync::mpsc,
        thread,
        time::{Duration, Instant},
    };

    use super::{FileIdentity, PHONE_LOCKS};

    #[test]
    #[allow(clippy::panic, reason = "fixture process must be stopped on timeout")]
    fn unsupported_file_does_not_stall_phone_registry() {
        const CHILD_ROOT: &str = "LOUISELM_TEST_PHONE_FIFO_ROOT";
        if let Some(root) = std::env::var_os(CHILD_ROOT) {
            let root = std::path::PathBuf::from(root);
            let (sender, received) = mpsc::channel();
            let fifo = root.join("fifo");
            let regular = root.join("regular");
            let fifo_sender = sender.clone();
            thread::spawn(move || {
                let _ = fifo_sender.send(("fifo", PHONE_LOCKS.open_read(&fifo).is_err()));
            });
            thread::spawn(move || {
                let _ = sender.send(("regular", PHONE_LOCKS.open_read(&regular).is_ok()));
            });
            let mut results = [
                received.recv_timeout(Duration::from_secs(2)).unwrap(),
                received.recv_timeout(Duration::from_secs(2)).unwrap(),
            ];
            results.sort_unstable();
            assert_eq!(results, [("fifo", true), ("regular", true)]);
            return;
        }

        let root = tempfile::tempdir().unwrap();
        rustix::fs::mkfifoat(
            rustix::fs::CWD,
            root.path().join("fifo"),
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        )
        .unwrap();
        fs::write(root.path().join("regular"), b"fixture").unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "phone_lock::tests::unsupported_file_does_not_stall_phone_registry",
            ])
            .env(CHILD_ROOT, root.path())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(8);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("unsupported-file check did not finish");
            }
            thread::sleep(Duration::from_millis(10));
        };
        assert!(status.success(), "unsupported-file check failed: {status}");
    }

    #[test]
    fn local_reopen_does_not_release_the_original_record_lock() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("lock");
        fs::write(&path, b"").unwrap();
        let identity = FileIdentity::at(&path).unwrap();
        let first = PHONE_LOCKS
            .acquire(&path, identity, Duration::ZERO)
            .unwrap();
        let second = PHONE_LOCKS.acquire(&path, identity, Duration::ZERO);
        assert!(matches!(second, Err(error) if error.kind() == io::ErrorKind::WouldBlock));
        let status = Command::new("python3")
            .args([
                "-c",
                "import fcntl, sys\nf = open(sys.argv[1], 'r+')\ntry:\n fcntl.lockf(f, fcntl.LOCK_EX | fcntl.LOCK_NB)\nexcept BlockingIOError:\n sys.exit(0)\nsys.exit(1)",
            ])
            .arg(&path)
            .status()
            .unwrap();
        assert!(status.success());
        first.release().unwrap();
        PHONE_LOCKS
            .acquire(&path, identity, Duration::ZERO)
            .unwrap()
            .release()
            .unwrap();
    }

    #[test]
    fn unexpected_hardlink_open_cannot_release_the_original_record_lock() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("lock");
        let alias = root.path().join("unexpected");
        fs::write(&path, b"").unwrap();
        let identity = FileIdentity::at(&path).unwrap();
        let first = PHONE_LOCKS
            .acquire(&path, identity, Duration::ZERO)
            .unwrap();
        fs::hard_link(&path, &alias).unwrap();
        assert!(
            matches!(PHONE_LOCKS.open_read(&alias), Err(error) if error.kind() == io::ErrorKind::WouldBlock)
        );
        let status = Command::new("python3")
            .args([
                "-c",
                "import fcntl, sys\nf = open(sys.argv[1], 'r+')\ntry:\n fcntl.lockf(f, fcntl.LOCK_EX | fcntl.LOCK_NB)\nexcept BlockingIOError:\n sys.exit(0)\nsys.exit(1)",
            ])
            .arg(&path)
            .status()
            .unwrap();
        assert!(status.success());
        fs::remove_file(alias).unwrap();
        first.validate().unwrap();
        first.release().unwrap();
    }

    #[test]
    fn pairing_paths_cannot_release_a_live_barrier_lock() {
        for alias in ["pairing.lock", "pairing.json"] {
            let root = tempfile::tempdir().unwrap();
            let pairing = root.path().join("pairing");
            drop(crate::PairingRegistry::open(&pairing).unwrap());
            let path = root.path().join("barrier");
            fs::write(&path, b"not pairing state").unwrap();
            let identity = FileIdentity::at(&path).unwrap();
            let held = PHONE_LOCKS
                .acquire(&path, identity, Duration::ZERO)
                .unwrap();
            fs::remove_file(pairing.join(alias)).unwrap();
            symlink("../barrier", pairing.join(alias)).unwrap();
            let _ = crate::PairingRegistry::open(&pairing);
            let status = Command::new("python3")
                .args([
                    "-c",
                    "import fcntl, sys\nf = open(sys.argv[1], 'r+')\ntry:\n fcntl.lockf(f, fcntl.LOCK_EX | fcntl.LOCK_NB)\nexcept BlockingIOError:\n sys.exit(0)\nsys.exit(1)",
                ])
                .arg(&path)
                .status()
                .unwrap();
            assert!(status.success(), "{alias} released the barrier lock");
            held.release().unwrap();
        }
    }

    #[test]
    fn lock_cannot_start_until_an_earlier_local_open_is_closed() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("lock");
        fs::write(&path, b"").unwrap();
        let identity = FileIdentity::at(&path).unwrap();
        let reader = PHONE_LOCKS.open_read(&path).unwrap();
        assert!(matches!(
            PHONE_LOCKS.acquire(&path, identity, Duration::ZERO),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock
        ));
        drop(reader);
        PHONE_LOCKS
            .acquire(&path, identity, Duration::ZERO)
            .unwrap()
            .release()
            .unwrap();
    }

    #[test]
    fn replacement_and_missing_path_do_not_authorize_a_new_inode() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("lock");
        fs::write(&path, b"").unwrap();
        let identity = FileIdentity::at(&path).unwrap();
        let original = PHONE_LOCKS
            .acquire(&path, identity, Duration::ZERO)
            .unwrap();
        fs::rename(&path, root.path().join("old-lock")).unwrap();
        assert!(original.validate().is_err());
        assert!(
            PHONE_LOCKS
                .acquire(&path, identity, Duration::ZERO)
                .is_err()
        );
        fs::write(&path, b"").unwrap();
        assert!(original.validate().is_err());
        assert!(
            PHONE_LOCKS
                .acquire(&path, identity, Duration::ZERO)
                .is_err()
        );
        original.release().unwrap();
    }
}
