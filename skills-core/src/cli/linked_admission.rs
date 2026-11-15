//! Optional linked ceremony. Global Admission success is never rolled back by delivery.

use super::{AdmissionRequest, CliError, Options, Policy, SshKeygenSigner, Store, now_ms, report};
use crate::{
    admission,
    broker::{admission_source::AdmissionSource, operator},
    skill_request::{SkillRequestOutcome, SkillRequestStatus},
};
use serde::Serialize;
use std::{cell::Cell, path::Path};

#[derive(Serialize)]
struct ResultRecord {
    schema: &'static str,
    admission: crate::generation::GenerationRecord,
    operation_id: String,
    broker_status: Option<SkillRequestStatus>,
    resolution_error: Option<operator::InspectError>,
}

fn installed_source(store: &Store, policy: &Policy) -> Result<AdmissionSource, CliError> {
    let source = AdmissionSource::installed()
        .map_err(|_| CliError::Invalid("Admission source configuration unavailable".into()))?
        .ok_or_else(|| CliError::Invalid("broker-linked Admission is not configured".into()))?;
    if rustix::process::geteuid().as_raw() != source.operator_uid
        || store.root() != source.store
        || policy.digest() != Policy::embedded().digest()
    {
        return Err(CliError::Invalid(
            "linked Admission must use the installed operator, store and policy".into(),
        ));
    }
    let trust =
        crate::trust::TrustStore::load(store)?.ok_or(crate::trust::TrustError::NotBootstrapped)?;
    if trust.trust_domain != source.trust_domain {
        return Err(CliError::Invalid("Admission trust domain mismatch".into()));
    }
    Ok(source)
}

fn matches_request(
    members: &[crate::admission::AdmissionMember],
    pending: &SkillRequestStatus,
) -> bool {
    let mut packages = members
        .iter()
        .map(|member| member.package.to_string())
        .collect::<Vec<_>>();
    packages.sort();
    packages == pending.packages
        && members.iter().all(|member| {
            let mut agents = member.agents.clone();
            agents.sort();
            agents.dedup();
            agents == pending.agents
        })
}

fn verify_reservation_replay(
    source: &AdmissionSource,
    operation: &str,
    sources: &[String],
    pending: &SkillRequestStatus,
) -> Result<Option<operator::InspectError>, CliError> {
    let prior = match operator::inspect_skill_signing(
        Path::new(operator::SOCKET),
        source.broker_uid,
        operation,
        sources,
        operator::TIMEOUT,
    ) {
        Ok(prior) => prior,
        Err(operator::InspectError::BrokerUnavailable) => {
            return Ok(Some(operator::InspectError::BrokerUnavailable));
        }
        Err(_) => {
            return Err(CliError::Invalid(
                "Admission already signed; source requests could not be confirmed".into(),
            ));
        }
    };
    if prior.request_id != pending.request_id
        || prior.subject != pending.subject
        || prior.session_id != pending.session_id
        || prior.run_id != pending.run_id
        || prior.packages != pending.packages
        || prior.agents != pending.agents
    {
        return Err(CliError::Invalid(
            "Admission already signed; source requests could not be confirmed".into(),
        ));
    }
    Ok(None)
}

fn sorted_sources(options: &Options) -> Result<Vec<String>, CliError> {
    let mut sources = options.source_requests.clone();
    sources.sort();
    if sources.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(CliError::Invalid("duplicate source request".into()));
    }
    Ok(sources)
}

pub(super) fn run(
    options: &Options,
    store: &Store,
    policy: &Policy,
    operation: &str,
) -> Result<i32, CliError> {
    let source = installed_source(store, policy)?;
    let inspect = || {
        operator::skill_request(
            Path::new(operator::SOCKET),
            source.broker_uid,
            operation,
            None,
            operator::TIMEOUT,
        )
    };
    let pending = inspect().map_err(|_| {
        CliError::Invalid("broker request could not be inspected; no Admission attempted".into())
    })?;
    if matches!(
        pending.outcome,
        SkillRequestOutcome::Rejected
            | SkillRequestOutcome::Cancelled
            | SkillRequestOutcome::Superseded
    ) {
        return Err(CliError::Invalid(
            "broker request has ended; no Admission attempted".into(),
        ));
    }
    let members = options.admission_members()?;
    if !matches_request(&members, &pending) {
        return Err(CliError::Invalid(
            "Admission members must exactly match the broker request".into(),
        ));
    }
    let signer = SshKeygenSigner::new(&options.signing_key()?);
    let sources = sorted_sources(options)?;
    let reserved_here = Cell::new(false);
    let reviewed_profile = options
        .reviewed_profile_dossier
        .as_deref()
        .map(crate::Digest::parse)
        .transpose()?;
    let admission = admission::admit_linked(
        store,
        policy,
        &AdmissionRequest {
            members,
            signer: &signer,
            admitted_at_ms: now_ms(),
            configured_agents: options
                .registry
                .is_some()
                .then(|| options.registered_agents())
                .transpose()?,
        },
        operation,
        reviewed_profile.as_ref(),
        &|| {
            let reserved = operator::reserve_skill_signing(
                Path::new(operator::SOCKET),
                source.broker_uid,
                operation,
                &sources,
                operator::TIMEOUT,
            )
            .map_err(|_| admission::AdmissionError::SigningReservation)?;
            if reserved != pending {
                return Err(admission::AdmissionError::SigningReservation);
            }
            reserved_here.set(true);
            Ok(())
        },
    )?;
    let replay_error = if reserved_here.get() {
        None
    } else {
        verify_reservation_replay(&source, operation, &sources, &pending)?
    };
    let (broker_status, resolution_error) = match replay_error {
        Some(error) => (None, Some(error)),
        None => match inspect() {
            Ok(status) => (Some(status), None),
            Err(error) => (None, Some(error)),
        },
    };
    report(
        options,
        &ResultRecord {
            schema: "louiselm.linked-admission/1",
            admission,
            operation_id: operation.into(),
            broker_status,
            resolution_error,
        },
        |result| {
            format!(
                "generation {} signed and persisted; broker outcome {:?}; witness/activation remain separate. Reinspect operation {} if resolution is pending or unavailable.",
                result.admission.generation,
                result.broker_status.as_ref().map(|status| status.outcome),
                result.operation_id
            )
        },
    )
}
