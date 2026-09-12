//! Broker-owned recovery evidence and admission at an exact authorized launch.

use serde::{Deserialize, Serialize};
use std::{fs, time::Instant};

use super::{
    BrokerError, BrokerService, BrokerSession,
    lifecycle::LifecycleCaller,
    read_record, record_name,
    service::{receive, send},
    sync_directory, write_new_record,
};
use crate::{
    launch_protocol::{
        BrokerConnection, ChannelState, RecoveryRequest, ResponseResult, RetentionEvidence,
    },
    launch_receipt::{ReceiptOutcome, SessionState},
    launch_transport::LauncherPacket,
};

/// Bounded broker status; a retained point may be lossy and a later load may fail.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum RecoveryReadiness {
    /// No supervisor-backed durable recovery point was registered.
    Unavailable,
    /// Original retention expiry passed; retries never renew it.
    Expired,
    /// Broker quarantine forbids using this evidence for admission.
    Quarantined,
    /// Exact durable evidence is current for this authorized Session.
    Ready {
        /// Immutable retention operation identity.
        operation_id: String,
        /// Original exclusive expiry.
        expires_at_ms: u64,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegisteredRecovery {
    request: RecoveryRequest,
    evidence: RetentionEvidence,
}

impl BrokerService {
    /// Registers recovery on the existing serialized Session worker.
    /// Caller identity comes from the trusted controller boundary, never a wire role.
    /// The supervisor validates storage asynchronously; this broker worker waits
    /// with bounded transport deadlines. Registration never Parks or Resumes.
    /// # Errors
    /// Refuses unauthorized callers, foreign/stale bindings, expired or conflicting
    /// operations, failed retention, unavailable storage or uncertain transport.
    pub fn register_recovery<F>(
        &self,
        session: &mut BrokerSession,
        caller: &LifecycleCaller,
        request: &RecoveryRequest,
        now_ms: u64,
        mut verify: F,
    ) -> Result<RetentionEvidence, BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        request.validate()?;
        let authorization = session.authorization();
        if !matches!(caller, LifecycleCaller::Operator { uid } if *uid == authorization.controller_uid)
            || request.launch.digest().to_string() != authorization.request_digest
            || request.launch.authorization_id != authorization.authorization_id
        {
            return Err(BrokerError::ControllerMismatch);
        }
        self.check_recovery_binding(request)?;
        let clock = Instant::now();
        let current_time = || {
            now_ms.saturating_add(u64::try_from(clock.elapsed().as_millis()).unwrap_or(u64::MAX))
        };
        if current_time() >= request.retention.expires_at_ms {
            return Err(BrokerError::Expired);
        }
        let deadline = clock
            .checked_add(std::time::Duration::from_millis(
                request.retention.expires_at_ms - now_ms,
            ))
            .ok_or(BrokerError::InvalidGrant)?;
        let directory = self.authorizations().root.join("recovery");
        fs::create_dir_all(&directory).map_err(BrokerError::Storage)?;
        sync_directory(&self.authorizations().root)?;
        let path = directory.join(record_name(&request.launch.session_id)?);
        let intent_path = directory.join(format!(
            "intent-{}",
            record_name(&request.launch.session_id)?
        ));
        if let Some(prior) = read_record::<RecoveryRequest>(&intent_path)? {
            if prior != *request {
                return Err(BrokerError::RequestMismatch);
            }
        } else {
            write_new_record(&intent_path, request)?;
        }
        // A failed revalidation must not leave a previous Ready record usable.
        // Keep this marker durable until exact evidence is acknowledged again.
        let pending_path = directory.join(format!(
            "pending-{}",
            record_name(&request.launch.session_id)?
        ));
        match read_record::<bool>(&pending_path)? {
            Some(true) => {}
            Some(false) => return Err(BrokerError::InvalidGrant),
            None => write_new_record(&pending_path, &true)?,
        }
        session.recovery_admitted_until = None;
        let result = (|| {
            let status = self.supervisor_status(session, &mut verify)?;
            if status.state != SessionState::Parked
                || status.channel_state != ChannelState::Revoked
                || status.broker_connection != BrokerConnection::Connected
                || status.pending_operation.is_some()
                || status.pending_receipt_count != 0
                || status.broker_head.as_ref() != Some(&request.head)
            {
                return Err(BrokerError::RequestMismatch);
            }
            send(session.channel(), request.canonical_bytes())?;
            let evidence = self.receive_recovery_evidence(session, request, &mut verify)?;
            self.validate_recovery_evidence(request, &evidence)?;
            // Recheck after asynchronous I/O before making readiness visible.
            let status = self.supervisor_status(session, &mut verify)?;
            if status.state != SessionState::Parked
                || status.pending_operation.is_some()
                || status.broker_head.as_ref() != Some(&request.head)
                || current_time() >= request.retention.expires_at_ms
            {
                return Err(BrokerError::Expired);
            }
            if let Some(prior) = read_record::<RegisteredRecovery>(&path)? {
                if prior.request != *request || prior.evidence != evidence {
                    return Err(BrokerError::RequestMismatch);
                }
                // Retry after a failed fsync must establish durability again.
                fs::File::open(&path)
                    .and_then(|file| file.sync_all())
                    .map_err(BrokerError::Storage)?;
                sync_directory(&directory)?;
            } else {
                write_new_record(
                    &path,
                    &RegisteredRecovery {
                        request: request.clone(),
                        evidence: evidence.clone(),
                    },
                )?;
            }
            fs::remove_file(&pending_path).map_err(BrokerError::Storage)?;
            sync_directory(&directory)?;
            if Instant::now() >= deadline {
                return Err(BrokerError::Expired);
            }
            session.recovery_admitted_until = Some(deadline);
            Ok(evidence)
        })();
        if result.is_err() && !matches!(result, Err(BrokerError::Policy(_))) {
            session.close();
        }
        result
    }

    /// Keeps the receipt/command stream serviced while awaiting retention.
    fn receive_recovery_evidence<F>(
        &self,
        session: &mut BrokerSession,
        request: &RecoveryRequest,
        verify: &mut F,
    ) -> Result<RetentionEvidence, BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        loop {
            let packet = receive(session.channel())?;
            if let LauncherPacket::Response(response) = &packet.packet {
                if response.request_id != request.retention.request_id {
                    return Err(BrokerError::RequestMismatch);
                }
                return match &response.result {
                    ResponseResult::RecoveryRetention { evidence } => Ok(evidence.clone()),
                    ResponseResult::Error { error } => Err(error.clone().into()),
                    _ => Err(BrokerError::RequestMismatch),
                };
            }
            self.lifecycle_packet(session, packet, verify)?;
        }
    }

    fn check_recovery_binding(&self, request: &RecoveryRequest) -> Result<(), BrokerError> {
        let launch = self
            .authorizations()
            .consumed_for_session(&request.launch.session_id)?
            .ok_or(BrokerError::UnknownAuthorization)?;
        if launch.request_digest != request.launch.digest().to_string()
            || launch.authorization_id != request.launch.authorization_id
            || launch.run_id != request.launch.run_id
            || launch.envelope_revision != request.launch.envelope_revision
            || self.lifecycle.is_quarantined(&launch.session_id)?
        {
            return Err(BrokerError::RequestMismatch);
        }
        let receipts = self.receipts().chain(&launch.session_id)?;
        if !receipts.iter().any(|receipt| {
            receipt.payload.sequence == request.head.sequence
                && receipt.digest().to_string() == request.head.digest
                && receipt.payload.resulting_state == SessionState::Parked
        }) {
            return Err(BrokerError::ReceiptUnauthorized);
        }
        Ok(())
    }

    fn validate_recovery_evidence(
        &self,
        request: &RecoveryRequest,
        evidence: &RetentionEvidence,
    ) -> Result<(), BrokerError> {
        request.validate()?;
        evidence.validate()?;
        self.check_recovery_binding(request)?;
        let receipts = self.receipts().chain(&request.launch.session_id)?;
        if evidence.launch != request.launch || evidence.request != request.retention
            || !receipts.get(1).is_some_and(|receipt| matches!(&receipt.payload.outcome,
                ReceiptOutcome::Start { evidence: start, .. } if start.tool_isolation_digest == evidence.integration_digest)) {
            return Err(BrokerError::RequestMismatch);
        }
        Ok(())
    }

    /// Reads durable evidence without inferring lossless recovery or future load success.
    /// # Errors
    /// Refuses unknown Sessions, malformed records and foreign evidence.
    pub fn recovery_readiness(
        &self,
        session_id: &str,
        now_ms: u64,
    ) -> Result<RecoveryReadiness, BrokerError> {
        self.authorizations()
            .consumed_for_session(session_id)?
            .ok_or(BrokerError::UnknownAuthorization)?;
        if self.lifecycle.is_quarantined(session_id)? {
            return Ok(RecoveryReadiness::Quarantined);
        }
        let pending_path = self
            .authorizations()
            .root
            .join("recovery")
            .join(format!("pending-{}", record_name(session_id)?));
        match read_record::<bool>(&pending_path)? {
            Some(true) => return Ok(RecoveryReadiness::Unavailable),
            Some(false) => return Err(BrokerError::InvalidGrant),
            None => {}
        }
        let path = self
            .authorizations()
            .root
            .join("recovery")
            .join(record_name(session_id)?);
        let Some(record) = read_record::<RegisteredRecovery>(&path)? else {
            return Ok(RecoveryReadiness::Unavailable);
        };
        if record.request.launch.session_id != session_id {
            return Err(BrokerError::RequestMismatch);
        }
        self.validate_recovery_evidence(&record.request, &record.evidence)?;
        if now_ms >= record.request.retention.expires_at_ms {
            return Ok(RecoveryReadiness::Expired);
        }
        Ok(RecoveryReadiness::Ready {
            operation_id: record.request.retention.request_id,
            expires_at_ms: record.request.retention.expires_at_ms,
        })
    }

    /// Enforces the trusted Run's recovery requirement before work dispatch.
    /// Ordinary work does not require recovery, including Agents without loadSession.
    /// This check grants no process, capability, Resume or reconstruction authority.
    /// # Errors
    /// Required recovery refuses absent, expired, quarantined or invalid evidence.
    pub fn admit_recovery(&self, session_id: &str, now_ms: u64) -> Result<(), BrokerError> {
        let launch = self
            .authorizations()
            .consumed_for_session(session_id)?
            .ok_or(BrokerError::UnknownAuthorization)?;
        if !launch.require_cold_recovery {
            return Ok(());
        }
        if matches!(
            self.recovery_readiness(session_id, now_ms)?,
            RecoveryReadiness::Ready { .. }
        ) {
            Ok(())
        } else {
            Err(BrokerError::InvalidGrant)
        }
    }
}
