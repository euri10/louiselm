//! Durable receipt chains held as the supervisor's exact signed bytes.
//!
//! The Launch supervisor signs lifecycle outcomes; the broker decides whether
//! to store them and owns the stored bytes afterwards. Nothing here signs,
//! reserializes, or repairs a receipt: bytes that do not verify are refused
//! whole, and bytes that do verify are written exactly as they arrived.
//!
//! An acknowledgement means the exact envelope is durable. Storage failures
//! therefore return an error instead, because an acknowledgement the launch
//! treats as durability must never outrun the filesystem.

use std::{
    fs,
    io::Read,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use crate::{
    broker::{BrokerError, corrupt, is_record_identifier, lock, sync_directory, write_new_bytes},
    launch::PROTOCOL_VERSION,
    launch_protocol::{
        LaunchAuthorization, RECEIPT_ACK_SCHEMA, ReceiptAcknowledgement, ReceiptDisposition,
    },
    launch_receipt::{
        ChainAnchor, ReceiptError, ReceiptHead, ReceiptOutcome, SessionState, SignedReceipt,
        VerifiedReceiptHead, verify_chain, verify_suffix,
    },
    launcher_install::LauncherVerifier,
};

/// Directory holding one subdirectory per Session chain.
const SESSIONS_DIRECTORY: &str = "sessions";

/// Longest chain this build reads back for one Session.
///
/// A launch transaction writes two receipts. The bound exists so a corrupted
/// or hostile state directory cannot turn chain recovery into unbounded work.
const MAX_CHAIN_RECEIPTS: usize = 1024;

/// Caller-supplied authority for new chains in an explicitly owned store.
/// Installed brokers instead use the launcher's registered Session identities.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrustedRelease {
    /// Digest of the trusted launcher release.
    pub release_id: String,
    /// Stable launcher signing-key identity pinned for new chains.
    pub signing_key_id: String,
}

/// Durable store for exact signed receipt bytes, one chain per Session.
#[derive(Debug)]
pub struct ReceiptStore {
    root: PathBuf,
    trust: ReceiptTrust,
    /// Serializes append decisions so two receipts cannot claim one sequence.
    appending: Mutex<()>,
}

#[derive(Debug)]
enum ReceiptTrust {
    Fixed(TrustedRelease),
    Installed(Arc<LauncherVerifier>),
}

impl ReceiptStore {
    pub(super) fn check_authority(&self) -> Result<(), BrokerError> {
        if let ReceiptTrust::Installed(verifier) = &self.trust {
            verifier
                .check_authority()
                .map_err(BrokerError::InstallationAuthority)?;
        }
        Ok(())
    }

    pub(super) fn inspection_chain(
        &self,
        authorization: &LaunchAuthorization,
    ) -> Result<Vec<SignedReceipt>, BrokerError> {
        match &self.trust {
            ReceiptTrust::Fixed(_) => self.chain(&authorization.session_id),
            ReceiptTrust::Installed(verifier) => self
                .verified_chain(authorization, &mut |key, payload, signature| {
                    verifier.verify(key, payload, signature).is_ok()
                }),
        }
    }

    /// Opens or creates the durable receipt store under `root`.
    ///
    /// # Errors
    /// Returns [`BrokerError::Storage`] when the state directory cannot be
    /// created, or [`BrokerError::InvalidGrant`] for an unusable release
    /// identity.
    pub fn open(root: &Path, release: TrustedRelease) -> Result<Self, BrokerError> {
        if release.release_id.is_empty() || release.signing_key_id.is_empty() {
            return Err(BrokerError::InvalidGrant);
        }
        fs::create_dir_all(root.join(SESSIONS_DIRECTORY)).map_err(BrokerError::Storage)?;
        sync_directory(root)?;
        Ok(Self {
            root: root.to_owned(),
            trust: ReceiptTrust::Fixed(release),
            appending: Mutex::new(()),
        })
    }

    /// Opens production storage using root-registered Session chain identities.
    pub(crate) fn installed(
        root: &Path,
        verifier: Arc<LauncherVerifier>,
    ) -> Result<Self, BrokerError> {
        fs::create_dir_all(root.join(SESSIONS_DIRECTORY)).map_err(BrokerError::Storage)?;
        sync_directory(root)?;
        Ok(Self {
            root: root.to_owned(),
            trust: ReceiptTrust::Installed(verifier),
            appending: Mutex::new(()),
        })
    }

    /// Verifies one exact signed envelope and stores it durably.
    ///
    /// The receipt must answer `authorization`, match this Session's admitted
    /// release/key identity, and continue its chain at the next sequence. A new
    /// chain needs current caller authority or a root-registered genesis.
    /// The returned acknowledgement is only produced after the exact bytes and
    /// their directory entry are durable.
    ///
    /// # Errors
    /// Returns [`BrokerError::ReceiptRefused`] for bytes this chain does not
    /// accept, [`BrokerError::ReceiptUnauthorized`] when the receipt does not
    /// answer its authorization, and [`BrokerError::Storage`] when the bytes
    /// cannot be made durable. No acknowledgement follows any of them.
    pub fn append<F>(
        &self,
        authorization: &LaunchAuthorization,
        receipt_bytes: &[u8],
        mut verify_signature: F,
    ) -> Result<ReceiptAcknowledgement, BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        authorization
            .validate()
            .map_err(|_| BrokerError::ReceiptUnauthorized)?;
        let session = self.session_directory(&authorization.session_id)?;
        let receipt =
            SignedReceipt::parse_canonical(receipt_bytes).map_err(BrokerError::ReceiptRefused)?;
        check_authorized(&receipt, authorization)?;

        let appending = lock(&self.appending);
        let stored = self.chain(&authorization.session_id)?;
        let head = self.verified_head(&stored, authorization, &mut verify_signature)?;
        match head {
            Some(head) => {
                verify_suffix(std::slice::from_ref(&receipt), &head, &mut verify_signature)
                    .map_err(BrokerError::ReceiptRefused)?;
            }
            None => {
                verify_chain(
                    std::slice::from_ref(&receipt),
                    &self.anchor(authorization, None)?,
                    &mut verify_signature,
                )
                .map_err(BrokerError::ReceiptRefused)?;
            }
        }

        fs::create_dir_all(&session).map_err(BrokerError::Storage)?;
        sync_directory(&self.root.join(SESSIONS_DIRECTORY))?;
        write_new_bytes(
            &receipt_path(&session, receipt.payload.sequence),
            receipt_bytes,
        )?;
        drop(appending);

        let acknowledgement = ReceiptAcknowledgement {
            schema: RECEIPT_ACK_SCHEMA.to_owned(),
            protocol_version: PROTOCOL_VERSION,
            session_id: receipt.payload.session_id.clone(),
            run_id: receipt.payload.run_id.clone(),
            sequence: receipt.payload.sequence,
            receipt_digest: receipt.digest().to_string(),
            disposition: ReceiptDisposition::DurablyStored,
        };
        acknowledgement
            .validate()
            .map_err(|_| BrokerError::ReceiptUnauthorized)?;
        Ok(acknowledgement)
    }

    /// Returns the stored chain's current head, or `None` for an empty chain.
    ///
    /// # Errors
    /// Returns [`BrokerError::Storage`] when durable state cannot be read and
    /// [`BrokerError::InvalidGrant`] for an unusable Session identity.
    pub fn head(&self, session_id: &str) -> Result<Option<ReceiptHead>, BrokerError> {
        let stored = self.chain(session_id)?;
        Ok(stored.last().map(|receipt| ReceiptHead {
            sequence: receipt.payload.sequence,
            digest: receipt.digest().to_string(),
        }))
    }

    /// Returns the Session state the last durable receipt reached.
    ///
    /// This is the broker's own truth: the state a signed, verified, durably
    /// stored outcome recorded, not anything a peer reports about itself.
    ///
    /// # Errors
    /// Returns [`BrokerError::Storage`] when durable state cannot be read and
    /// [`BrokerError::InvalidGrant`] for an unusable Session identity.
    pub fn state(&self, session_id: &str) -> Result<Option<SessionState>, BrokerError> {
        Ok(self
            .chain(session_id)?
            .last()
            .map(|receipt| receipt.payload.resulting_state))
    }

    /// Returns each stored envelope's exact bytes, in sequence order.
    ///
    /// # Errors
    /// Returns [`BrokerError::Storage`] when durable state cannot be read and
    /// [`BrokerError::InvalidGrant`] for an unusable Session identity.
    pub fn stored_bytes(&self, session_id: &str) -> Result<Vec<Vec<u8>>, BrokerError> {
        let session = self.session_directory(session_id)?;
        let entries = match fs::read_dir(&session) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(BrokerError::Storage(error)),
        };
        let count = entries
            .take(MAX_CHAIN_RECEIPTS + 1)
            .collect::<Result<Vec<_>, _>>()
            .map_err(BrokerError::Storage)?
            .len();
        if count > MAX_CHAIN_RECEIPTS {
            return Err(corrupt("stored chain exceeds its bound"));
        }
        let mut stored = Vec::new();
        for sequence in 0..u64::try_from(count).map_err(|_| BrokerError::InvalidGrant)? {
            let bytes = read_receipt(&receipt_path(&session, sequence))?;
            stored.push(bytes);
        }
        Ok(stored)
    }

    /// Parses the stored chain, which the broker wrote and therefore trusts to
    /// be canonical; corrupted bytes are a storage failure, not a refusal.
    pub(super) fn chain(&self, session_id: &str) -> Result<Vec<SignedReceipt>, BrokerError> {
        let mut chain = Vec::new();
        for bytes in self.stored_bytes(session_id)? {
            let receipt = SignedReceipt::parse_canonical(&bytes)
                .map_err(|_| corrupt("stored receipt is not canonical"))?;
            chain.push(receipt);
        }
        Ok(chain)
    }

    /// Revalidates every stored binding and signature before broker reattachment.
    pub(super) fn verified_chain<F>(
        &self,
        authorization: &LaunchAuthorization,
        verify: &mut F,
    ) -> Result<Vec<SignedReceipt>, BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        authorization.validate()?;
        let chain = self.chain(&authorization.session_id)?;
        for receipt in &chain {
            check_authorized(receipt, authorization)?;
        }
        self.verified_head(&chain, authorization, verify)?;
        // Restart may follow a write that reached the page cache before fsync
        // failed. A checkpoint reply must establish durability again.
        let directory = self.session_directory(&authorization.session_id)?;
        for receipt in &chain {
            fs::File::open(receipt_path(&directory, receipt.payload.sequence))
                .and_then(|file| file.sync_all())
                .map_err(BrokerError::Storage)?;
        }
        if !chain.is_empty() {
            sync_directory(&directory)?;
        }
        Ok(chain)
    }

    /// Re-verifies the stored chain so an append extends verified state only.
    fn verified_head<F>(
        &self,
        stored: &[SignedReceipt],
        authorization: &LaunchAuthorization,
        verify_signature: &mut F,
    ) -> Result<Option<VerifiedReceiptHead>, BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        if stored.is_empty() {
            return Ok(None);
        }
        verify_chain(
            stored,
            &self.anchor(authorization, stored.first())?,
            verify_signature,
        )
        .map(Some)
        .map_err(|error| match error {
            ReceiptError::InvalidSignature { .. } => corrupt("stored chain no longer verifies"),
            other => BrokerError::ReceiptRefused(other),
        })
    }

    /// The trusted identity a chain for this authorization must match.
    fn anchor(
        &self,
        authorization: &LaunchAuthorization,
        admitted: Option<&SignedReceipt>,
    ) -> Result<ChainAnchor, BrokerError> {
        let release = match &self.trust {
            ReceiptTrust::Installed(verifier) => {
                return verifier
                    .receipt_anchor(&authorization.session_id)
                    .map_err(BrokerError::Verification);
            }
            ReceiptTrust::Fixed(release) => release,
        };
        // This branch serves explicitly supplied authority, including in-memory
        // peers. Existing bytes were admitted by this store; new genesis still
        // requires the caller's current release. Production uses root history.
        Ok(ChainAnchor {
            session_id: authorization.session_id.clone(),
            run_id: authorization.run_id.clone(),
            release_id: admitted
                .map_or(&release.release_id, |r| &r.payload.release_id)
                .clone(),
            signing_key_id: admitted
                .map_or(&release.signing_key_id, |r| &r.payload.signing_key_id)
                .clone(),
        })
    }

    fn session_directory(&self, session_id: &str) -> Result<PathBuf, BrokerError> {
        if is_record_identifier(session_id) {
            Ok(self.root.join(SESSIONS_DIRECTORY).join(session_id))
        } else {
            Err(BrokerError::InvalidGrant)
        }
    }
}

/// Checks every binding the broker owns before a receipt reaches storage.
fn check_authorized(
    receipt: &SignedReceipt,
    authorization: &LaunchAuthorization,
) -> Result<(), BrokerError> {
    let payload = &receipt.payload;
    if payload.session_id != authorization.session_id
        || payload.run_id != authorization.run_id
        || payload.envelope_revision != authorization.envelope_revision
    {
        return Err(BrokerError::ReceiptUnauthorized);
    }
    if let ReceiptOutcome::Launch {
        authorization: claimed,
        evidence,
    } = &payload.outcome
        && (claimed.authorization_id != authorization.authorization_id
            || claimed.request_id != authorization.request_id
            || claimed.request_digest != authorization.request_digest
            || evidence.launch_request_digest != authorization.request_digest
            || evidence.broker_loss_grace_ms != authorization.broker_loss_grace_ms)
    {
        return Err(BrokerError::ReceiptUnauthorized);
    }
    if let ReceiptOutcome::Start { evidence, .. } = &payload.outcome
        && (evidence.assigned_uid != authorization.assigned_uid
            || evidence.assigned_gid != authorization.assigned_gid)
    {
        return Err(BrokerError::ReceiptUnauthorized);
    }
    Ok(())
}

/// The durable path holding one sequence's exact signed bytes.
fn receipt_path(session: &Path, sequence: u64) -> PathBuf {
    session.join(format!("{sequence:020}.receipt.json"))
}

fn read_receipt(path: &Path) -> Result<Vec<u8>, BrokerError> {
    // Refuse links and special files without blocking on a FIFO in damaged state.
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(
            (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK)
                .bits()
                .cast_signed(),
        )
        .open(path)
        .map_err(BrokerError::Storage)?;
    if !file.metadata().map_err(BrokerError::Storage)?.is_file() {
        return Err(corrupt("stored receipt is not a regular file"));
    }
    let mut bytes = Vec::new();
    file.take(super::MAX_RECORD_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(BrokerError::Storage)?;
    if bytes.len() as u64 > super::MAX_RECORD_BYTES {
        return Err(corrupt("stored receipt exceeds its bound"));
    }
    Ok(bytes)
}
