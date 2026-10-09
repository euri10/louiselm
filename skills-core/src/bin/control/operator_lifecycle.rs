//! Operator lifecycle requests are serialized on the existing owning worker.

use super::{QUERY_TIMEOUT, Queries, WorkerQuery};
use louiselm_skills::{
    broker::{
        BrokerError, BrokerSession, InstalledBroker,
        lifecycle::LifecycleCaller,
        operator::{self, InspectError},
    },
    launch_protocol::{LifecycleRequest, ProtocolError},
    launch_receipt::SignedReceipt,
};
use std::{
    sync::mpsc::{self, SyncSender},
    time::Instant,
};

impl Queries {
    pub(super) fn lifecycle(
        &self,
        broker: &InstalledBroker,
        request: &LifecycleRequest,
        deadline: Instant,
    ) -> Result<Result<SignedReceipt, ProtocolError>, InspectError> {
        operator::validate_lifecycle_request(request)?;
        let sender = self
            .sessions
            .lock()
            .map_err(|_| InspectError::StatusUnavailable)?
            .get(&request.session_id)
            .cloned();
        let Some(sender) = sender else {
            return match broker.recorded_lifecycle_result(
                &LifecycleCaller::Operator {
                    uid: self.operator_uid,
                },
                request,
            ) {
                Ok(Some(receipt)) => Ok(Ok(receipt)),
                Err(BrokerError::Policy(error)) => Ok(Err(error)),
                Err(BrokerError::UnknownAuthorization) => Err(InspectError::UnknownSession),
                Ok(None) | Err(_) => Err(InspectError::StatusUnavailable),
            };
        };
        request_lifecycle(&sender, request, deadline)
    }

    pub(super) fn answer_lifecycle(
        &self,
        broker: &InstalledBroker,
        session: &mut BrokerSession,
        request: &LifecycleRequest,
        expires: Instant,
        reply: &SyncSender<Result<Result<SignedReceipt, ProtocolError>, InspectError>>,
    ) -> bool {
        if Instant::now() >= expires {
            return false;
        }
        let caller = LifecycleCaller::Operator {
            uid: self.operator_uid,
        };
        let result = match broker.request_lifecycle(session, &caller, request) {
            Ok(receipt) => Ok(Ok(receipt)),
            Err(BrokerError::Policy(error)) => Ok(Err(error)),
            Err(_) => Err(InspectError::StatusUnavailable),
        };
        let terminal = matches!(&result, Ok(Ok(receipt))
            if receipt.payload.resulting_state == louiselm_skills::launch_receipt::SessionState::Terminal);
        // A vanished reader does not roll back the retained outcome.
        let _ = reply.send(result);
        if terminal {
            session.close();
        }
        terminal
    }
}

fn request_lifecycle(
    sender: &SyncSender<WorkerQuery>,
    request: &LifecycleRequest,
    deadline: Instant,
) -> Result<Result<SignedReceipt, ProtocolError>, InspectError> {
    let now = Instant::now();
    if now >= deadline {
        return Err(InspectError::StatusUnavailable);
    }
    let expires = deadline.min(now + QUERY_TIMEOUT);
    let (reply, receive) = mpsc::sync_channel(1);
    sender
        .try_send(WorkerQuery::Lifecycle {
            expires,
            request: Box::new(request.clone()),
            reply,
        })
        .map_err(|_| InspectError::StatusUnavailable)?;
    receive
        .recv_timeout(expires.saturating_duration_since(Instant::now()))
        .map_err(|_| InspectError::StatusUnavailable)?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{thread, time::Duration};

    #[test]
    #[allow(
        clippy::unwrap_used,
        reason = "Bounded queue fixture asserts deadlines and exact CAS."
    )]
    fn lifecycle_queue_never_renews_a_deadline_or_replays_after_timeout() {
        let request = LifecycleRequest {
            schema: louiselm_skills::launch_protocol::LIFECYCLE_REQUEST_SCHEMA.into(),
            protocol_version: louiselm_skills::launch::PROTOCOL_VERSION,
            request_id: "request".into(),
            session_id: "session".into(),
            run_id: "run".into(),
            authorization_id: "authorization".into(),
            action: louiselm_skills::launch_protocol::LifecycleAction::Park,
            expected_state: louiselm_skills::launch_receipt::SessionState::Running,
            expected_receipt_sequence: Some(1),
            envelope_revision: 1,
        };
        let (sender, receiver) = mpsc::sync_channel(1);
        assert_eq!(
            request_lifecycle(&sender, &request, Instant::now()),
            Err(InspectError::StatusUnavailable)
        );
        assert!(receiver.try_recv().is_err());
        let expected = request.clone();
        let deadline = Instant::now() + Duration::from_millis(50);
        let worker = thread::spawn(move || {
            let WorkerQuery::Lifecycle {
                expires,
                request,
                reply,
            } = receiver.recv_timeout(Duration::from_secs(1)).unwrap()
            else {
                return;
            };
            assert_eq!(*request, expected);
            assert_eq!(expires, deadline);
            thread::sleep(Duration::from_millis(100));
            assert!(reply.send(Err(InspectError::UnknownSession)).is_err());
            assert!(receiver.try_recv().is_err());
        });
        assert_eq!(
            request_lifecycle(&sender, &request, deadline),
            Err(InspectError::StatusUnavailable)
        );
        worker.join().unwrap();
    }
}
