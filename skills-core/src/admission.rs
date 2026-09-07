//! The Skill Admission ceremony and the Generation lifecycle.
//!
//! Admission is a transaction with three separable failures, and the ordering
//! is chosen so that each one leaves the prior Generation in force:
//!
//! 1. **Recompute.** Every member's Dossier is rebuilt from stored bytes. A
//!    package that no longer verifies, or that Inspection calls unreviewable,
//!    stops the ceremony before anything is signed. Nothing a caller supplies
//!    — digest, root, or Dossier — is accepted as authority.
//! 2. **Sign, then verify our own signature.** The signature that comes back
//!    is checked against the enrolled key and its assertion policy before it
//!    is stored, so a signer that returned something unexpected fails here and
//!    not at activation.
//! 3. **Witness, then activate.** A signed Generation is stored
//!    `pending_witness` and governs nothing. It becomes current only after the
//!    exact bytes are read back from the witness remote and its sequence is
//!    higher than what is current. A witness outage therefore changes nothing.

use std::{collections::BTreeMap, fs, io, path::PathBuf};

use serde::Serialize;
use thiserror::Error;

use crate::{
    canonical::{Digest, DigestError},
    dossier::{Dossier, DossierError, DossierRequest, NextAction, ReviewDepth},
    generation::{
        GENERATION_SCHEMA, GenerationPayload, GenerationRecord, GenerationState, Member,
        RECORD_SCHEMA,
    },
    policy::Policy,
    quarantine::{self, QuarantineError},
    signer::{Signer, SignerError},
    sshsig::{self, ADMISSION_NAMESPACE},
    store::{Store, StoreError},
    trust::{Role, TrustError, TrustStore, persistence::LockedTrust},
    witness::{Witness, WitnessError, WitnessEvidence},
};

/// The status schema this build reports.
pub const STATUS_SCHEMA: &str = "louiselm.skills.generation-status/1";

/// What to admit.
pub struct AdmissionRequest<'a> {
    /// Packages to admit, with the review depth claimed for each.
    pub members: Vec<(Digest, ReviewDepth)>,
    /// Provider view roots to bind, keyed by Provider name.
    pub view_roots: BTreeMap<String, String>,
    /// What will authorize the payload.
    pub signer: &'a dyn Signer,
    /// When the ceremony ran.
    pub admitted_at_ms: u64,
}

/// An Admission or lifecycle step that was refused.
#[derive(Debug, Error)]
pub enum AdmissionError {
    /// A filesystem operation failed.
    #[error("admission I/O failed at '{path}': {source}")]
    Io {
        /// Path being operated on.
        path: String,
        /// Underlying failure.
        source: io::Error,
    },
    /// A stored record is unreadable.
    #[error("generation record is malformed: {0}")]
    Malformed(String),
    /// The store could not produce a package or its records.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// A Dossier could not be rebuilt.
    #[error(transparent)]
    Dossier(#[from] DossierError),
    /// Trust is not usable.
    #[error(transparent)]
    Trust(#[from] TrustError),
    /// The signer failed.
    #[error(transparent)]
    Signer(#[from] SignerError),
    /// The signature does not authorize this payload.
    #[error(transparent)]
    Signature(#[from] sshsig::SignatureError),
    /// A digest argument is malformed.
    #[error(transparent)]
    Digest(#[from] DigestError),
    /// The witness could not be reached or refused.
    #[error(transparent)]
    Witness(#[from] WitnessError),
    /// The quarantine could not be read.
    #[error(transparent)]
    Quarantine(#[from] QuarantineError),
    /// A member does not verify or is not reviewable.
    #[error("package {package} cannot be admitted: {reason}")]
    NotReviewable {
        /// The package that stopped the ceremony.
        package: String,
        /// Why it stopped.
        reason: String,
    },
    /// The record's stored identity does not match its payload.
    #[error("record claims {claimed} but its payload is {actual}")]
    DigestMismatch {
        /// Identity the record claims.
        claimed: String,
        /// Identity its payload actually has.
        actual: String,
    },
    /// The payload's set root does not cover the members it lists.
    #[error("set root does not cover the members it claims")]
    RootMismatch,
    /// The payload does not fit the chain.
    #[error("generation does not follow the chain: {0}")]
    Chain(String),
    /// The requested Generation is not stored.
    #[error("generation {0} is not stored")]
    Unknown(String),
    /// Activation was attempted before the witness confirmed the bytes.
    #[error("generation {digest} has not been witnessed")]
    NotWitnessed {
        /// The Generation that is not witnessed.
        digest: String,
    },
    /// The witness holds different bytes.
    #[error("witness holds different bytes for {digest}")]
    WitnessMismatch {
        /// The contested Generation.
        digest: String,
    },
    /// A development build tried to change a trusted store.
    #[error(
        "this is not a trusted release ({reason}); it may not activate a Generation in a trusted store"
    )]
    NotTrustedRelease {
        /// Why the running executable is not a release.
        reason: String,
    },
    /// Activation would move the supply backwards.
    #[error("generation {attempted} would roll back from sequence {current}")]
    Rollback {
        /// The Generation someone tried to activate.
        attempted: String,
        /// Sequence currently in force.
        current: u64,
    },
}

/// What a caller should do next with the Generation chain.
#[derive(Debug, Serialize)]
pub struct GenerationStatus {
    /// Schema identifier.
    pub schema: String,
    /// State of the Generation in force, when one is.
    pub state: Option<GenerationState>,
    /// Identity of the Generation in force.
    pub generation: Option<String>,
    /// Its sequence.
    pub sequence: Option<u64>,
    /// Its predecessor.
    pub predecessor: Option<String>,
    /// The role that signed it.
    pub signer_role: Option<String>,
    /// Evidence the witness holds its bytes.
    pub witness: Option<WitnessEvidence>,
    /// Members a Session may use right now.
    pub effective_members: Vec<String>,
    /// Members an active quarantine excludes.
    pub excluded_members: Vec<String>,
    /// Generations signed but not yet in force.
    pub pending: Vec<String>,
    /// Why the chain is stuck, when it is.
    pub failure: Option<String>,
    /// The one safe thing to do next.
    pub next_action: NextAction,
}

/// Runs the Admission ceremony and stores the signed Generation.
///
/// # Errors
/// Returns trust/signature, package review, predecessor-loading, or record-write errors.
pub fn admit(
    store: &Store,
    policy: &Policy,
    request: &AdmissionRequest<'_>,
) -> Result<GenerationRecord, AdmissionError> {
    let locked = LockedTrust::acquire(store)?;
    let mut trust = locked.load()?.ok_or(TrustError::NotBootstrapped)?;
    let signing_key = trust.admission_key()?.clone();

    let mut members = Vec::with_capacity(request.members.len());
    for (digest, depth) in &request.members {
        let dossier = Dossier::build(
            store,
            policy,
            &DossierRequest::new(digest).with_review_depth(*depth),
        )?;
        if !dossier.verification.intact {
            return Err(AdmissionError::NotReviewable {
                package: digest.to_string(),
                reason: format!(
                    "stored bytes do not verify: {}",
                    dossier.verification.failures.join("; ")
                ),
            });
        }
        if dossier.inspection.is_fatal() {
            return Err(AdmissionError::NotReviewable {
                package: digest.to_string(),
                reason: dossier
                    .inspection
                    .fatal
                    .iter()
                    .map(|fatal| fatal.message.as_str())
                    .collect::<Vec<_>>()
                    .join("; "),
            });
        }
        members.push(Member {
            package_digest: digest.to_string(),
            dossier_digest: dossier.portable_digest().to_string(),
            review_depth: depth.name().to_owned(),
        });
    }

    let previous = current(store)?;
    let payload = GenerationPayload::new(
        &trust.trust_domain,
        previous
            .as_ref()
            .map_or(1, |record| record.payload.sequence + 1),
        previous.as_ref().map(|record| record.digest().to_string()),
        &policy.digest().to_string(),
        members,
        request.view_roots.clone(),
    );

    let signature = request
        .signer
        .sign(ADMISSION_NAMESPACE, &payload.canonical_bytes())?;
    sshsig::verify(
        &signature,
        ADMISSION_NAMESPACE,
        &payload.canonical_bytes(),
        &signing_key.public_key,
        signing_key.sk_policy,
    )?;

    let record = GenerationRecord {
        schema: RECORD_SCHEMA.to_owned(),
        generation: payload.digest().to_string(),
        payload,
        signature,
        state: GenerationState::PendingWitness,
        witness: None,
        admitted_at_ms: request.admitted_at_ms,
        invalid_reason: None,
    };
    write_record(store, &record)?;
    trust.approved_admissions.insert(record.generation.clone());
    locked.write(&trust)?;
    Ok(record)
}

/// Checks a stored record against the chain and the enrolled keys.
///
/// # Errors
/// Rejects schema, digest, trust-domain, signer, or predecessor/sequence inconsistencies; propagates trust, signature, and stored-chain read errors.
pub fn verify_record(
    store: &Store,
    record: &GenerationRecord,
    trust: &TrustStore,
) -> Result<(), AdmissionError> {
    if record.payload.schema != GENERATION_SCHEMA {
        return Err(AdmissionError::Chain(format!(
            "unsupported payload schema '{}'",
            record.payload.schema
        )));
    }
    let actual = record.payload.digest();
    if record.generation != actual.to_string() {
        return Err(AdmissionError::DigestMismatch {
            claimed: record.generation.clone(),
            actual: actual.to_string(),
        });
    }
    if record.payload.trust_domain != trust.trust_domain {
        return Err(AdmissionError::Chain(format!(
            "trust domain '{}' is not '{}'",
            record.payload.trust_domain, trust.trust_domain
        )));
    }
    if record.payload.signer_role != Role::Primary.name() {
        return Err(AdmissionError::Chain(format!(
            "signer role '{}' may not admit a Generation",
            record.payload.signer_role
        )));
    }
    if record.payload.member_root != record.payload.recomputed_member_root().to_string() {
        return Err(AdmissionError::RootMismatch);
    }
    verify_chain_position(store, record)?;

    let payload_bytes = record.payload.canonical_bytes();
    let mut last = None;
    for key in trust.verification_keys(Role::Primary, &record.generation) {
        match sshsig::verify(
            &record.signature,
            ADMISSION_NAMESPACE,
            &payload_bytes,
            &key.public_key,
            key.sk_policy,
        ) {
            Ok(_) => return Ok(()),
            Err(error) => last = Some(error),
        }
    }
    Err(AdmissionError::Signature(last.unwrap_or(
        sshsig::SignatureError::KeyMismatch {
            found: "no enrolled primary key".to_owned(),
        },
    )))
}

fn verify_chain_position(store: &Store, record: &GenerationRecord) -> Result<(), AdmissionError> {
    match (&record.payload.predecessor, record.payload.sequence) {
        (None, 1) => Ok(()),
        (None, sequence) => Err(AdmissionError::Chain(format!(
            "sequence {sequence} declares no predecessor"
        ))),
        (Some(_), 1) => Err(AdmissionError::Chain(
            "the first Generation cannot have a predecessor".to_owned(),
        )),
        (Some(predecessor), sequence) => {
            let digest = Digest::parse(predecessor)?;
            let earlier = load(store, &digest)?;
            if earlier.payload.sequence + 1 != sequence {
                return Err(AdmissionError::Chain(format!(
                    "sequence {sequence} does not follow {}",
                    earlier.payload.sequence
                )));
            }
            Ok(())
        }
    }
}

/// Publishes and confirms a Generation's exact bytes on the witness remote.
///
/// # Errors
/// Returns verification or remote transport failures; refuses missing or byte-different witness data and propagates record-write errors.
pub fn witness(
    store: &Store,
    digest: &Digest,
    witness: &dyn Witness,
    confirmed_at_ms: u64,
) -> Result<GenerationRecord, AdmissionError> {
    let mut record = load_verified(store, digest)?;
    let bytes = record.witness_bytes();

    let held = if let Some(held) = witness.fetch(digest)? {
        Some(held)
    } else {
        witness.publish(digest, &bytes)?;
        witness.fetch(digest)?
    };
    let Some((found, mut evidence)) = held else {
        return Err(AdmissionError::WitnessMismatch {
            digest: digest.to_string(),
        });
    };
    if found != bytes {
        return Err(AdmissionError::WitnessMismatch {
            digest: digest.to_string(),
        });
    }
    evidence.confirmed_at_ms = confirmed_at_ms;
    record.witness = Some(evidence);
    write_record(store, &record)?;
    Ok(record)
}

/// Makes a witnessed Generation the supply in force.
///
/// # Errors
/// Rejects an untrusted running release, unwitnessed or invalid Generation, or rollback; propagates trust, chain-read, and persistence errors.
pub fn activate(
    store: &Store,
    digest: &Digest,
    _activated_at_ms: u64,
) -> Result<GenerationRecord, AdmissionError> {
    let identity = crate::release::running_identity();
    if store.is_trusted() && !identity.verified {
        return Err(AdmissionError::NotTrustedRelease {
            reason: identity
                .failure_code
                .unwrap_or_else(|| "unverified".to_owned()),
        });
    }
    let mut record = load_verified(store, digest)?;
    if record.witness.is_none() {
        return Err(AdmissionError::NotWitnessed {
            digest: digest.to_string(),
        });
    }
    if let Some(previous) = current(store)? {
        if record.payload.sequence <= previous.payload.sequence {
            return Err(AdmissionError::Rollback {
                attempted: digest.to_string(),
                current: previous.payload.sequence,
            });
        }
        let mut superseded = previous;
        superseded.state = GenerationState::Superseded;
        write_record(store, &superseded)?;
    }
    record.state = GenerationState::Current;
    write_record(store, &record)?;
    record_pin(store, &record)?;
    Ok(record)
}

/// Returns the Generation in force, when there is one.
///
/// # Errors
/// Returns record-listing or decoding errors. No current Generation is `Ok(None)`.
pub fn current(store: &Store) -> Result<Option<GenerationRecord>, AdmissionError> {
    Ok(list(store)?
        .into_iter()
        .find(|record| record.state == GenerationState::Current))
}

/// Reads a stored record without verifying it.
///
/// # Errors
/// Returns `Unknown` for an absent record, otherwise I/O or malformed-record errors.
pub fn load(store: &Store, digest: &Digest) -> Result<GenerationRecord, AdmissionError> {
    let path = record_path(store, digest);
    let bytes = fs::read(&path).map_err(|source| match source.kind() {
        io::ErrorKind::NotFound => AdmissionError::Unknown(digest.to_string()),
        _ => AdmissionError::Io {
            path: path.display().to_string(),
            source,
        },
    })?;
    serde_json::from_slice(&bytes).map_err(|error| AdmissionError::Malformed(error.to_string()))
}

/// Reads a stored record and verifies it against trust and the chain.
///
/// # Errors
/// Returns record/trust loading errors or any failure from [`verify_record`].
pub fn load_verified(store: &Store, digest: &Digest) -> Result<GenerationRecord, AdmissionError> {
    let record = load(store, digest)?;
    let trust = TrustStore::load(store)?.ok_or(TrustError::NotBootstrapped)?;
    verify_record(store, &record, &trust)?;
    Ok(record)
}

/// Reads every stored record, ordered by sequence.
///
/// # Errors
/// Returns directory/record read or JSON errors. An absent Generations directory is empty.
pub fn list(store: &Store) -> Result<Vec<GenerationRecord>, AdmissionError> {
    let directory = store.root().join("generations");
    let listing = match fs::read_dir(&directory) {
        Ok(listing) => listing,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(AdmissionError::Io {
                path: directory.display().to_string(),
                source,
            });
        }
    };
    let mut records = Vec::new();
    for entry in listing {
        let entry = entry.map_err(|source| AdmissionError::Io {
            path: directory.display().to_string(),
            source,
        })?;
        let bytes = fs::read(entry.path()).map_err(|source| AdmissionError::Io {
            path: entry.path().display().to_string(),
            source,
        })?;
        let record: GenerationRecord = serde_json::from_slice(&bytes)
            .map_err(|error| AdmissionError::Malformed(error.to_string()))?;
        records.push(record);
    }
    records.sort_by_key(|record| record.payload.sequence);
    Ok(records)
}

/// Returns the path a Generation record is stored at.
#[must_use]
pub fn record_path(store: &Store, digest: &Digest) -> PathBuf {
    store
        .root()
        .join("generations")
        .join(format!("{}.json", digest.directory_name()))
}

/// Reports the chain's state and the one safe next step.
///
/// # Errors
/// Returns Generation-listing or quarantine-loading errors.
pub fn status(store: &Store) -> Result<GenerationStatus, AdmissionError> {
    let records = list(store)?;
    let quarantine = quarantine::load(store)?;
    let in_force = records
        .iter()
        .find(|record| record.state == GenerationState::Current);
    let pending = records
        .iter()
        .filter(|record| record.state == GenerationState::PendingWitness)
        .collect::<Vec<_>>();

    let (effective_members, excluded_members) = match in_force {
        Some(record) => {
            quarantine::partition(quarantine.as_ref(), &record.payload.member_digests())
        }
        None => (Vec::new(), Vec::new()),
    };
    let state = in_force.map(|record| {
        if excluded_members.is_empty() {
            record.state
        } else {
            GenerationState::Quarantined
        }
    });

    let next_action = if records.is_empty() {
        NextAction {
            id: "admit_first_generation".to_owned(),
            detail: "No Skill Generation exists; admit one to give a verified Session any supply."
                .to_owned(),
        }
    } else if let Some(record) = pending.iter().find(|record| record.witness.is_some()) {
        NextAction {
            id: "activate_witnessed_generation".to_owned(),
            detail: format!(
                "Generation {} is witnessed; activate it to put it in force.",
                record.generation
            ),
        }
    } else if let Some(record) = pending.first() {
        NextAction {
            id: "witness_pending_generation".to_owned(),
            detail: format!(
                "Generation {} is signed but unwitnessed; publish it before it can govern anything.",
                record.generation
            ),
        }
    } else if !excluded_members.is_empty() {
        NextAction {
            id: "admit_after_quarantine".to_owned(),
            detail: format!(
                "{} member(s) are quarantined; restoring them requires a newly admitted Generation.",
                excluded_members.len()
            ),
        }
    } else {
        NextAction {
            id: "none".to_owned(),
            detail: "The current Generation is witnessed and in force.".to_owned(),
        }
    };

    Ok(GenerationStatus {
        schema: STATUS_SCHEMA.to_owned(),
        state,
        generation: in_force.map(|record| record.generation.clone()),
        sequence: in_force.map(|record| record.payload.sequence),
        predecessor: in_force.and_then(|record| record.payload.predecessor.clone()),
        signer_role: in_force.map(|record| record.payload.signer_role.clone()),
        witness: in_force.and_then(|record| record.witness.clone()),
        effective_members,
        excluded_members,
        pending: pending
            .iter()
            .map(|record| record.generation.clone())
            .collect(),
        failure: in_force.and_then(|record| record.invalid_reason.clone()),
        next_action,
    })
}

fn write_record(store: &Store, record: &GenerationRecord) -> Result<(), AdmissionError> {
    let path = record_path(store, &record.digest());
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| AdmissionError::Io {
            path: parent.display().to_string(),
            source,
        })?;
    }
    let bytes =
        serde_json::to_vec(record).map_err(|error| AdmissionError::Malformed(error.to_string()))?;
    fs::write(&path, bytes).map_err(|source| AdmissionError::Io {
        path: path.display().to_string(),
        source,
    })
}

/// Appends the activated pin to Supply lineage.
///
/// Lineage records which policy and set root were in force and when. It has no
/// approval authority: reading it can tell an operator what changed, never
/// that a change was allowed.
fn record_pin(store: &Store, record: &GenerationRecord) -> Result<(), AdmissionError> {
    #[derive(Serialize)]
    struct Pin<'a> {
        sequence: u64,
        generation: &'a str,
        policy_digest: &'a str,
        member_root: &'a str,
        activated_at_ms: u64,
    }

    let path = store.root().join("pins.jsonl");
    let pin = Pin {
        sequence: record.payload.sequence,
        generation: &record.generation,
        policy_digest: &record.payload.policy_digest,
        member_root: &record.payload.member_root,
        activated_at_ms: record.admitted_at_ms,
    };
    let mut line = serde_json::to_string(&pin)
        .map_err(|error| AdmissionError::Malformed(error.to_string()))?;
    line.push('\n');
    let mut existing = match fs::read_to_string(&path) {
        Ok(existing) => existing,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(source) => {
            return Err(AdmissionError::Io {
                path: path.display().to_string(),
                source,
            });
        }
    };
    existing.push_str(&line);
    fs::write(&path, existing).map_err(|source| AdmissionError::Io {
        path: path.display().to_string(),
        source,
    })
}
