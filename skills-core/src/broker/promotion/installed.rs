//! Promotion verifies installed receipt signatures and existing operator ownership.

use super::{BrokerError, BrokerSession, InstalledBroker};
use crate::broker::promotion::{PromotionRequest, PromotionStatus, SavedResult};
use std::os::unix::net::UnixStream;

impl InstalledBroker {
    /// Publishes verified work while its original authenticated producer still exists.
    /// # Errors
    /// Refuses foreign ownership, signature/evidence failures and uncertain publication.
    pub fn save_result(
        &self,
        producer: &mut BrokerSession,
        request: &PromotionRequest,
    ) -> Result<SavedResult, BrokerError> {
        if producer.authorization().controller_uid != self.verifier.config().operator_uid {
            return Err(BrokerError::ControllerMismatch);
        }
        let mut failure = None;
        let saved = self
            .service
            .save_result(producer, request, |key, bytes, signature| {
                match self.verifier.verify(key, bytes, signature) {
                    Ok(()) => true,
                    Err(error) => {
                        failure = Some(error);
                        false
                    }
                }
            });
        if let Some(error) = failure {
            return Err(BrokerError::Verification(error));
        }
        let saved = saved?;
        let launch = &saved.verification.producer.request.launch;
        let head = self
            .service
            .receipts()
            .head(&launch.session_id)?
            .ok_or(BrokerError::ReceiptUnauthorized)?;
        let identity = crate::Digest::of(request.request_id.as_bytes());
        let disposal = crate::launch_protocol::LifecycleRequest {
            schema: crate::launch_protocol::LIFECYCLE_REQUEST_SCHEMA.into(),
            protocol_version: crate::launch_protocol::PROTOCOL_VERSION,
            request_id: format!("saved-disposal-{}", identity.hex()),
            authorization_id: format!("saved-disposal-{}", identity.hex()),
            session_id: launch.session_id.clone(),
            run_id: launch.run_id.clone(),
            action: crate::launch_protocol::LifecycleAction::Disposal,
            expected_state: crate::launch_receipt::SessionState::Parked,
            expected_receipt_sequence: Some(head.sequence),
            envelope_revision: launch.envelope_revision,
        };
        self.request_lifecycle(
            producer,
            &crate::broker::lifecycle::LifecycleCaller::Operator {
                uid: request.destination.uid,
            },
            &disposal,
        )?;
        Ok(saved)
    }

    /// Inspects private saved work without requiring its producer or granting authority.
    /// # Errors
    /// Refuses foreign operators, incomplete publication and changed bytes.
    pub fn saved_result(
        &self,
        uid: u32,
        id: &str,
        discard: bool,
    ) -> Result<SavedResult, BrokerError> {
        if uid != self.verifier.config().operator_uid {
            return Err(BrokerError::ControllerMismatch);
        }
        if discard {
            self.service.discard_saved_result(uid, id)
        } else {
            self.service.saved_result(uid, id)
        }
    }

    /// Applies saved work through the trusted operator after producer Disposal.
    /// # Errors
    /// Refuses missing/discarded work, signature/policy failures and uncertain effects.
    pub fn serve_saved_promotion(
        &self,
        result_id: &str,
        operator: UnixStream,
    ) -> Result<PromotionStatus, BrokerError> {
        let mut failure = None;
        let result =
            self.service
                .serve_saved_promotion(result_id, operator, |key, bytes, signature| {
                    match self.verifier.verify(key, bytes, signature) {
                        Ok(()) => true,
                        Err(error) => {
                            failure = Some(error);
                            false
                        }
                    }
                });
        match failure {
            Some(error) => Err(BrokerError::Verification(error)),
            None => result,
        }
    }

    /// Serves an explicit operator promotion through the original producer worker.
    /// Blocks on authenticated local transport; no root or broker checkout writes.
    /// The operator client owns checkout writer exclusion and local effect recovery.
    /// # Errors
    /// Refuses wrong installed operator, invalid signatures/evidence, conflicts,
    /// expiry, unapproved taint, verifier quarantine and unavailable transport or durable state.
    pub fn serve_promotion(
        &self,
        producer: &mut BrokerSession,
        operator: UnixStream,
    ) -> Result<PromotionStatus, BrokerError> {
        if producer.authorization().controller_uid != self.verifier.config().operator_uid {
            return Err(BrokerError::ControllerMismatch);
        }
        let mut failure = None;
        let result = self
            .service
            .serve_promotion(producer, operator, |key, bytes, signature| {
                match self.verifier.verify(key, bytes, signature) {
                    Ok(()) => true,
                    Err(error) => {
                        failure = Some(error);
                        false
                    }
                }
            });
        match failure {
            Some(error) => Err(BrokerError::Verification(error)),
            None => result,
        }
    }

    /// Reads historical promotion effects; never retries an interrupted write.
    /// # Errors
    /// Refuses corrupt or unavailable durable state and invalid operation identities.
    pub fn promotion_status(&self, id: &str) -> Result<PromotionStatus, BrokerError> {
        self.service.promotion_status(id)
    }
}
