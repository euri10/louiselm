//! Active supply is `/3` only. `/2` is verified solely as retained history.

use super::{AdmissionError, GenerationRecord, Store, list_unlocked, load_unlocked};
use crate::{
    Digest,
    generation::{GENERATION_SCHEMA, HISTORICAL_SCHEMA, RECORD_SCHEMA},
    sshsig::{self, ADMISSION_NAMESPACE},
    trust::{Role, TrustStore},
};

/// Checks an active-schema record and its complete signed predecessor chain.
///
/// Historical `/2` predecessors are checked without making them usable supply.
/// # Errors
/// Refuses unsupported schemas, invalid signatures, roots, domains, identities
/// or sequence links, and propagates stored-chain read errors.
pub fn verify_record(
    store: &Store,
    record: &GenerationRecord,
    trust: &TrustStore,
) -> Result<(), AdmissionError> {
    if record.payload.schema != GENERATION_SCHEMA {
        return Err(AdmissionError::Chain(format!(
            "unsupported active payload schema '{}'",
            record.payload.schema
        )));
    }
    verify_history(store, record, trust)
}

pub(super) fn admission_predecessor(
    store: &Store,
    previous: Option<&GenerationRecord>,
    trust: &TrustStore,
    transition: Option<&Digest>,
) -> Result<(), AdmissionError> {
    if let Some(expected) = transition {
        let record = previous
            .filter(|record| {
                record.payload.schema == HISTORICAL_SCHEMA
                    && record.generation == expected.to_string()
            })
            .ok_or_else(|| {
                AdmissionError::Chain(
                    "--transition-from must name the exact current generation/2".to_owned(),
                )
            })?;
        verify_history(store, record, trust)?;
        if record.witness.is_none() {
            return Err(AdmissionError::NotWitnessed {
                digest: record.generation.clone(),
            });
        }
    } else if let Some(record) = previous {
        if record.payload.schema == HISTORICAL_SCHEMA {
            return Err(AdmissionError::Chain(format!(
                "generation/2 requires explicit --transition-from {} and freshly reviewed members",
                record.generation
            )));
        }
        verify_record(store, record, trust)?;
    } else {
        require_no_legacy_history(store)?;
    }
    Ok(())
}

pub(super) fn require_no_legacy_history(store: &Store) -> Result<(), AdmissionError> {
    if list_unlocked(store)?
        .iter()
        .any(|record| record.payload.schema == HISTORICAL_SCHEMA)
    {
        return Err(AdmissionError::Chain(
            "generation/2 history has no current predecessor; inspect retained history before Admission or activation".to_owned(),
        ));
    }
    Ok(())
}

fn verify_history(
    store: &Store,
    record: &GenerationRecord,
    trust: &TrustStore,
) -> Result<(), AdmissionError> {
    verify_signature(record, trust)?;
    let mut sequence = record.payload.sequence;
    let mut predecessor = record.payload.predecessor.clone();
    let mut historical = record.payload.schema == HISTORICAL_SCHEMA;
    // Strictly decreasing sequences bound traversal; no recursion or cycle set.
    while let Some(digest) = predecessor {
        let earlier = load_unlocked(store, &Digest::parse(&digest)?)?;
        if sequence <= 1 || earlier.payload.sequence.checked_add(1) != Some(sequence) {
            return Err(AdmissionError::Chain(format!(
                "sequence {sequence} does not follow {}",
                earlier.payload.sequence
            )));
        }
        if earlier.generation != digest {
            return Err(AdmissionError::Chain(
                "predecessor identity mismatch".to_owned(),
            ));
        }
        if historical && earlier.payload.schema != HISTORICAL_SCHEMA {
            return Err(AdmissionError::Chain(
                "generation/2 history cannot follow generation/3".to_owned(),
            ));
        }
        verify_signature(&earlier, trust)?;
        historical = earlier.payload.schema == HISTORICAL_SCHEMA;
        sequence = earlier.payload.sequence;
        predecessor = earlier.payload.predecessor;
    }
    if sequence != 1 {
        return Err(AdmissionError::Chain(format!(
            "sequence {sequence} declares no predecessor"
        )));
    }
    Ok(())
}

fn verify_signature(record: &GenerationRecord, trust: &TrustStore) -> Result<(), AdmissionError> {
    if record.schema != RECORD_SCHEMA
        || !matches!(
            record.payload.schema.as_str(),
            GENERATION_SCHEMA | HISTORICAL_SCHEMA
        )
    {
        return Err(AdmissionError::Chain(
            "unsupported Generation schema".to_owned(),
        ));
    }
    if record.payload.schema == HISTORICAL_SCHEMA
        && record
            .payload
            .members
            .iter()
            .any(|member| !member.relied_on.is_empty())
    {
        return Err(AdmissionError::Chain(
            "generation/2 cannot carry Endorsement reliance".to_owned(),
        ));
    }
    let actual = record.digest().to_string();
    if record.generation != actual {
        return Err(AdmissionError::DigestMismatch {
            claimed: record.generation.clone(),
            actual,
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
    let payload = record.payload.canonical_bytes();
    let mut last = None;
    for key in trust.verification_keys(Role::Primary, &record.generation) {
        match sshsig::verify(
            &record.signature,
            ADMISSION_NAMESPACE,
            &payload,
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
