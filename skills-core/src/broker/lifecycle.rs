//! Durable broker lifecycle authorization, independent of supervisor mechanics.

use super::{
    BrokerError, corrupt, is_record_identifier, lock, read_record, sync_directory, write_new_record,
};
use crate::{
    Digest,
    launch_protocol::{
        CompletedRequest, ErrorCode, LaunchAuthorization, LifecycleAction, LifecycleRequest,
        ProtocolError, RequestDisposition, SupervisorStatus, evaluate_request, transition,
    },
    launch_receipt::{ReceiptAuthority, ReceiptOutcome, SessionState, SignedReceipt},
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
};

/// Authenticated caller and its current trusted Run policy.
///
/// Construct only at the broker's authenticated operator/coordinator boundary.
/// In particular, descendant membership and expiry come from the trusted Run
/// controller, never from an Agent request. This is intentionally not a wire type.
#[derive(Clone, Debug)]
pub enum LifecycleCaller {
    /// Operator identity authenticated by the local transport.
    Operator {
        /// Kernel-authenticated operator UID.
        uid: u32,
    },
    /// Coordinator scope already approved by the operator's Run controller.
    Coordinator {
        /// Authenticated coordinator Session.
        session_id: String,
        /// Run whose supervision tree supplied the descendants.
        run_id: String,
        /// Exact descendants within the current approved envelope.
        descendants: Vec<String>,
        /// Current approved envelope revision.
        envelope_revision: u64,
        /// Exclusive absolute expiry; request retries never renew it.
        expires_at_ms: u64,
    },
    /// An Agent capability channel has no lifecycle authority.
    Agent,
}

impl LifecycleCaller {
    pub(super) fn permits(
        &self,
        launch: &LaunchAuthorization,
        request: &LifecycleRequest,
        now_ms: u64,
    ) -> bool {
        self.allows(
            &request.session_id,
            &request.run_id,
            request.envelope_revision,
            request.action,
            launch,
            now_ms,
        )
    }

    /// The one scope rule shared by request authorization and status rendering.
    ///
    /// Status must never advertise an action the same caller would then be
    /// refused, so both paths ask this predicate rather than restating policy.
    fn allows(
        &self,
        target_session: &str,
        target_run: &str,
        target_revision: u64,
        action: LifecycleAction,
        launch: &LaunchAuthorization,
        now_ms: u64,
    ) -> bool {
        match self {
            Self::Operator { uid } => *uid == launch.controller_uid,
            Self::Coordinator {
                session_id,
                run_id,
                descendants,
                envelope_revision,
                expires_at_ms,
            } => {
                is_record_identifier(session_id)
                    && session_id != target_session
                    && run_id == target_run
                    && *envelope_revision == target_revision
                    && now_ms < *expires_at_ms
                    && action != LifecycleAction::Resume
                    && descendants.iter().any(|entry| entry == target_session)
            }
            Self::Agent => false,
        }
    }

    /// Lifecycle actions this caller may request against `launch` right now.
    ///
    /// The result is the mechanically valid transitions out of `state`, narrowed
    /// by this caller's scope and by a durable quarantine marker, which withdraws
    /// Resume while leaving every unrelated action intact. Ascending and
    /// duplicate-free, so
    /// [`SessionStatus::compose`](crate::launch_protocol::SessionStatus::compose)
    /// accepts it directly.
    ///
    /// This answers what policy permits, not whether the mechanic will succeed:
    /// the supervisor still owns process mechanics and can refuse.
    #[must_use]
    pub fn allowed_actions(
        &self,
        launch: &LaunchAuthorization,
        state: SessionState,
        quarantined: bool,
        now_ms: u64,
    ) -> Vec<LifecycleAction> {
        let mut actions: Vec<LifecycleAction> = [
            LifecycleAction::Park,
            LifecycleAction::Resume,
            LifecycleAction::Interrupt,
            LifecycleAction::Disposal,
        ]
        .into_iter()
        .filter(|action| transition(state, *action).is_ok())
        .filter(|action| !(quarantined && *action == LifecycleAction::Resume))
        .filter(|action| {
            self.allows(
                &launch.session_id,
                &launch.run_id,
                launch.envelope_revision,
                *action,
                launch,
                now_ms,
            )
        })
        .collect();
        actions.sort_unstable();
        actions
    }

    fn identity(&self) -> String {
        match self {
            Self::Operator { uid } => format!("operator:{uid}"),
            Self::Coordinator { session_id, .. } => format!("coordinator:{session_id}"),
            Self::Agent => "agent".into(),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AcceptedRequest {
    request: LifecycleRequest,
    caller: String,
}

/// Durable request identities owned by one broker instance.
///
/// The broker holds one instance for its state directory. Its mutex serializes
/// authorization and CAS across Session workers; the supervisor independently
/// serializes mechanics. Methods perform blocking disk I/O on broker workers.
pub struct LifecycleStore {
    root: PathBuf,
    preparing: Mutex<()>,
}

impl LifecycleStore {
    /// Serializes promotion admission with quarantine and lifecycle authorization.
    pub(super) fn promotion_guard(&self) -> std::sync::MutexGuard<'_, ()> {
        lock(&self.preparing)
    }
    /// Opens the broker-owned lifecycle request directory.
    ///
    /// # Errors
    /// Returns a storage failure when the directory cannot be made durable.
    pub fn open(root: &Path) -> Result<Self, BrokerError> {
        fs::create_dir_all(root).map_err(BrokerError::Storage)?;
        sync_directory(root)?;
        if let Some(parent) = root.parent() {
            sync_directory(parent)?;
        }
        Ok(Self {
            root: root.into(),
            preparing: Mutex::new(()),
        })
    }

    /// Authorizes and durably reserves one exact lifecycle request before dispatch.
    ///
    /// `receipts` must be the broker's verified canonical chain, never peer-supplied
    /// claims. Completed requests replay their exact receipt before current-state
    /// CAS. An unresolved request prevents another request from spending that
    /// Session's state, including after restart. Retrying it never changes bytes.
    ///
    /// # Errors
    /// Returns typed policy/CAS failures or unavailable/corrupt durable state.
    pub fn prepare(
        &self,
        launch: &LaunchAuthorization,
        status: &SupervisorStatus,
        caller: &LifecycleCaller,
        request: &LifecycleRequest,
        now_ms: u64,
        receipts: &[SignedReceipt],
    ) -> Result<RequestDisposition, BrokerError> {
        launch.validate()?;
        status.validate()?;
        request.validate()?;
        let refusal = |code| {
            BrokerError::Policy(ProtocolError::new(
                code,
                Some(status.state),
                status.broker_head.as_ref().map(|head| head.sequence),
            ))
        };
        if launch.session_id != request.session_id || launch.run_id != request.run_id {
            return Err(refusal(ErrorCode::SubjectMismatch));
        }
        if launch.envelope_revision != request.envelope_revision {
            return Err(refusal(ErrorCode::EnvelopeRevisionMismatch));
        }
        if !caller.permits(launch, request, now_ms) {
            return Err(refusal(ErrorCode::InvalidRequest));
        }
        let _guard = lock(&self.preparing);
        if request.action == LifecycleAction::Resume && self.is_quarantined(&request.session_id)? {
            return Err(refusal(ErrorCode::InvalidRequest));
        }
        let directory = self.root.join(&request.session_id).join("requests");
        let path = directory.join(format!(
            "{}.json",
            Digest::of(request.request_id.as_bytes()).hex()
        ));
        let accepted: Option<AcceptedRequest> = read_record(&path)?;
        if let Some(accepted) = &accepted
            && (accepted.request != *request || accepted.caller != caller.identity())
        {
            return Err(refusal(ErrorCode::RequestIdConflict));
        }
        let completed = receipts
            .iter()
            .find(|receipt| receipt.payload.request_id == request.request_id)
            .map(|receipt| CompletedRequest::new(request, receipt.clone()));
        if completed.is_some() && accepted.is_none() {
            return Err(refusal(ErrorCode::RequestIdConflict));
        }
        if let Some(error) = self.failure(request)? {
            return Err(error.into());
        }
        let disposition = evaluate_request(status, completed.as_ref(), request)?;
        if accepted.is_some() {
            // A previous write may have succeeded before its durability check failed.
            fs::File::open(&path)
                .and_then(|file| file.sync_all())
                .map_err(BrokerError::Storage)?;
            sync_directory(&directory)?;
            return Ok(disposition);
        }
        self.check_pending(&directory, receipts)
            .map_err(|error| match error {
                BrokerError::Policy(_) => refusal(ErrorCode::OperationPending),
                other => other,
            })?;
        fs::create_dir_all(&directory).map_err(BrokerError::Storage)?;
        sync_directory(&self.root)?;
        sync_directory(&self.root.join(&request.session_id))?;
        write_new_record(
            &path,
            &AcceptedRequest {
                request: request.clone(),
                caller: caller.identity(),
            },
        )?;
        Ok(disposition)
    }

    /// Durably prevents widening authority after an emergency quarantine.
    /// Revocation and Park still must be requested through the existing owner.
    ///
    /// # Errors
    /// Rejects invalid identifiers or unavailable durable state.
    pub fn quarantine(&self, session_id: &str) -> Result<(), BrokerError> {
        if !is_record_identifier(session_id) {
            return Err(BrokerError::InvalidGrant);
        }
        let _guard = lock(&self.preparing);
        let directory = self.root.join(session_id);
        let path = directory.join("quarantined.json");
        if self.is_quarantined(session_id)? {
            fs::File::open(path)
                .and_then(|file| file.sync_all())
                .map_err(BrokerError::Storage)?;
            return sync_directory(&directory);
        }
        fs::create_dir_all(&directory).map_err(BrokerError::Storage)?;
        sync_directory(&self.root)?;
        write_new_record(&path, &true)
    }

    /// Whether broker-owned quarantine still prohibits capability enablement.
    ///
    /// # Errors
    /// Rejects malformed identifiers or unreadable durable state.
    pub fn is_quarantined(&self, session_id: &str) -> Result<bool, BrokerError> {
        if !is_record_identifier(session_id) {
            return Err(BrokerError::InvalidGrant);
        }
        match read_record(&self.root.join(session_id).join("quarantined.json"))? {
            None => Ok(false),
            Some(true) => Ok(true),
            Some(false) => Err(corrupt("quarantine marker is invalid")),
        }
    }

    /// Persists one correlated supervisor refusal before returning it to a caller.
    /// An uncertain transport failure is not a refusal and must never reach here.
    ///
    /// # Errors
    /// Refuses an unknown/conflicting request, changed outcome or failed persistence.
    pub fn record_failure(
        &self,
        request: &LifecycleRequest,
        error: &ProtocolError,
    ) -> Result<(), BrokerError> {
        request.validate()?;
        error.validate()?;
        let _guard = lock(&self.preparing);
        let session = self.root.join(&request.session_id);
        let name = format!("{}.json", Digest::of(request.request_id.as_bytes()).hex());
        let accepted: AcceptedRequest =
            read_record(&session.join("requests").join(&name))?.ok_or(BrokerError::InvalidGrant)?;
        if accepted.request != *request {
            return Err(BrokerError::InvalidGrant);
        }
        let directory = session.join("failures");
        if let Some(stored) = self.failure(request)? {
            if stored != *error {
                return Err(BrokerError::InvalidGrant);
            }
            fs::File::open(directory.join(&name))
                .and_then(|file| file.sync_all())
                .map_err(BrokerError::Storage)?;
            return sync_directory(&directory);
        }
        fs::create_dir_all(&directory).map_err(BrokerError::Storage)?;
        sync_directory(&session)?;
        write_new_record(&directory.join(name), error)
    }

    fn failure(&self, request: &LifecycleRequest) -> Result<Option<ProtocolError>, BrokerError> {
        let path = self
            .root
            .join(&request.session_id)
            .join("failures")
            .join(format!(
                "{}.json",
                Digest::of(request.request_id.as_bytes()).hex()
            ));
        let error: Option<ProtocolError> = read_record(&path)?;
        if let Some(error) = &error {
            error.validate()?;
        }
        Ok(error)
    }

    /// Requires an authorized outcome to answer the broker's exact durable intent.
    /// Supervisor-observed causes retain their existing signed protocol authority.
    ///
    /// # Errors
    /// Rejects unknown, failed, changed or mismatched lifecycle authorizations.
    pub fn check_receipt(&self, receipt: &SignedReceipt) -> Result<(), BrokerError> {
        receipt.validate().map_err(BrokerError::ReceiptRefused)?;
        let (authority, action) = match &receipt.payload.outcome {
            ReceiptOutcome::Park {
                authority: ReceiptAuthority::Authorized(authority),
            } => (authority, LifecycleAction::Park),
            ReceiptOutcome::Resume { authorization } => (authorization, LifecycleAction::Resume),
            ReceiptOutcome::Interrupt { authorization } => {
                (authorization, LifecycleAction::Interrupt)
            }
            ReceiptOutcome::Disposal {
                authority: ReceiptAuthority::Authorized(authority),
            } => (authority, LifecycleAction::Disposal),
            _ => return Ok(()),
        };
        let path = self
            .root
            .join(&receipt.payload.session_id)
            .join("requests")
            .join(format!(
                "{}.json",
                Digest::of(authority.request_id.as_bytes()).hex()
            ));
        let accepted: AcceptedRequest =
            read_record(&path)?.ok_or(BrokerError::ReceiptUnauthorized)?;
        let request = &accepted.request;
        if request.authorization_id != authority.authorization_id
            || request.request_id != authority.request_id
            || request.digest().to_string() != authority.request_digest
            || request.action != action
            || request.session_id != receipt.payload.session_id
            || request.run_id != receipt.payload.run_id
            || request.envelope_revision != receipt.payload.envelope_revision
            || self.failure(request)?.is_some()
        {
            return Err(BrokerError::ReceiptUnauthorized);
        }
        Ok(())
    }

    fn check_pending(
        &self,
        directory: &Path,
        receipts: &[SignedReceipt],
    ) -> Result<(), BrokerError> {
        let entries = match fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(BrokerError::Storage(error)),
        };
        // ponytail: bounded scan of durable requests; index only if measured load requires it.
        for (index, entry) in entries.enumerate() {
            if index >= 4096 {
                return Err(corrupt("lifecycle request history exceeds its bound"));
            }
            let entry = entry.map_err(BrokerError::Storage)?;
            let accepted: AcceptedRequest = read_record(&entry.path())?
                .ok_or_else(|| corrupt("lifecycle request disappeared"))?;
            accepted.request.validate()?;
            if !receipts
                .iter()
                .any(|receipt| receipt.payload.request_id == accepted.request.request_id)
                && self.failure(&accepted.request)?.is_none()
            {
                return Err(ProtocolError::new(ErrorCode::OperationPending, None, None).into());
            }
        }
        Ok(())
    }
}
