//! The operator's local endorser layer, kept in the protected store.
//!
//! Raising trust needs the operator's Primary key; lowering it never does.
//! An addition, or the restore of a removed key, is a Primary-signed payload
//! in its own SSHSIG namespace. A removal is a plain record: it can only cause
//! more review, so a compromised Session gains nothing by adding one.
//!
//! Stored additions are re-verified on every load against the *current*
//! Primary key and trust domain. One that fails — tampered bytes, another
//! trust domain, or a Primary rotation since it was signed — is dropped and
//! reported, never trusted. Re-adding it after a rotation is the safe path.
//!
//! A removal beats any addition of the same key, including a later one; only
//! a signed restore lifts it. Signing happens before the store lock is taken,
//! so no child process is ever forked while the lock is held.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

use rustix::fs::{FlockOperation, flock};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{
    trust::{self, EndorserEntry, EndorserError, EndorserSet},
    valid_public_key,
};
use crate::{
    scan::escape,
    signer::{Signer, SignerError},
    sshsig::{self, SignatureError},
    store::{Store, share_evidence},
    trust::{Role, TrustError, TrustStore},
};

/// The SSHSIG namespace for local endorser changes.
pub const ENDORSER_NAMESPACE: &str = "louiselm.skills.endorser/1";

/// Schema of a signed endorser addition payload.
pub const ADDITION_SCHEMA: &str = "louiselm.skills.endorser-addition/1";

/// Schema of a signed restore payload.
pub const RESTORE_SCHEMA: &str = "louiselm.skills.endorser-restore/1";

/// Schema of the stored local layer.
pub const LOCAL_SCHEMA: &str = "louiselm.skills.endorsers-local/1";

const DIRECTORY: &str = "endorsers";
const STATE: &str = "local.json";
const LOCK: &str = "local.lock";
const MAX_STATE_BYTES: u64 = 1024 * 1024;

/// The verified local layer, plus additions that failed verification.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct LocalLayer {
    /// Additions whose Primary signature verified under the current key.
    pub additions: Vec<EndorserEntry>,
    /// Removed keys. Unsigned by design.
    pub removals: Vec<String>,
    /// Stored additions that did not verify and are therefore not trusted.
    pub dropped: Vec<DroppedAddition>,
}

/// A stored addition that is not trusted, and why.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DroppedAddition {
    /// The key the addition named.
    pub public_key: String,
    /// Why it did not verify.
    pub reason: String,
}

/// Why the local layer could not be read or changed.
#[derive(Debug, Error)]
pub enum LocalError {
    /// The local state file could not be read or written.
    #[error("cannot access {path}: {source}")]
    Io {
        /// The path involved.
        path: String,
        /// The underlying error.
        source: io::Error,
    },
    /// The local state file is not well-formed.
    #[error("malformed endorser layer: {0}")]
    Malformed(String),
    /// An entry or key is invalid.
    #[error(transparent)]
    Endorser(#[from] EndorserError),
    /// The trust store could not be read.
    #[error(transparent)]
    Trust(#[from] TrustError),
    /// No Primary key is enrolled, so trust cannot be raised.
    #[error("no Primary key is enrolled; enroll trust before adding endorsers")]
    NoPrimary,
    /// The signature is not by the enrolled Primary key.
    #[error("endorser change is not signed by Primary: {0}")]
    Signature(#[from] SignatureError),
    /// The signer failed.
    #[error("cannot sign endorser change: {0}")]
    Signing(#[from] SignerError),
    /// A restore named a key that is not removed.
    #[error("endorser key is not removed: {key}")]
    NotRemoved {
        /// The key named.
        key: String,
    },
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AdditionPayload {
    entry: EndorserEntry,
    schema: String,
    signed_at_ms: u64,
    trust_domain: String,
}

#[derive(Serialize)]
struct RestorePayload<'a> {
    public_key: &'a str,
    schema: &'static str,
    signed_at_ms: u64,
    trust_domain: &'a str,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SignedAddition {
    payload: AdditionPayload,
    signature: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LocalDocument {
    additions: Vec<SignedAddition>,
    removals: Vec<String>,
    schema: String,
}

impl Default for LocalDocument {
    fn default() -> Self {
        Self {
            additions: Vec::new(),
            removals: Vec::new(),
            schema: LOCAL_SCHEMA.to_owned(),
        }
    }
}

/// Returns the effective endorser set: the build's defaults plus verified
/// local additions, minus removals, together with the local layer itself.
///
/// # Errors
/// Returns state read/format errors and trust-store read errors. Additions
/// that fail verification are reported in [`LocalLayer::dropped`], not errors.
pub fn effective(store: &Store) -> Result<(EndorserSet, LocalLayer), LocalError> {
    effective_with(store, TrustStore::load(store)?.as_ref())
}

/// As [`effective`], verifying additions against an already-loaded trust
/// store. For callers that hold the trust lock, where reading it again would
/// report it busy.
///
/// # Errors
/// Returns state read/format errors.
pub fn effective_with(
    store: &Store,
    trust: Option<&TrustStore>,
) -> Result<(EndorserSet, LocalLayer), LocalError> {
    let layer = load_with(store, trust)?;
    let set = EndorserSet::effective(
        &trust::embedded_defaults(),
        &layer.additions,
        &layer.removals,
    )?;
    Ok((set, layer))
}

/// Reads and verifies the local layer.
///
/// # Errors
/// Returns state read/format errors and trust-store read errors.
pub fn load(store: &Store) -> Result<LocalLayer, LocalError> {
    load_with(store, TrustStore::load(store)?.as_ref())
}

fn load_with(store: &Store, trust: Option<&TrustStore>) -> Result<LocalLayer, LocalError> {
    let document = read(&state_path(store))?;
    let mut layer = LocalLayer {
        removals: document.removals,
        ..LocalLayer::default()
    };
    for addition in document.additions {
        match verify_addition(trust, &addition) {
            Ok(()) => layer.additions.push(addition.payload.entry),
            Err(reason) => layer.dropped.push(DroppedAddition {
                public_key: addition.payload.entry.public_key,
                reason,
            }),
        }
    }
    Ok(layer)
}

/// Adds or replaces a local endorser entry, signed by the Primary key.
///
/// An existing removal of the same key still wins; see [`restore`].
///
/// # Errors
/// Returns [`LocalError::Endorser`] for an invalid entry,
/// [`LocalError::NoPrimary`] without enrolled trust, signing or
/// [`LocalError::Signature`] failures when `signer` is not Primary, and
/// state I/O errors. Nothing is written on failure.
pub fn add(
    store: &Store,
    entry: &EndorserEntry,
    signer: &dyn Signer,
    now_ms: u64,
) -> Result<(), LocalError> {
    trust::validate(entry)?;
    let trust = TrustStore::load(store)?.ok_or(LocalError::NoPrimary)?;
    let payload = AdditionPayload {
        entry: entry.clone(),
        schema: ADDITION_SCHEMA.to_owned(),
        signed_at_ms: now_ms,
        trust_domain: trust.trust_domain.clone(),
    };
    let signature = sign_as_primary(&trust, &encode(&payload), signer)?;
    update(store, |document| {
        document
            .additions
            .retain(|addition| addition.payload.entry.public_key != entry.public_key);
        document
            .additions
            .push(SignedAddition { payload, signature });
        Ok(())
    })
}

/// Removes trust in `public_key`, whatever layer granted it. Unsigned.
///
/// # Errors
/// Returns [`LocalError::Endorser`] for a malformed key and state I/O errors.
pub fn remove(store: &Store, public_key: &str) -> Result<(), LocalError> {
    check_key(public_key)?;
    update(store, |document| {
        if !document.removals.iter().any(|key| key == public_key) {
            document.removals.push(public_key.to_owned());
            document.removals.sort();
        }
        Ok(())
    })
}

/// Lifts a removal of `public_key`, signed by the Primary key.
///
/// # Errors
/// Returns [`LocalError::NotRemoved`] when the key is not removed,
/// [`LocalError::NoPrimary`], signing or [`LocalError::Signature`] failures,
/// and state I/O errors. Nothing is written on failure.
pub fn restore(
    store: &Store,
    public_key: &str,
    signer: &dyn Signer,
    now_ms: u64,
) -> Result<(), LocalError> {
    check_key(public_key)?;
    if !read(&state_path(store))?
        .removals
        .iter()
        .any(|key| key == public_key)
    {
        return Err(LocalError::NotRemoved {
            key: public_key.to_owned(),
        });
    }
    let trust = TrustStore::load(store)?.ok_or(LocalError::NoPrimary)?;
    let payload = RestorePayload {
        public_key,
        schema: RESTORE_SCHEMA,
        signed_at_ms: now_ms,
        trust_domain: &trust.trust_domain,
    };
    sign_as_primary(&trust, &encode(&payload), signer)?;
    update(store, |document| {
        let before = document.removals.len();
        document.removals.retain(|key| key != public_key);
        if document.removals.len() == before {
            return Err(LocalError::NotRemoved {
                key: public_key.to_owned(),
            });
        }
        Ok(())
    })
}

fn sign_as_primary(
    trust: &TrustStore,
    payload: &[u8],
    signer: &dyn Signer,
) -> Result<String, LocalError> {
    let primary = trust.key_for(Role::Primary).ok_or(LocalError::NoPrimary)?;
    let signature = signer.sign(ENDORSER_NAMESPACE, payload)?;
    sshsig::verify(
        &signature,
        ENDORSER_NAMESPACE,
        payload,
        &primary.public_key,
        primary.sk_policy,
    )?;
    Ok(signature)
}

fn verify_addition(trust: Option<&TrustStore>, addition: &SignedAddition) -> Result<(), String> {
    let trust = trust.ok_or_else(|| "no enrolled Primary key".to_owned())?;
    let primary = trust
        .key_for(Role::Primary)
        .ok_or_else(|| "no enrolled Primary key".to_owned())?;
    let payload = &addition.payload;
    if payload.schema != ADDITION_SCHEMA {
        return Err(format!("unsupported schema {}", escape(&payload.schema)));
    }
    if payload.trust_domain != trust.trust_domain {
        return Err("signed for another trust domain".to_owned());
    }
    trust::validate(&payload.entry).map_err(|error| error.to_string())?;
    sshsig::verify(
        &addition.signature,
        ENDORSER_NAMESPACE,
        &encode(payload),
        &primary.public_key,
        primary.sk_policy,
    )
    .map(|_| ())
    .map_err(|error| error.to_string())
}

fn check_key(public_key: &str) -> Result<(), LocalError> {
    if valid_public_key(public_key) {
        Ok(())
    } else {
        Err(LocalError::Endorser(EndorserError::Invalid {
            field: "public_key",
            reason: "must be '<algorithm> <base64>' with no comment",
        }))
    }
}

fn update(
    store: &Store,
    change: impl FnOnce(&mut LocalDocument) -> Result<(), LocalError>,
) -> Result<(), LocalError> {
    let directory = store.root().join(DIRECTORY);
    fs::create_dir_all(&directory).map_err(|source| io_error(&directory, source))?;
    let lock_path = directory.join(LOCK);
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits().cast_signed())
        .open(&lock_path)
        .map_err(|source| io_error(&lock_path, source))?;
    flock(&lock, FlockOperation::LockExclusive)
        .map_err(|source| io_error(&lock_path, source.into()))?;
    let result = read(&state_path(store)).and_then(|mut document| {
        change(&mut document)?;
        write_atomic(&state_path(store), &encode(&document))
    });
    // Released explicitly: flock belongs to the open file description, and a
    // descriptor inherited elsewhere would otherwise hold it (louiselm-xx07b).
    let unlocked =
        flock(&lock, FlockOperation::Unlock).map_err(|source| io_error(&lock_path, source.into()));
    result.and(unlocked)
}

fn state_path(store: &Store) -> PathBuf {
    store.root().join(DIRECTORY).join(STATE)
}

fn read(path: &Path) -> Result<LocalDocument, LocalError> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(
            (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK)
                .bits()
                .cast_signed(),
        )
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(LocalDocument::default());
        }
        Err(source) => return Err(io_error(path, source)),
    };
    let metadata = file.metadata().map_err(|source| io_error(path, source))?;
    if !metadata.is_file() || metadata.nlink() > 1 {
        return Err(LocalError::Malformed(
            "endorser layer is not a single regular file".into(),
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_STATE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| io_error(path, source))?;
    if u64::try_from(bytes.len()).map_or(true, |size| size > MAX_STATE_BYTES) {
        return Err(LocalError::Malformed("endorser layer is too large".into()));
    }
    let document: LocalDocument = serde_json::from_slice(&bytes)
        .map_err(|error| LocalError::Malformed(escape(&error.to_string())))?;
    if document.schema != LOCAL_SCHEMA {
        return Err(LocalError::Malformed(format!(
            "unsupported schema {}",
            escape(&document.schema)
        )));
    }
    for key in &document.removals {
        check_key(key)?;
    }
    Ok(document)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), LocalError> {
    let parent = path
        .parent()
        .ok_or_else(|| io_error(path, io::ErrorKind::InvalidInput.into()))?;
    let mut staged = tempfile::Builder::new()
        .prefix(".endorsers-")
        .tempfile_in(parent)
        .map_err(|source| io_error(parent, source))?;
    share_evidence(
        &File::open(parent).map_err(|source| io_error(parent, source))?,
        staged.as_file(),
    )
    .map_err(|source| io_error(path, source))?;
    staged
        .write_all(bytes)
        .and_then(|()| staged.as_file().sync_all())
        .map_err(|source| io_error(path, source))?;
    staged
        .persist(path)
        .map_err(|error| io_error(path, error.error))?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| io_error(parent, source))
}

#[expect(
    clippy::expect_used,
    reason = "Local layer types hold only strings, integers, vectors and unit enums; serialization cannot fail."
)]
fn encode<T: Serialize>(value: &T) -> Vec<u8> {
    serde_json::to_vec(value).expect("endorser layer types are always serializable")
}

fn io_error(path: &Path, source: io::Error) -> LocalError {
    LocalError::Io {
        path: path.display().to_string(),
        source,
    }
}
