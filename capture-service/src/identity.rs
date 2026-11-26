use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

use rcgen::{KeyPair, PublicKeyData, generate_simple_self_signed};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::hex::lower_hex;
use crate::permissions::set_private_permissions;
use crate::phone_lock::PHONE_LOCKS;

const MAX_MANAGED_IDENTITY_BYTES: u64 = 65_536;

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

/// Persistent TLS key, certificate, and public-key identity for the receiver.
#[derive(Clone, Debug)]
pub struct TlsIdentity {
    certificate_path: PathBuf,
    private_key_path: PathBuf,
    public_key_sha256: String,
}

impl TlsIdentity {
    /// Open an existing identity without creating or replacing either file.
    pub(crate) fn load_existing(directory: &Path) -> Result<Self, IdentityError> {
        let certificate_path = directory.join("receiver-cert.pem");
        let private_key_path = directory.join("receiver-key.pem");
        let certificate_pem = read_managed_identity_file(&certificate_path, false)?;
        pem::parse(certificate_pem)?;
        let private_key_pem = read_managed_identity_file(&private_key_path, true)?;
        let private_key_pem = std::str::from_utf8(&private_key_pem)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "receiver key is not UTF-8"))?;
        let key_pair = KeyPair::from_pem(private_key_pem)?;
        let public_key_sha256 = lower_hex(&Sha256::digest(key_pair.subject_public_key_info()));
        Ok(Self {
            certificate_path,
            private_key_path,
            public_key_sha256,
        })
    }

    pub(crate) fn existing_public_key_sha256(directory: &Path) -> Result<String, IdentityError> {
        let mut private_key_pem = String::new();
        PHONE_LOCKS
            .open_read(&directory.join("receiver-key.pem"))?
            .file_mut()?
            .read_to_string(&mut private_key_pem)?;
        let key_pair = KeyPair::from_pem(&private_key_pem)?;
        Ok(lower_hex(&Sha256::digest(
            key_pair.subject_public_key_info(),
        )))
    }

    /// Load or generate the receiver's persistent self-signed identity.
    ///
    /// The phone trusts the persistent public key from the one-time QR payload;
    /// it does not rely on a public certificate authority or one certificate.
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
        pem::parse(certificate_pem)?;
        let private_key_pem = fs::read_to_string(&private_key_path)?;
        let key_pair = KeyPair::from_pem(&private_key_pem)?;
        let public_key_sha256 = lower_hex(&Sha256::digest(key_pair.subject_public_key_info()));
        Ok(Self {
            certificate_path,
            private_key_path,
            public_key_sha256,
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

    /// Lowercase SHA-256 fingerprint of the `SubjectPublicKeyInfo` DER.
    #[must_use]
    pub fn public_key_sha256(&self) -> &str {
        &self.public_key_sha256
    }
}

fn read_managed_identity_file(path: &Path, private: bool) -> io::Result<Vec<u8>> {
    let mut file = PHONE_LOCKS.open_read(path)?;
    let metadata = file.file()?.metadata()?;
    if metadata.len() > MAX_MANAGED_IDENTITY_BYTES || (private && metadata.mode() & 0o077 != 0) {
        return Err(io::Error::other("managed receiver identity is unavailable"));
    }
    let mut contents = Vec::new();
    file.file_mut()?
        .take(MAX_MANAGED_IDENTITY_BYTES + 1)
        .read_to_end(&mut contents)?;
    if contents.len() as u64 > MAX_MANAGED_IDENTITY_BYTES {
        return Err(io::Error::other("managed receiver identity is unavailable"));
    }
    Ok(contents)
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
