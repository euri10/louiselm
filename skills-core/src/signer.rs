//! Producing an Admission signature.
//!
//! The signer is an interface so the ceremony can be driven by a `YubiKey` in
//! production and by a software key in tests, without the verification path
//! ever learning which one it was: a signature is checked against the enrolled
//! key and its assertion policy regardless of what produced it.

use std::{
    fs,
    io::{self, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use thiserror::Error;

/// A signature that could not be produced.
#[derive(Debug, Error)]
pub enum SignerError {
    /// Scratch files could not be written.
    #[error("cannot prepare signing input: {0}")]
    Io(#[from] io::Error),
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
        self.sign_until(namespace, payload, Instant::now() + Duration::from_mins(5))
    }
}

impl SshKeygenSigner {
    /// Signs before one absolute deadline, cleaning up the child process group
    /// and restoring foreground terminal ownership before returning. Interactive
    /// prompts and diagnostics go directly to the private terminal, never into
    /// captured errors; noninteractive diagnostics are captured and escaped.
    ///
    /// # Errors
    /// Refuses expired deadlines, lost terminal ownership, process/cleanup
    /// failures, or a refused signature. Interactive signing requires Linux.
    pub fn sign_until(
        &self,
        namespace: &str,
        payload: &[u8],
        deadline: Instant,
    ) -> Result<String, SignerError> {
        // Never let a root caller inherit an attacker-selected TMPDIR. tempfile
        // creates this directory exclusively with mode 0700 in the shared /tmp.
        let scratch = tempfile::Builder::new()
            .prefix("louiselm-sign-")
            .tempdir_in("/tmp")?;
        let result = (|| {
            let message = scratch.path().join("payload");
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&message)?;
            file.write_all(payload)?;
            let terminal = match fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/tty")
            {
                Ok(terminal) => Some(terminal),
                Err(error)
                    if error.raw_os_error() == Some(rustix::io::Errno::NXIO.raw_os_error())
                        || error.kind() == io::ErrorKind::NotFound =>
                {
                    None
                }
                Err(error) => return Err(SignerError::Io(error)),
            };
            let output = crate::launcher_install::run_signing_command(
                &crate::launcher_install::CommandInvocation {
                    program: PathBuf::from("/usr/bin/ssh-keygen"),
                    arguments: vec![
                        "-Y".into(),
                        "sign".into(),
                        "-q".into(),
                        "-n".into(),
                        namespace.into(),
                        "-f".into(),
                        self.key_path.as_os_str().to_owned(),
                        message.into_os_string(),
                    ],
                    stdin: Vec::new(),
                    current_dir: None,
                },
                deadline,
                terminal.as_ref(),
            )?;

            if output.success {
                fs::read_to_string(scratch.path().join("payload.sig")).map_err(SignerError::Io)
            } else if terminal.is_some() {
                Err(SignerError::Failed(
                    "ssh-keygen refused signing; see private terminal for diagnostics".to_owned(),
                ))
            } else {
                Err(SignerError::Failed(crate::scan::escape(
                    String::from_utf8_lossy(&output.stderr).trim(),
                )))
            }
        })();
        scratch.close()?;
        result
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "Isolated temporary-file regression fixtures."
    )]
    use super::*;

    #[test]
    fn precreated_payload_symlink_never_overwrites_an_external_file() {
        let fixture = tempfile::tempdir().unwrap();
        let target = fixture.path().join("unrelated");
        fs::write(&target, b"must remain unchanged").unwrap();
        // The displaced implementation starts at zero in this test process.
        let scratch =
            std::env::temp_dir().join(format!("louiselm-skills-sign-{}-0", std::process::id()));
        fs::create_dir(&scratch).unwrap();
        std::os::unix::fs::symlink(&target, scratch.join("payload")).unwrap();
        let result = SshKeygenSigner::new(&fixture.path().join("absent-key"))
            .sign("test/isolated", b"unauthorized overwrite");
        assert!(result.is_err());
        let actual = fs::read(&target).unwrap();
        // Only remove the exact test-owned collision, if the signer left it alone.
        if scratch.exists() {
            fs::remove_dir_all(&scratch).unwrap();
        }
        assert_eq!(actual, b"must remain unchanged");
    }
}
