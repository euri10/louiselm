//! Retained guard mechanics; policy and exact receipt admission remain in their owners.
use super::{
    Arc, AtomicBool, LaunchBroker, Ordering, RunningAgent, SandboxMechanicalState,
    SupervisorCompletion, SupervisorError, SystemRunningAgent, lock, map_guard,
};
use crate::launch_protocol::GuardResumeRequest;
use std::os::fd::AsFd;

impl SystemRunningAgent {
    pub(super) fn prepare_guarded_resume(
        &mut self,
        authority: Option<GuardResumeRequest>,
        broker: &Arc<dyn LaunchBroker>,
        complete: SupervisorCompletion<()>,
    ) -> Result<(), SupervisorError> {
        let Some(guard) = self.sender_guard.clone() else {
            complete(if authority.is_none() {
                Ok(())
            } else {
                Err(SupervisorError::IsolationRejected)
            });
            return Ok(());
        };
        let authority = authority.ok_or(SupervisorError::IsolationRejected)?;
        authority
            .validate()
            .map_err(|_| SupervisorError::IsolationRejected)?;
        if lock(&self.session)
            .mechanical_state()
            .map_err(|_| SupervisorError::CleanupUnproven)?
            != SandboxMechanicalState::Parked
        {
            return Err(SupervisorError::CleanupUnproven);
        }
        self.close_guard_handoff()?;
        let channel = broker.sender_guard_channel()?;
        let (prepared, revoker) = {
            let mut guard = lock(&guard);
            let prepared = guard
                .prepare_resume_handoff(authority.scope.clone(), channel)
                .map_err(map_guard);
            let revoker = guard.revoker().map_err(map_guard)?;
            (prepared, revoker)
        };
        self.guard_revoker = Some(revoker.clone());
        let (enrollment, descriptors) = prepared?;
        self.guard_scope = Some(authority.scope);
        self.guard_broker = Some(Arc::clone(broker));
        self.guard_resume_acknowledged = Arc::new(AtomicBool::new(false));
        let acknowledged = Arc::clone(&self.guard_resume_acknowledged);
        broker.send_guarded_listener(
            &authority.request.request_id,
            enrollment,
            descriptors.each_ref().map(AsFd::as_fd),
            Box::new(move |result| {
                // No guard lock on the reader: closure may be waiting for its next ACK.
                let result = result.and_then(|()| {
                    if revoker.active() {
                        Ok(())
                    } else {
                        Err(SupervisorError::IsolationRejected)
                    }
                });
                if result.is_ok() {
                    acknowledged.store(true, Ordering::Release);
                }
                complete(result);
            }),
        )
    }

    pub(super) fn activate_guarded_resume(&mut self, complete: SupervisorCompletion<()>) {
        let result = match &self.sender_guard {
            None => Ok(()),
            Some(guard) => self
                .guard_scope
                .as_ref()
                .ok_or(SupervisorError::IsolationRejected)
                .and_then(|scope| lock(guard).activate(scope).map_err(map_guard)),
        };
        complete(result);
    }
}
