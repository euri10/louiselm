//! Exact broker-directed jobs; prepared bytes alone never establish verification.

mod execution;
mod records;

use super::{
    BrokerError, BrokerService, BrokerSession, PendingAuthorization,
    lifecycle::LifecycleCaller,
    read_record, record_name,
    service::{receive_for, send},
    sync_directory, write_new_record,
};
use crate::{
    Digest,
    launch_protocol::{
        BrokerConnection, ChannelState, ResponseResult, VerificationExecution, VerificationExport,
        VerificationOperation, VerificationRequest,
    },
    launch_receipt::{ReceiptHead, ReceiptOutcome, SessionState},
    launch_transport::LauncherPacket,
    workspace::provenance::OutputProvenance,
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// Durable actual results plus the terminal cleanup receipt from the verifier.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationRecord {
    /// Authenticated observation from the actual frozen producer.
    pub producer: VerificationExport,
    /// Actual exact-plan command outcomes from the distinct verifier.
    pub execution: VerificationExecution,
    /// Signed, durably acknowledged verifier disposal in the broker receipt chain.
    pub terminal_head: ReceiptHead,
}

/// Current applicability of durable evidence; never an installed promotion claim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VerificationStatus {
    /// No authority was spent for this verifier.
    NotRequested,
    /// Authority was spent but complete authenticated cleanup evidence is absent.
    Unknown,
    /// Producer or verifier quarantine invalidates the job's applicability.
    Quarantined {
        /// Current producer output provenance; unknown when its binding is unavailable.
        output_provenance: OutputProvenance,
    },
    /// The observed command prefix and verifier disposal have durable actual outcomes.
    Completed(Box<VerificationRecord>),
}

impl BrokerService {
    /// Copies exact approved baseline and plan bytes into private broker storage.
    /// This blocking worker operation executes nothing and grants no job authority.
    /// # Errors
    /// Refuses duplicate identifiers, malformed inputs, digest mismatch or failed storage.
    pub fn stage_verification(
        &self,
        input_id: &str,
        snapshot: &Path,
        snapshot_digest: &Digest,
        plan: &Path,
        plan_digest: &Digest,
    ) -> Result<Digest, BrokerError> {
        record_name(input_id)?;
        match fs::DirBuilder::new()
            .mode(0o700)
            .create(&self.verification_inputs)
        {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(error) => return Err(BrokerError::Storage(error)),
        }
        sync_directory(
            self.verification_inputs
                .parent()
                .ok_or(BrokerError::InvalidGrant)?,
        )?;
        Ok(crate::workspace::verification::stage_inputs(
            snapshot,
            snapshot_digest,
            plan,
            plan_digest,
            &self.verification_inputs.join(input_id),
        )?)
    }

    /// Captures the actual Parked producer through its original supervisor channel.
    /// The trusted controller selects exact staged inputs. This does not Park implicitly.
    /// Run on the owning broker worker; transport completions remain asynchronous.
    /// # Errors
    /// Refuses unauthorized, stale, quarantined or contradictory evidence and uncertain I/O.
    pub fn export_verification<F>(
        &self,
        session: &mut BrokerSession,
        caller: &LifecycleCaller,
        request: &VerificationRequest,
        now_ms: u64,
        mut verify: F,
    ) -> Result<VerificationExport, BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        self.authorize_verification(session, caller, request)?;
        if !matches!(request.operation, VerificationOperation::Export { .. }) {
            return Err(BrokerError::InvalidGrant);
        }
        let deadline = deadline(request, now_ms)?;
        self.verification_directory()?;
        let path = self.export_path(&request.launch.session_id, &request.request_id)?;
        let result = (|| {
            self.verification_current(session, request, &mut verify)?;
            send(session.channel(), request.canonical_bytes())?;
            let ResponseResult::VerificationExport { evidence } =
                self.verification_response(session, request, deadline, &mut verify)?
            else {
                return Err(BrokerError::RequestMismatch);
            };
            if evidence.request != *request {
                return Err(BrokerError::RequestMismatch);
            }
            self.validate_export(&evidence)?;
            self.verification_current(session, request, &mut verify)?;
            if Instant::now() >= deadline {
                return Err(BrokerError::Expired);
            }
            if let Some(prior) = read_record::<VerificationExport>(&path)? {
                if prior != evidence {
                    return Err(BrokerError::RequestMismatch);
                }
                fs::File::open(&path)
                    .and_then(|file| file.sync_all())
                    .map_err(BrokerError::Storage)?;
                sync_directory(path.parent().ok_or(BrokerError::InvalidGrant)?)?;
            } else {
                write_new_record(&path, &evidence)?;
            }
            let digest = evidence.digest()?.to_string();
            self.retain_workspace_reference(&request.launch.session_id, |references| {
                references.exports.insert(digest);
                references
                    .bundles
                    .insert(evidence.job.bundle_digest.clone());
            })?;
            Ok(evidence)
        })();
        if result.is_err() {
            session.close();
        }
        result
    }

    fn authorize_verification(
        &self,
        session: &BrokerSession,
        caller: &LifecycleCaller,
        request: &VerificationRequest,
    ) -> Result<(), BrokerError> {
        request.validate()?;
        if !matches!(caller, LifecycleCaller::Operator { uid } if *uid == session.authorization().controller_uid)
            || request.launch.digest().to_string() != session.authorization().request_digest
        {
            return Err(BrokerError::ControllerMismatch);
        }
        self.verification_binding(request)?;
        Ok(())
    }

    fn verification_binding(
        &self,
        request: &VerificationRequest,
    ) -> Result<PendingAuthorization, BrokerError> {
        request.validate()?;
        let launch = self
            .authorizations()
            .consumed_for_session(&request.launch.session_id)?
            .ok_or(BrokerError::UnknownAuthorization)?;
        if launch.request_digest != request.launch.digest().to_string()
            || self.lifecycle.is_quarantined(&launch.session_id)?
        {
            return Err(BrokerError::RequestMismatch);
        }
        let state = match request.operation {
            VerificationOperation::Export { .. } | VerificationOperation::Transfer { .. } => {
                SessionState::Parked
            }
            VerificationOperation::Run { .. } => SessionState::Running,
        };
        if !self
            .receipts()
            .chain(&launch.session_id)?
            .iter()
            .any(|receipt| {
                receipt.payload.sequence == request.head.sequence
                    && receipt.digest().to_string() == request.head.digest
                    && receipt.payload.resulting_state == state
            })
        {
            return Err(BrokerError::ReceiptUnauthorized);
        }
        Ok(launch)
    }

    pub(super) fn verification_current<F>(
        &self,
        session: &mut BrokerSession,
        request: &VerificationRequest,
        verify: &mut F,
    ) -> Result<(), BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        self.verification_binding(request)?;
        let status = self.supervisor_status(session, verify)?;
        let state = match request.operation {
            VerificationOperation::Export { .. } | VerificationOperation::Transfer { .. } => {
                SessionState::Parked
            }
            VerificationOperation::Run { .. } => SessionState::Running,
        };
        if status.state != state
            || status.broker_head.as_ref() != Some(&request.head)
            || status.broker_connection != BrokerConnection::Connected
            || status.pending_operation.is_some()
            || status.pending_receipt_count != 0
            || (state == SessionState::Parked && status.channel_state != ChannelState::Revoked)
        {
            return Err(BrokerError::RequestMismatch);
        }
        Ok(())
    }

    pub(super) fn verification_response<F>(
        &self,
        session: &mut BrokerSession,
        request: &VerificationRequest,
        deadline: Instant,
        verify: &mut F,
    ) -> Result<ResponseResult, BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or(BrokerError::Expired)?;
            let packet = receive_for(session.channel(), remaining)?;
            if let LauncherPacket::Response(response) = packet.packet {
                if response.request_id != request.request_id {
                    return Err(BrokerError::RequestMismatch);
                }
                return match response.result {
                    ResponseResult::Error { error } => Err(error.into()),
                    result => Ok(result),
                };
            }
            self.lifecycle_packet(session, packet, verify)?;
        }
    }

    fn verification_directory(&self) -> Result<PathBuf, BrokerError> {
        let directory = self.authorizations().root.join("verification");
        fs::create_dir_all(&directory).map_err(BrokerError::Storage)?;
        sync_directory(&self.authorizations().root)?;
        Ok(directory)
    }

    fn export_path(&self, session: &str, request: &str) -> Result<PathBuf, BrokerError> {
        record_name(session)?;
        record_name(request)?;
        let key = Digest::of(
            &serde_json::to_vec(&(session, request)).map_err(|_| BrokerError::InvalidGrant)?,
        );
        Ok(self
            .authorizations()
            .root
            .join("verification")
            .join(format!("export-{}.json", key.hex())))
    }
}

pub(super) fn deadline(request: &VerificationRequest, now_ms: u64) -> Result<Instant, BrokerError> {
    let budget = request
        .expires_at_ms
        .checked_sub(now_ms)
        .filter(|ms| (1..=3_660_000).contains(ms))
        .ok_or(BrokerError::Expired)?;
    Instant::now()
        .checked_add(Duration::from_millis(budget))
        .ok_or(BrokerError::InvalidGrant)
}
