//! Measured warm bytes never become shared writable Session state.
#![allow(clippy::unwrap_used, reason = "Tests abort on fixture failures.")]

use louiselm_skills::{
    Digest,
    cache::{CacheBase, CacheError},
    sandbox::IdentityPlan,
};
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
fn capture_traverses_ancestors_without_listing_them() {
    if rustix::process::geteuid().is_root() {
        eprintln!("SKIP: ancestor-read denial requires an unprivileged reader");
        return;
    }
    let root = private_home();
    let ancestor = root.path().join("operator-home");
    let source = ancestor.join("cache");
    fs::create_dir_all(&source).unwrap();
    // sey9w.19.4: broker GID1500 can traverse /home/vm (0710) and read the
    // selected cache, but cannot list the home. Model those effective rights
    // as the fixture owner, without changing identities or real directories.
    for populated in [false, true] {
        if populated {
            fs::write(source.join("dependency"), b"warm").unwrap();
        }
        let expected = CacheBase::capture(&source).unwrap();
        fs::set_permissions(&ancestor, fs::Permissions::from_mode(0o100)).unwrap();
        let listing = fs::read_dir(&ancestor);
        let captured = CacheBase::capture(&source);
        let mode = fs::metadata(&ancestor).unwrap().mode() & 0o7777;
        fs::set_permissions(&ancestor, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(
            listing.unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );
        assert_eq!(mode, 0o100, "capture must not widen ancestor permissions");
        assert_eq!(captured.unwrap().digest(), expected.digest());
    }
}

#[test]
fn capture_still_requires_ancestor_search_and_selected_tree_read_access() {
    if rustix::process::geteuid().is_root() {
        eprintln!("SKIP: search/read denial requires an unprivileged reader");
        return;
    }
    let root = private_home();
    let ancestor = root.path().join("operator-home");
    let source = ancestor.join("cache");
    fs::create_dir_all(&source).unwrap();
    let file = source.join("dependency");
    fs::write(&file, b"private").unwrap();
    for (path, denied_mode, restored_mode) in [
        (&ancestor, 0o400, 0o700),
        (&source, 0o100, 0o700),
        (&file, 0o000, 0o600),
    ] {
        fs::set_permissions(path, fs::Permissions::from_mode(denied_mode)).unwrap();
        let captured = CacheBase::capture(&source);
        fs::set_permissions(path, fs::Permissions::from_mode(restored_mode)).unwrap();
        assert!(matches!(captured, Err(CacheError::Io(error))
            if error.kind() == std::io::ErrorKind::PermissionDenied));
    }
}

#[test]
fn capture_refuses_links_in_the_root_and_its_ancestors() {
    let root = private_home();
    let ancestor = root.path().join("operator-home");
    let source = ancestor.join("cache");
    fs::create_dir_all(&source).unwrap();
    let alias = root.path().join("alias");
    let linked_cache = ancestor.join("linked-cache");
    symlink(&ancestor, &alias).unwrap();
    symlink(&source, &linked_cache).unwrap();
    for path in [alias.join("cache"), linked_cache] {
        assert!(
            matches!(CacheBase::capture(&path), Err(CacheError::Io(error))
            if error.raw_os_error() == Some(rustix::io::Errno::NOTDIR.raw_os_error())
                || error.raw_os_error() == Some(rustix::io::Errno::LOOP.raw_os_error()))
        );
    }
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
