//! Withdrawal alerts: which admitted members depended on withdrawn evidence.
//!
//! A withdrawal never changes a Generation, never quarantines, and never
//! changes Admission. It tells the operator, who decides. Two urgencies
//! exist:
//!
//! - [`Urgency::ReliedOn`]: a member's signed `relied_on` names an Endorsement
//!   (of its exact bytes or of the base its review was framed against) whose
//!   signer has since withdrawn it. Its review was focused by evidence that
//!   no longer stands.
//! - [`Urgency::SubjectOnly`]: a currently trusted endorser withdrew a
//!   member's exact bytes, but the member's review did not rely on it.
//!
//! Withdrawals by signers that are neither relied on nor trusted raise
//! nothing: anyone can sign a withdrawal of any digest, and noise would bury
//! the alerts that matter. Withdrawals are checked for proof and signature,
//! not expiry, so an old withdrawal keeps alerting.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::{Record, StatementKind, focus::FocusError, import, local, trust::EndorserSet};
use crate::{generation::GenerationRecord, sshsig::SkPolicy, store::Store};

/// How much a withdrawal matters to one admitted member.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Urgency {
    /// The member's review relied on the withdrawn Endorsement.
    ReliedOn,
    /// A trusted endorser withdrew the member's bytes; its review did not rely on it.
    SubjectOnly,
}

/// One admitted member affected by one withdrawal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct WithdrawalAlert {
    /// The admitted package.
    pub member: String,
    /// The withdrawing endorser's local label, or its key when untrusted now.
    pub endorser: String,
    /// The withdrawal record.
    pub withdrawal: String,
    /// How much it matters.
    pub urgency: Urgency,
    /// What the operator may do. Never done automatically.
    pub next_action: String,
}

/// Lists alerts for the members of `generation` that are in `effective`
/// (admitted and not quarantined), most urgent first.
///
/// # Errors
/// Returns endorser-layer and stored-record read errors.
pub fn withdrawal_alerts(
    store: &Store,
    generation: &GenerationRecord,
    effective: &[String],
) -> Result<Vec<WithdrawalAlert>, FocusError> {
    let (endorsers, _) = local::effective(store)?;
    let mut records = import::records(store)?.records;
    records.retain(|record| {
        record.kind() != StatementKind::Withdrawal
            || record.verify_signature(SkPolicy::none()).is_ok()
    });
    Ok(from_records(generation, effective, &endorsers, &records))
}

/// Shared relevance rules. Callers have already verified every withdrawal;
/// strict consumers additionally reject incomplete or invalid stored evidence.
pub(crate) fn from_records(
    generation: &GenerationRecord,
    effective: &[String],
    endorsers: &EndorserSet,
    records: &[Record],
) -> Vec<WithdrawalAlert> {
    let by_digest: BTreeMap<String, &Record> = records
        .iter()
        .map(|record| (record.digest().to_string(), record))
        .collect();
    let withdrawals: Vec<&Record> = records
        .iter()
        .filter(|record| record.kind() == StatementKind::Withdrawal)
        .collect();
    let mut alerts = Vec::new();
    for member in &generation.payload.members {
        if !effective.contains(&member.package_digest) {
            continue;
        }
        let mut seen = BTreeSet::new();
        for relied in &member.relied_on {
            let Some(endorsement) = by_digest.get(relied) else {
                continue;
            };
            let subject = endorsement.subject().package_digest();
            for withdrawal in &withdrawals {
                if withdrawal.signing().signer_key == endorsement.signing().signer_key
                    && withdrawal.covers(subject)
                    && seen.insert(withdrawal.digest())
                {
                    alerts.push(alert(
                        endorsers,
                        &member.package_digest,
                        withdrawal,
                        Urgency::ReliedOn,
                    ));
                }
            }
        }
        for withdrawal in &withdrawals {
            let trusted = endorsers
                .entries()
                .iter()
                .any(|entry| entry.public_key == withdrawal.signing().signer_key);
            if trusted
                && withdrawal.subject().package_digest().to_string() == member.package_digest
                && seen.insert(withdrawal.digest())
            {
                alerts.push(alert(
                    endorsers,
                    &member.package_digest,
                    withdrawal,
                    Urgency::SubjectOnly,
                ));
            }
        }
    }
    alerts.sort_by_key(|alert| alert.urgency != Urgency::ReliedOn);
    alerts
}

fn alert(
    endorsers: &EndorserSet,
    member: &str,
    withdrawal: &Record,
    urgency: Urgency,
) -> WithdrawalAlert {
    let key = &withdrawal.signing().signer_key;
    let endorser = endorsers
        .entries()
        .iter()
        .find(|entry| &entry.public_key == key)
        .map_or_else(|| key.clone(), |entry| entry.label.clone());
    WithdrawalAlert {
        member: member.to_owned(),
        endorser,
        withdrawal: withdrawal.digest().to_string(),
        urgency,
        next_action: format!(
            "review {member}; to stop its use, run: louiselm-skills quarantine exclude {member} --reason endorsement_withdrawn"
        ),
    }
}
