//! Spend job authority once, then require actual verifier disposal before publishing.

use super::{
    BrokerError, BrokerService, BrokerSession, Digest, Instant, LifecycleCaller, ReceiptHead,
    ResponseResult, SessionState, VerificationRecord, VerificationRequest, deadline, record_name,
    send, write_new_record,
};
use crate::{
    launch::PROTOCOL_VERSION,
    launch_protocol::{LIFECYCLE_REQUEST_SCHEMA, LifecycleAction, LifecycleRequest},
};

impl BrokerService {
    /// Runs the exact job in a fresh distinct configured Agent Session and disposes it.
    /// Only the trusted controller can authorize this operation. No Agent tool packet
    /// is needed; ordinary tool approvals are absent on this dedicated verifier launch.
    /// Execute on the Session's broker worker, never beside another channel receiver.
    /// # Errors
    /// Refuses stale/foreign/tainted/replayed jobs, command authority on the verifier,
    /// uncertain transport, missing actual outcomes or unproven terminal cleanup.
    /// Failures after admission leave durable spent authority and close the channel.
    pub fn run_verification<F>(
        &self,
        session: &mut BrokerSession,
        caller: &LifecycleCaller,
        request: &VerificationRequest,
        now_ms: u64,
        mut verify: F,
    ) -> Result<VerificationRecord, BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        self.authorize_verification(session, caller, request)?;
        let producer = self.verification_producer_with_history(request, false)?;
        let deadline = deadline(request, now_ms)?;
        let directory = self.verification_directory()?;
        let name = record_name(&request.launch.session_id)?;
        // Immutable per-launch intent: no retry can refund an unknown start, including after restart.
        write_new_record(&directory.join(format!("intent-{name}")), request)?;
        let result = (|| {
            self.verification_current(session, request, &mut verify)?;
            if Instant::now() >= deadline {
                return Err(BrokerError::Expired);
            }
            send(session.channel(), request.canonical_bytes())?;
            let response = match self.verification_response(session, request, deadline, &mut verify)
            {
                Err(BrokerError::Policy(error)) => {
                    // A correlated mechanic refusal still has a usable authenticated channel.
                    // Dispose before closing it; missing job observations stay Unknown.
                    self.dispose_verifier(session, caller, request, now_ms, &mut verify)?;
                    return Err(BrokerError::Policy(error));
                }
                result => result?,
            };
            let ResponseResult::VerificationExecution { evidence } = response else {
                return Err(BrokerError::RequestMismatch);
            };
            self.validate_execution_with_history(request, &producer, &evidence, false)?;
            // Record partial/nonzero outcomes before asking for whole-Session cleanup.
            write_new_record(&directory.join(format!("execution-{name}")), &evidence)?;
            self.verification_current(session, request, &mut verify)?;
            let terminal_head =
                self.dispose_verifier(session, caller, request, now_ms, &mut verify)?;
            let record = VerificationRecord {
                producer,
                execution: evidence,
                terminal_head,
            };
            self.validate_verification_record_with_history(&record, false)?;
            if Instant::now() >= deadline {
                return Err(BrokerError::Expired);
            }
            write_new_record(&directory.join(format!("result-{name}")), &record)?;
            let digest =
                Digest::of(&serde_json::to_vec(&record).map_err(|_| BrokerError::InvalidGrant)?)
                    .to_string();
            for id in [
                &request.launch.session_id,
                &record.producer.request.launch.session_id,
            ] {
                self.retain_workspace_reference(id, |references| {
                    references.verifications.insert(digest.clone());
                })?;
            }
            Ok(record)
        })();
        if result.is_err() {
            session.close();
        }
        result
    }

    fn dispose_verifier<F>(
        &self,
        session: &mut BrokerSession,
        caller: &LifecycleCaller,
        request: &VerificationRequest,
        now_ms: u64,
        verify: &mut F,
    ) -> Result<ReceiptHead, BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        let disposal = LifecycleRequest {
            schema: LIFECYCLE_REQUEST_SCHEMA.into(),
            protocol_version: PROTOCOL_VERSION,
            request_id: format!(
                "verification-cleanup-{}",
                Digest::of(request.request_id.as_bytes()).hex()
            ),
            session_id: request.launch.session_id.clone(),
            run_id: request.launch.run_id.clone(),
            authorization_id: format!(
                "verification-cleanup-{}",
                Digest::of(&request.canonical_bytes()).hex()
            ),
            action: LifecycleAction::Disposal,
            expected_state: SessionState::Running,
            expected_receipt_sequence: Some(request.head.sequence),
            envelope_revision: request.launch.envelope_revision,
        };
        let terminal = self.request_lifecycle(session, caller, &disposal, now_ms, verify)?;
        Ok(ReceiptHead {
            sequence: terminal.payload.sequence,
            digest: terminal.digest().to_string(),
        })
    }
}
