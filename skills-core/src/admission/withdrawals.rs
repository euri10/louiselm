//! Non-mutating current-supply evidence for the broker's withdrawal projection.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::Path,
};

use super::{AdmissionError, GenerationRecord, GenerationState, verify_record};
use crate::{
    Digest, Policy, Store, TrustStore,
    endorsement::{
        Record, StatementKind,
        alerts::{self, WithdrawalAlert},
        focus::FocusError,
        import, local,
        trust::EndorserSet,
    },
    quarantine,
    sshsig::SkPolicy,
    trust::{TrustError, persistence::LockedTrust},
};

pub(crate) fn current_withdrawal_alerts(
    store: &Store,
    domain: &str,
    expected: &[String],
) -> Result<Vec<WithdrawalAlert>, AdmissionError> {
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
    let (endorsers, evidence) = verified_endorsements(store, &trust)?;
    let records: BTreeMap<_, _> = evidence
        .iter()
        .map(|record| (record.digest().to_string(), record))
        .collect();
    for expected in expected {
        if records
            .get(expected)
            .is_none_or(|record| record.kind() != StatementKind::Withdrawal)
        {
            return Err(AdmissionError::Chain(
                "previously observed Withdrawal evidence is missing".into(),
            ));
        }
    }
    let quarantine = quarantine::load(store)?;
    let Some(record) = current(store, &trust.approved_admissions)? else {
        locked.confirm()?;
        return Ok(Vec::new());
    };
    verify_record(store, &record, &trust)?;
    if record.schema != crate::generation::RECORD_SCHEMA
        || record.witness.is_none()
        || !trust.approved_admissions.contains(&record.generation)
        || record.payload.policy_digest != Policy::embedded().digest().to_string()
    {
        return Err(AdmissionError::Chain(
            "current Admission is not usable".into(),
        ));
    }
    let (effective, _) = quarantine::partition(
        quarantine.as_ref(),
        &record.generation,
        &record.payload.member_digests(),
    );
    for member in &record.payload.members {
        for relied in &member.relied_on {
            if records
                .get(relied)
                .is_none_or(|record| record.kind() != StatementKind::Endorsement)
            {
                return Err(AdmissionError::Chain(
                    "relied-on Endorsement evidence is missing".into(),
                ));
            }
        }
    }
    let mut alerts = alerts::from_records(&record, &effective, &endorsers, &evidence);
    for alert in &mut alerts {
        // CLI retains the public key fallback; broker metadata uses a bounded
        // fingerprint when that key no longer has a trusted local label.
        let withdrawal = records
            .get(&alert.withdrawal)
            .ok_or_else(|| AdmissionError::Chain("withdrawal evidence is missing".into()))?;
        let key = &withdrawal.signing().signer_key;
        alert.endorser = endorsers
            .entries()
            .iter()
            .find(|entry| &entry.public_key == key)
            .map_or_else(
                || Digest::of(key.as_bytes()).to_string(),
                |entry| entry.label.clone(),
            );
    }
    locked.confirm()?;
    Ok(alerts)
}

fn verified_endorsements(
    store: &Store,
    trust: &TrustStore,
) -> Result<(EndorserSet, Vec<Record>), AdmissionError> {
    let (endorsers, layer) = local::effective_with(store, Some(trust)).map_err(FocusError::from)?;
    if !layer.dropped.is_empty() {
        return Err(AdmissionError::Chain(
            "unverified local Endorser evidence".into(),
        ));
    }
    let stored = import::records(store).map_err(FocusError::from)?;
    if !stored.unreadable.is_empty() {
        return Err(AdmissionError::Chain(
            "unreadable Endorsement evidence".into(),
        ));
    }
    for record in &stored.records {
        record
            .verify_signature(SkPolicy::none())
            .map_err(|_| AdmissionError::Chain("unverified Endorsement evidence".into()))?;
    }
    Ok((endorsers, stored.records))
}

fn current(
    store: &Store,
    approved: &BTreeSet<String>,
) -> Result<Option<GenerationRecord>, AdmissionError> {
    let directory = store.root().join("generations");
    let entries = fs::read_dir(&directory).map_err(|source| io_error(&directory, source))?;
    let mut current = None;
    let mut superseded = false;
    let mut seen = BTreeSet::new();
    for (index, entry) in entries.enumerate() {
        if index >= 4096 {
            return Err(AdmissionError::Chain(
                "Generation scan bound exceeded".into(),
            ));
        }
        let path = entry.map_err(|source| io_error(&directory, source))?.path();
        if path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with(".admission-"))
        {
            continue;
        }
        let mut bytes = Vec::new();
        fs::File::open(&path)
            .and_then(|file| file.take(1024 * 1024 + 1).read_to_end(&mut bytes))
            .map_err(|source| io_error(&path, source))?;
        if bytes.len() > 1024 * 1024 {
            return Err(AdmissionError::Chain(
                "Generation record bound exceeded".into(),
            ));
        }
        let record: GenerationRecord = serde_json::from_slice(&bytes)
            .map_err(|error| AdmissionError::Malformed(error.to_string()))?;
        if path != super::record_path(store, &Digest::parse(&record.generation)?) {
            return Err(AdmissionError::Chain("Generation identity mismatch".into()));
        }
        seen.insert(record.generation.clone());
        match record.state {
            GenerationState::Current if current.is_none() => current = Some(record),
            GenerationState::Current | GenerationState::Quarantined => {
                return Err(AdmissionError::Chain("ambiguous current Generation".into()));
            }
            GenerationState::Superseded => superseded = true,
            GenerationState::PendingWitness | GenerationState::Invalid => (),
        }
    }
    if !approved.is_subset(&seen) || (current.is_none() && (superseded || !approved.is_empty())) {
        return Err(AdmissionError::Chain(
            "current Generation is missing".into(),
        ));
    }
    Ok(current)
}

fn io_error(path: &Path, source: std::io::Error) -> AdmissionError {
    AdmissionError::Io {
        path: path.display().to_string(),
        source,
    }
}
