//! Process-local proof that every handed-off Provider descriptor is gone.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, Weak},
};

use crate::{
    launch_protocol::{
        GUARD_RESUME_SCHEMA, GuardEnrollment, GuardResumeRequest, GuardScope, LifecycleRequest,
        PROTOCOL_VERSION,
    },
    launch_receipt::ReceiptHead,
};

use super::BrokerError;

#[derive(Default)]
struct Revision {
    enrollment: Option<GuardEnrollment>,
    authority: Option<GuardResumeRequest>,
    enrolled: bool,
    closed: bool,
    leases: Vec<Weak<()>>,
}

impl Revision {
    fn has_live_lease(&mut self) -> bool {
        self.leases.retain(|lease| lease.strong_count() != 0);
        !self.leases.is_empty()
    }
}

/// Shared by all concurrent broker workers, including reconnected Sessions.
#[derive(Default)]
pub(super) struct ProviderOwnership {
    revisions: Mutex<BTreeMap<(String, u64), Revision>>,
}

impl ProviderOwnership {
    pub(super) fn guarded_session(&self, session: &str) -> bool {
        self.revisions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .any(|((owner, _), entry)| owner == session && entry.enrollment.is_some())
    }
    pub(super) fn pending_resume(&self, request: &LifecycleRequest) -> Option<GuardResumeRequest> {
        self.revisions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .filter_map(|entry| entry.authority.as_ref())
            .find(|authority| authority.request == *request)
            .cloned()
    }

    pub(super) fn authorize_resume(
        &self,
        request: &LifecycleRequest,
        parked_head: &ReceiptHead,
        now_ns: u64,
    ) -> Result<GuardResumeRequest, BrokerError> {
        let mut revisions = self
            .revisions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for ((session, _), entry) in revisions.iter() {
            if session == &request.session_id
                && let Some(authority) = &entry.authority
                && authority.request.request_id == request.request_id
            {
                return if authority.request == *request
                    && authority.parked_head == *parked_head
                    && !entry.closed
                    && !entry.enrolled
                    && authority.scope.deadline_ns > now_ns
                {
                    Ok(authority.clone())
                } else {
                    Err(BrokerError::ProviderUnavailable)
                };
            }
        }
        let ((_, revision), prior) = revisions
            .iter_mut()
            .rev()
            .find(|((session, _), _)| session == &request.session_id)
            .ok_or(BrokerError::ProviderUnavailable)?;
        if !prior.closed || prior.has_live_lease() {
            return Err(BrokerError::ProviderUnavailable);
        }
        let mut enrollment = prior
            .enrollment
            .clone()
            .ok_or(BrokerError::ProviderUnavailable)?;
        if enrollment.scope.run_id != request.run_id
            || enrollment.scope.envelope_revision != request.envelope_revision
            || enrollment.scope.deadline_ns <= now_ns
        {
            return Err(BrokerError::RequestMismatch);
        }
        enrollment.scope.revision = revision
            .checked_add(1)
            .ok_or(BrokerError::ProviderUnavailable)?;
        let authority = GuardResumeRequest {
            schema: GUARD_RESUME_SCHEMA.into(),
            protocol_version: PROTOCOL_VERSION,
            request: request.clone(),
            parked_head: parked_head.clone(),
            scope: enrollment.scope.clone(),
        };
        authority.validate()?;
        revisions.insert(
            (request.session_id.clone(), authority.scope.revision),
            Revision {
                enrollment: Some(enrollment),
                authority: Some(authority.clone()),
                ..Revision::default()
            },
        );
        Ok(authority)
    }

    pub(super) fn permits_enrollment(
        &self,
        enrollment: &GuardEnrollment,
        request_id: &str,
    ) -> bool {
        let revisions = self
            .revisions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match revisions.get(&(
            enrollment.scope.session_id.clone(),
            enrollment.scope.revision,
        )) {
            Some(entry) => {
                !entry.closed
                    && !entry.enrolled
                    && entry.enrollment.as_ref() == Some(enrollment)
                    && entry
                        .authority
                        .as_ref()
                        .is_some_and(|authority| authority.request.request_id == request_id)
            }
            None => {
                enrollment.scope.revision == 1
                    && !revisions
                        .keys()
                        .any(|(session, _)| session == &enrollment.scope.session_id)
            }
        }
    }

    pub(super) fn listener(
        &self,
        enrollment: &GuardEnrollment,
        request_id: &str,
        lease: &Arc<()>,
    ) -> Result<(), BrokerError> {
        let scope = &enrollment.scope;
        let mut revisions = self
            .revisions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for ((session_id, revision), prior) in revisions.iter_mut() {
            if session_id == &scope.session_id
                && *revision != scope.revision
                && (*revision > scope.revision || !prior.closed || prior.has_live_lease())
            {
                return Err(BrokerError::ProviderUnavailable);
            }
        }
        let current = revisions
            .entry((scope.session_id.clone(), scope.revision))
            .or_default();
        if current.enrolled || current.closed {
            return Err(BrokerError::ProviderUnavailable);
        }
        if scope.revision != 1
            && (current.enrollment.as_ref() != Some(enrollment)
                || current
                    .authority
                    .as_ref()
                    .is_none_or(|authority| authority.request.request_id != request_id))
        {
            return Err(BrokerError::RequestMismatch);
        }
        current.enrollment = Some(enrollment.clone());
        current.enrolled = true;
        current.leases.push(Arc::downgrade(lease));
        Ok(())
    }

    pub(super) fn socket(&self, scope: &GuardScope, lease: &Arc<()>) -> Result<(), BrokerError> {
        let mut revisions = self
            .revisions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let current = revisions
            .get_mut(&(scope.session_id.clone(), scope.revision))
            .ok_or(BrokerError::ProviderUnavailable)?;
        if !current.enrolled || current.closed {
            return Err(BrokerError::ProviderUnavailable);
        }
        if current
            .enrollment
            .as_ref()
            .is_none_or(|enrollment| enrollment.scope != *scope)
        {
            return Err(BrokerError::ProviderUnavailable);
        }
        current.leases.push(Arc::downgrade(lease));
        Ok(())
    }

    pub(super) fn close(&self, scope: &GuardScope) -> Result<(), BrokerError> {
        let mut revisions = self
            .revisions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let current = revisions
            .entry((scope.session_id.clone(), scope.revision))
            .or_default();
        if current
            .enrollment
            .as_ref()
            .is_some_and(|enrollment| enrollment.scope != *scope)
        {
            return Err(BrokerError::ProviderUnavailable);
        }
        if current.has_live_lease() {
            return Err(BrokerError::ProviderUnavailable);
        }
        current.closed = true;
        Ok(())
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "Deterministic authority fixtures assert successful setup before inspecting exact values."
)]
mod tests {
    use super::*;

    fn enrollment() -> GuardEnrollment {
        GuardEnrollment {
            scope: GuardScope {
                session_id: "session".into(),
                run_id: "run".into(),
                envelope_revision: 7,
                revision: 1,
                deadline_ns: 100,
            },
            guard_id: 1,
            runtime_pid: 2,
            broker_pid: 3,
            address: std::net::SocketAddr::from(([127, 0, 0, 1], 9000)),
            listener_cookie: 4,
            network_id: 5,
        }
    }

    fn request(id: &str) -> LifecycleRequest {
        LifecycleRequest {
            schema: crate::launch_protocol::LIFECYCLE_REQUEST_SCHEMA.into(),
            protocol_version: PROTOCOL_VERSION,
            request_id: id.into(),
            session_id: "session".into(),
            run_id: "run".into(),
            authorization_id: "operator".into(),
            action: crate::launch_protocol::LifecycleAction::Resume,
            expected_state: crate::launch_receipt::SessionState::Parked,
            expected_receipt_sequence: Some(2),
            envelope_revision: 7,
        }
    }

    fn head() -> ReceiptHead {
        ReceiptHead {
            sequence: 2,
            digest: crate::Digest::of(b"park").to_string(),
        }
    }

    #[test]
    fn resume_preserves_original_authority_and_requires_all_descriptor_owners_closed() {
        let ownership = ProviderOwnership::default();
        let original = enrollment();
        assert!(!ownership.guarded_session(&original.scope.session_id));
        let lease = Arc::new(());
        assert!(ownership.listener(&original, "initial", &lease).is_ok());
        assert!(ownership.close(&original.scope).is_err());
        assert!(
            ownership
                .authorize_resume(&request("resume"), &head(), 50)
                .is_err()
        );
        drop(lease);
        assert!(ownership.close(&original.scope).is_ok());
        assert!(
            ownership.guarded_session(&original.scope.session_id),
            "original live broker retains guard identity across reconnect"
        );
        let authority = ownership
            .authorize_resume(&request("resume"), &head(), 50)
            .unwrap();
        let mut expected = original.clone();
        expected.scope.revision = 2;
        assert_eq!(authority.scope, expected.scope);
        assert_eq!(authority.request, request("resume"));
        assert_eq!(authority.parked_head, head());
        assert_eq!(
            ownership
                .authorize_resume(&request("resume"), &head(), 50)
                .unwrap(),
            authority
        );
        assert!(!ownership.permits_enrollment(&expected, "other-request"));
        let mut changed = expected.clone();
        changed.runtime_pid += 1;
        assert!(!ownership.permits_enrollment(&changed, "resume"));
        assert!(ownership.permits_enrollment(&expected, "resume"));
        assert!(
            ownership
                .listener(&expected, "resume", &Arc::new(()))
                .is_ok()
        );
        assert!(!ownership.permits_enrollment(&expected, "resume"));
    }

    #[test]
    fn failed_reservation_is_retired_and_next_request_uses_a_new_revision() {
        let ownership = ProviderOwnership::default();
        let original = enrollment();
        assert!(
            ownership
                .listener(&original, "initial", &Arc::new(()))
                .is_ok()
        );
        assert!(ownership.close(&original.scope).is_ok());
        let failed = ownership
            .authorize_resume(&request("failed"), &head(), 50)
            .unwrap();
        assert!(ownership.close(&failed.scope).is_ok());
        assert!(
            ownership
                .authorize_resume(&request("failed"), &head(), 50)
                .is_err()
        );
        let fresh = ownership
            .authorize_resume(&request("fresh"), &head(), 50)
            .unwrap();
        assert_eq!(fresh.scope.revision, 3);
        assert_eq!(fresh.scope.envelope_revision, 7);
        assert_eq!(fresh.scope.deadline_ns, original.scope.deadline_ns);
        let mut retired = original;
        retired.scope = failed.scope;
        assert!(!ownership.permits_enrollment(&retired, "failed"));
    }

    #[test]
    fn resume_refuses_expiry_and_changed_run_or_envelope() {
        let ownership = ProviderOwnership::default();
        let original = enrollment();
        assert!(
            ownership
                .listener(&original, "initial", &Arc::new(()))
                .is_ok()
        );
        assert!(ownership.close(&original.scope).is_ok());
        assert!(
            ownership
                .authorize_resume(&request("expired"), &head(), 100)
                .is_err()
        );
        let mut changed = request("wrong-run");
        changed.run_id = "other".into();
        assert!(ownership.authorize_resume(&changed, &head(), 50).is_err());
        changed = request("wrong-envelope");
        changed.envelope_revision += 1;
        assert!(ownership.authorize_resume(&changed, &head(), 50).is_err());
        assert_eq!(
            ownership
                .authorize_resume(&request("fresh"), &head(), 50)
                .unwrap()
                .scope
                .revision,
            2
        );
    }

    #[test]
    fn retired_networking_revision_keeps_its_high_water_mark() {
        let ownership = ProviderOwnership::default();
        let mut scope = GuardScope {
            session_id: "session".into(),
            run_id: "run".into(),
            envelope_revision: 1,
            revision: 2,
            deadline_ns: u64::MAX,
        };
        assert!(ownership.close(&scope).is_ok());

        scope.revision = 1;
        assert!(matches!(
            ownership.listener(
                &GuardEnrollment {
                    scope,
                    guard_id: 1,
                    runtime_pid: 2,
                    broker_pid: 3,
                    address: std::net::SocketAddr::from(([127, 0, 0, 1], 9000)),
                    listener_cookie: 4,
                    network_id: 5,
                },
                "initial",
                &Arc::new(())
            ),
            Err(BrokerError::ProviderUnavailable)
        ));
    }
}
