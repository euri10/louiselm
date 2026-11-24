//! Private result publication is independent of disposable Session storage.

use super::{BrokerError, BrokerService, PromotionRequest, PromotionStatus, SavedSelection};
use crate::{
    Digest,
    broker::{
        BrokerSession, read_record, sync_directory, verification::VerificationRecord,
        write_new_record,
    },
    workspace::{
        filesystem,
        promotion::{ChangePreview, transfer},
    },
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, MetadataExt},
    path::{Path, PathBuf},
};

fn private_directory(path: &Path) -> Result<fs::File, BrokerError> {
    let directory = filesystem::open_directory(path)?;
    let metadata = directory.metadata().map_err(BrokerError::Storage)?;
    if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o077 != 0 {
        return Err(BrokerError::ControllerMismatch);
    }
    Ok(directory)
}

fn sync_record(root: &fs::File, name: &str) -> Result<(), BrokerError> {
    fs::File::from(
        rustix::fs::openat(
            root,
            name,
            rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(|error| BrokerError::Storage(error.into()))?,
    )
    .sync_all()
    .map_err(BrokerError::Storage)
}

/// Exact saved work and historical evidence, never execution or application authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedResult {
    /// Fixed closed storage contract.
    pub schema: String,
    /// Original selection; its expiry bounds publication, not saved-byte retention.
    pub request: PromotionRequest,
    /// Reopenable operator context, never an execution grant.
    pub selection: SavedSelection,
    /// Complete authenticated verification observation and cleanup identity.
    pub verification: VerificationRecord,
    /// Exact path changes, remeasured on inspection.
    pub changes: ChangePreview,
    /// Explicit discard is a durable decision, not deletion or application.
    pub discarded: bool,
    /// Confirmed operator-owned Git commit after complete application.
    pub accepted: Option<SavedAcceptance>,
    /// Durable application effects, including uncertainty; never inferred rollback.
    pub application: PromotionStatus,
}

/// Authenticated operator attestation following synchronized application and Git commit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedAcceptance {
    /// Exact resulting Run-branch commit, not a host-main merge.
    pub commit: String,
    /// Stable exact review approved by the operator.
    pub approval_digest: String,
    /// One exact spent application request; no renewed authority.
    pub request_digest: String,
}

impl BrokerService {
    fn saved_path(&self, id: &str) -> Result<PathBuf, BrokerError> {
        super::super::record_name(id)?;
        Ok(self.authorizations().root.join("saved-results").join(id))
    }

    /// Reads exact saved bytes after producer Disposal and ordinary Session cleanup.
    /// This blocking operator API grants no authority and ignores elapsed review time.
    /// # Errors
    /// Refuses foreign operators, absent/incomplete publication, unsafe paths and changed bytes.
    pub fn saved_result(&self, uid: u32, id: &str) -> Result<SavedResult, BrokerError> {
        let directory = self.saved_path(id)?;
        let parent = private_directory(directory.parent().ok_or(BrokerError::InvalidGrant)?)?;
        let root = private_directory(&directory)?;
        let bytes = filesystem::read_source(&root, "saved.json", 16 * 1024 * 1024)?
            .ok_or(BrokerError::InvalidGrant)?;
        let confirmation = filesystem::read_source(&root, "published.json", 128)?
            .ok_or(BrokerError::InvalidGrant)?;
        let confirmed: String =
            serde_json::from_slice(&confirmation.bytes).map_err(|_| BrokerError::InvalidGrant)?;
        if confirmed != Digest::of(&bytes.bytes).to_string() {
            return Err(BrokerError::RequestMismatch);
        }
        let mut saved: SavedResult =
            serde_json::from_slice(&bytes.bytes).map_err(|_| BrokerError::InvalidGrant)?;
        saved.request.validate()?;
        saved.selection.validate(&saved.request)?;
        if saved.schema != "louiselm.workspace.saved-result/1"
            || saved.request.request_id != id
            || saved.discarded
            || saved.accepted.is_some()
            || saved.application != PromotionStatus::NotRequested
            || saved.selection.run_id != saved.verification.producer.request.launch.run_id
            || saved.selection.run_id != saved.verification.execution.request.launch.run_id
            || !saved.verification.execution.commands_passed()
            || saved.verification.execution.job != saved.request.job
            || saved.verification.producer.job != saved.request.job
            || saved.verification.producer.request.launch.session_id
                != saved.request.producer_session_id
            || saved.verification.execution.request.launch.session_id
                != saved.request.verifier_session_id
            || Digest::of(
                &serde_json::to_vec(&saved.verification).map_err(|_| BrokerError::InvalidGrant)?,
            )
            .to_string()
                != saved.request.verification_digest
        {
            return Err(BrokerError::RequestMismatch);
        }
        if saved.request.destination.uid != uid {
            return Err(BrokerError::ControllerMismatch);
        }
        let changes = transfer::load(&directory.join("transfer"), &saved.request.job)?;
        let baseline = crate::workspace::snapshot_base_commit(
            &directory.join("transfer/snapshot"),
            &Digest::parse(&saved.request.job.snapshot_digest)
                .map_err(|_| BrokerError::InvalidGrant)?,
        )?;
        if saved.changes != changes.preview || saved.selection.expected_head != baseline {
            return Err(BrokerError::RequestMismatch);
        }
        saved.discarded = match read_record::<bool>(&directory.join("discarded.json"))? {
            None => false,
            Some(true) => true,
            Some(false) => return Err(BrokerError::InvalidGrant),
        };
        saved.accepted = read_record::<SavedAcceptance>(&directory.join("accepted.json"))?;
        saved.application = self.promotion_status(id)?;
        if let Some(accepted) = &saved.accepted {
            if saved.discarded
                || !super::selection::valid_commit(&accepted.commit)
                || Digest::parse(&accepted.approval_digest).is_err()
                || Digest::parse(&accepted.request_digest).is_err()
                || !matches!(saved.application, PromotionStatus::Completed { .. })
            {
                return Err(BrokerError::InvalidGrant);
            }
            sync_record(&root, "accepted.json")?;
        }
        // Reconfirm durability after an interrupted acknowledgement, without
        // treating a merely visible publication as a complete saved result.
        sync_record(&root, "published.json")?;
        if saved.discarded {
            sync_record(&root, "discarded.json")?;
        }
        root.sync_all().map_err(BrokerError::Storage)?;
        parent.sync_all().map_err(BrokerError::Storage)?;
        Ok(saved)
    }

    /// Saves immutable work before the caller may dispose its producer.
    /// Duplicate publication inspects the complete copy; failed publication grants no cleanup permission.
    /// # Errors
    /// Refuses stale/failed evidence, foreign ownership, mismatching duplicates and uncertain persistence.
    pub fn save_result<F>(
        &self,
        producer: &mut BrokerSession,
        request: &PromotionRequest,
        selection: &SavedSelection,
        mut verify: F,
    ) -> Result<SavedResult, BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        request.validate()?;
        selection.validate(request)?;
        let uid = producer.authorization().controller_uid;
        let path = self.saved_path(&request.request_id)?;
        if path.try_exists().map_err(BrokerError::Storage)? {
            let saved = self.saved_result(uid, &request.request_id)?;
            if saved.request != *request
                || saved.selection != *selection
                || saved.discarded
                || saved.accepted.is_some()
            {
                return Err(BrokerError::RequestMismatch);
            }
            return Ok(saved);
        }
        transfer::remaining(request.expires_at_ms)?;
        let verification = self.promotion_evidence(request, uid, &mut verify)?;
        if verification.producer.request.launch.digest().to_string()
            != producer.authorization().request_digest
        {
            return Err(BrokerError::RequestMismatch);
        }
        let (changes, source) =
            self.promotion_transfer(producer, request, &verification, &mut verify)?;
        if crate::workspace::snapshot_base_commit(
            &source.join("snapshot"),
            &Digest::parse(&request.job.snapshot_digest).map_err(|_| BrokerError::InvalidGrant)?,
        )? != selection.expected_head
        {
            return Err(BrokerError::RequestMismatch);
        }
        let saved = SavedResult {
            schema: "louiselm.workspace.saved-result/1".into(),
            request: request.clone(),
            selection: selection.clone(),
            verification,
            changes: changes.preview,
            discarded: false,
            accepted: None,
            application: PromotionStatus::NotRequested,
        };
        let parent = path.parent().ok_or(BrokerError::InvalidGrant)?;
        match fs::DirBuilder::new().mode(0o700).create(parent) {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(error) => return Err(BrokerError::Storage(error)),
        }
        private_directory(parent)?;
        sync_directory(&self.authorizations().root)?;
        let _guard = self.lifecycle.promotion_guard();
        self.promotion_evidence(request, uid, &mut verify)?;
        filesystem::publish(&path, |staging| {
            transfer::copy_transfer(
                &source.join("snapshot"),
                &source.join("job"),
                &request.job,
                &staging.join("transfer"),
            )?;
            filesystem::write_file(
                &staging.join("saved.json"),
                &serde_json::to_vec(&saved)?,
                0o400,
            )
        })?;
        write_new_record(
            &path.join("published.json"),
            &Digest::of(&serde_json::to_vec(&saved).map_err(|_| BrokerError::InvalidGrant)?)
                .to_string(),
        )?;
        self.saved_result(uid, &request.request_id)
    }

    /// Records explicit discard without application, continuation or authority renewal.
    /// # Errors
    /// Refuses foreign/corrupt results and any prior application intent, including uncertain effects.
    pub fn discard_saved_result(&self, uid: u32, id: &str) -> Result<SavedResult, BrokerError> {
        let _guard = self.lifecycle.promotion_guard();
        let saved = self.saved_result(uid, id)?;
        if self.promotion_status(id)? != PromotionStatus::NotRequested {
            return Err(BrokerError::RequestMismatch);
        }
        if !saved.discarded {
            write_new_record(&self.saved_path(id)?.join("discarded.json"), &true)?;
        }
        self.saved_result(uid, id)
    }

    /// Records the trusted operator's Git commit only after complete exact application.
    /// No checkout is opened by the broker and no execution or application is retried.
    /// # Errors
    /// Refuses conflicting decisions, foreign peers, incomplete effects and invalid attestations.
    pub fn accept_saved_result(
        &self,
        uid: u32,
        id: &str,
        accepted: &SavedAcceptance,
    ) -> Result<SavedResult, BrokerError> {
        let _guard = self.lifecycle.promotion_guard();
        let saved = self.saved_result(uid, id)?;
        if saved.discarded
            || !super::selection::valid_commit(&accepted.commit)
            || Digest::parse(&accepted.approval_digest).is_err()
        {
            return Err(BrokerError::RequestMismatch);
        }
        if let Some(prior) = &saved.accepted {
            return if prior == accepted {
                Ok(saved)
            } else {
                Err(BrokerError::RequestMismatch)
            };
        }
        let request: PromotionRequest =
            read_record(&self.promotion_path(id)?.join("request.json"))?
                .ok_or(BrokerError::RequestMismatch)?;
        let mut selected = request.clone();
        selected.expires_at_ms = saved.request.expires_at_ms;
        if selected != saved.request
            || accepted.request_digest
                != Digest::of(&serde_json::to_vec(&request).map_err(|_| BrokerError::InvalidGrant)?)
                    .to_string()
            || !matches!(self.promotion_status(id)?, PromotionStatus::Completed { result, .. } if result.complete)
        {
            return Err(BrokerError::RequestMismatch);
        }
        write_new_record(&self.saved_path(id)?.join("accepted.json"), accepted)?;
        self.saved_result(uid, id)
    }

    pub(super) fn saved_changes(
        &self,
        request: &PromotionRequest,
        uid: u32,
    ) -> Result<Option<crate::workspace::promotion::Changes>, BrokerError> {
        let path = self.saved_path(&request.request_id)?;
        if !path.try_exists().map_err(BrokerError::Storage)? {
            return Ok(None);
        }
        let saved = self.saved_result(uid, &request.request_id)?;
        let mut selected = request.clone();
        selected.expires_at_ms = saved.request.expires_at_ms;
        if selected != saved.request || saved.discarded {
            return Err(BrokerError::RequestMismatch);
        }
        Ok(Some(transfer::load(&path.join("transfer"), &request.job)?))
    }
}
