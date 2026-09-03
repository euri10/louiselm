//! Shared fixtures for the integration tests.
//!
//! Each test binary compiles this module separately, so helpers used by only
//! some of them are not dead code from the suite's point of view.
#![allow(dead_code)]

use std::{
    fs,
    path::{Path, PathBuf},
};

use louiselm_skills::{Package, Policy, PublishOutcome, Store, StoreError};
use tempfile::TempDir;

/// A temporary working directory holding candidates and one store.
pub struct Fixture {
    directory: TempDir,
}

impl Fixture {
    /// Creates an empty fixture with an initialized store.
    pub fn new() -> Self {
        let directory = TempDir::new().expect("temporary directory is creatable");
        fs::create_dir_all(directory.path().join("store")).expect("store root is creatable");
        Self { directory }
    }

    /// Returns a path inside the fixture, without creating it.
    pub fn path(&self, relative: &str) -> PathBuf {
        self.directory.path().join(relative)
    }

    /// Creates and returns a candidate directory.
    pub fn candidate(&self, relative: &str) -> PathBuf {
        let path = self.path(relative);
        fs::create_dir_all(&path).expect("candidate directory is creatable");
        path
    }

    /// Returns the store root.
    pub fn store_root(&self) -> PathBuf {
        self.path("store")
    }

    /// Opens the fixture's store.
    pub fn store(&self) -> Store {
        Store::open(&self.store_root()).expect("store opens")
    }

    /// Captures a candidate under the embedded policy.
    pub fn capture(&self, source: &Path) -> Result<(Package, PublishOutcome), StoreError> {
        self.capture_with(source, &Policy::embedded())
    }

    /// Captures a candidate under a caller-supplied policy.
    pub fn capture_with(
        &self,
        source: &Path,
        policy: &Policy,
    ) -> Result<(Package, PublishOutcome), StoreError> {
        self.store().capture(source, policy, 1_756_800_000_000)
    }

    /// Asserts no package and no staging leftovers exist.
    pub fn assert_store_is_empty(&self) {
        let store = self.store();
        assert!(
            store.list().expect("store lists").is_empty(),
            "a refused capture must publish nothing",
        );
        let staging = self.store_root().join("staging");
        let leftovers = fs::read_dir(&staging)
            .expect("staging directory exists")
            .count();
        assert_eq!(leftovers, 0, "a refused capture must leave no staging tree");
    }
}

/// Writes `content` to `path`, creating parent directories.
pub fn write_file(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("parent directory is creatable");
    }
    fs::write(path, content).expect("file is writable");
}

/// Returns the embedded policy with `replacements` applied to its bytes.
pub fn policy_with(replacements: &[(&str, &str)]) -> Policy {
    let mut text =
        String::from_utf8(Policy::embedded_bytes().to_vec()).expect("policy bytes are UTF-8");
    for (from, to) in replacements {
        assert!(text.contains(from), "policy does not contain '{from}'");
        text = text.replace(from, to);
    }
    Policy::from_bytes(text.as_bytes()).expect("edited policy is valid")
}
