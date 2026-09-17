//! Releasing the lock must not depend on closing every inherited descriptor.
#![allow(clippy::unwrap_used, reason = "Fixtures assert exact lock outcomes.")]
use super::*;
use crate::workspace::retention::tests::owner;

/// A child forked before its exec inherits the open file description, so closing
/// only the parent descriptor leaves the flock held and refuses the next
/// operator request until that child execs (louiselm-xx07b).
#[test]
fn an_inherited_descriptor_cannot_keep_the_lock_held_after_release() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("policy");
    Store::create(&path).unwrap();
    let store = Store::lock(&path, owner()).unwrap();
    let inherited = store.root.try_clone().unwrap();
    drop(store);
    Store::lock(&path, owner()).unwrap();
    drop(inherited);
}
