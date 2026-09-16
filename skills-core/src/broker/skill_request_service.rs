//! Authenticated request orchestration, separate from Admission and lifecycle authority.

use super::attention::{AttentionEndpoint, AttentionSubject};
use super::service::send;
use super::skill_requests::Binding;
use super::{BrokerError, BrokerService, BrokerSession};
use crate::{
    launch_protocol::{ChannelState, CommandMessage, CommandOperation, ErrorCode},
    launch_receipt::SessionState,
    skill_request::{SkillRequestOutcome, SkillRequestStatus, SkillSubject},
};

impl BrokerService {
    pub(super) fn answer_skill_request<F>(
        &self,
        session: &mut BrokerSession,
        query: &CommandMessage,
        now_ms: u64,
        endpoint: Option<&AttentionEndpoint>,
        verify: &mut F,
    ) -> Result<(), BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        query.validate()?;
        self.require_trusted_history(session, verify)?;
        let clock = std::time::Instant::now();
        let CommandOperation::SkillRequest { request } = &query.operation else {
            return Err(BrokerError::InvalidGrant);
        };
        let accepted = (|| {
            let auth = session.authorization();
            if query.session_id != auth.session_id
                || query.run_id != auth.run_id
                || query.envelope_revision != auth.envelope_revision
            {
                return Err(BrokerError::RequestMismatch);
            }
            let approved = self
                .authorizations()
                .consumed_for_session(&auth.session_id)?
                .ok_or(BrokerError::UnknownAuthorization)?;
            if !approved
                .skill_requests
                .as_ref()
                .is_some_and(|permission| permission.permits(request, now_ms))
            {
                return Err(BrokerError::InvalidGrant);
            }
            let binding = Binding {
                session_id: approved.session_id.clone(),
                run_id: approved.run_id.clone(),
                envelope_revision: approved.envelope_revision,
                controller_uid: approved.controller_uid,
            };
            if request.subject == SkillSubject::Run
                && !self.skill_requests.contains(&binding, request)?
            {
                self.refresh_run(endpoint.ok_or(BrokerError::InvalidGrant)?, &auth.run_id)?;
            }
            // Re-observe after any external read; queued lifecycle receipts win.
            let status = self.supervisor_status(session, &mut *verify)?;
            if status.state != SessionState::Running
                || status.channel_state != ChannelState::Enabled
                || status.pending_operation.is_some()
                || self.lifecycle.is_quarantined(&query.session_id)?
            {
                return Err(BrokerError::InvalidGrant);
            }
            let current = now_ms
                .saturating_add(u64::try_from(clock.elapsed().as_millis()).unwrap_or(u64::MAX));
            self.skill_requests.accept(
                &binding,
                request,
                approved.skill_requests.as_ref(),
                current,
                &self.attention,
            )
        })();
        let operation = match accepted {
            Ok(status) => CommandOperation::SkillRequestResult { status },
            Err(_) => CommandOperation::SkillRequestRefused {
                error: ErrorCode::InvalidRequest,
            },
        };
        let reply = CommandMessage {
            session_id: session.authorization().session_id.clone(),
            run_id: session.authorization().run_id.clone(),
            envelope_revision: session.authorization().envelope_revision,
            operation,
            ..query.clone()
        };
        send(session.channel(), reply.canonical_bytes())
    }

    /// Reads or settles one durable request for its authenticated operator.
    /// No live Session or capture-service availability is required. Terminal
    /// outcomes cannot reopen requests or cause Admission, Resume or replacement.
    /// # Errors
    /// Refuses foreign operators, unknown operations, conflicting outcomes or storage failure.
    pub fn skill_request_control(
        &self,
        operator_uid: u32,
        operation_id: &str,
        outcome: Option<SkillRequestOutcome>,
    ) -> Result<SkillRequestStatus, BrokerError> {
        match outcome {
            Some(outcome) => {
                self.skill_requests
                    .finish(operator_uid, operation_id, outcome, &self.attention)
            }
            None => self.skill_requests.inspect(operator_uid, operation_id),
        }
    }

    fn refresh_run(&self, endpoint: &AttentionEndpoint, run: &str) -> Result<(), BrokerError> {
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        let worker = endpoint.read_run(
            run.to_owned(),
            Box::new(move |result| {
                let _ = send.send(result);
            }),
        )?;
        worker.join().map_err(|_| BrokerError::InvalidGrant)?;
        let observed = receive.recv().map_err(|_| BrokerError::InvalidGrant)??;
        self.skill_requests.observe_run(&observed, &self.attention)
    }

    /// Reconciles pending subjects against authenticated durable lifecycle evidence.
    /// Runs independently of editor/Session lifetimes, on the delivery worker.
    /// # Errors
    /// Retains pending intent on unavailable or invalid evidence; never infers termination.
    pub fn reconcile_skill_requests<F>(
        &self,
        endpoint: Option<&AttentionEndpoint>,
        mut verify: F,
    ) -> Result<(), BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        self.skill_requests.reconcile(&self.attention)?;
        let mut subjects = std::collections::BTreeSet::new();
        let mut failure = None;
        for record in self.skill_requests.records()? {
            if self.skill_requests.status(&record.operation_id)?.outcome
                != SkillRequestOutcome::Pending
            {
                continue;
            }
            let subject = record.subject();
            let identity = match &subject {
                AttentionSubject::Session(id) => (false, id.clone()),
                AttentionSubject::Run(id) => (true, id.clone()),
            };
            if !subjects.insert(identity) {
                continue;
            }
            let result = (|| {
                match subject {
                    AttentionSubject::Session(id) => {
                        let auth = self
                            .authorizations()
                            .consumed_for_session(&id)?
                            .ok_or(BrokerError::UnknownAuthorization)?;
                        let history =
                            self.verified_history(&auth.launch_authorization(), &mut verify)?;
                        if history.last().is_some_and(|receipt| {
                            receipt.payload.resulting_state == SessionState::Terminal
                        }) {
                            self.skill_requests
                                .end_subject(&AttentionSubject::Session(id), &self.attention)?;
                        }
                    }
                    AttentionSubject::Run(id) => {
                        self.refresh_run(endpoint.ok_or(BrokerError::InvalidGrant)?, &id)?;
                    }
                }
                Ok(())
            })();
            if let Err(error) = result {
                failure.get_or_insert(error);
            }
        }
        failure.map_or(Ok(()), Err)
    }
}
