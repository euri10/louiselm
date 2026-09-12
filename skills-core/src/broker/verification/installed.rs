//! Installed verification uses the same dedicated broker and installed signing authority.

use super::{BrokerError, BrokerSession, InstalledBroker, LifecycleCaller, Path, now_ms};
use crate::{
    Digest,
    broker::verification::{VerificationRecord, VerificationStatus},
    launch_protocol::{VerificationExport, VerificationRequest},
};

impl InstalledBroker {
    /// Stages exact baseline/plan bytes without executing candidate code.
    /// # Errors
    /// Refuses invalid identifiers, changed inputs and unavailable durable storage.
    pub fn stage_verification(
        &self,
        input_id: &str,
        snapshot: &Path,
        snapshot_digest: &Digest,
        plan: &Path,
        plan_digest: &Digest,
    ) -> Result<Digest, BrokerError> {
        self.service
            .stage_verification(input_id, snapshot, snapshot_digest, plan, plan_digest)
    }

    /// Observes the actual frozen producer through its authenticated supervisor.
    /// Run on the owning Session worker; the controller identity must be trusted.
    /// # Errors
    /// Refuses unauthorized/stale exports, signature failure and uncertain storage or transport.
    pub fn export_verification(
        &self,
        session: &mut BrokerSession,
        caller: &LifecycleCaller,
        request: &VerificationRequest,
    ) -> Result<VerificationExport, BrokerError> {
        let mut failure = None;
        let result = self.service.export_verification(
            session,
            caller,
            request,
            now_ms()?,
            |key, bytes, signature| match self.verifier.verify(key, bytes, signature) {
                Ok(()) => true,
                Err(error) => {
                    failure = Some(error);
                    false
                }
            },
        );
        match failure {
            Some(error) => Err(BrokerError::Verification(error)),
            None => result,
        }
    }

    /// Executes one exact approved job and proves disposal of the distinct verifier.
    /// Blocks only its owning broker worker; never receive concurrently on this Session.
    /// # Errors
    /// Refuses unauthorized, replayed, tainted or mismatched jobs and missing cleanup evidence.
    pub fn run_verification(
        &self,
        session: &mut BrokerSession,
        caller: &LifecycleCaller,
        request: &VerificationRequest,
    ) -> Result<VerificationRecord, BrokerError> {
        let mut failure = None;
        let result = self.service.run_verification(
            session,
            caller,
            request,
            now_ms()?,
            |key, bytes, signature| match self.verifier.verify(key, bytes, signature) {
                Ok(()) => true,
                Err(error) => {
                    failure = Some(error);
                    false
                }
            },
        );
        match failure {
            Some(error) => Err(BrokerError::Verification(error)),
            None => result,
        }
    }

    /// Reads durable actual outcomes and current quarantine applicability without effects.
    /// # Errors
    /// Refuses unknown Sessions, corrupt records and inconsistent trusted chains.
    pub fn verification_status(&self, session_id: &str) -> Result<VerificationStatus, BrokerError> {
        self.service.verification_status(session_id)
    }
}
