//! Promotion verifies installed receipt signatures and existing operator ownership.

use super::{BrokerError, BrokerSession, InstalledBroker};
use crate::broker::promotion::PromotionStatus;
use std::os::unix::net::UnixStream;

impl InstalledBroker {
    /// Serves an explicit operator promotion through the original producer worker.
    /// Blocks on authenticated local transport; no root or broker checkout writes.
    /// The operator client owns checkout writer exclusion and local effect recovery.
    /// # Errors
    /// Refuses wrong installed operator, invalid signatures/evidence, conflicts,
    /// expiry, quarantine and unavailable transport or durable state.
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
