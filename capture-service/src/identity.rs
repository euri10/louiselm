use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

use rcgen::generate_simple_self_signed;
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

/// TLS identity generation or persistence failure.
#[derive(Debug, Error)]
pub enum IdentityError {
    /// Filesystem operation failed.
    #[error("TLS identity storage failed: {0}")]
    Io(#[from] io::Error),
    /// Certificate generation failed.
    #[error("TLS identity generation failed: {0}")]
    Certificate(#[from] rcgen::Error),
    /// Persisted certificate PEM is malformed.
    #[error("TLS certificate is malformed: {0}")]
    Pem(#[from] pem::PemError),
    /// Only one member of the persisted keypair exists.
    #[error("TLS identity is incomplete")]
    Incomplete,
}

/// Persistent private key, certificate, and pin fingerprint for the receiver.
#[derive(Clone, Debug)]
pub struct TlsIdentity {
    certificate_path: PathBuf,
    private_key_path: PathBuf,
    certificate_sha256: String,
}

impl TlsIdentity {
    /// Load or generate the receiver's persistent self-signed identity.
    ///
    /// The phone trusts this exact certificate fingerprint from the one-time QR
    /// payload; it does not rely on a public certificate authority.
    ///
    /// # Errors
    ///
    /// Returns filesystem, generation, malformed PEM, or incomplete-pair errors.
    pub fn load_or_create(directory: impl AsRef<Path>) -> Result<Self, IdentityError> {
        fs::create_dir_all(directory.as_ref())?;
        set_private_permissions(directory.as_ref(), true)?;
        let certificate_path = directory.as_ref().join("receiver-cert.pem");
        let private_key_path = directory.as_ref().join("receiver-key.pem");

        match (certificate_path.exists(), private_key_path.exists()) {
            (false, false) => {
                let certified = generate_simple_self_signed(vec![
                    "localhost".to_owned(),
                    "louiselm-capture.local".to_owned(),
                ])?;
                write_private(&certificate_path, certified.cert.pem().as_bytes())?;
                write_private(
                    &private_key_path,
                    certified.signing_key.serialize_pem().as_bytes(),
                )?;
                File::open(directory.as_ref())?.sync_all()?;
            }
            (true, true) => {}
            _ => return Err(IdentityError::Incomplete),
        }

        set_private_permissions(&certificate_path, false)?;
        set_private_permissions(&private_key_path, false)?;
        let certificate_pem = fs::read(&certificate_path)?;
        let certificate = pem::parse(certificate_pem)?;
        let certificate_sha256 = format!("{:x}", Sha256::digest(certificate.contents()));
        Ok(Self {
            certificate_path,
            private_key_path,
            certificate_sha256,
        })
    }

    /// PEM certificate path accepted by the TLS server.
    #[must_use]
    pub fn certificate_path(&self) -> &Path {
        &self.certificate_path
    }

    /// PEM private-key path accepted by the TLS server.
    #[must_use]
    pub fn private_key_path(&self) -> &Path {
        &self.private_key_path
    }

    /// Lowercase SHA-256 fingerprint of certificate DER.
    #[must_use]
    pub fn certificate_sha256(&self) -> &str {
        &self.certificate_sha256
    }
}

fn write_private(path: &Path, contents: &[u8]) -> io::Result<()> {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "identity path is invalid"))?;
    let temporary = path.with_file_name(format!(".{name}.{}", Uuid::new_v4()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        set_private_permissions(&temporary, false)?;
        file.write_all(contents)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    if temporary.exists() {
        let _ = fs::remove_file(temporary);
    }
    result
}

#[cfg(unix)]
fn set_private_permissions(path: &Path, directory: bool) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(
        path,
        fs::Permissions::from_mode(if directory { 0o700 } else { 0o600 }),
    )
}

#[cfg(not(unix))]
fn set_private_permissions(_path: &Path, _directory: bool) -> io::Result<()> {
    Ok(())
}
