//! Optional linked ceremony. Global Admission success is never rolled back by delivery.

use super::{AdmissionRequest, CliError, Options, Policy, SshKeygenSigner, Store, now_ms, report};
use crate::{
    admission,
    broker::{admission_source::AdmissionSource, operator},
    skill_request::{SkillRequestOutcome, SkillRequestStatus},
};
use serde::Serialize;
use std::path::Path;

#[derive(Serialize)]
struct ResultRecord {
    schema: &'static str,
    admission: crate::generation::GenerationRecord,
    operation_id: String,
    broker_status: Option<SkillRequestStatus>,
    resolution_error: Option<operator::InspectError>,
}

pub(super) fn run(
    options: &Options,
    store: &Store,
    policy: &Policy,
    operation: &str,
) -> Result<i32, CliError> {
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
        SkillRequestOutcome::Rejected | SkillRequestOutcome::Cancelled
    ) {
        return Err(CliError::Invalid(
            "broker request has ended; no Admission attempted".into(),
        ));
    }
    let members = options.admission_members()?;
    let mut packages = members
        .iter()
        .map(|member| member.package.to_string())
        .collect::<Vec<_>>();
    packages.sort();
    if packages != pending.packages
        || members.iter().any(|member| {
            let mut agents = member.agents.clone();
            agents.sort();
            agents.dedup();
            agents != pending.agents
        })
    {
        return Err(CliError::Invalid(
            "Admission members must exactly match the broker request".into(),
        ));
    }
    let trust =
        crate::trust::TrustStore::load(store)?.ok_or(crate::trust::TrustError::NotBootstrapped)?;
    if trust.trust_domain != source.trust_domain {
        return Err(CliError::Invalid("Admission trust domain mismatch".into()));
    }
    let signer = SshKeygenSigner::new(&options.signing_key()?);
    let admission = admission::admit_linked(
        store,
        policy,
        &AdmissionRequest {
            members,
            signer: &signer,
            admitted_at_ms: now_ms(),
        },
        operation,
    )?;
    let (broker_status, resolution_error) = match inspect() {
        Ok(status) => (Some(status), None),
        Err(error) => (None, Some(error)),
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
