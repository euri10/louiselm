//! Characterize exact physical retention and bounded targeting.
#![allow(
    clippy::unwrap_used,
    reason = "Test assertions and fixture construction."
)]
use super::*;

#[test]
fn default_baseline_foreign_and_noncanonical_targets_refuse() {
    let old = Digest::of(b"baseline").to_string();
    let retired = Digest::of(b"new retired QA").to_string();
    let current = Digest::of(b"new default QA").to_string();
    check_ids(&old, &retired, &current).unwrap();
    let bad = current.to_uppercase();
    let cases: [(&str, &str, &str); 5] = [
        (&old, &old, &current),
        (&old, &current, &current),
        (&old, &retired, &old),
        (&old, &retired, "../../outside"),
        (&old, &retired, &bad),
    ];
    for (a, b, c) in cases {
        assert!(
            matches!(check_ids(a, b, c), Err(LauncherError::Invalid(message)) if message == "retired QA release fault review or retained authority mismatch")
        );
    }
}

#[test]
fn hardlinked_or_changed_permission_targets_are_never_modified() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("launcher");
    fs::write(&path, b"original").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o555)).unwrap();
    fs::hard_link(&path, root.path().join("other")).unwrap();
    assert!(
        matches!(change_mode(&File::open(&path).unwrap(), 0o555, 0o444), Err(LauncherError::Invalid(message)) if message == "retired QA release fault review or retained authority mismatch")
    );
    assert_eq!(path.metadata().unwrap().mode() & 0o777, 0o555);
}

#[test]
fn unexpected_special_permission_bits_refuse_without_altering_the_file() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("launcher");
    fs::write(&path, b"original").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o4555)).unwrap();
    assert!(
        matches!(change_mode(&File::open(&path).unwrap(), 0o555, 0o444), Err(LauncherError::Invalid(message)) if message == "retired QA release fault review or retained authority mismatch")
    );
    assert_eq!(path.metadata().unwrap().mode() & 0o7777, 0o4555);
}

#[test]
fn permission_fault_and_explicit_restore_preserve_exact_bytes_and_inode() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("launcher");
    fs::write(&path, b"exact retained release bytes").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o555)).unwrap();
    let file = File::open(&path).unwrap();
    let before = file.metadata().unwrap();
    change_mode(&file, 0o555, 0o444).unwrap();
    assert_eq!(file.metadata().unwrap().mode() & 0o777, 0o444);
    assert!(
        matches!(change_mode(&file, 0o555, 0o444), Err(LauncherError::Invalid(message)) if message == "retired QA release fault review or retained authority mismatch")
    );
    change_mode(&file, 0o444, 0o555).unwrap();
    assert_eq!(file.metadata().unwrap().mode() & 0o777, 0o555);
    assert_eq!(file.metadata().unwrap().ino(), before.ino());
    assert_eq!(fs::read(&path).unwrap(), b"exact retained release bytes");
}
