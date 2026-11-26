use std::{fmt::Write as _, fs, io::Write as _, os::unix::fs::MetadataExt, path::Path};

use louiselm_capture::{ActionEvidenceError, ActionEvidenceKind, ActionEvidenceStore};
use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Serialize)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

#[derive(Serialize)]
struct BarrierBody<'a> {
    v: u8,
    receiver: &'a str,
    generation: &'a str,
    lock_nonce: &'a str,
    anchor: FileIdentity,
    lock: FileIdentity,
}

#[derive(Serialize)]
struct BarrierRecord<'a> {
    body: BarrierBody<'a>,
    sha256: String,
}

#[allow(
    dead_code,
    reason = "the fixture is shared by several integration tests"
)]
pub fn initialize_barrier(tls: &Path, receiver: &str) {
    initialize_barrier_with_nonce(tls, receiver, &"ab".repeat(16));
}

#[allow(dead_code, reason = "shared integration-test fixture")]
pub fn initialize_barrier_with_nonce(tls: &Path, receiver: &str, lock_nonce: &str) {
    let lock = tls.join("phone-admission-barrier.lock");
    let anchor = tls.join("phone-admission-barrier.json");
    fs::write(&lock, lock_nonce).unwrap();
    let lock_metadata = fs::metadata(&lock).unwrap();
    let mut file = fs::File::create(&anchor).unwrap();
    let anchor_metadata = file.metadata().unwrap();
    let generation = "09".repeat(16);
    let body = BarrierBody {
        v: 1,
        receiver,
        generation: &generation,
        lock_nonce,
        anchor: FileIdentity {
            device: anchor_metadata.dev(),
            inode: anchor_metadata.ino(),
        },
        lock: FileIdentity {
            device: lock_metadata.dev(),
            inode: lock_metadata.ino(),
        },
    };
    let mut sha256 = String::new();
    for byte in Sha256::digest(serde_json::to_vec(&body).unwrap()) {
        write!(sha256, "{byte:02x}").unwrap();
    }
    file.write_all(&serde_json::to_vec(&BarrierRecord { body, sha256 }).unwrap())
        .unwrap();
    file.sync_all().unwrap();
}

#[allow(dead_code, reason = "shared integration-test fixture")]
pub fn bind_barrier_to_owner(tls: &Path) {
    let anchor = fs::metadata(tls.join("phone-admission-barrier.json")).unwrap();
    let lock_path = tls.join("phone-admission-barrier.lock");
    let lock = fs::metadata(&lock_path).unwrap();
    let lock_nonce = fs::read_to_string(lock_path).unwrap();
    let guard = tls.join("phone-action-owner.json");
    let mut record: serde_json::Value = serde_json::from_slice(&fs::read(&guard).unwrap()).unwrap();
    record["barrier"] = serde_json::json!({
        "anchor": { "device": anchor.dev(), "inode": anchor.ino() },
        "lock": { "device": lock.dev(), "inode": lock.ino() },
        "lock_nonce": lock_nonce,
    });
    fs::write(guard, serde_json::to_vec(&record).unwrap()).unwrap();
}

pub fn initialize(directory: &Path, guard: &Path, receiver: &str, kind: ActionEvidenceKind) {
    fs::create_dir_all(directory).unwrap();
    fs::create_dir_all(guard.parent().unwrap()).unwrap();
    let mut guard_lock = guard.as_os_str().to_os_string();
    guard_lock.push(".lock");
    fs::write(guard_lock, b"").unwrap();
    fs::write(directory.with_extension("lock"), b"").unwrap();
    let bytes = br#"{"v":1,"actions":{}}"#;
    let mut digest = String::new();
    for byte in Sha256::digest(bytes) {
        write!(digest, "{byte:02x}").unwrap();
    }
    fs::write(directory.join("actions.json"), bytes).unwrap();
    fs::write(
        directory.join("seal.json"),
        serde_json::to_vec(&serde_json::json!({"v":1,"bytes":bytes.len(),"sha256":digest}))
            .unwrap(),
    )
    .unwrap();
    fs::write(
        guard,
        serde_json::to_vec(&serde_json::json!({
            "v":1,"kind":kind,"receiver_identity":receiver,"generation":"09".repeat(16),
            "bytes":bytes.len(),"sha256":digest
        }))
        .unwrap(),
    )
    .unwrap();
}

pub fn open(directory: &Path) -> Result<ActionEvidenceStore, ActionEvidenceError> {
    ActionEvidenceStore::open_bound(
        directory,
        directory.with_extension("guard"),
        &"02".repeat(32),
        ActionEvidenceKind::Archive,
    )
}

pub fn initialize_store(directory: &Path) {
    initialize(
        directory,
        &directory.with_extension("guard"),
        &"02".repeat(32),
        ActionEvidenceKind::Archive,
    );
}
