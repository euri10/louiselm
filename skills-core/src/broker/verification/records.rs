//! Revalidate current policy and canonical signed launch/cleanup chains on every read.

use super::{
    BrokerError, BrokerService, ReceiptOutcome, SessionState, VerificationExecution,
    VerificationExport, VerificationOperation, VerificationRecord, VerificationRequest,
    VerificationStatus, read_record, record_name,
};

impl BrokerService {
    pub(super) fn validate_export(&self, evidence: &VerificationExport) -> Result<(), BrokerError> {
        self.validate_export_with_history(evidence, false)
    }

    fn validate_export_with_history(
        &self,
        evidence: &VerificationExport,
        historical: bool,
    ) -> Result<(), BrokerError> {
        evidence.validate()?;
        if historical {
            self.verification_binding_historical(&evidence.request)?;
        } else {
            self.verification_binding(&evidence.request)?;
        }
        self.verification_integration(&evidence.request, &evidence.integration_digest)
    }

    fn verification_integration(
        &self,
        request: &VerificationRequest,
        digest: &str,
    ) -> Result<(), BrokerError> {
        if !self.receipts().chain(&request.launch.session_id)?.get(1).is_some_and(|receipt| {
            matches!(&receipt.payload.outcome, ReceiptOutcome::Start { evidence, .. } if evidence.tool_isolation_digest == digest)
        }) { return Err(BrokerError::ReceiptUnauthorized); }
        Ok(())
    }

    pub(super) fn verification_producer_with_history(
        &self,
        request: &VerificationRequest,
        historical: bool,
    ) -> Result<VerificationExport, BrokerError> {
        let VerificationOperation::Run {
            producer_session_id,
            export_request_id,
            export_digest,
            job_digest,
        } = &request.operation
        else {
            return Err(BrokerError::InvalidGrant);
        };
        let producer: VerificationExport =
            read_record(&self.export_path(producer_session_id, export_request_id)?)?
                .ok_or(BrokerError::InvalidGrant)?;
        self.validate_export_with_history(&producer, historical)?;
        let verifier = self.verification_binding(request)?;
        let source = if historical {
            self.verification_binding_historical(&producer.request)?
        } else {
            self.verification_binding(&producer.request)?
        };
        if producer.request.launch.session_id != *producer_session_id
            || producer.request.request_id != *export_request_id
            || producer.digest()?.to_string() != *export_digest
            || producer.job.job_digest != *job_digest
            || source.identity == verifier.identity
            || source.controller_uid != verifier.controller_uid
            || producer.request.launch.run_id != request.launch.run_id
            || producer.request.launch.skill_generation_id != request.launch.skill_generation_id
            || verifier.commands.is_some()
            || verifier.require_cold_recovery
        {
            return Err(BrokerError::RequestMismatch);
        }
        if historical {
            use crate::workspace::provenance::OutputProvenanceCode;
            if self
                .workspace_output_provenance(&producer.request.launch)?
                .code
                != OutputProvenanceCode::SessionOutputTainted
                || self.workspace_output_provenance(&request.launch)?.code
                    != OutputProvenanceCode::Untainted
            {
                return Err(BrokerError::ReceiptUnauthorized);
            }
        }
        Ok(producer)
    }

    pub(super) fn validate_execution_with_history(
        &self,
        request: &VerificationRequest,
        producer: &VerificationExport,
        evidence: &VerificationExecution,
        historical: bool,
    ) -> Result<(), BrokerError> {
        evidence.validate()?;
        if evidence.request != *request
            || evidence.job != producer.job
            || self.verification_producer_with_history(request, historical)? != *producer
        {
            return Err(BrokerError::RequestMismatch);
        }
        self.verification_integration(request, &evidence.integration_digest)
    }

    pub(super) fn validate_verification_record(
        &self,
        record: &VerificationRecord,
    ) -> Result<(), BrokerError> {
        self.validate_verification_record_with_history(record, false)
    }

    pub(super) fn validate_verification_record_with_history(
        &self,
        record: &VerificationRecord,
        historical: bool,
    ) -> Result<(), BrokerError> {
        let request = &record.execution.request;
        self.validate_execution_with_history(
            request,
            &record.producer,
            &record.execution,
            historical,
        )?;
        let receipts = self.receipts().chain(&request.launch.session_id)?;
        if !receipts.last().is_some_and(|receipt| {
            receipt.payload.sequence == record.terminal_head.sequence
                && receipt.digest().to_string() == record.terminal_head.digest
                && receipt.payload.sequence == request.head.sequence + 1
                && receipt.payload.resulting_state == SessionState::Terminal
                && receipt.payload.request_id
                    == format!(
                        "verification-cleanup-{}",
                        crate::Digest::of(request.request_id.as_bytes()).hex()
                    )
                && matches!(receipt.payload.outcome, ReceiptOutcome::Disposal { .. })
        }) {
            return Err(BrokerError::ReceiptUnauthorized);
        }
        Ok(())
    }

    pub(in crate::broker) fn tainted_verification_record(
        &self,
        session_id: &str,
    ) -> Result<VerificationRecord, BrokerError> {
        let name = record_name(session_id)?;
        let directory = self.authorizations().root.join("verification");
        let request =
            read_record::<VerificationRequest>(&directory.join(format!("intent-{name}")))?
                .ok_or(BrokerError::ReceiptUnauthorized)?;
        let record = read_record::<VerificationRecord>(&directory.join(format!("result-{name}")))?
            .ok_or(BrokerError::ReceiptUnauthorized)?;
        if request.launch.session_id != session_id || record.execution.request != request {
            return Err(BrokerError::RequestMismatch);
        }
        self.validate_verification_record_with_history(&record, true)?;
        Ok(record)
    }

    /// Reads current normalized evidence; spent-but-incomplete work remains Unknown.
    /// This never re-executes a job or treats prepared bytes as successful execution.
    /// # Errors
    /// Refuses unknown Sessions, malformed records or inconsistent trusted chains.
    pub fn verification_status(&self, session_id: &str) -> Result<VerificationStatus, BrokerError> {
        self.authorizations()
            .consumed_for_session(session_id)?
            .ok_or(BrokerError::UnknownAuthorization)?;
        let directory = self.authorizations().root.join("verification");
        let name = record_name(session_id)?;
        let Some(request) =
            read_record::<VerificationRequest>(&directory.join(format!("intent-{name}")))?
        else {
            return Ok(VerificationStatus::NotRequested);
        };
        request.validate()?;
        let VerificationOperation::Run {
            producer_session_id,
            ..
        } = &request.operation
        else {
            return Err(BrokerError::InvalidGrant);
        };
        if request.launch.session_id != session_id {
            return Err(BrokerError::RequestMismatch);
        }
        if self.lifecycle.is_quarantined(session_id)?
            || self.lifecycle.is_quarantined(producer_session_id)?
        {
            return Ok(VerificationStatus::Quarantined {
                output_provenance: self
                    .workspace_output_provenance_for_session(producer_session_id)
                    .unwrap_or_else(|_| crate::workspace::provenance::OutputProvenance::unknown()),
            });
        }
        let Some(record) =
            read_record::<VerificationRecord>(&directory.join(format!("result-{name}")))?
        else {
            return Ok(VerificationStatus::Unknown);
        };
        if record.execution.request != request {
            return Err(BrokerError::RequestMismatch);
        }
        self.validate_verification_record(&record)?;
        Ok(VerificationStatus::Completed(Box::new(record)))
    }
}
