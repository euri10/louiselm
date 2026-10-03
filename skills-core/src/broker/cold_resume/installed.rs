//! Installed verification at the operator-only cold Resume boundary.
use super::{
    BrokerError, ColdLoadOutcome, ColdResumeAllocation, LaunchRequest, LifecycleCaller,
    check_operator,
};
use crate::{
    broker::{BrokerSession, GrantRequest, InstalledBroker},
    launch_protocol::{ConformanceAuthorization, RecoveryRestoreRequest},
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
        let original = self
            .service
            .authorizations()
            .consumed_for_session(source_id)?
            .ok_or(BrokerError::UnknownAuthorization)?;
        // Reconstruction drops these capabilities; commands can only shrink.
        let child = GrantRequest {
            role: crate::launch_protocol::LaunchRole::Agent,
            dependencies: None,
            conformance: ConformanceAuthorization::default(),
            require_cold_recovery: original.require_cold_recovery,
            request: target.clone(),
            controller_uid: original.controller_uid,
            expires_at_ms: original.expires_at_ms,
            broker_loss_grace_ms: original.broker_loss_grace_ms,
            commands: original.commands,
            skill_requests: None,
            beads_mutations: None,
            provider_requests: None,
        };
        let now = super::super::now_ms()?;
        let mut failure = None;
        let result = self.run_envelopes.with_child(&child, now, |envelope| {
            if self
                .service
                .authorizations()
                .session_count_for_run(&target.run_id)?
                >= envelope.max_sessions as usize
                && !self
                    .service
                    .authorizations()
                    .has_session(&target.session_id)?
            {
                return Err(BrokerError::InvalidGrant);
            }
            self.service.authorize_cold_resume(
                source_id,
                target,
                caller,
                now,
                |key, payload, signature| match self.verifier.verify(key, payload, signature) {
                    Ok(()) => true,
                    Err(error) => {
                        failure = Some(error);
                        false
                    }
                },
            )
        });
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
