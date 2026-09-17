//! Original producer worker plus an authenticated operator stream, never host execution.

use super::{
    BrokerError, BrokerService, OperatorMessage, PromotionRequest, PromotionStatus, Reply,
};
use crate::{
    Digest,
    broker::{
        BrokerSession, now_ms, read_record,
        service::send,
        sync_directory,
        verification::{VerificationRecord, VerificationStatus},
        write_new_record,
    },
    launch_protocol::{ResponseResult, VerificationOperation, VerificationRequest},
    workspace::promotion::{StepEvent, transfer},
};
use std::{fs, net::Shutdown, os::unix::net::UnixStream, path::Path};

impl BrokerService {
    pub(super) fn promotion_evidence<F>(
        &self,
        request: &PromotionRequest,
        uid: u32,
        verify: &mut F,
    ) -> Result<VerificationRecord, BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        request.validate()?;
        if request.destination.uid != uid {
            return Err(BrokerError::ControllerMismatch);
        }
        let VerificationStatus::Completed(record) =
            self.verification_status(&request.verifier_session_id)?
        else {
            return Err(BrokerError::ReceiptUnauthorized);
        };
        if !record.execution.commands_passed()
            || record.execution.job != request.job
            || Digest::of(&serde_json::to_vec(&record).map_err(|_| BrokerError::InvalidGrant)?)
                .to_string()
                != request.verification_digest
        {
            return Err(BrokerError::RequestMismatch);
        }
        for verification in [&record.producer.request, &record.execution.request] {
            let authorization = self
                .authorizations()
                .consumed_for_session(&verification.launch.session_id)?
                .ok_or(BrokerError::UnknownAuthorization)?;
            if authorization.controller_uid != uid
                || authorization.request_digest != verification.launch.digest().to_string()
            {
                return Err(BrokerError::ControllerMismatch);
            }
            self.verified_history(&authorization.launch_authorization(), verify)?;
        }
        if self
            .receipts()
            .head(&record.producer.request.launch.session_id)?
            .as_ref()
            != Some(&record.producer.request.head)
        {
            return Err(BrokerError::ReceiptUnauthorized);
        }
        Ok(*record)
    }

    /// Serves one explicit operator preview/commit on the original producer worker.
    /// The stream must be connected directly to the trusted operator applicator;
    /// kernel peer credentials are checked against canonical launch ownership.
    /// All I/O blocks this worker. Failure closes the stream and never retries effects.
    /// # Errors
    /// Refuses foreign peers, stale/failed/tainted evidence, replay conflicts, expiry,
    /// unsafe transfer bytes, lost transport and uncertain persistence/application.
    pub fn serve_promotion<F>(
        &self,
        producer: &mut BrokerSession,
        mut operator: UnixStream,
        mut verify: F,
    ) -> Result<PromotionStatus, BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        let result = self.promotion_transaction(producer, &mut operator, &mut verify);
        // Shutdown is only transport cleanup, never evidence of checkout rollback.
        let shutdown = operator.shutdown(Shutdown::Both);
        match result {
            Err(error) => Err(error),
            Ok(status) => {
                shutdown.map_err(BrokerError::Storage)?;
                Ok(status)
            }
        }
    }

    fn promotion_transaction<F>(
        &self,
        producer: &mut BrokerSession,
        operator: &mut UnixStream,
        verify: &mut F,
    ) -> Result<PromotionStatus, BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        transfer::configure_stream(operator)?;
        let uid = rustix::net::sockopt::socket_peercred(&*operator)
            .map_err(|e| BrokerError::Storage(e.into()))?
            .uid
            .as_raw();
        let request: PromotionRequest = transfer::receive(operator)?;
        request.validate()?;
        if uid == 0 || producer.authorization().controller_uid != uid {
            return Err(BrokerError::ControllerMismatch);
        }
        let directory = self.promotion_path(&request.request_id)?;
        if let Some(prior) = read_record::<PromotionRequest>(&directory.join("request.json"))? {
            if prior != request {
                return Err(BrokerError::RequestMismatch);
            }
            let status = self.promotion_status(&request.request_id)?;
            transfer::send(
                operator,
                &Reply::Finished {
                    status: status.clone(),
                },
            )?;
            return Ok(status);
        }
        transfer::remaining(request.expires_at_ms)?;
        let record = self.promotion_evidence(&request, uid, verify)?;
        if record.producer.request.launch.digest().to_string()
            != producer.authorization().request_digest
        {
            return Err(BrokerError::RequestMismatch);
        }
        let changes = self.promotion_transfer(producer, &request, &record, verify)?;
        transfer::send(operator, &Reply::Prepared)?;
        transfer::send_changes(operator, &changes, &request.job)?;
        let review_time = transfer::remaining(request.expires_at_ms)?;
        operator
            .set_read_timeout(Some(review_time))
            .map_err(BrokerError::Storage)?;
        let OperatorMessage::Commit { request_digest } = transfer::receive(operator)? else {
            return Err(BrokerError::InvalidGrant);
        };
        transfer::configure_stream(operator)?;
        if request_digest
            != Digest::of(&serde_json::to_vec(&request).map_err(|_| BrokerError::InvalidGrant)?)
                .to_string()
        {
            return Err(BrokerError::RequestMismatch);
        }
        {
            let _guard = self.lifecycle.promotion_guard();
            transfer::remaining(request.expires_at_ms)?;
            self.promotion_evidence(&request, uid, verify)?;
            let parent = directory.parent().ok_or(BrokerError::InvalidGrant)?;
            fs::create_dir_all(parent).map_err(BrokerError::Storage)?;
            sync_directory(&self.authorizations().root)?;
            fs::create_dir(&directory).map_err(BrokerError::Storage)?;
            sync_directory(parent)?;
            write_new_record(&directory.join("request.json"), &request)?;
            for id in [
                &request.verifier_session_id,
                &record.producer.request.launch.session_id,
            ] {
                self.retain_workspace_reference(id, |references| {
                    references.promotions.insert(request_digest.clone());
                })?;
            }
        }
        for index in 0..changes.steps() {
            self.promotion_step(operator, &request, &directory, index, uid, verify)?;
        }
        let OperatorMessage::Complete { result } = transfer::receive(operator)? else {
            return Err(BrokerError::InvalidGrant);
        };
        if !result.complete || result.completed_steps != changes.steps() {
            return Err(BrokerError::RequestMismatch);
        }
        write_new_record(&directory.join("result.json"), &result)?;
        let status = self.promotion_status(&request.request_id)?;
        transfer::send(
            operator,
            &Reply::Finished {
                status: status.clone(),
            },
        )?;
        Ok(status)
    }

    fn promotion_transfer<F>(
        &self,
        producer: &mut BrokerSession,
        request: &PromotionRequest,
        record: &VerificationRecord,
        verify: &mut F,
    ) -> Result<crate::workspace::promotion::Changes, BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        let transfer_request = VerificationRequest {
            request_id: format!(
                "promotion-{}",
                Digest::of(request.request_id.as_bytes()).hex()
            ),
            expires_at_ms: request.expires_at_ms,
            operation: VerificationOperation::Transfer {
                export_request_id: record.producer.request.request_id.clone(),
                export_digest: record.producer.digest()?.to_string(),
                job_digest: request.job.job_digest.clone(),
            },
            ..record.producer.request.clone()
        };
        self.verification_current(producer, &transfer_request, verify)?;
        send(producer.channel(), transfer_request.canonical_bytes())?;
        let response = self.verification_response(
            producer,
            &transfer_request,
            super::super::verification::deadline(&transfer_request, now_ms()?)?,
            verify,
        )?;
        let ResponseResult::VerificationTransfer {
            request: observed,
            directory: source,
        } = response
        else {
            return Err(BrokerError::RequestMismatch);
        };
        if *observed != transfer_request {
            return Err(BrokerError::RequestMismatch);
        }
        let changes = transfer::load(Path::new(&source), &request.job)?;
        self.verification_current(producer, &transfer_request, verify)?;
        Ok(changes)
    }

    fn promotion_step<F>(
        &self,
        operator: &mut UnixStream,
        request: &PromotionRequest,
        directory: &Path,
        index: usize,
        uid: u32,
        verify: &mut F,
    ) -> Result<(), BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        let OperatorMessage::Step {
            event: StepEvent::Begin { index: observed },
        } = transfer::receive(operator)?
        else {
            return Err(BrokerError::InvalidGrant);
        };
        if observed != index {
            return Err(BrokerError::RequestMismatch);
        }
        {
            // Durable grant is this effect's linearization point against quarantine.
            // A subsequently revoked grant may already have completed; never undo evidence.
            let _guard = self.lifecycle.promotion_guard();
            transfer::remaining(request.expires_at_ms)?;
            self.promotion_evidence(request, uid, verify)?;
            write_new_record(&directory.join(format!("{index}.grant")), &index)?;
        }
        transfer::send(operator, &Reply::Granted { index })?;
        let OperatorMessage::Step {
            event: StepEvent::Done { index: observed },
        } = transfer::receive(operator)?
        else {
            return Err(BrokerError::InvalidGrant);
        };
        if observed != index {
            return Err(BrokerError::RequestMismatch);
        }
        write_new_record(&directory.join(format!("{index}.done")), &index)?;
        transfer::send(operator, &Reply::Recorded { index })?;
        Ok(())
    }
}
