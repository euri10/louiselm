//! Signed operation linkage and non-mutating evidence verification.

use super::{
    AdmissionError, AdmissionRequest, GenerationRecord, Store, admit_inner, verify_record,
};
use crate::{
    Digest, Policy,
    trust::{TrustError, persistence::LockedTrust},
};
use std::{fs, path::Path};

/// Performs or recovers one broker-linked Admission without repeating its signature.
/// The caller must authenticate and match the broker request before invoking this.
/// # Errors
/// Refuses malformed operation IDs, changed requests, invalid evidence or persistence failure.
pub fn admit_linked(
    store: &Store,
    policy: &Policy,
    request: &AdmissionRequest<'_>,
    operation: &str,
) -> Result<GenerationRecord, AdmissionError> {
    if !crate::broker::attention::canonical_uuid(operation) {
        return Err(AdmissionError::Chain("invalid approval operation".into()));
    }
    admit_inner(store, policy, request, Some(operation))
}

/// Independently verifies exact signed Admission and confirms its durability.
/// Opens existing files read-only under a shared trust lock. Never creates files,
/// repairs activation, signs, witnesses, activates or changes an Instruction view.
/// A signed `PendingWitness` record is sufficient; this is not a usability claim.
/// # Errors
/// Refuses unsettled recovery, unknown trust, mismatched scope/bytes, invalid
/// signatures, missing approval registration or unconfirmed persistence.
pub fn verify_linked(
    store: &Store,
    policy: &Policy,
    domain: &str,
    operation: &str,
    packages: &[String],
    agents: &[String],
) -> Result<Option<String>, AdmissionError> {
    if !crate::broker::attention::canonical_uuid(operation)
        || !(crate::skill_request::SkillRequest {
            request_id: "admission".into(),
            subject: crate::skill_request::SkillSubject::Session,
            packages: packages.to_vec(),
            agents: agents.to_vec(),
        })
        .valid()
    {
        return Err(AdmissionError::Chain(
            "invalid linked Admission scope".into(),
        ));
    }
    let locked = LockedTrust::read_only(store)?;
    if store
        .root()
        .join("activation.pending.json")
        .try_exists()
        .map_err(|source| io_error(store.root(), source))?
    {
        return Err(AdmissionError::Chain("activation recovery required".into()));
    }
    let trust = locked.load()?.ok_or(TrustError::NotBootstrapped)?;
    if trust.trust_domain != domain {
        return Err(AdmissionError::Chain(
            "Admission trust domain mismatch".into(),
        ));
    }
    let Some(record) = find(store, operation)? else {
        return Ok(None);
    };
    verify_record(store, &record, &trust)?;
    if record.schema != crate::generation::RECORD_SCHEMA
        || !trust.approved_admissions.contains(&record.generation)
        || record.payload.policy_digest != policy.digest().to_string()
        || record.payload.member_digests() != packages
        || record
            .payload
            .members
            .iter()
            .any(|member| member.agents != agents)
    {
        return Err(AdmissionError::Chain(
            "Admission approval or scope mismatch".into(),
        ));
    }
    for package in packages {
        let report = store.verify(&Digest::parse(package)?, policy)?;
        if !report.is_intact() {
            return Err(AdmissionError::Chain(
                "Admission package bytes changed".into(),
            ));
        }
    }
    confirm_record(store, &record)?;
    locked.confirm()?;
    Ok(Some(record.generation))
}

/// Reads the verified package members of one stored Generation without writing.
///
/// Same read-only shared trust lock as [`verify_linked`]: never creates files,
/// repairs activation or changes an Instruction view. The broker uses it to
/// decide which live Sessions a skill quarantine reaches.
/// # Errors
/// Refuses unsettled recovery, unknown trust or domain, an absent, malformed or
/// unverifiable record, or unreadable storage.
pub fn linked_generation_members(
    store: &Store,
    domain: &str,
    generation: &Digest,
) -> Result<Vec<String>, AdmissionError> {
    let locked = LockedTrust::read_only(store)?;
    if store
        .root()
        .join("activation.pending.json")
        .try_exists()
        .map_err(|source| io_error(store.root(), source))?
    {
        return Err(AdmissionError::Chain("activation recovery required".into()));
    }
    let trust = locked.load()?.ok_or(TrustError::NotBootstrapped)?;
    if trust.trust_domain != domain {
        return Err(AdmissionError::Chain(
            "Admission trust domain mismatch".into(),
        ));
    }
    let record = super::load_unlocked(store, generation)?;
    verify_record(store, &record, &trust)?;
    locked.confirm()?;
    Ok(record.payload.member_digests())
}

pub(super) fn find(
    store: &Store,
    operation: &str,
) -> Result<Option<GenerationRecord>, AdmissionError> {
    let directory = store.root().join("generations");
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(io_error(&directory, source)),
    };
    let mut found = None;
    for (index, entry) in entries.enumerate() {
        if index >= 4096 {
            return Err(AdmissionError::Chain(
                "Generation scan bound exceeded".into(),
            ));
        }
        let path = entry.map_err(|source| io_error(&directory, source))?.path();
        if path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        let bytes = fs::read(&path).map_err(|source| io_error(&path, source))?;
        let record: GenerationRecord = serde_json::from_slice(&bytes)
            .map_err(|error| AdmissionError::Malformed(error.to_string()))?;
        if record.payload.approval_operation.as_deref() == Some(operation) {
            if found.is_some()
                || path != super::record_path(store, &Digest::parse(&record.generation)?)
            {
                return Err(AdmissionError::Chain("ambiguous linked Admission".into()));
            }
            found = Some(record);
        }
    }
    Ok(found)
}

pub(super) fn recover(
    store: &Store,
    payload: &crate::generation::GenerationPayload,
    locked: &LockedTrust,
    trust: &mut crate::trust::TrustStore,
) -> Result<Option<GenerationRecord>, AdmissionError> {
    let Some(operation) = &payload.approval_operation else {
        return Ok(None);
    };
    let Some(record) = find(store, operation)? else {
        return Ok(None);
    };
    verify_record(store, &record, trust)?;
    if record.payload.members != payload.members
        || record.payload.policy_digest != payload.policy_digest
    {
        return Err(AdmissionError::Chain(
            "linked Admission request changed".into(),
        ));
    }
    // Only the skills tool repairs registration; the broker merely confirms
    // durability of complete evidence through its read-only descriptors.
    confirm_record(store, &record)?;
    trust.approved_admissions.insert(record.generation.clone());
    locked.write(trust)?;
    Ok(Some(record))
}

pub(super) fn confirm_record(
    store: &Store,
    record: &GenerationRecord,
) -> Result<(), AdmissionError> {
    for path in [
        super::record_path(store, &record.digest()),
        store.root().join("generations"),
        store.root().to_owned(),
    ] {
        fs::File::open(&path)
            .and_then(|file| file.sync_all())
            .map_err(|source| io_error(&path, source))?;
    }
    Ok(())
}

fn io_error(path: &Path, source: std::io::Error) -> AdmissionError {
    AdmissionError::Io {
        path: path.display().to_string(),
        source,
    }
}
