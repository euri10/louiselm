//! The Skill Admission ceremony and the Generation lifecycle.
//!
//! Admission separates preparation from activation so that failures before
//! commit preserve the prior Generation, with recovery required after an
//! interrupted write:
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
//!
//! Activation holds the trust lock and journals both Generation records and
//! Supply lineage before replacing them. Reads recover an interrupted activation
//! under that same lock. Ordinary write failures restore the old supply; failure
//! to confirm the final commit is reported separately as uncertain durability.

mod linked;
mod transaction;
mod verification;
mod withdrawals;
pub use linked::{admit_linked, linked_generation_members, verify_linked};
pub use verification::verify_record;
pub(crate) use withdrawals::supply_withdrawal_alerts;

use std::{fs, io, path::PathBuf};

use serde::Serialize;
use thiserror::Error;

use crate::{
    canonical::{Digest, DigestError},
    dossier::{Dossier, DossierError, DossierRequest, NextAction, ReviewDepth},
    endorsement::focus::FocusContext,
    generation::{
        GENERATION_SCHEMA, GenerationPayload, GenerationRecord, GenerationState, HISTORICAL_SCHEMA,
        Member, RECORD_SCHEMA,
    },
    inspect::PackageKind,
    policy::Policy,
    quarantine::{self, QuarantineError},
    signer::{Signer, SignerError},
    sshsig::{self, ADMISSION_NAMESPACE},
    store::{Store, StoreError},
    trust::{TrustError, TrustStore},
    witness::{Witness, WitnessError, WitnessEvidence},
};

/// The status schema this build reports.
pub const STATUS_SCHEMA: &str = "louiselm.skills.generation-status/1";

/// One package to admit, and the Instruction views it enters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdmissionMember {
    /// The package being admitted.
    pub package: Digest,
    /// The review depth the reviewer claims for it.
    pub depth: ReviewDepth,
    /// Agents whose Instruction view it enters. Naming none is a refusal.
    pub agents: Vec<String>,
}

/// What to admit.
pub struct AdmissionRequest<'a> {
    /// Packages to admit, each with its review depth and Agent scope.
    pub members: Vec<AdmissionMember>,
    /// What will authorize the payload.
    pub signer: &'a dyn Signer,
    /// When the ceremony ran.
    pub admitted_at_ms: u64,
    /// Every Agent configured on this host. Required to admit an Operator
    /// profile, which must reach all of them; skills do not need it.
    pub configured_agents: Option<Vec<String>>,
}

/// Which Operator profile a Generation carries, and which configured Agents
/// it does not reach under the caller's scope or file-delivery check.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProfileDelivery {
    /// The profile member's package digest, when the Generation has one.
    pub profile: Option<String>,
    /// Configured Agents not reached, sorted. Admission checks signed scope;
    /// ordinary delivery additionally checks the declared native files.
    pub undelivered: Vec<String>,
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
    /// Activation failed and its durable journal could not yet be rolled back.
    #[error("activation failed: {failure}; recovery is required before reading supply: {source}")]
    RecoveryRequired {
        /// The original activation failure.
        failure: Box<AdmissionError>,
        /// Why rollback could not complete.
        source: Box<AdmissionError>,
    },
    /// All activation files were published but commit durability is unconfirmed.
    #[error(
        "generation {generation} is published but commit durability is uncertain; retry activation: {source}"
    )]
    CommitUncertain {
        /// The exact Generation whose activation should be retried.
        generation: String,
        /// The failure after removing the rollback journal.
        source: Box<AdmissionError>,
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
    /// Endorsement focus could not be computed for a member.
    #[error(transparent)]
    Focus(#[from] crate::endorsement::focus::FocusError),
    /// A Generation may carry at most one Operator profile.
    #[error("a Generation holds at most one Operator profile")]
    ProfileTwice,
    /// An Operator profile was offered without the configured Agent list.
    #[error(
        "Operator profile {package} needs the configured Agent list to prove it reaches every Agent"
    )]
    ProfileScopeUnknown {
        /// The profile package.
        package: String,
    },
    /// An Operator profile does not reach every configured Agent.
    #[error("Operator profile {package} does not reach configured Agents: {}", missing.join(", "))]
    ProfileScope {
        /// The profile package.
        package: String,
        /// Configured Agents missing from its scope.
        missing: Vec<String>,
    },
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
    admit_inner(store, policy, request, None, None)
}

/// Admits a freshly reviewed `/3` successor to the exact current `/2` history.
///
/// This is an explicit new Admission, not conversion of an old signature. It
/// preserves the trust domain and historical bytes, and returns a candidate
/// requiring the ordinary witness and activation steps. Legacy records remain
/// unusable as active supply in this build.
///
/// # Errors
/// Refuses an absent, changed, unwitnessed or unverifiable legacy predecessor,
/// and returns the same review, signing and persistence errors as [`admit`].
pub fn admit_transition(
    store: &Store,
    policy: &Policy,
    request: &AdmissionRequest<'_>,
    expected_predecessor: &Digest,
) -> Result<GenerationRecord, AdmissionError> {
    admit_inner(store, policy, request, None, Some(expected_predecessor))
}

fn admit_inner(
    store: &Store,
    policy: &Policy,
    request: &AdmissionRequest<'_>,
    operation: Option<&str>,
    transition: Option<&Digest>,
) -> Result<GenerationRecord, AdmissionError> {
    let locked = transaction::lock(store)?;
    let mut trust = locked.load()?.ok_or(TrustError::NotBootstrapped)?;
    let signing_key = trust.admission_key()?.clone();
    let previous = current_unlocked(store)?;
    verification::admission_predecessor(store, previous.as_ref(), &trust, transition)?;
    let sequence = match &previous {
        None => 1,
        Some(record) => record
            .payload
            .sequence
            .checked_add(1)
            .ok_or_else(|| AdmissionError::Chain("Generation sequence exhausted".to_owned()))?,
    };

    // Focus is recomputed here from local evidence, exactly as the Dossier
    // view shows it; what it relied on is signed with the member.
    let admitted = admitted_unlocked(store)?;
    let focus = FocusContext::load_with(store, Some(&trust), &admitted, request.admitted_at_ms)?;
    let mut members = Vec::with_capacity(request.members.len());
    let mut has_profile = false;
    for AdmissionMember {
        package: digest,
        depth,
        agents,
    } in &request.members
    {
        if agents.is_empty() {
            return Err(AdmissionError::NotReviewable {
                package: digest.to_string(),
                reason: "no Agent was named, so the package would enter no Instruction view"
                    .to_owned(),
            });
        }
        let dossier = Dossier::build(
            store,
            policy,
            &DossierRequest::new(digest).with_review_depth(*depth),
        )?;
        refuse_unreviewable(digest, &dossier)?;
        if dossier.inspection.package_kind == PackageKind::OperatorProfile {
            check_profile(
                digest,
                agents,
                request.configured_agents.as_deref(),
                has_profile,
            )?;
            has_profile = true;
        }
        members.push(Member {
            package_digest: digest.to_string(),
            dossier_digest: dossier.portable_digest().to_string(),
            review_depth: depth.name().to_owned(),
            agents: agents.clone(),
            relied_on: focus.evaluate(store, policy, digest, None)?.relied_on,
        });
    }

    let mut payload = GenerationPayload::new(
        &trust.trust_domain,
        sequence,
        previous.as_ref().map(|record| record.digest().to_string()),
        &policy.digest().to_string(),
        members,
    );
    payload.approval_operation = operation.map(str::to_owned);
    if let Some(record) = linked::recover(store, &payload, &locked, &mut trust)? {
        return Ok(record);
    }

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
    let record = load_verified(store, digest)?;
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
    // Network I/O holds no trust lock. Re-read after acquiring it so a concurrent
    // activation cannot be overwritten by the pre-network lifecycle snapshot.
    let _locked = transaction::lock(store)?;
    let mut record = load_verified_unlocked(store, digest)?;
    if record.witness_bytes() != bytes {
        return Err(AdmissionError::WitnessMismatch {
            digest: digest.to_string(),
        });
    }
    record.witness = Some(evidence);
    write_record(store, &record)?;
    Ok(record)
}

/// Makes a witnessed Generation the supply in force.
///
/// Readers and mutations share the trust lock and recover an interrupted
/// activation before proceeding. A new pin records the supplied `activated_at_ms`.
/// Retrying the current digest confirms commit durability without appending
/// another pin or changing its original activation timestamp.
///
/// # Errors
/// Rejects an untrusted release, unwitnessed/invalid Generation, rollback, or
/// busy store. Pre-commit failures restore the previous supply; failed rollback
/// returns [`AdmissionError::RecoveryRequired`] and blocks reads until repaired.
/// [`AdmissionError::CommitUncertain`] means the complete new state is visible
/// but its commit durability is unknown; retry the same digest to settle it.
pub fn activate(
    store: &Store,
    digest: &Digest,
    activated_at_ms: u64,
) -> Result<GenerationRecord, AdmissionError> {
    let identity = crate::release::running_identity();
    if store.is_trusted() && !identity.verified {
        return Err(AdmissionError::NotTrustedRelease {
            reason: identity
                .failure_code
                .unwrap_or_else(|| "unverified".to_owned()),
        });
    }
    let _locked = transaction::lock(store)?;
    let mut record = load_verified_unlocked(store, digest)?;
    if record.witness.is_none() {
        return Err(AdmissionError::NotWitnessed {
            digest: digest.to_string(),
        });
    }
    let previous = current_unlocked(store)?;
    if let Some(previous) = &previous {
        if previous.digest() == *digest {
            transaction::confirm(store, digest)?;
            return Ok(record);
        }
        if record.payload.sequence <= previous.payload.sequence {
            return Err(AdmissionError::Rollback {
                attempted: digest.to_string(),
                current: previous.payload.sequence,
            });
        }
        if record.payload.predecessor.as_deref() != Some(previous.generation.as_str()) {
            return Err(AdmissionError::Chain(
                "candidate does not follow the current Generation".to_owned(),
            ));
        }
    } else {
        verification::require_no_legacy_history(store)?;
        if record.payload.predecessor.is_some() {
            return Err(AdmissionError::Chain(
                "candidate has no current predecessor".to_owned(),
            ));
        }
    }
    record.state = GenerationState::Current;
    transaction::activate(store, previous.as_ref(), &record, activated_at_ms)?;
    Ok(record)
}

/// Reads the locally current record, without granting supply authority.
/// Historical `/2` records are inspectable here; consumers must verify them.
///
/// # Errors
/// Returns lock, interrupted-activation recovery, listing, or decoding errors.
/// No current Generation is `Ok(None)`.
pub fn current(store: &Store) -> Result<Option<GenerationRecord>, AdmissionError> {
    let _locked = transaction::lock(store)?;
    current_unlocked(store)
}

// Materialization keeps this guard until publication completes, preventing
// activation or key rotation from changing the authority it is reading.
pub(crate) fn locked_current(
    store: &Store,
) -> Result<
    (
        crate::trust::persistence::LockedTrust,
        Option<GenerationRecord>,
    ),
    AdmissionError,
> {
    let locked = transaction::lock(store)?;
    let current = current_unlocked(store)?;
    if let Some(record) = &current {
        let trust = locked.load()?.ok_or(TrustError::NotBootstrapped)?;
        verify_record(store, record, &trust)?;
    }
    Ok((locked, current))
}

/// Refuses a member whose stored bytes do not verify or whose Inspection is fatal.
fn refuse_unreviewable(digest: &Digest, dossier: &Dossier) -> Result<(), AdmissionError> {
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
    Ok(())
}

/// One Operator profile per Generation, scoped to every configured Agent.
fn check_profile(
    digest: &Digest,
    agents: &[String],
    configured: Option<&[String]>,
    already_has_profile: bool,
) -> Result<(), AdmissionError> {
    if already_has_profile {
        return Err(AdmissionError::ProfileTwice);
    }
    let configured = configured.ok_or_else(|| AdmissionError::ProfileScopeUnknown {
        package: digest.to_string(),
    })?;
    let mut missing: Vec<String> = configured
        .iter()
        .filter(|agent| !agents.contains(agent))
        .cloned()
        .collect();
    missing.sort();
    missing.dedup();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(AdmissionError::ProfileScope {
            package: digest.to_string(),
            missing,
        })
    }
}

/// Reports which member of `generation` is the Operator profile and which of
/// `configured` Agents it does not reach.
///
/// # Errors
/// Returns store errors opening a member package.
pub fn profile_delivery(
    store: &Store,
    generation: &GenerationRecord,
    configured: &[String],
) -> Result<ProfileDelivery, AdmissionError> {
    let policy = Policy::embedded();
    for member in &generation.payload.members {
        let digest = Digest::parse(&member.package_digest)?;
        let package = store.open_package(&digest, &policy)?;
        if PackageKind::of(&package.manifest) == PackageKind::OperatorProfile {
            let mut undelivered: Vec<String> = configured
                .iter()
                .filter(|agent| !member.agents.contains(agent))
                .cloned()
                .collect();
            undelivered.sort();
            undelivered.dedup();
            return Ok(ProfileDelivery {
                profile: Some(member.package_digest.clone()),
                undelivered,
            });
        }
    }
    let mut undelivered = configured.to_vec();
    undelivered.sort();
    undelivered.dedup();
    Ok(ProfileDelivery {
        profile: None,
        undelivered,
    })
}

/// Members of the Generation in force that no quarantine excludes, read
/// under the Admission lock the caller already holds.
fn admitted_unlocked(store: &Store) -> Result<Vec<Digest>, AdmissionError> {
    let Some(record) = current_unlocked(store)? else {
        return Ok(Vec::new());
    };
    // Match the Dossier/status view: retained history is not current supply
    // and does not earn an admitted focus base for the new review.
    if record.payload.schema == HISTORICAL_SCHEMA {
        return Ok(Vec::new());
    }
    let quarantine = quarantine::load(store)?;
    let (effective, _) = quarantine::partition(
        quarantine.as_ref(),
        &record.generation,
        &record.payload.member_digests(),
    );
    effective
        .iter()
        .map(|member| Digest::parse(member).map_err(AdmissionError::from))
        .collect()
}

fn current_unlocked(store: &Store) -> Result<Option<GenerationRecord>, AdmissionError> {
    Ok(unique_current(&list_unlocked(store)?)?.cloned())
}

fn unique_current(
    records: &[GenerationRecord],
) -> Result<Option<&GenerationRecord>, AdmissionError> {
    let mut current = records
        .iter()
        .filter(|record| record.state == GenerationState::Current);
    let first = current.next();
    if current.next().is_some() {
        return Err(AdmissionError::Chain(
            "multiple current Generations".to_owned(),
        ));
    }
    Ok(first)
}

/// Reads a stored record without verifying it.
///
/// # Errors
/// Returns `Unknown` for an absent record, otherwise lock, recovery, I/O, or
/// malformed-record errors. Interrupted activation is recovered before reading.
pub fn load(store: &Store, digest: &Digest) -> Result<GenerationRecord, AdmissionError> {
    let _locked = transaction::lock(store)?;
    load_unlocked(store, digest)
}

fn load_unlocked(store: &Store, digest: &Digest) -> Result<GenerationRecord, AdmissionError> {
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
    let _locked = transaction::lock(store)?;
    load_verified_unlocked(store, digest)
}

fn load_verified_unlocked(
    store: &Store,
    digest: &Digest,
) -> Result<GenerationRecord, AdmissionError> {
    let record = load_unlocked(store, digest)?;
    let trust = TrustStore::load(store)?.ok_or(TrustError::NotBootstrapped)?;
    verify_record(store, &record, &trust)?;
    Ok(record)
}

/// Reads every stored record, ordered by sequence.
///
/// # Errors
/// Returns lock, interrupted-activation recovery, directory/record read, or JSON
/// errors. An absent Generations directory is empty.
pub fn list(store: &Store) -> Result<Vec<GenerationRecord>, AdmissionError> {
    let _locked = transaction::lock(store)?;
    list_unlocked(store)
}

fn list_unlocked(store: &Store) -> Result<Vec<GenerationRecord>, AdmissionError> {
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
        // An interrupted atomic write can leave only an inert staging file.
        if entry
            .file_name()
            .to_string_lossy()
            .starts_with(".admission-")
        {
            continue;
        }
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
    let in_force = unique_current(&records)?;
    let historical = in_force.filter(|record| record.payload.schema == HISTORICAL_SCHEMA);
    let pending = records
        .iter()
        .filter(|record| {
            record.state == GenerationState::PendingWitness
                && record.payload.schema == GENERATION_SCHEMA
        })
        .collect::<Vec<_>>();

    let (effective_members, excluded_members) = match in_force {
        Some(record) if historical.is_none() => quarantine::partition(
            quarantine.as_ref(),
            &record.generation,
            &record.payload.member_digests(),
        ),
        _ => (Vec::new(), Vec::new()),
    };
    let state = in_force.map(|record| {
        if historical.is_some() {
            GenerationState::Invalid
        } else if excluded_members.is_empty() {
            record.state
        } else {
            GenerationState::Quarantined
        }
    });

    let next_action = status_next_action(&records, in_force, &pending, excluded_members.len());
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
        failure: historical
            .map(|_| {
                "generation/2 is retained history, not active supply in this release".to_owned()
            })
            .or_else(|| in_force.and_then(|record| record.invalid_reason.clone())),
        next_action,
    })
}

// Guidance is derived from the same snapshot as the status; it is not authority
// to skip the verification at the selected operation's boundary.
fn status_next_action(
    records: &[GenerationRecord],
    in_force: Option<&GenerationRecord>,
    pending: &[&GenerationRecord],
    excluded: usize,
) -> NextAction {
    if records.is_empty() {
        NextAction {
            id: "admit_first_generation".to_owned(),
            detail: "No Skill Generation exists; admit one to give a verified Session any supply."
                .to_owned(),
        }
    } else if in_force.is_none()
        && records
            .iter()
            .any(|record| record.payload.schema == HISTORICAL_SCHEMA)
    {
        NextAction {
            id: "inspect_generation_history".to_owned(),
            detail: "Historical generation/2 records exist without a current predecessor; inspect retained history before Admission or activation.".to_owned(),
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
    } else if let Some(record) =
        in_force.filter(|record| record.payload.schema == HISTORICAL_SCHEMA)
    {
        NextAction {
            id: "admit_history_successor".to_owned(),
            detail: format!(
                "Historical generation/2 is not active supply in this release; review members and admit a new Generation with --transition-from {}, then witness and activate it.",
                record.generation
            ),
        }
    } else if excluded != 0 {
        NextAction {
            id: "admit_after_quarantine".to_owned(),
            detail: format!(
                "{excluded} member(s) are quarantined; restoring them requires a newly admitted Generation."
            ),
        }
    } else {
        NextAction {
            id: "none".to_owned(),
            detail: "The current Generation is witnessed and in force.".to_owned(),
        }
    }
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
    transaction::write_atomic(&path, &bytes)
}

/// Serializes the activated pin for the journaled Supply lineage update.
///
/// Lineage records which policy and set root were in force and when. It has no
/// approval authority: reading it can tell an operator what changed, never
/// that a change was allowed.
fn pin_line(record: &GenerationRecord, activated_at_ms: u64) -> Result<String, AdmissionError> {
    #[derive(Serialize)]
    struct Pin<'a> {
        sequence: u64,
        generation: &'a str,
        policy_digest: &'a str,
        member_root: &'a str,
        activated_at_ms: u64,
    }

    let pin = Pin {
        sequence: record.payload.sequence,
        generation: &record.generation,
        policy_digest: &record.payload.policy_digest,
        member_root: &record.payload.member_root,
        activated_at_ms,
    };
    let mut line = serde_json::to_string(&pin)
        .map_err(|error| AdmissionError::Malformed(error.to_string()))?;
    line.push('\n');
    Ok(line)
}
