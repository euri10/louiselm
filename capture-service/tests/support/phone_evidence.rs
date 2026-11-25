use std::{fmt::Write as _, fs, path::Path};

use louiselm_capture::{ActionEvidenceError, ActionEvidenceStore};
use sha2::{Digest, Sha256};

pub fn initialize(directory: &Path, guard: &Path, receiver: &str) {
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
            "v":1,"receiver_identity":receiver,"generation":"09".repeat(16),
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
    )
}

pub fn initialize_store(directory: &Path) {
    initialize(
        directory,
        &directory.with_extension("guard"),
        &"02".repeat(32),
    );
}
