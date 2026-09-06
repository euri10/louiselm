//! Producing an Admission signature.
//!
//! The signer is an interface so the ceremony can be driven by a `YubiKey` in
//! production and by a software key in tests, without the verification path
//! ever learning which one it was: a signature is checked against the enrolled
//! key and its assertion policy regardless of what produced it.

use std::{
    fs, io,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

use thiserror::Error;

static SCRATCH_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A signature that could not be produced.
#[derive(Debug, Error)]
pub enum SignerError {
    /// Scratch files could not be written.
    #[error("cannot prepare signing input: {0}")]
    Io(#[from] io::Error),
    /// `ssh-keygen` is not installed.
    #[error("ssh-keygen is required to sign: {0}")]
    ToolMissing(String),
    /// `ssh-keygen` refused to sign.
    #[error("signing failed: {0}")]
    Failed(String),
}

/// Something that can authorize bytes in a namespace.
pub trait Signer {
    /// Signs `payload` in `namespace`, returning an armored SSH signature.
    ///
    /// # Errors
    /// Returns signer/tool failures, including key access, input/output, or signing refusal.
    fn sign(&self, namespace: &str, payload: &[u8]) -> Result<String, SignerError>;
}

/// Signs with `ssh-keygen -Y sign`, which drives a `YubiKey` when the key is one.
///
/// For a FIDO key, `ssh-keygen` prompts for the touch itself. Nothing in this
/// crate can fake that, and nothing here tries to: the assertion flags in the
/// resulting signature are what later verification actually checks.
pub struct SshKeygenSigner {
    key_path: PathBuf,
}

impl SshKeygenSigner {
    /// Signs with the private key at `key_path`.
    #[must_use]
    pub fn new(key_path: &Path) -> Self {
        Self {
            key_path: key_path.to_path_buf(),
        }
    }
}

impl Signer for SshKeygenSigner {
    fn sign(&self, namespace: &str, payload: &[u8]) -> Result<String, SignerError> {
        let counter = SCRATCH_COUNTER.fetch_add(1, Ordering::Relaxed);
        let scratch = std::env::temp_dir().join(format!(
            "louiselm-skills-sign-{}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&scratch)?;
        let message = scratch.join("payload");
        fs::write(&message, payload)?;

        let output = Command::new("ssh-keygen")
            .arg("-Y")
            .arg("sign")
            .arg("-q")
            .arg("-n")
            .arg(namespace)
            .arg("-f")
            .arg(&self.key_path)
            .arg(&message)
            .output()
            .map_err(|error| SignerError::ToolMissing(error.to_string()))?;

        let result = if output.status.success() {
            fs::read_to_string(scratch.join("payload.sig")).map_err(SignerError::Io)
        } else {
            Err(SignerError::Failed(crate::scan::escape(
                String::from_utf8_lossy(&output.stderr).trim(),
            )))
        };
        let _ = fs::remove_dir_all(&scratch);
        result
    }
}
