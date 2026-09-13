//! Installed verification at the operator-only cold Resume boundary.
use super::{
    BrokerError, ColdLoadOutcome, ColdResumeAllocation, LaunchRequest, LifecycleCaller,
    check_operator,
};
use crate::{
    broker::{BrokerSession, InstalledBroker},
    launch_protocol::RecoveryRestoreRequest,
};

impl InstalledBroker {
    /// Reserves one cold reconstruction and authorizes its distinct target launch.
    /// Run on the broker worker after authenticating the local operator.
    /// # Errors
    /// Refuses wrong operator, unavailable/expired recovery, signature failure,
    /// conflicting allocation, or uncertain durable authorization.
    pub fn authorize_cold_resume(
        &self,
        source_id: &str,
        target: &LaunchRequest,
        caller: &LifecycleCaller,
    ) -> Result<ColdResumeAllocation, BrokerError> {
        check_operator(caller, self.verifier.config().operator_uid)?;
        let mut failure = None;
        let result = self.service.authorize_cold_resume(
            source_id,
            target,
            caller,
            super::super::now_ms()?,
            |key, payload, signature| match self.verifier.verify(key, payload, signature) {
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

    /// Copies retained bytes through the owning supervisor into the frozen target.
    /// This blocks its serialized broker worker; it never directly accesses storage.
    /// # Errors
    /// Refuses wrong caller/state, failed verification, restore or durable evidence.
    pub fn restore_cold_resume(
        &self,
        session: &mut BrokerSession,
        caller: &LifecycleCaller,
    ) -> Result<RecoveryRestoreRequest, BrokerError> {
        check_operator(caller, self.verifier.config().operator_uid)?;
        let mut failure = None;
        let result = self.service.restore_cold_resume(
            session,
            caller,
            super::super::now_ms()?,
            |key, payload, signature| match self.verifier.verify(key, payload, signature) {
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

    /// Commits the trusted controller's exact load outcome and remaining authority.
    /// Run only on the serialized target worker; late/replayed results grant nothing.
    /// # Errors
    /// Refuses failed verification, foreign/expired binding or conflicting completion.
    pub fn finish_cold_resume(
        &self,
        session: &mut BrokerSession,
        caller: &LifecycleCaller,
        outcome: &ColdLoadOutcome,
    ) -> Result<(), BrokerError> {
        check_operator(caller, self.verifier.config().operator_uid)?;
        let mut failure = None;
        let result = self.service.finish_cold_resume(
            session,
            caller,
            outcome,
            super::super::now_ms()?,
            |key, payload, signature| match self.verifier.verify(key, payload, signature) {
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
}
