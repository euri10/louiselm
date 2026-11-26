//! Receiver-bound exclusion for phone admission and authority changes.

use std::{
    fs::File,
    io::{self, Read, Seek},
    path::{Path, PathBuf},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::fixed;
use crate::{
    hex::lower_hex,
    phone_lock::{FileIdentity, PHONE_LOCKS, PhoneFile, PhoneLock},
};

const MAX_ANCHOR_BYTES: u64 = 4096;
const LOCK_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct AnchorBody {
    v: u8,
    receiver: String,
    generation: String,
    lock_nonce: String,
    anchor: FileIdentity,
    lock: FileIdentity,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct AnchorRecord {
    body: AnchorBody,
    sha256: String,
}

/// The immutable part of an anchor, retained independently with the receiver.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BarrierBinding {
    pub(super) anchor: FileIdentity,
    pub(super) lock: FileIdentity,
    pub(super) lock_nonce: String,
}

/// A receiver's existing barrier; construction never initializes storage.
pub(super) struct Barrier {
    anchor_path: PathBuf,
    lock_path: PathBuf,
    receiver: String,
    binding: BarrierBinding,
}

struct HeldBarrier {
    anchor: PhoneFile,
    anchor_path: PathBuf,
    receiver: String,
    record: AnchorRecord,
    lock: PhoneLock,
}

impl Barrier {
    /// `binding` must come from independently retained receiver state, not
    /// from the anchor being checked. This does not initialize or prove cutoff.
    pub(super) fn existing(
        tls: &Path,
        receiver: &str,
        binding: BarrierBinding,
    ) -> io::Result<Self> {
        fixed::<32>(receiver).map_err(|_| io::Error::other("receiver identity is invalid"))?;
        fixed::<16>(&binding.lock_nonce)
            .map_err(|_| io::Error::other("barrier binding is invalid"))?;
        if binding.anchor == binding.lock {
            return Err(io::Error::other("barrier binding is invalid"));
        }
        Ok(Self {
            anchor_path: tls.join("phone-admission-barrier.json"),
            lock_path: tls.join("phone-admission-barrier.lock"),
            receiver: receiver.to_owned(),
            binding,
        })
    }

    fn acquire(&self) -> io::Result<HeldBarrier> {
        let mut anchor = PHONE_LOCKS.open_read(&self.anchor_path)?;
        let record = read_anchor(anchor.file_mut()?, &self.anchor_path, &self.receiver)?;
        if record.binding() != self.binding {
            return Err(io::Error::other("barrier binding changed"));
        }
        let lock = PHONE_LOCKS.acquire(&self.lock_path, record.body.lock, LOCK_TIMEOUT)?;
        let mut held = HeldBarrier {
            anchor,
            anchor_path: self.anchor_path.clone(),
            receiver: self.receiver.clone(),
            record,
            lock,
        };
        held.validate()?;
        Ok(held)
    }

    /// Validate a generation and complete its durable operation under one lock.
    /// The operation must also revalidate the independently retained binding.
    pub(super) fn with_generation<T>(
        &self,
        generation: &str,
        operation: impl FnOnce() -> T,
    ) -> io::Result<T> {
        fixed::<16>(generation).map_err(|_| io::Error::other("generation is invalid"))?;
        let mut held = self.acquire()?;
        if held.record.body.generation != generation {
            return Err(io::Error::other("generation is stale"));
        }
        let result = operation();
        held.validate()?;
        held.lock.release()?;
        Ok(result)
    }
}

impl AnchorRecord {
    fn binding(&self) -> BarrierBinding {
        BarrierBinding {
            anchor: self.body.anchor,
            lock: self.body.lock,
            lock_nonce: self.body.lock_nonce.clone(),
        }
    }
}

impl HeldBarrier {
    fn validate(&mut self) -> io::Result<()> {
        self.lock
            .validate_contents(self.record.body.lock_nonce.as_bytes())?;
        if read_anchor(self.anchor.file_mut()?, &self.anchor_path, &self.receiver)? != self.record {
            return Err(io::Error::other("barrier anchor changed"));
        }
        Ok(())
    }
}

fn read_anchor(file: &mut File, path: &Path, receiver: &str) -> io::Result<AnchorRecord> {
    let identity = FileIdentity::from_metadata(&file.metadata()?)?;
    if FileIdentity::at(path)? != identity {
        return Err(io::Error::other("barrier anchor identity changed"));
    }
    file.rewind()?;
    let mut bytes = Vec::new();
    file.take(MAX_ANCHOR_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_ANCHOR_BYTES {
        return Err(io::Error::other("barrier anchor is too large"));
    }
    let record: AnchorRecord = serde_json::from_slice(&bytes)
        .map_err(|_| io::Error::other("barrier anchor is corrupt"))?;
    let digest = lower_hex(&Sha256::digest(
        serde_json::to_vec(&record.body).map_err(io::Error::other)?,
    ));
    if record.body.v != 1
        || record.body.anchor != identity
        || record.body.receiver != receiver
        || fixed::<16>(&record.body.generation).is_err()
        || fixed::<16>(&record.body.lock_nonce).is_err()
        || record.sha256 != digest
        || FileIdentity::at(path)? != identity
    {
        return Err(io::Error::other("barrier anchor is corrupt or unbound"));
    }
    Ok(record)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test fixtures and assertions")]
mod tests {
    use std::{
        fs,
        io::{Read, Write},
        os::unix::net::{UnixListener, UnixStream},
        process::{Child, Command, Stdio},
    };

    use sha2::{Digest, Sha256};

    use super::{AnchorBody, AnchorRecord, Barrier, BarrierBinding};
    use crate::{hex::lower_hex, phone_lock::FileIdentity};

    struct OwnedChild(Child);

    impl Drop for OwnedChild {
        fn drop(&mut self) {
            if self.0.try_wait().ok().flatten().is_none() {
                // This child is owned by the fixture, including on assertion failure.
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
    }

    fn fixture(root: &std::path::Path) -> (String, String, BarrierBinding) {
        let receiver = "02".repeat(32);
        let generation = "09".repeat(16);
        let lock = root.join("phone-admission-barrier.lock");
        let anchor = root.join("phone-admission-barrier.json");
        fs::write(&lock, "ab".repeat(16)).unwrap();
        let mut file = fs::File::create(&anchor).unwrap();
        let body = AnchorBody {
            v: 1,
            receiver: receiver.clone(),
            generation: generation.clone(),
            lock_nonce: "ab".repeat(16),
            anchor: FileIdentity::from_metadata(&file.metadata().unwrap()).unwrap(),
            lock: FileIdentity::at(&lock).unwrap(),
        };
        let record = AnchorRecord {
            sha256: lower_hex(&Sha256::digest(serde_json::to_vec(&body).unwrap())),
            body,
        };
        file.write_all(&serde_json::to_vec(&record).unwrap())
            .unwrap();
        file.sync_all().unwrap();
        let binding = record.binding();
        fs::write(
            root.join("test-binding.json"),
            serde_json::to_vec(&binding).unwrap(),
        )
        .unwrap();
        (receiver, generation, binding)
    }

    fn identity(root: &std::path::Path) -> super::super::ActionIdentity {
        super::super::ActionIdentity {
            operation_id: "01".repeat(16),
            receiver_identity: "02".repeat(32),
            controller_id: "03".repeat(16),
            subject_id: "04".repeat(16),
            action: 3,
            payload_hash: "05".repeat(32),
            grant_id: "06".repeat(16),
            grant_revision: 1,
            period_id: "07".repeat(16),
            expected_revision: 1,
            project: fs::canonicalize(root).unwrap().to_str().unwrap().into(),
            agent: "codex".into(),
            recorded_at_ms: 1000,
        }
    }

    #[test]
    fn absent_barrier_is_never_initialized_by_an_operation() {
        let root = tempfile::tempdir().unwrap();
        let binding = BarrierBinding {
            anchor: FileIdentity {
                device: 1,
                inode: 1,
            },
            lock: FileIdentity {
                device: 1,
                inode: 2,
            },
            lock_nonce: "ab".repeat(16),
        };
        let barrier = Barrier::existing(root.path(), &"02".repeat(32), binding).unwrap();
        let error = barrier
            .with_generation(&"09".repeat(16), || ())
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[test]
    fn generation_and_both_original_inodes_are_required() {
        let root = tempfile::tempdir().unwrap();
        let (receiver, generation, binding) = fixture(root.path());
        let barrier = Barrier::existing(root.path(), &receiver, binding).unwrap();
        assert_eq!(barrier.with_generation(&generation, || 17).unwrap(), 17);
        assert_eq!(
            barrier
                .with_generation(&"08".repeat(16), || ())
                .unwrap_err()
                .to_string(),
            "generation is stale"
        );

        let anchor = root.path().join("phone-admission-barrier.json");
        let anchor_bytes = fs::read(&anchor).unwrap();
        fs::rename(&anchor, root.path().join("old-anchor")).unwrap();
        fs::write(&anchor, anchor_bytes).unwrap();
        assert_eq!(
            barrier
                .with_generation(&generation, || ())
                .unwrap_err()
                .to_string(),
            "barrier anchor is corrupt or unbound"
        );
    }

    #[test]
    fn replaced_lock_is_never_a_new_exclusion_authority() {
        let root = tempfile::tempdir().unwrap();
        let (receiver, generation, binding) = fixture(root.path());
        let barrier = Barrier::existing(root.path(), &receiver, binding).unwrap();
        let lock = root.path().join("phone-admission-barrier.lock");
        fs::rename(&lock, root.path().join("old-lock")).unwrap();
        fs::write(&lock, b"").unwrap();
        assert_eq!(
            barrier
                .with_generation(&generation, || ())
                .unwrap_err()
                .to_string(),
            "phone lock identity changed"
        );
    }

    #[test]
    fn loss_during_an_operation_is_reported_as_unavailable() {
        let root = tempfile::tempdir().unwrap();
        let (receiver, generation, binding) = fixture(root.path());
        let barrier = Barrier::existing(root.path(), &receiver, binding).unwrap();
        let anchor = root.path().join("phone-admission-barrier.json");
        let error = barrier
            .with_generation(&generation, || fs::remove_file(anchor).unwrap())
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Other);
        assert_eq!(error.to_string(), "phone lock is not a single-linked file");
    }

    #[test]
    fn generation_begin_child() {
        let Ok(root) = std::env::var("LOUISELM_TEST_BARRIER_ROOT") else {
            return;
        };
        let root = std::path::Path::new(&root);
        let mut peer = UnixStream::connect(root.join("coordination.sock")).unwrap();
        let mut start = [0];
        peer.read_exact(&mut start).unwrap();
        assert_eq!(start, *b"S");
        peer.write_all(b"R").unwrap();
        let binding =
            serde_json::from_slice(&fs::read(root.join("test-binding.json")).unwrap()).unwrap();
        let barrier = Barrier::existing(root, &"02".repeat(32), binding).unwrap();
        let result = barrier.with_generation(&"09".repeat(16), || {
            let owner =
                super::super::owner_journal::OwnerJournal::open(root.join("owner")).unwrap();
            owner.begin(&identity(root)).unwrap()
        });
        peer.write_all(if result.is_ok() { b"B" } else { b"U" })
            .unwrap();
    }

    #[test]
    fn generation_change_excludes_a_waiting_real_owner_begin() {
        let root = tempfile::tempdir().unwrap();
        let (receiver, generation, binding) = fixture(root.path());
        let owner =
            super::super::owner_journal::OwnerJournal::open(root.path().join("owner")).unwrap();
        drop(owner);
        let snapshot = fs::read(root.path().join("owner/actions.json")).unwrap();
        let listener = UnixListener::bind(root.path().join("coordination.sock")).unwrap();
        let mut child = OwnedChild(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "phone_control::barrier::tests::generation_begin_child",
                    "--nocapture",
                ])
                .env("LOUISELM_TEST_BARRIER_ROOT", root.path())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let (mut peer, _) = listener.accept().unwrap();
        let barrier = Barrier::existing(root.path(), &receiver, binding).unwrap();
        let result = barrier.with_generation(&generation, || {
            peer.write_all(b"S").unwrap();
            let mut ready = [0];
            peer.read_exact(&mut ready).unwrap();
            assert_eq!(ready, *b"R");
            let path = root.path().join("phone-admission-barrier.json");
            let mut record: AnchorRecord =
                serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            record.body.generation = "0a".repeat(16);
            record.sha256 = lower_hex(&Sha256::digest(serde_json::to_vec(&record.body).unwrap()));
            let mut file = fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(path)
                .unwrap();
            file.write_all(&serde_json::to_vec(&record).unwrap())
                .unwrap();
            file.sync_all().unwrap();
        });
        assert_eq!(result.unwrap_err().to_string(), "barrier anchor changed");
        let mut outcome = [0];
        peer.read_exact(&mut outcome).unwrap();
        assert_eq!(outcome, *b"U");
        assert!(child.0.wait().unwrap().success());
        assert_eq!(
            fs::read(root.path().join("owner/actions.json")).unwrap(),
            snapshot
        );
    }
}
