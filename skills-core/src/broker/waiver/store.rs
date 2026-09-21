//! Atomic private decision records. Inspection never writes or renews authority.

use super::{Context, Outcome, Plan, Receipt, Request, WaiverError, validate_plan};
use crate::{
    Digest,
    broker::{read_record, sync_directory},
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
};

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ledger {
    revision: u64,
    active: Option<String>,
    plans: Vec<Record>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    context: Context,
    revision: u64,
    plan: Plan,
    receipt: Option<Receipt>,
}

impl Ledger {
    fn check_subject(&self, context: &Context) -> Result<(), WaiverError> {
        for record in &self.plans {
            if record.context.operator_uid != context.operator_uid {
                return Err(WaiverError::WrongOperator);
            }
            if record.context.session_id != context.session_id
                || record.context.run_id != context.run_id
                || record.context.authorization_id != context.authorization_id
                || record.context.request_digest != context.request_digest
                || record.context.envelope_revision != context.envelope_revision
                || record.context.attendance != context.attendance
            {
                return Err(WaiverError::StalePlan);
            }
        }
        Ok(())
    }

    fn plan(
        &mut self,
        context: &Context,
        proposal: &super::Proposal,
        now: u64,
    ) -> Result<String, WaiverError> {
        validate_plan(context, proposal, now)?;
        if let Some(record) = self
            .plans
            .iter()
            .find(|record| record.plan.proposal.request_id == proposal.request_id)
        {
            if record.context != *context || record.plan.proposal != *proposal {
                return Err(WaiverError::Conflict);
            }
            return Ok(record.plan.digest.clone());
        }
        if self.plans.len() >= 32 {
            return Err(WaiverError::Unavailable);
        }
        let digest = digest(&(context, self.revision, proposal))?;
        self.plans.push(Record {
            context: context.clone(),
            revision: self.revision,
            plan: Plan {
                schema: "louiselm.conformance-waiver-plan/1".into(),
                session_id: context.session_id.clone(),
                run_id: context.run_id.clone(),
                envelope_revision: context.envelope_revision,
                operator_uid: context.operator_uid,
                proposal: proposal.clone(),
                digest: digest.clone(),
            },
            receipt: None,
        });
        Ok(digest)
    }

    fn approve(&mut self, context: &Context, id: &str, now: u64) -> Result<(), WaiverError> {
        let record = self
            .plans
            .iter_mut()
            .find(|record| record.plan.digest == id)
            .ok_or(WaiverError::Unknown)?;
        if record.context.operator_uid != context.operator_uid {
            return Err(WaiverError::WrongOperator);
        }
        if record.receipt.is_some() {
            return Ok(());
        }
        validate_plan(context, &record.plan.proposal, now)?;
        if record.context != *context || record.revision != self.revision {
            return Err(WaiverError::StalePlan);
        }
        let receipt = Receipt {
            plan: record.plan.clone(),
            approved_at_ms: now,
            digest: digest(&(&record.plan, now))?,
        };
        self.active = Some(receipt.digest.clone());
        record.receipt = Some(receipt);
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or(WaiverError::Unavailable)?;
        Ok(())
    }

    fn revoke(&mut self, context: &Context, id: &str) -> Result<String, WaiverError> {
        let record = self
            .plans
            .iter()
            .find(|record| {
                record
                    .receipt
                    .as_ref()
                    .is_some_and(|receipt| receipt.digest == id)
            })
            .ok_or(WaiverError::Unknown)?;
        if record.context.operator_uid != context.operator_uid {
            return Err(WaiverError::WrongOperator);
        }
        if self.active.as_deref() == Some(id) {
            self.active = None;
            self.revision = self
                .revision
                .checked_add(1)
                .ok_or(WaiverError::Unavailable)?;
        }
        Ok(record.plan.digest.clone())
    }
}

pub(in crate::broker) struct Waivers {
    root: PathBuf,
    writing: Mutex<()>,
}

impl Waivers {
    pub(in crate::broker) fn decision(
        &self,
        authorization: &crate::launch_protocol::LaunchAuthorization,
    ) -> Result<(u64, Option<crate::launch_protocol::ConformanceWaiver>), WaiverError> {
        let _guard = crate::broker::lock(&self.writing);
        let ledger = self.load(&authorization.session_id)?;
        ledger.check_subject(&Context {
            session_id: authorization.session_id.clone(),
            run_id: authorization.run_id.clone(),
            authorization_id: authorization.authorization_id.clone(),
            request_digest: authorization.request_digest.clone(),
            envelope_revision: authorization.envelope_revision,
            operator_uid: authorization.controller_uid,
            attendance: authorization.conformance.attendance,
            condition: super::Condition::Missing,
            receipt_head: String::new(),
        })?;
        if ledger.revision == 0 {
            return Ok((0, authorization.conformance.waiver.clone()));
        }
        let receipt = ledger
            .plans
            .iter()
            .filter_map(|record| record.receipt.as_ref())
            .find(|receipt| ledger.active.as_ref() == Some(&receipt.digest));
        Ok((
            ledger.revision,
            receipt.map(|receipt| crate::launch_protocol::ConformanceWaiver {
                session_id: authorization.session_id.clone(),
                request_digest: authorization.request_digest.clone(),
                operator_uid: receipt.plan.operator_uid,
                condition: receipt.plan.proposal.condition,
                expires_at_ms: receipt.plan.proposal.expires_at_ms,
                receipt_digest: receipt.digest.clone(),
            }),
        ))
    }
    pub(in crate::broker) fn open(root: &Path) -> Result<Self, crate::broker::BrokerError> {
        fs::create_dir_all(root).map_err(crate::broker::BrokerError::Storage)?;
        sync_directory(root)?;
        if let Some(parent) = root.parent() {
            sync_directory(parent)?;
        }
        Ok(Self {
            root: root.into(),
            writing: Mutex::new(()),
        })
    }

    pub(super) fn control(
        &self,
        context: &Context,
        request: &Request,
        now: u64,
    ) -> Result<Outcome, WaiverError> {
        let _guard = crate::broker::lock(&self.writing);
        let mut ledger = self.load(&context.session_id)?;
        ledger.check_subject(context)?;
        let selected = match request {
            Request::Plan { proposal } => Some(ledger.plan(context, proposal, now)?),
            Request::Apply { plan_digest } => {
                ledger.approve(context, plan_digest, now)?;
                Some(plan_digest.clone())
            }
            Request::Revoke { receipt_digest } => Some(ledger.revoke(context, receipt_digest)?),
            Request::Result { plan_digest } => Some(plan_digest.clone()),
            Request::Inspect => ledger
                .plans
                .iter()
                .find(|record| {
                    record
                        .receipt
                        .as_ref()
                        .is_some_and(|receipt| ledger.active.as_ref() == Some(&receipt.digest))
                })
                .map(|record| record.plan.digest.clone()),
        };
        // Replaying a mutation also proves durability after an earlier fsync failure.
        if !matches!(request, Request::Inspect | Request::Result { .. }) {
            self.save(&context.session_id, &ledger)?;
        }
        let record = selected
            .as_ref()
            .map(|id| {
                ledger
                    .plans
                    .iter()
                    .find(|record| record.plan.digest == *id)
                    .ok_or(WaiverError::Unknown)
            })
            .transpose()?;
        Ok(outcome(context, &ledger, record, now))
    }

    #[cfg(test)]
    pub(super) fn current(&self, session: &str, now: u64) -> Result<Option<Receipt>, WaiverError> {
        let _guard = crate::broker::lock(&self.writing);
        let ledger = self.load(session)?;
        Ok(ledger
            .plans
            .into_iter()
            .filter_map(|record| record.receipt)
            .find(|receipt| {
                ledger.active.as_ref() == Some(&receipt.digest)
                    && now < receipt.plan.proposal.expires_at_ms
            }))
    }

    fn load(&self, session: &str) -> Result<Ledger, WaiverError> {
        if !crate::broker::is_record_identifier(session) {
            return Err(WaiverError::InvalidRequest);
        }
        let ledger: Ledger = read_record(&self.root.join(format!("{session}.json")))
            .map_err(|_| WaiverError::Unavailable)?
            .unwrap_or_default();
        for record in &ledger.plans {
            if record.context.session_id != session
                || record.plan.digest
                    != digest(&(&record.context, record.revision, &record.plan.proposal))?
                || record.plan.schema != "louiselm.conformance-waiver-plan/1"
                || record.plan.session_id != record.context.session_id
                || record.plan.run_id != record.context.run_id
                || record.plan.operator_uid != record.context.operator_uid
                || record.plan.envelope_revision != record.context.envelope_revision
                || record.revision > ledger.revision
                || validate_plan(&record.context, &record.plan.proposal, 0).is_err()
                || record.receipt.as_ref().is_some_and(|receipt| {
                    receipt.plan != record.plan
                        || receipt.approved_at_ms >= receipt.plan.proposal.expires_at_ms
                        || record.revision >= ledger.revision
                        || digest(&(&receipt.plan, receipt.approved_at_ms)).as_ref()
                            != Ok(&receipt.digest)
                })
            {
                return Err(WaiverError::Unavailable);
            }
        }
        if ledger.plans.len() > 32
            || ledger.active.as_ref().is_some_and(|active| {
                !ledger.plans.iter().any(|record| {
                    record
                        .receipt
                        .as_ref()
                        .is_some_and(|receipt| receipt.digest == *active)
                })
            })
        {
            return Err(WaiverError::Unavailable);
        }
        Ok(ledger)
    }

    fn save(&self, session: &str, ledger: &Ledger) -> Result<(), WaiverError> {
        let bytes = serde_json::to_vec(ledger).map_err(|_| WaiverError::Unavailable)?;
        if bytes.len() as u64 > crate::broker::MAX_RECORD_BYTES {
            return Err(WaiverError::Unavailable);
        }
        let mut file =
            tempfile::NamedTempFile::new_in(&self.root).map_err(|_| WaiverError::Unavailable)?;
        file.write_all(&bytes)
            .and_then(|()| file.as_file().sync_all())
            .map_err(|_| WaiverError::Unavailable)?;
        file.persist(self.root.join(format!("{session}.json")))
            .map_err(|_| WaiverError::Unavailable)?;
        sync_directory(&self.root).map_err(|_| WaiverError::Unavailable)
    }
}

fn digest(value: &impl Serialize) -> Result<String, WaiverError> {
    serde_json::to_vec(value)
        .map(|bytes| Digest::of(&bytes).to_string())
        .map_err(|_| WaiverError::Unavailable)
}

fn outcome(context: &Context, ledger: &Ledger, record: Option<&Record>, now: u64) -> Outcome {
    let receipt = record.and_then(|record| record.receipt.clone());
    Outcome {
        schema: "louiselm.conformance-waiver-outcome/1".into(),
        session_id: context.session_id.clone(),
        plan: record.map(|record| record.plan.clone()),
        active: receipt.as_ref().is_some_and(|receipt| {
            ledger.active.as_ref() == Some(&receipt.digest)
                && now < receipt.plan.proposal.expires_at_ms
        }),
        receipt,
    }
}
