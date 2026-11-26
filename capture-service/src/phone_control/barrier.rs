//! Receiver-bound exclusion for phone admission and authority changes.

pub(super) mod namespace_exclusion;

use std::{
    fs::File,
    io::{self, Read, Seek},
    path::{Path, PathBuf},
    time::Duration,
};

#[cfg(test)]
use std::fs;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::fixed;
use super::{
    authority::random_hex,
    barrier_log::{self, CutoffIntent, CutoffKind},
};
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
    registrar_nonce: String,
    cutoff_nonce: String,
    anchor: FileIdentity,
    lock: FileIdentity,
    registrar: FileIdentity,
    cutoff: FileIdentity,
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
    pub(super) registrar: FileIdentity,
    pub(super) registrar_nonce: String,
    pub(super) cutoff: FileIdentity,
    pub(super) cutoff_nonce: String,
}

/// A receiver's existing barrier; construction never initializes storage.
#[derive(Clone)]
pub(super) struct Barrier {
    anchor_path: PathBuf,
    lock_path: PathBuf,
    registrar_path: PathBuf,
    cutoff_path: PathBuf,
    receiver: String,
    binding: BarrierBinding,
}

struct HeldBarrier {
    anchor: PhoneFile,
    anchor_path: PathBuf,
    receiver: String,
    record: AnchorRecord,
    lock: PhoneLock,
    cutoff: PhoneFile,
    cutoff_state: barrier_log::State,
}

/// An established receiver which alone may register new gate-capable owners.
pub(super) struct ReceiverBarrier {
    barrier: Barrier,
    registrar: PhoneLock,
    // Drop last, after the receiver's bound registrar lock is gone.
    _namespace_registrar: namespace_exclusion::Registrar,
}

#[derive(Debug)]
pub(super) enum CutoffError<E> {
    Barrier(io::Error),
    Operation(E),
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
        if [
            binding.anchor,
            binding.lock,
            binding.registrar,
            binding.cutoff,
        ]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>()
        .len()
            != 4
            || fixed::<16>(&binding.registrar_nonce).is_err()
            || fixed::<16>(&binding.cutoff_nonce).is_err()
        {
            return Err(io::Error::other("barrier binding is invalid"));
        }
        Ok(Self {
            anchor_path: tls.join("phone-admission-barrier.json"),
            lock_path: tls.join("phone-admission-barrier.lock"),
            registrar_path: tls.join("phone-admission-registrar.lock"),
            cutoff_path: tls.join("phone-admission-cutoff.json"),
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
        let mut cutoff = PHONE_LOCKS.open_rw(&self.cutoff_path, false)?;
        if FileIdentity::from_metadata(&cutoff.file()?.metadata()?)? != record.body.cutoff {
            return Err(io::Error::other("barrier cutoff identity changed"));
        }
        let cutoff_state = barrier_log::read(
            cutoff.file_mut()?,
            &self.receiver,
            &record.body.generation,
            &record.body.cutoff_nonce,
        )?;
        let mut held = HeldBarrier {
            anchor,
            anchor_path: self.anchor_path.clone(),
            receiver: self.receiver.clone(),
            record,
            lock,
            cutoff,
            cutoff_state,
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
        if held.cutoff_state.pending.is_some() {
            return Err(io::Error::other("barrier cutoff is pending"));
        }
        let result = operation();
        held.validate()?;
        held.lock.release()?;
        Ok(result)
    }

    /// Bind registration to this receiver and retain namespace-local exclusion.
    /// The namespace lease does not establish complete host or registrar scope.
    pub(super) fn receiver(self, generation: &str) -> io::Result<ReceiverBarrier> {
        fixed::<16>(generation).map_err(|_| io::Error::other("generation is invalid"))?;
        let startup = Self::namespace_startup(&self.registrar_path)?;
        let namespace_registrar = startup.registrar()?;
        let registrar =
            PHONE_LOCKS.acquire(&self.registrar_path, self.binding.registrar, LOCK_TIMEOUT)?;
        registrar.validate_contents(self.binding.registrar_nonce.as_bytes())?;
        self.with_generation(generation, || ())?;
        Ok(ReceiverBarrier {
            barrier: self,
            registrar,
            _namespace_registrar: namespace_registrar,
        })
    }

    fn namespace_startup(registrar_path: &Path) -> io::Result<namespace_exclusion::Startup> {
        #[cfg(test)]
        {
            let root = registrar_path
                .parent()
                .ok_or_else(|| io::Error::other("receiver registrar has no parent directory"))?;
            namespace_exclusion::Startup::for_fixture(root)
        }
        #[cfg(not(test))]
        {
            let _ = registrar_path;
            namespace_exclusion::Startup::try_acquire()
        }
    }

    /// Exclude cooperating registrars and durably perform the offline cutoff.
    /// This does not independently prove the complete old registrar scope.
    pub(super) fn offline_cutoff<T, E>(
        &self,
        generation: &str,
        kind: CutoffKind,
        device_id: &str,
        operation: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, CutoffError<E>> {
        let startup =
            Self::namespace_startup(&self.registrar_path).map_err(CutoffError::Barrier)?;
        let _namespace_registrar = startup.registrar().map_err(CutoffError::Barrier)?;
        let registrar = PHONE_LOCKS
            .acquire(&self.registrar_path, self.binding.registrar, LOCK_TIMEOUT)
            .map_err(CutoffError::Barrier)?;
        registrar
            .validate_contents(self.binding.registrar_nonce.as_bytes())
            .map_err(CutoffError::Barrier)?;
        let result = self.cutoff(generation, kind, device_id, || Ok(()), operation);
        drop(registrar);
        result
    }

    fn cutoff<T, E>(
        &self,
        generation: &str,
        kind: CutoffKind,
        device_id: &str,
        cut: impl FnOnce() -> Result<(), E>,
        operation: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, CutoffError<E>> {
        fixed::<16>(generation)
            .map_err(|_| CutoffError::Barrier(io::Error::other("generation is invalid")))?;
        let mut held = self.acquire().map_err(CutoffError::Barrier)?;
        if held.record.body.generation != generation {
            return Err(CutoffError::Barrier(io::Error::other(
                "generation is stale",
            )));
        }
        cut().map_err(CutoffError::Operation)?;
        let existing = held.cutoff_state.pending.clone();
        if let Some(intent) = &existing
            && (intent.kind != kind || intent.device_id != device_id)
        {
            return Err(CutoffError::Barrier(io::Error::other(
                "another barrier cutoff is pending",
            )));
        }
        if existing.is_none() {
            let intent = CutoffIntent {
                kind,
                device_id: device_id.to_owned(),
                intent_id: random_hex(16).map_err(|_| {
                    CutoffError::Barrier(io::Error::other("cutoff identity unavailable"))
                })?,
            };
            barrier_log::append_pending(
                held.cutoff.file_mut().map_err(CutoffError::Barrier)?,
                &mut held.cutoff_state,
                &self.receiver,
                generation,
                &held.record.body.cutoff_nonce,
                &intent,
            )
            .map_err(CutoffError::Barrier)?;
        }
        let result = operation().map_err(CutoffError::Operation)?;
        barrier_log::append_complete(
            held.cutoff.file_mut().map_err(CutoffError::Barrier)?,
            &mut held.cutoff_state,
            &self.receiver,
            generation,
            &held.record.body.cutoff_nonce,
        )
        .map_err(CutoffError::Barrier)?;
        held.validate().map_err(CutoffError::Barrier)?;
        held.lock.release().map_err(CutoffError::Barrier)?;
        Ok(result)
    }
}

impl ReceiverBarrier {
    pub(super) fn register<T>(
        &self,
        generation: &str,
        operation: impl FnOnce() -> T,
    ) -> io::Result<T> {
        self.registrar
            .validate_contents(self.barrier.binding.registrar_nonce.as_bytes())?;
        self.barrier.with_generation(generation, operation)
    }

    pub(super) fn cutoff<T, E>(
        &self,
        generation: &str,
        kind: CutoffKind,
        device_id: &str,
        cut: impl FnOnce() -> Result<(), E>,
        operation: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, CutoffError<E>> {
        self.registrar
            .validate_contents(self.barrier.binding.registrar_nonce.as_bytes())
            .map_err(CutoffError::Barrier)?;
        self.barrier
            .cutoff(generation, kind, device_id, cut, operation)
    }

    /// Serialize a non-revoke authority change with registration and gate cut.
    pub(super) fn invalidate<T, E>(
        &self,
        generation: &str,
        cut: impl FnOnce() -> Result<(), E>,
        operation: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, CutoffError<E>> {
        self.registrar
            .validate_contents(self.barrier.binding.registrar_nonce.as_bytes())
            .map_err(CutoffError::Barrier)?;
        self.barrier
            .with_generation(generation, || cut().and_then(|()| operation()))
            .map_err(CutoffError::Barrier)?
            .map_err(CutoffError::Operation)
    }
}

impl AnchorRecord {
    fn binding(&self) -> BarrierBinding {
        BarrierBinding {
            anchor: self.body.anchor,
            lock: self.body.lock,
            lock_nonce: self.body.lock_nonce.clone(),
            registrar: self.body.registrar,
            registrar_nonce: self.body.registrar_nonce.clone(),
            cutoff: self.body.cutoff,
            cutoff_nonce: self.body.cutoff_nonce.clone(),
        }
    }
}

pub(super) fn initialized_anchor(
    receiver: &str,
    generation: &str,
    binding: &BarrierBinding,
) -> io::Result<Vec<u8>> {
    let body = AnchorBody {
        v: 1,
        receiver: receiver.to_owned(),
        generation: generation.to_owned(),
        lock_nonce: binding.lock_nonce.clone(),
        registrar_nonce: binding.registrar_nonce.clone(),
        cutoff_nonce: binding.cutoff_nonce.clone(),
        anchor: binding.anchor,
        lock: binding.lock,
        registrar: binding.registrar,
        cutoff: binding.cutoff,
    };
    let sha256 = lower_hex(&Sha256::digest(
        serde_json::to_vec(&body).map_err(io::Error::other)?,
    ));
    serde_json::to_vec(&AnchorRecord { body, sha256 }).map_err(io::Error::other)
}

impl HeldBarrier {
    fn validate(&mut self) -> io::Result<()> {
        self.lock
            .validate_contents(self.record.body.lock_nonce.as_bytes())?;
        if FileIdentity::from_metadata(&self.cutoff.file()?.metadata()?)? != self.record.body.cutoff
            || FileIdentity::at(
                &self
                    .anchor_path
                    .with_file_name("phone-admission-cutoff.json"),
            )? != self.record.body.cutoff
        {
            return Err(io::Error::other("barrier cutoff identity changed"));
        }
        let current = barrier_log::read(
            self.cutoff.file_mut()?,
            &self.receiver,
            &self.record.body.generation,
            &self.record.body.cutoff_nonce,
        )?;
        if current.pending != self.cutoff_state.pending
            || current.next_seq != self.cutoff_state.next_seq
        {
            return Err(io::Error::other("barrier cutoff changed"));
        }
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
        || fixed::<16>(&record.body.registrar_nonce).is_err()
        || fixed::<16>(&record.body.cutoff_nonce).is_err()
        || record.sha256 != digest
        || record.binding().anchor == record.binding().lock
        || [
            record.body.anchor,
            record.body.lock,
            record.body.registrar,
            record.body.cutoff,
        ]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>()
        .len()
            != 4
        || FileIdentity::at(path)? != identity
    {
        return Err(io::Error::other("barrier anchor is corrupt or unbound"));
    }
    Ok(record)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test fixture setup")]
pub(super) fn receiver_fixture(root: &Path) -> (String, String, ReceiverBarrier) {
    use std::io::Write;

    let receiver = "02".repeat(32);
    let generation = "09".repeat(16);
    let lock = root.join("phone-admission-barrier.lock");
    let registrar = root.join("phone-admission-registrar.lock");
    let cutoff = root.join("phone-admission-cutoff.json");
    let anchor = root.join("phone-admission-barrier.json");
    fs::write(&lock, "ab".repeat(16)).unwrap();
    fs::write(&registrar, "cd".repeat(16)).unwrap();
    fs::write(
        &cutoff,
        barrier_log::initialized_bytes(&receiver, &generation, &"ef".repeat(16)).unwrap(),
    )
    .unwrap();
    let mut file = File::create(&anchor).unwrap();
    let body = AnchorBody {
        v: 1,
        receiver: receiver.clone(),
        generation: generation.clone(),
        lock_nonce: "ab".repeat(16),
        registrar_nonce: "cd".repeat(16),
        cutoff_nonce: "ef".repeat(16),
        anchor: FileIdentity::from_metadata(&file.metadata().unwrap()).unwrap(),
        lock: FileIdentity::at(&lock).unwrap(),
        registrar: FileIdentity::at(&registrar).unwrap(),
        cutoff: FileIdentity::at(&cutoff).unwrap(),
    };
    let record = AnchorRecord {
        sha256: lower_hex(&Sha256::digest(serde_json::to_vec(&body).unwrap())),
        body,
    };
    file.write_all(&serde_json::to_vec(&record).unwrap())
        .unwrap();
    file.sync_all().unwrap();
    let receiver_barrier = Barrier::existing(root, &receiver, record.binding())
        .unwrap()
        .receiver(&generation)
        .unwrap();
    (receiver, generation, receiver_barrier)
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

    use super::{AnchorBody, AnchorRecord, Barrier, BarrierBinding, CutoffError, CutoffKind};
    use crate::phone_control::barrier_log;
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
        let registrar = root.join("phone-admission-registrar.lock");
        let cutoff = root.join("phone-admission-cutoff.json");
        let anchor = root.join("phone-admission-barrier.json");
        fs::write(&lock, "ab".repeat(16)).unwrap();
        fs::write(&registrar, "cd".repeat(16)).unwrap();
        fs::write(
            &cutoff,
            barrier_log::initialized_bytes(&receiver, &generation, &"ef".repeat(16)).unwrap(),
        )
        .unwrap();
        let mut file = fs::File::create(&anchor).unwrap();
        let body = AnchorBody {
            v: 1,
            receiver: receiver.clone(),
            generation: generation.clone(),
            lock_nonce: "ab".repeat(16),
            registrar_nonce: "cd".repeat(16),
            cutoff_nonce: "ef".repeat(16),
            anchor: FileIdentity::from_metadata(&file.metadata().unwrap()).unwrap(),
            lock: FileIdentity::at(&lock).unwrap(),
            registrar: FileIdentity::at(&registrar).unwrap(),
            cutoff: FileIdentity::at(&cutoff).unwrap(),
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
            registrar: FileIdentity {
                device: 1,
                inode: 3,
            },
            registrar_nonce: "cd".repeat(16),
            cutoff: FileIdentity {
                device: 1,
                inode: 4,
            },
            cutoff_nonce: "ef".repeat(16),
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
    fn receiver_retains_namespace_registrar_until_disposed() {
        let root = tempfile::tempdir().unwrap();
        let (_, _, receiver) = super::receiver_fixture(root.path());
        // Other parallel tests may briefly inherit a CLOEXEC descriptor between
        // fork and exec. Contention stays unavailable until that reference ends.
        let startup = super::namespace_exclusion::Startup::wait_for_fixture(root.path()).unwrap();
        assert!(startup.registrar().is_err());
        drop(receiver);
        assert!(startup.wait_for_fixture_registrar().is_ok());
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
    fn missing_lock_or_anchor_is_never_recreated() {
        for target in [
            "phone-admission-barrier.lock",
            "phone-admission-barrier.json",
        ] {
            let root = tempfile::tempdir().unwrap();
            let (receiver, generation, binding) = fixture(root.path());
            let path = root.path().join(target);
            fs::remove_file(&path).unwrap();
            let barrier = Barrier::existing(root.path(), &receiver, binding).unwrap();
            assert!(barrier.with_generation(&generation, || ()).is_err());
            assert!(!path.exists(), "missing {target} was recreated");
        }
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
    fn receiver_owner_child() {
        let Ok(root) = std::env::var("LOUISELM_TEST_RECEIVER_BARRIER_ROOT") else {
            return;
        };
        let root = std::path::Path::new(&root);
        let binding =
            serde_json::from_slice(&fs::read(root.join("test-binding.json")).unwrap()).unwrap();
        let _receiver = Barrier::existing(root, &"02".repeat(32), binding)
            .unwrap()
            .receiver(&"09".repeat(16))
            .unwrap();
        let mut peer = UnixStream::connect(root.join("receiver.sock")).unwrap();
        peer.write_all(b"R").unwrap();
        let mut finish = [0];
        peer.read_exact(&mut finish).unwrap();
    }

    #[test]
    fn offline_cutoff_requires_receiver_death_despite_a_surviving_duplicate() {
        let root = tempfile::tempdir().unwrap();
        let (receiver, generation, binding) = fixture(root.path());
        let listener = UnixListener::bind(root.path().join("receiver.sock")).unwrap();
        let duplicate = fs::File::open(root.path().join("phone-admission-registrar.lock")).unwrap();
        let mut child = OwnedChild(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "phone_control::barrier::tests::receiver_owner_child",
                    "--nocapture",
                ])
                .env("LOUISELM_TEST_RECEIVER_BARRIER_ROOT", root.path())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let (mut peer, _) = listener.accept().unwrap();
        let mut ready = [0];
        peer.read_exact(&mut ready).unwrap();
        assert_eq!(ready, *b"R");
        let barrier = Barrier::existing(root.path(), &receiver, binding.clone()).unwrap();
        assert!(matches!(
            barrier.offline_cutoff(
                &generation,
                CutoffKind::Forget,
                "11111111-1111-4111-8111-111111111111",
                || Ok::<_, ()>(())
            ),
            Err(CutoffError::Barrier(error)) if error.kind() == std::io::ErrorKind::WouldBlock
        ));
        Command::new("kill")
            .args(["-STOP", &child.0.id().to_string()])
            .status()
            .unwrap();
        assert!(matches!(
            barrier.offline_cutoff(
                &generation,
                CutoffKind::Forget,
                "11111111-1111-4111-8111-111111111111",
                || Ok::<_, ()>(())
            ),
            Err(CutoffError::Barrier(error)) if error.kind() == std::io::ErrorKind::WouldBlock
        ));
        child.0.kill().unwrap();
        child.0.wait().unwrap();
        barrier
            .offline_cutoff(
                &generation,
                CutoffKind::Forget,
                "11111111-1111-4111-8111-111111111111",
                || Ok::<_, ()>(()),
            )
            .unwrap();
        assert!(duplicate.metadata().is_ok());
        drop(duplicate);
        drop(peer);
    }

    #[test]
    fn failed_storage_leaves_a_durable_cutoff_and_only_exact_retry_completes_it() {
        fn retry_namespace<T, E>(
            mut operation: impl FnMut() -> Result<T, CutoffError<E>>,
        ) -> Result<T, CutoffError<E>> {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            loop {
                match operation() {
                    Err(CutoffError::Barrier(error))
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            && error.to_string() == "phone namespace exclusion is held"
                            && std::time::Instant::now() < deadline =>
                    {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    result => return result,
                }
            }
        }
        let root = tempfile::tempdir().unwrap();
        let (receiver, generation, binding) = fixture(root.path());
        let barrier = Barrier::existing(root.path(), &receiver, binding).unwrap();
        let device = "11111111-1111-4111-8111-111111111111";
        let failed = retry_namespace(|| {
            barrier.offline_cutoff(&generation, CutoffKind::Disable, device, || {
                Err::<(), _>("storage")
            })
        });
        assert!(matches!(failed, Err(CutoffError::Operation("storage"))));
        assert_eq!(
            barrier
                .with_generation(&generation, || ())
                .unwrap_err()
                .to_string(),
            "barrier cutoff is pending"
        );
        let other = "22222222-2222-4222-8222-222222222222";
        let other_result = retry_namespace(|| {
            barrier.offline_cutoff(&generation, CutoffKind::Disable, other, || Ok::<_, ()>(()))
        });
        assert!(matches!(
            other_result,
            Err(CutoffError::Barrier(error)) if error.to_string() == "another barrier cutoff is pending"
        ));
        retry_namespace(|| {
            barrier.offline_cutoff(&generation, CutoffKind::Disable, device, || Ok::<_, ()>(()))
        })
        .unwrap();
        barrier.with_generation(&generation, || ()).unwrap();
    }

    #[test]
    fn corrupt_or_replaced_cutoff_and_registrar_anchors_fail_closed() {
        for target in [
            "phone-admission-cutoff.json",
            "phone-admission-registrar.lock",
        ] {
            let root = tempfile::tempdir().unwrap();
            let (receiver, generation, binding) = fixture(root.path());
            let path = root.path().join(target);
            fs::rename(&path, root.path().join(format!("old-{target}"))).unwrap();
            fs::write(&path, b"replacement").unwrap();
            let barrier = Barrier::existing(root.path(), &receiver, binding).unwrap();
            let result = if target.ends_with("cutoff.json") {
                barrier.with_generation(&generation, || ())
            } else {
                barrier.receiver(&generation).map(drop)
            };
            assert!(result.is_err(), "replacement {target} was accepted");
        }

        let root = tempfile::tempdir().unwrap();
        let (receiver, generation, binding) = fixture(root.path());
        fs::write(
            root.path().join("phone-admission-cutoff.json"),
            b"corrupt\n",
        )
        .unwrap();
        let barrier = Barrier::existing(root.path(), &receiver, binding).unwrap();
        assert!(barrier.with_generation(&generation, || ()).is_err());
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
