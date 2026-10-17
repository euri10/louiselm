//! Canonical recovery bytes never enter or replace Session receipt chains.
use super::{ReceiptStore, ReceiptTrust, read_receipt};
use crate::{
    broker::{BrokerError, PendingAuthorization, lock, record_name, sync_directory},
    launch_recovery::{Acknowledgement, Binding, Proof, ProofError},
};
use std::{fs, io::Write as _, path::PathBuf};

impl ReceiptStore {
    fn recovery_path(&self, id: &str) -> Result<PathBuf, BrokerError> {
        Ok(self.root.join("consumed-recoveries").join(record_name(id)?))
    }

    pub(super) fn recovery_exists(&self, id: &str) -> Result<bool, BrokerError> {
        match fs::symlink_metadata(self.recovery_path(id)?) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(BrokerError::Storage(error)),
        }
    }

    fn read_recovery(&self, id: &str) -> Result<Option<Proof>, BrokerError> {
        if !self.recovery_exists(id)? {
            return Ok(None);
        }
        Proof::parse(&read_receipt(&self.recovery_path(id)?)?)
            .map(Some)
            .map_err(Into::into)
    }

    fn verify_recovery<F>(
        &self,
        proof: &Proof,
        held: &PendingAuthorization,
        digest: &str,
        verify: &mut F,
    ) -> Result<(), BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        proof.validate()?;
        if proof.payload.binding
            != (Binding {
                authorization: held.launch_authorization(),
                consumption_digest: digest.into(),
            })
        {
            return Err(BrokerError::RequestMismatch);
        }
        match &self.trust {
            ReceiptTrust::Installed(verifier) => verifier
                .verify_recovery(proof)
                .map_err(BrokerError::Verification),
            ReceiptTrust::Fixed(release) => {
                if proof.payload.release_id != release.release_id
                    || proof.payload.signing_key_id != release.signing_key_id
                {
                    return Err(BrokerError::InvalidGrant);
                }
                if !verify(
                    &proof.payload.signing_key_id,
                    &proof.payload.canonical_bytes(),
                    &proof.signature,
                ) {
                    return Err(ProofError::InvalidSignature.into());
                }
                Ok(())
            }
        }
    }

    pub(in crate::broker) fn verified_recovery<F>(
        &self,
        held: &PendingAuthorization,
        digest: &str,
        mut verify: F,
    ) -> Result<Option<Proof>, BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        let _appending = lock(&self.appending);
        let Some(proof) = self.read_recovery(&held.authorization_id)? else {
            return Ok(None);
        };
        if !self.chain(&held.session_id)?.is_empty() {
            return Err(BrokerError::InvalidGrant);
        }
        self.verify_recovery(&proof, held, digest, &mut verify)?;
        let path = self.recovery_path(&held.authorization_id)?;
        fs::File::open(&path)
            .and_then(|file| file.sync_all())
            .map_err(BrokerError::Storage)?;
        sync_directory(path.parent().ok_or(BrokerError::InvalidGrant)?)?;
        Ok(Some(proof))
    }

    pub(in crate::broker) fn store_recovery<F>(
        &self,
        held: &PendingAuthorization,
        digest: &str,
        proof: &Proof,
        mut verify: F,
    ) -> Result<Acknowledgement, BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        let _appending = lock(&self.appending);
        if !self.chain(&held.session_id)?.is_empty() {
            return Err(BrokerError::InvalidGrant);
        }
        self.verify_recovery(proof, held, digest, &mut verify)?;
        let path = self.recovery_path(&held.authorization_id)?;
        if let Some(stored) = self.read_recovery(&held.authorization_id)? {
            if stored != *proof {
                return Err(BrokerError::RequestMismatch);
            }
            fs::File::open(&path)
                .and_then(|file| file.sync_all())
                .map_err(BrokerError::Storage)?;
        } else {
            let directory = path.parent().ok_or(BrokerError::InvalidGrant)?;
            fs::create_dir_all(directory).map_err(BrokerError::Storage)?;
            sync_directory(&self.root)?;
            // A killed writer may leave an unreferenced staging file, never a
            // partial canonical outcome that would strand an exact retry.
            let mut staging =
                tempfile::NamedTempFile::new_in(directory).map_err(BrokerError::Storage)?;
            staging
                .write_all(&proof.canonical_bytes())
                .and_then(|()| staging.as_file().sync_all())
                .map_err(BrokerError::Storage)?;
            staging
                .persist_noclobber(&path)
                .map_err(|error| BrokerError::Storage(error.error))?;
        }
        sync_directory(path.parent().ok_or(BrokerError::InvalidGrant)?)?;
        self.check_authority()?;
        // Key revocation during persistence cannot turn an earlier check into
        // a new durable acknowledgement. Exact bytes remain for inspection.
        self.verify_recovery(proof, held, digest, &mut verify)?;
        Ok(Acknowledgement::for_proof(proof))
    }

    pub(in crate::broker) fn inspection_recovery(
        &self,
        held: &PendingAuthorization,
        digest: &str,
    ) -> Result<Option<Proof>, BrokerError> {
        // Fixed stores use the same previously-admitted-byte trust as inspection_chain.
        // Installed stores always authenticate through the dedicated recovery verifier.
        self.verified_recovery(held, digest, |_, _, _| true)
    }
}
