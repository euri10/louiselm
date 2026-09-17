//! Authenticated canonical Beads mutation mediation, separate from lifecycle authority.

use super::service::send;
use super::{BrokerError, BrokerService, BrokerSession, beads_mutation::Binding};
use crate::{
    beads_mutation::BeadsMutationRequest,
    launch_protocol::{ChannelState, CommandMessage, CommandOperation, ErrorCode},
    launch_receipt::SessionState,
};

impl BrokerService {
    /// Inspects one durable Beads operation or records its operator decision.
    /// This blocking API neither invokes `br` nor grants or refunds capability.
    /// Reconciliation is an attributed attestation beside the original outcome.
    /// # Errors
    /// Refuses foreign operators, malformed/unknown operations, contradictory
    /// decisions, reconciliation of completed writes, or unavailable durable storage.
    pub fn beads_mutation_control(
        &self,
        operator_uid: u32,
        operation_id: &str,
        decision: Option<&crate::beads_mutation::BeadsControlDecision>,
    ) -> Result<crate::beads_mutation::BeadsInspection, BrokerError> {
        self.beads_mutations.control(
            operator_uid,
            operation_id,
            decision,
            super::now_ms()?,
            &self.attention,
        )
    }

    /// Enables mutation mediation for one trusted canonical project and exact `br` bytes.
    ///
    /// Call before sharing the service. The operator must protect the canonical
    /// project and executable from Session writes. Installed startup validates
    /// protected provisioning before calling this; an unconfigured service denies mutations.
    /// This blocks while checking the executable digest and existing database.
    ///
    /// # Errors
    /// Refuses relative paths, a missing database, changed executable bytes or I/O failure.
    pub fn configure_beads_tracker(
        &mut self,
        workspace: &std::path::Path,
        program: &std::path::Path,
        expected_digest: &crate::Digest,
    ) -> Result<(), BrokerError> {
        if !workspace.is_absolute()
            || !program.is_absolute()
            || !workspace.join(".beads/beads.db").is_file()
        {
            return Err(BrokerError::InvalidGrant);
        }
        let workspace_root = std::fs::canonicalize(workspace).map_err(BrokerError::Storage)?;
        let program = std::fs::canonicalize(program).map_err(BrokerError::Storage)?;
        let bytes = std::fs::read(&program).map_err(BrokerError::Storage)?;
        if crate::Digest::of(&bytes) != *expected_digest {
            return Err(BrokerError::InvalidGrant);
        }
        self.tracker = Some(super::beads_mutation::TrackerConfig {
            workspace_root,
            program,
            program_digest: expected_digest.clone(),
            scratch: self.beads_mutations.scratch(),
        });
        Ok(())
    }

    /// Mediates one canonical Beads mutation for the authenticated Session.
    ///
    /// The broker derives the actor from its own trusted, consumed
    /// authorization record; the request payload never names an actor. `br`
    /// runs against this broker's one configured canonical workspace, never
    /// a Session's private confined snapshot.
    ///
    /// # Errors
    /// Returns transport, protocol, or durable-audit failure from the reply.
    pub(super) fn answer_beads_mutation<F>(
        &self,
        session: &mut BrokerSession,
        query: &CommandMessage,
        now_ms: u64,
        verify: &mut F,
    ) -> Result<(), BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        query.validate()?;
        self.require_trusted_history(session, verify)?;
        let CommandOperation::BeadsMutation { request } = &query.operation else {
            return Err(BrokerError::InvalidGrant);
        };
        let accepted = self.accept_beads_mutation(session, query, request, now_ms, verify);
        let operation = match accepted {
            Ok(operation) => operation,
            Err(BrokerError::Policy(error)) => CommandOperation::BeadsMutationRefused {
                error: error.code,
                escalation: None,
            },
            Err(BrokerError::Storage(error)) => return Err(BrokerError::Storage(error)),
            Err(BrokerError::TrackerInvocation(error)) => {
                return Err(BrokerError::TrackerInvocation(error));
            }
            Err(error @ (BrokerError::Workspace(_) | BrokerError::TrackerConfiguration(_))) => {
                return Err(error);
            }
            Err(_) => CommandOperation::BeadsMutationRefused {
                error: ErrorCode::InvalidRequest,
                escalation: None,
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

    fn accept_beads_mutation<F>(
        &self,
        session: &mut BrokerSession,
        query: &CommandMessage,
        request: &BeadsMutationRequest,
        now_ms: u64,
        verify: &mut F,
    ) -> Result<CommandOperation, BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        let clock = std::time::Instant::now();
        let tracker = self.tracker.as_ref().ok_or(BrokerError::InvalidGrant)?;
        let auth = session.authorization();
        if query.session_id != auth.session_id || query.run_id != auth.run_id {
            return Err(BrokerError::RequestMismatch);
        }
        if query.envelope_revision != auth.envelope_revision {
            return Err(crate::launch_protocol::ProtocolError::new(
                ErrorCode::EnvelopeRevisionMismatch,
                None,
                None,
            )
            .into());
        }
        let approved = self
            .authorizations()
            .consumed_for_session(&auth.session_id)?
            .ok_or(BrokerError::UnknownAuthorization)?;
        let permission = approved.beads_mutations.as_ref().filter(|permission| {
            permission.permits(request, now_ms)
                && permission.project_digest == tracker.project_digest()
        });
        if permission.is_none() && !request.required {
            return Ok(CommandOperation::BeadsMutationRefused {
                error: ErrorCode::CapabilityDenied,
                escalation: None,
            });
        }
        let status = self.supervisor_status(session, &mut *verify)?;
        if status.state != SessionState::Running
            || status.channel_state != ChannelState::Enabled
            || status.pending_operation.is_some()
            || self.lifecycle.is_quarantined(&query.session_id)?
        {
            return Err(BrokerError::InvalidGrant);
        }
        let binding = Binding {
            session_id: approved.session_id.clone(),
            run_id: approved.run_id.clone(),
            agent_id: approved.agent_id.clone(),
            envelope_revision: approved.envelope_revision,
            controller_uid: approved.controller_uid,
        };
        let Some(permission) = permission else {
            return self.missing_beads_capability(&binding, request, tracker, now_ms);
        };
        match self.beads_mutations.accept(
            &binding,
            request,
            permission,
            now_ms.saturating_add(u64::try_from(clock.elapsed().as_millis()).unwrap_or(u64::MAX)),
            &super::tracker_runner::SystemTrackerRunner::new(std::time::Duration::from_secs(30)),
            tracker,
        ) {
            Ok(status) => {
                self.refresh_beads_replica(session.authorization(), &status)?;
                Ok(CommandOperation::BeadsMutationResult { status })
            }
            Err(BrokerError::BeadsBudgetExhausted | BrokerError::Expired) => {
                self.missing_beads_capability(&binding, request, tracker, now_ms)
            }
            Err(error) => Err(error),
        }
    }

    fn missing_beads_capability(
        &self,
        binding: &Binding,
        request: &BeadsMutationRequest,
        tracker: &super::beads_mutation::TrackerConfig,
        now_ms: u64,
    ) -> Result<CommandOperation, BrokerError> {
        let escalation = if request.required {
            Some(Box::new(self.beads_mutations.escalate(
                binding,
                request,
                tracker.project_digest(),
                now_ms,
                &self.attention,
            )?))
        } else {
            None
        };
        Ok(CommandOperation::BeadsMutationRefused {
            error: ErrorCode::CapabilityDenied,
            escalation,
        })
    }
}
