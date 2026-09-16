//! Descriptor-relative trust storage. The store root is an authority boundary:
//! its owner must protect it from untrusted writers, as for the rest of Store.
//! Advisory locking serializes cooperating tools, not an attacker owning that root.

use std::{
    fs::File,
    io::{self, Read, Write},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use rustix::{
    fs::{self, AtFlags, FlockOperation, Mode, OFlags},
    io::Errno,
};

use super::{TrustError, TrustStore};
use crate::store::Store;

const STATE: &str = "roles.json";
const LOCK: &str = "roles.lock";
const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

struct Directory {
    file: File,
    path: PathBuf,
}

impl Directory {
    fn open(root: &File, store: &Store) -> Result<Self, TrustError> {
        let path = store.root().join("trust");
        let file = fs::openat(root, "trust", DIRECTORY_FLAGS, Mode::empty())
            .map_err(|source| io_error(&path, source.into()))?
            .into();
        Ok(Self { file, path })
    }

    fn open_regular(&self, name: &str, flags: OFlags) -> Result<File, TrustError> {
        let path = self.path.join(name);
        let file = File::from(
            fs::openat(
                &self.file,
                name,
                flags | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
                Mode::RUSR | Mode::WUSR,
            )
            .map_err(|source| io_error(&path, source.into()))?,
        );
        let metadata = file.metadata().map_err(|source| io_error(&path, source))?;
        // A reader may have opened the previous inode just before publication
        // unlinked it. Zero links is safe then; multiple names are not.
        if !metadata.is_file() || metadata.nlink() > 1 {
            return Err(io_error(
                &path,
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "expected an unaliased regular file",
                ),
            ));
        }
        Ok(file)
    }

    fn state_file(&self) -> Result<Option<File>, TrustError> {
        absent_is_none(self.open_regular(STATE, OFlags::RDONLY))
    }

    fn load(&self) -> Result<Option<TrustStore>, TrustError> {
        let Some(mut file) = self.state_file()? else {
            return Ok(None);
        };
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|source| io_error(&self.path.join(STATE), source))?;
        let trust: TrustStore = serde_json::from_slice(&bytes)
            .map_err(|error| TrustError::Malformed(error.to_string()))?;
        trust.validate()?;
        Ok(Some(trust))
    }

    fn sync(&self) -> Result<(), TrustError> {
        self.file
            .sync_all()
            .map_err(|source| io_error(&self.path, source))
    }
}

pub(super) fn load(store: &Store) -> Result<Option<TrustStore>, TrustError> {
    let root = open_root(store)?;
    match absent_is_none(Directory::open(&root, store))? {
        Some(directory) => directory.load(),
        None => Ok(None),
    }
}

/// Owns the persistent lock inode until the mutation and its durability checks
/// finish. Never unlink the lock: waiters must all refer to the same inode.
pub(crate) struct LockedTrust {
    directory: Directory,
    lock: File,
}

impl Drop for LockedTrust {
    fn drop(&mut self) {
        // A concurrent fork can retain this open-file description until exec;
        // closing ours alone would extend an already finished operation's lock.
        // State publication/durability has settled before guard disposal. Failed
        // unlock can only retain exclusion until the last descriptor closes,
        // not permit an uncommitted write or unsafe authority/identity reuse.
        let _ = fs::flock(&self.lock, FlockOperation::Unlock);
    }
}

impl LockedTrust {
    pub(crate) fn read_only(store: &Store) -> Result<Self, TrustError> {
        let root = open_root(store)?;
        let directory = Directory::open(&root, store)?;
        let lock = directory.open_regular(LOCK, OFlags::RDONLY)?;
        fs::flock(&lock, FlockOperation::NonBlockingLockShared)
            .map_err(|source| io_error(&directory.path.join(LOCK), source.into()))?;
        Ok(Self { directory, lock })
    }

    pub(crate) fn confirm(&self) -> Result<(), TrustError> {
        let file = self
            .directory
            .state_file()?
            .ok_or(TrustError::NotBootstrapped)?;
        file.sync_all()
            .map_err(|source| io_error(&self.directory.path.join(STATE), source))?;
        self.directory.sync()
    }

    pub(crate) fn acquire(store: &Store) -> Result<Self, TrustError> {
        let root = open_root(store)?;
        match fs::mkdirat(&root, "trust", Mode::RWXU) {
            Ok(()) | Err(Errno::EXIST) => (),
            Err(source) => return Err(io_error(&store.root().join("trust"), source.into())),
        }
        let directory = Directory::open(&root, store)?;
        // Also sync when another process created the directory: its creator may
        // have died before making the new entry durable.
        root.sync_all()
            .map_err(|source| io_error(store.root(), source))?;
        let lock = directory.open_regular(LOCK, OFlags::RDWR | OFlags::CREATE)?;
        match fs::flock(&lock, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => (),
            Err(Errno::WOULDBLOCK) => {
                return Err(TrustError::Busy(directory.path.display().to_string()));
            }
            Err(source) => return Err(io_error(&directory.path.join(LOCK), source.into())),
        }
        Ok(Self { directory, lock })
    }

    pub(crate) fn load(&self) -> Result<Option<TrustStore>, TrustError> {
        self.directory.load()
    }

    pub(crate) fn write(&self, trust: &TrustStore) -> Result<(), TrustError> {
        trust.validate()?;
        let bytes =
            serde_json::to_vec(trust).map_err(|error| TrustError::Malformed(error.to_string()))?;
        let temporary = format!(
            ".roles-{}-{}.pending",
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let mut file = self
            .directory
            .open_regular(&temporary, OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL)?;
        crate::store::share_evidence(&self.directory.file, &file)
            .map_err(|source| io_error(&self.directory.path.join(&temporary), source))?;
        let result = (|| {
            file.write_all(&bytes)
                .and_then(|()| file.sync_all())
                .map_err(|source| io_error(&self.directory.path.join(&temporary), source))?;
            // Refuse unexpected targets even after staging. The protected root
            // and held lock exclude other legitimate namespace writers here.
            self.directory.state_file()?;
            fs::renameat(
                &self.directory.file,
                &temporary,
                &self.directory.file,
                STATE,
            )
            .map_err(|source| io_error(&self.directory.path.join(STATE), source.into()))?;
            self.directory.sync()
        })();
        // Only our exclusive-created temporary is eligible for cleanup. A stale
        // partial file is inert: readers only open roles.json and later writers
        // use exclusive creation. Preserve the original failure if cleanup fails.
        let _ = fs::unlinkat(&self.directory.file, &temporary, AtFlags::empty());
        result
    }

    pub(super) fn reset(&self) -> Result<(), TrustError> {
        if self.directory.state_file()?.is_some() {
            fs::unlinkat(&self.directory.file, STATE, AtFlags::empty())
                .map_err(|source| io_error(&self.directory.path.join(STATE), source.into()))?;
        }
        // Sync even if absent: a prior removal may not have acknowledged success.
        self.directory.sync()
    }
}

fn open_root(store: &Store) -> Result<File, TrustError> {
    fs::open(store.root(), DIRECTORY_FLAGS, Mode::empty())
        .map(File::from)
        .map_err(|source| io_error(store.root(), source.into()))
}

fn absent_is_none<T>(result: Result<T, TrustError>) -> Result<Option<T>, TrustError> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(TrustError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn io_error(path: &Path, source: io::Error) -> TrustError {
    TrustError::Io {
        path: path.display().to_string(),
        source,
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "Storage fixture setup and assertions abort only this test."
    )]
    use super::*;

    #[test]
    fn a_fork_equivalent_descriptor_cannot_extend_a_finished_trust_lock() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path()).unwrap();
        let guard = LockedTrust::acquire(&store).unwrap();
        // dup shares the open-file description exactly as fork does before exec.
        // This avoids timing-dependent fork hooks or unsafe code in the test.
        let inherited = guard.lock.try_clone().unwrap();
        assert!(matches!(
            LockedTrust::acquire(&store),
            Err(TrustError::Busy(_))
        ));
        drop(guard);
        let next = LockedTrust::acquire(&store).expect("completed operation released authority");
        drop(inherited);
        assert!(matches!(
            LockedTrust::acquire(&store),
            Err(TrustError::Busy(_))
        ));
        drop(next);
        LockedTrust::acquire(&store).unwrap();
    }
}
