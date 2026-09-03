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

    /// Returns a bare git repository standing in for the protected remote.
    ///
    /// A local bare repo exercises the real git witness path — fetch, commit,
    /// push, read-back — without a network or a server.
    pub fn witness_remote(&self) -> PathBuf {
        let path = self.path("witness-remote.git");
        if !path.exists() {
            let status = std::process::Command::new("git")
                .args(["init", "--bare", "-q"])
                .arg(&path)
                .status()
                .expect("git is installed; witnessing depends on it");
            assert!(status.success(), "git failed to create the witness remote");
        }
        path
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

/// A software SSH key generated for a test, used to make real signatures.
///
/// Hardware-backed keys cannot be produced in an automated test, so the
/// cryptographic path is exercised with software keys and the FIDO assertion
/// path with crafted blobs. Neither substitutes for the manual ceremony.
pub struct SshKey {
    path: PathBuf,
}

impl SshKey {
    /// Generates an ed25519 key pair inside the fixture.
    pub fn generate(fixture: &Fixture, name: &str) -> Self {
        let path = fixture.path(&format!("keys/{name}"));
        fs::create_dir_all(path.parent().expect("keys directory has a parent"))
            .expect("keys directory is creatable");
        let status = std::process::Command::new("ssh-keygen")
            .args(["-t", "ed25519", "-N", "", "-C", name, "-q", "-f"])
            .arg(&path)
            .status()
            .expect("ssh-keygen is installed; the trust ceremony depends on it");
        assert!(status.success(), "ssh-keygen failed to generate a key");
        Self { path }
    }

    /// Returns the public key in `authorized_keys` form.
    pub fn public_key(&self) -> String {
        let text = fs::read_to_string(self.path.with_extension("pub"))
            .expect("the generated public key is readable");
        text.split_whitespace()
            .take(2)
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Returns the private key path, for signers that take one.
    pub fn private_key_path(&self) -> &Path {
        &self.path
    }

    /// Signs `payload` under `namespace`, returning the armored signature.
    pub fn sign(&self, namespace: &str, payload: &[u8]) -> String {
        let message = self.path.with_extension("message");
        fs::write(&message, payload).expect("the message is writable");
        let status = std::process::Command::new("ssh-keygen")
            .args(["-Y", "sign", "-q", "-n", namespace, "-f"])
            .arg(&self.path)
            .arg(&message)
            .status()
            .expect("ssh-keygen is installed");
        assert!(status.success(), "ssh-keygen failed to sign");
        fs::read_to_string(message.with_extension("message.sig"))
            .or_else(|_| fs::read_to_string(format!("{}.sig", message.display())))
            .expect("the signature is readable")
    }
}

const SK_ALGORITHM: &str = "sk-ssh-ed25519@openssh.com";

fn ssh_string(bytes: &[u8]) -> Vec<u8> {
    let mut encoded = (bytes.len() as u32).to_be_bytes().to_vec();
    encoded.extend_from_slice(bytes);
    encoded
}

fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::new();
    for chunk in bytes.chunks(3) {
        let mut buffer = [0_u8; 3];
        buffer[..chunk.len()].copy_from_slice(chunk);
        let value = u32::from_be_bytes([0, buffer[0], buffer[1], buffer[2]]);
        let indices = [
            (value >> 18) & 0x3f,
            (value >> 12) & 0x3f,
            (value >> 6) & 0x3f,
            value & 0x3f,
        ];
        for (position, index) in indices.iter().enumerate() {
            if position <= chunk.len() {
                encoded.push(ALPHABET[*index as usize] as char);
            } else {
                encoded.push('=');
            }
        }
    }
    encoded
}

fn crafted_sk_public_key_blob() -> Vec<u8> {
    let mut blob = ssh_string(SK_ALGORITHM.as_bytes());
    blob.extend(ssh_string(&[0x42_u8; 32]));
    blob.extend(ssh_string(b"ssh:"));
    blob
}

/// Returns the public key of the crafted hardware-style signature.
pub fn crafted_sk_public_key() -> String {
    format!(
        "{SK_ALGORITHM} {}",
        base64_encode(&crafted_sk_public_key_blob())
    )
}

/// Builds an sk-ed25519 signature blob carrying exactly `flags`.
///
/// The signature bytes are not real, which is the point: it must be refused
/// for its flags before anything tries to verify it.
pub fn crafted_sk_signature(namespace: &str, flags: u8) -> String {
    let mut inner = ssh_string(SK_ALGORITHM.as_bytes());
    inner.extend(ssh_string(&[0x11_u8; 64]));
    inner.push(flags);
    inner.extend(7_u32.to_be_bytes());

    let mut blob = b"SSHSIG".to_vec();
    blob.extend(1_u32.to_be_bytes());
    blob.extend(ssh_string(&crafted_sk_public_key_blob()));
    blob.extend(ssh_string(namespace.as_bytes()));
    blob.extend(ssh_string(b""));
    blob.extend(ssh_string(b"sha512"));
    blob.extend(ssh_string(&inner));

    let encoded = base64_encode(&blob);
    let mut armored = String::from("-----BEGIN SSH SIGNATURE-----\n");
    for line in encoded.as_bytes().chunks(70) {
        armored.push_str(std::str::from_utf8(line).expect("base64 is ASCII"));
        armored.push('\n');
    }
    armored.push_str("-----END SSH SIGNATURE-----\n");
    armored
}
