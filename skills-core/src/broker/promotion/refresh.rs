//! Explicit refresh preparation and publication; neither mutates the Run checkout.

use super::{BrokerError, BrokerService, PromotionStatus, SavedResult};
use crate::{
    Digest,
    broker::{read_record, verification::VerificationRecord, write_new_record},
    launch_protocol::{VerificationOperation, VerificationRequest},
    workspace::{self, filesystem, promotion::transfer, verification},
};
use serde::{Deserialize, Serialize};
use std::{fs, os::unix::fs::DirBuilderExt, path::PathBuf};

/// Operator-selected new baseline and rules; no execution authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefreshSelection {
    /// Logical saved decision, unchanged by refresh.
    pub result_id: String,
    /// Current canonical verification digest; concurrent refreshes cannot overwrite it.
    pub revision_digest: String,
    /// Unique refresh attempt, never reused for different inputs.
    pub input_id: String,
    /// Frozen operator-selected baseline, not the writable checkout.
    pub snapshot: PathBuf,
    /// Exact snapshot identity.
    pub snapshot_digest: String,
    /// Exact new destination Run-branch HEAD.
    pub expected_head: String,
    /// Current fixed verification rules.
    pub plan: PathBuf,
    /// Exact selected rule bytes.
    pub plan_digest: String,
}

/// Immutable binding consumed by a separately authorized fresh fixed verifier.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefreshInput {
    /// Closed storage contract.
    pub schema: String,
    /// Exact explicit operator preparation.
    pub selection: RefreshSelection,
    /// Original authenticated producer observation, never relabelled as new output.
    pub producer: crate::launch_protocol::VerificationExport,
    /// Newly prepared combined bytes and complete current plan.
    pub job: verification::JobPreview,
    /// Installed operator owner.
    pub controller_uid: u32,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NextRevision {
    pub(super) input_id: String,
    pub(super) digest: String,
}

impl BrokerService {
    /// Prepares exact saved changes on a separate new-baseline copy. Executes nothing.
    /// Duplicate identical preparation is read-only; conflicts preserve the old result.
    /// # Errors
    /// Refuses decided/uncertain results, changed revisions, conflicts and invalid inputs.
    pub fn prepare_saved_refresh(
        &self,
        uid: u32,
        selection: &RefreshSelection,
    ) -> Result<RefreshInput, BrokerError> {
        super::super::record_name(&selection.input_id)?;
        let _guard = self.lifecycle.promotion_guard();
        let (saved, revision) = self.saved_revision(uid, &selection.result_id)?;
        if saved.discarded
            || saved.accepted.is_some()
            || saved.application != PromotionStatus::NotRequested
            || selection.revision_digest != saved.request.verification_digest
            || !super::selection::valid_commit(&selection.expected_head)
        {
            return Err(BrokerError::RequestMismatch);
        }
        let output = self.verification_inputs.join(&selection.input_id);
        self.prepare_verification_inputs()?;
        if let Some(prior) = read_record::<RefreshInput>(&output.join("refresh.json"))? {
            if prior.selection != *selection || prior.controller_uid != uid {
                return Err(BrokerError::RequestMismatch);
            }
            transfer::load(&output.join("transfer"), &prior.job)?;
            return Ok(prior);
        }
        let snapshot_digest =
            Digest::parse(&selection.snapshot_digest).map_err(|_| BrokerError::InvalidGrant)?;
        let plan_digest =
            Digest::parse(&selection.plan_digest).map_err(|_| BrokerError::InvalidGrant)?;
        let files = workspace::snapshot_files(&selection.snapshot, &snapshot_digest)?;
        if workspace::snapshot_base_commit(&selection.snapshot, &snapshot_digest)?
            != selection.expected_head
        {
            return Err(BrokerError::RequestMismatch);
        }
        let changes =
            transfer::load(&revision.join("transfer"), &saved.request.job)?.refresh(files)?;
        let mut input = None;
        filesystem::publish(&output, |staging| {
            let digest = verification::stage_inputs(
                &selection.snapshot,
                &snapshot_digest,
                &selection.plan,
                &plan_digest,
                &staging.join("inputs"),
            )?;
            filesystem::write_files(&staging.join("candidate"), &changes.files, true)?;
            let job = verification::export_job(
                &staging.join("inputs"),
                &digest,
                &staging.join("candidate"),
                &staging.join("export"),
                &snapshot_digest,
            )?;
            transfer::copy_transfer(
                &staging.join("inputs/snapshot"),
                &staging.join("export/job"),
                &job,
                &staging.join("transfer"),
            )?;
            let prepared = RefreshInput {
                schema: "louiselm.workspace.refresh/1".into(),
                selection: selection.clone(),
                producer: saved.verification.producer.clone(),
                job,
                controller_uid: uid,
            };
            filesystem::write_file(
                &staging.join("refresh.json"),
                &serde_json::to_vec(&prepared)?,
                0o400,
            )?;
            input = Some(prepared);
            Ok(())
        })?;
        input.ok_or(BrokerError::InvalidGrant)
    }

    pub(in crate::broker) fn refresh_input(
        &self,
        request: &VerificationRequest,
    ) -> Result<RefreshInput, BrokerError> {
        let VerificationOperation::Refresh {
            input_id,
            input_digest,
            job_digest,
        } = &request.operation
        else {
            return Err(BrokerError::InvalidGrant);
        };
        super::super::record_name(input_id)?;
        let path = self.verification_inputs.join(input_id);
        let root = filesystem::open_directory(&path)?;
        let bytes = filesystem::read_source(&root, "refresh.json", 16 * 1024 * 1024)?
            .ok_or(BrokerError::InvalidGrant)?;
        if Digest::of(&bytes.bytes).to_string() != *input_digest {
            return Err(BrokerError::RequestMismatch);
        }
        let input: RefreshInput =
            serde_json::from_slice(&bytes.bytes).map_err(|_| BrokerError::InvalidGrant)?;
        if input.schema != "louiselm.workspace.refresh/1"
            || input.selection.input_id != *input_id
            || input.job.job_digest != *job_digest
        {
            return Err(BrokerError::RequestMismatch);
        }
        transfer::load(&path.join("transfer"), &input.job)?;
        Ok(input)
    }

    pub(in crate::broker) fn check_saved_refresh_current(
        &self,
        request: &VerificationRequest,
    ) -> Result<(), BrokerError> {
        let input = self.refresh_input(request)?;
        let saved = self.saved_result(input.controller_uid, &input.selection.result_id)?;
        if saved.discarded
            || saved.accepted.is_some()
            || saved.application != PromotionStatus::NotRequested
            || input.selection.revision_digest != saved.request.verification_digest
        {
            return Err(BrokerError::RequestMismatch);
        }
        Ok(())
    }

    /// Publishes a new current revision only after complete fresh verified cleanup.
    /// A losing concurrent attempt or failed verification never replaces the original.
    /// # Errors
    /// Refuses failed/current-policy-invalid evidence, decisions and uncertain application.
    pub fn publish_saved_refresh(
        &self,
        uid: u32,
        record: &VerificationRecord,
    ) -> Result<SavedResult, BrokerError> {
        self.validate_verification_record(record)?;
        let input = self.refresh_input(&record.execution.request)?;
        if input.controller_uid != uid || !record.execution.commands_passed() {
            return Err(BrokerError::RequestMismatch);
        }
        let _guard = self.lifecycle.promotion_guard();
        let (prior, revision) = self.saved_revision(uid, &input.selection.result_id)?;
        if prior.verification == *record {
            return Ok(prior);
        }
        if prior.discarded
            || prior.accepted.is_some()
            || prior.application != PromotionStatus::NotRequested
            || input.selection.revision_digest != prior.request.verification_digest
        {
            return Err(BrokerError::RequestMismatch);
        }
        let mut saved = prior;
        saved.verification = record.clone();
        saved
            .request
            .verifier_session_id
            .clone_from(&record.execution.request.launch.session_id);
        saved.request.verification_digest =
            Digest::of(&serde_json::to_vec(record).map_err(|_| BrokerError::InvalidGrant)?)
                .to_string();
        saved.request.job = input.job.clone();
        saved
            .selection
            .verifier_session_id
            .clone_from(&saved.request.verifier_session_id);
        saved
            .selection
            .expected_head
            .clone_from(&input.selection.expected_head);
        saved.selection.plan.clone_from(&input.selection.plan);
        let source = self
            .verification_inputs
            .join(&input.selection.input_id)
            .join("transfer");
        saved.changes = transfer::load(&source, &input.job)?.preview;
        let parent = self
            .saved_path(&input.selection.result_id)?
            .join("revisions");
        match fs::DirBuilder::new().mode(0o700).create(&parent) {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(error) => return Err(BrokerError::Storage(error)),
        }
        let output = parent.join(&input.selection.input_id);
        filesystem::publish(&output, |staging| {
            transfer::copy_transfer(
                &source.join("snapshot"),
                &source.join("job"),
                &input.job,
                &staging.join("transfer"),
            )?;
            filesystem::write_file(
                &staging.join("saved.json"),
                &serde_json::to_vec(&saved)?,
                0o400,
            )
        })?;
        let digest =
            Digest::of(&serde_json::to_vec(&saved).map_err(|_| BrokerError::InvalidGrant)?)
                .to_string();
        write_new_record(&output.join("published.json"), &digest)?;
        write_new_record(
            &revision.join("next.json"),
            &NextRevision {
                input_id: input.selection.input_id,
                digest,
            },
        )?;
        self.saved_result(uid, &saved.request.request_id)
    }
}
