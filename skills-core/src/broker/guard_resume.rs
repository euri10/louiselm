//! Immutable broker authorization for fresh networking within a retained Session.
use super::{AcceptedRequest, LifecycleCaller, LifecycleStore};
use crate::{
    Digest,
    broker::{
        BrokerError, BrokerService, BrokerSession, lock, read_record, sync_directory,
        write_new_record,
    },
    launch_protocol::{
        ErrorCode, GuardResumeRequest, LifecycleRequest, ProtocolError, SupervisorStatus,
    },
};
use std::fs;

impl LifecycleStore {
    pub(in crate::broker) fn recorded_guard_resume(
        &self,
        request: &LifecycleRequest,
    ) -> Result<GuardResumeRequest, BrokerError> {
        let path = self
            .root
            .join(&request.session_id)
            .join("networking")
            .join(format!(
                "{}.json",
                Digest::of(request.request_id.as_bytes()).hex()
            ));
        let authority: GuardResumeRequest =
            read_record(&path)?.ok_or(BrokerError::ReceiptUnauthorized)?;
        authority.validate()?;
        if authority.request != *request {
            return Err(BrokerError::RequestMismatch);
        }
        Ok(authority)
    }

    fn record_guard_resume(
        &self,
        caller: &LifecycleCaller,
        authority: &GuardResumeRequest,
    ) -> Result<(), BrokerError> {
        authority.validate()?;
        let _guard = lock(&self.preparing);
        let session = self.root.join(&authority.request.session_id);
        let name = format!(
            "{}.json",
            Digest::of(authority.request.request_id.as_bytes()).hex()
        );
        let accepted: AcceptedRequest =
            read_record(&session.join("requests").join(&name))?.ok_or(BrokerError::InvalidGrant)?;
        if !matches!(caller, LifecycleCaller::Operator { .. })
            || accepted.caller != caller.identity()
            || accepted.request != authority.request
        {
            return Err(BrokerError::InvalidGrant);
        }
        let directory = session.join("networking");
        let path = directory.join(name);
        if let Some(stored) = read_record::<GuardResumeRequest>(&path)? {
            if stored != *authority {
                return Err(BrokerError::RequestMismatch);
            }
            fs::File::open(&path)
                .and_then(|file| file.sync_all())
                .map_err(BrokerError::Storage)?;
            return sync_directory(&directory);
        }
        fs::create_dir_all(&directory).map_err(BrokerError::Storage)?;
        sync_directory(&session)?;
        write_new_record(&path, authority)
    }
}

impl BrokerService {
    pub(in crate::broker) fn prepare_guard_resume(
        &self,
        session: &BrokerSession,
        caller: &LifecycleCaller,
        request: &LifecycleRequest,
        status: &SupervisorStatus,
    ) -> Result<GuardResumeRequest, BrokerError> {
        let head = status
            .broker_head
            .as_ref()
            .ok_or(BrokerError::ReceiptUnauthorized)?;
        let now = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
        let now_ns = u64::try_from(now.tv_sec)
            .ok()
            .and_then(|seconds| seconds.checked_mul(1_000_000_000))
            .and_then(|seconds| {
                u64::try_from(now.tv_nsec)
                    .ok()
                    .and_then(|ns| seconds.checked_add(ns))
            })
            .ok_or(BrokerError::InvalidGrant)?;
        let authority = self
            .provider_ownership
            .authorize_resume(request, head, now_ns)
            .map_err(|_| {
                BrokerError::Policy(ProtocolError::new(
                    ErrorCode::LifecycleMechanicUnavailable,
                    Some(status.state),
                    Some(head.sequence),
                ))
            })?;
        if session.authorization().session_id != authority.scope.session_id {
            return Err(BrokerError::RequestMismatch);
        }
        if let Err(error) = self.lifecycle.record_guard_resume(caller, &authority) {
            self.provider_ownership.close(&authority.scope)?;
            return Err(error);
        }
        Ok(authority)
    }
}
