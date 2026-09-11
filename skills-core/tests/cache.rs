//! Measured warm bytes never become shared writable Session state.
#![allow(clippy::unwrap_used, reason = "Tests abort on fixture failures.")]

use louiselm_skills::{Digest, cache::CacheBase, sandbox::IdentityPlan};
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt, symlink},
};

fn private_home() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    root
}

#[test]
fn captured_base_survives_source_and_overlay_poisoning() {
    let root = private_home();
    let source = root.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("dependency"), b"warm").unwrap();
    let base = CacheBase::capture(&source).unwrap();
    let digest = base.digest().clone();
    fs::write(source.join("dependency"), b"poisoned source").unwrap();
    let first = base
        .materialize(root.path(), "first", IdentityPlan::NamespaceOnly)
        .unwrap();
    let second = base
        .materialize(root.path(), "second", IdentityPlan::NamespaceOnly)
        .unwrap();
    fs::write(first.path().join("dependency"), b"poisoned Session").unwrap();
    assert_eq!(fs::read(second.path().join("dependency")).unwrap(), b"warm");
    assert_ne!(
        fs::metadata(first.path().join("dependency")).unwrap().ino(),
        fs::metadata(second.path().join("dependency"))
            .unwrap()
            .ino()
    );
    assert_eq!(base.digest(), &digest);
    assert_ne!(CacheBase::capture(&source).unwrap().digest(), &digest);
    assert_eq!(fs::metadata(second.path()).unwrap().mode() & 0o777, 0o700);
    assert!(
        base.materialize(root.path(), "first", IdentityPlan::NamespaceOnly)
            .is_err()
    );
    let retained = first.path().to_owned();
    drop(first);
    assert!(
        retained.exists(),
        "dropping a handle must preserve lifecycle-owned state"
    );
}

#[test]
fn capture_refuses_links_special_files_and_oversized_bytes() {
    let root = private_home();
    let source = root.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(root.path().join("outside"), b"secret").unwrap();
    symlink(root.path().join("outside"), source.join("link")).unwrap();
    assert!(CacheBase::capture(&source).is_err());
    fs::remove_file(source.join("link")).unwrap();
    fs::hard_link(root.path().join("outside"), source.join("hardlink")).unwrap();
    assert!(CacheBase::capture(&source).is_err());
    fs::remove_file(source.join("hardlink")).unwrap();
    let socket = std::os::unix::net::UnixListener::bind(source.join("socket")).unwrap();
    assert!(CacheBase::capture(&source).is_err());
    drop(socket);
    fs::remove_file(source.join("socket")).unwrap();
    fs::File::create(source.join("large"))
        .unwrap()
        .set_len(128 * 1024 * 1024 + 1)
        .unwrap();
    assert!(CacheBase::capture(&source).is_err());
}

#[test]
fn digest_binds_paths_modes_and_bytes_but_not_source_location() {
    let root = private_home();
    let left = root.path().join("left");
    let right = root.path().join("right");
    for path in [&left, &right] {
        fs::create_dir(path).unwrap();
        fs::write(path.join("file"), b"bytes").unwrap();
    }
    let digest = CacheBase::capture(&left).unwrap().digest().clone();
    assert_eq!(CacheBase::capture(&right).unwrap().digest(), &digest);
    fs::set_permissions(right.join("file"), fs::Permissions::from_mode(0o700)).unwrap();
    assert_ne!(CacheBase::capture(&right).unwrap().digest(), &digest);
    fs::set_permissions(right.join("file"), fs::Permissions::from_mode(0o600)).unwrap();
    fs::rename(right.join("file"), right.join("renamed")).unwrap();
    assert_ne!(CacheBase::capture(&right).unwrap().digest(), &digest);
}

#[test]
fn broker_downloads_use_pinned_overlay_and_never_follow_agent_links() {
    let root = private_home();
    let source = root.path().join("source");
    fs::create_dir(&source).unwrap();
    let base = CacheBase::capture(&source).unwrap();
    let mut overlay = base
        .materialize(root.path(), "one", IdentityPlan::NamespaceOnly)
        .unwrap();
    let bytes = b"download";
    let digest = Digest::of(bytes);
    let name = format!("artifact-{}", digest.hex());
    fs::write(root.path().join("outside"), b"unchanged").unwrap();
    symlink(root.path().join("outside"), overlay.path().join(&name)).unwrap();
    assert!(overlay.store_download(&digest, bytes).is_err());
    assert_eq!(fs::read(root.path().join("outside")).unwrap(), b"unchanged");
    fs::remove_file(overlay.path().join(&name)).unwrap();
    let saved = root.path().join("retained");
    fs::rename(overlay.path(), &saved).unwrap();
    symlink(&source, overlay.path()).unwrap();
    assert!(
        overlay
            .store_download(&Digest::of(b"wrong"), bytes)
            .is_err()
    );
    assert_eq!(overlay.store_download(&digest, bytes).unwrap(), name);
    assert_eq!(fs::read(saved.join(&name)).unwrap(), bytes);
    assert!(!source.join(&name).exists());
    assert!(
        overlay.store_download(&digest, bytes).is_err(),
        "existing Agent bytes are never accepted as a verified download"
    );
}
