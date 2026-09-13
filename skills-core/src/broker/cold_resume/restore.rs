//! Serialized restore and load finalization on the existing broker Session worker.
use super::{
    BrokerError, BrokerService, ColdResumeAllocation, Deserialize, LifecycleCaller, Serialize,
    check_operator, durable_again, identical_record, read_record, write_new_record,
};
use crate::{
    broker::{
        BrokerSession,
        commands::CommandAuthority,
        service::{receive, send},
    },
    launch_protocol::{
        BrokerConnection, ChannelState, RecoveryRestoreRequest, ResponseResult, SupervisorStatus,
    },
    launch_receipt::{ReceiptOutcome, SessionState},
    launch_supervisor::CapabilityBinding,
    launch_transport::LauncherPacket,
};

/// Result observed by the trusted controller from the exact ACP load operation.
/// This Rust boundary is not an Agent-supplied assertion of successful recovery.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum ColdLoadOutcome {
    /// Controller received success for the original retained ACP identity.
    Loaded {
        /// Exact retained ACP conversation identity.
        acp_session_id: String,
    },
    /// Load failed or was cancelled; late success cannot enable authority.
    Failed,
}

impl BrokerService {
    /// Restores through the authenticated supervisor into its frozen target.
    /// Blocks on the serialized Session worker with bounded transport waits.
    /// The immutable request precedes I/O; retries never choose another target
    /// or overwrite changed material. No process Resume or work is authorized here.
    /// # Errors
    /// Refuses wrong caller, lost/expired source, changed target, stale Park head,
    /// failed mechanical copy, late responses or unavailable durable evidence.
    pub fn restore_cold_resume<F>(
        &self,
        session: &mut BrokerSession,
        caller: &LifecycleCaller,
        now_ms: u64,
        mut verify: F,
    ) -> Result<RecoveryRestoreRequest, BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        let clock = std::time::Instant::now();
        let allocation = self.cold_session(session, caller, now_ms, &mut verify)?;
        let source_id = &allocation.source.launch.session_id;
        let result = (|| {
            if read_record::<ColdLoadOutcome>(&self.cold_path(source_id, "final")?)?.is_some() {
                return Err(BrokerError::RequestMismatch);
            }
            let status = self.supervisor_status(session, &mut verify)?;
            check_status(&status, SessionState::Parked)?;
            let request = RecoveryRestoreRequest {
                schema: crate::launch_protocol::RECOVERY_RESTORE_SCHEMA.into(),
                protocol_version: crate::launch::PROTOCOL_VERSION,
                request_id: allocation.target.request_id.clone(),
                source: allocation.source.clone(),
                target: allocation.target.clone(),
                head: status.broker_head.ok_or(BrokerError::ReceiptUnauthorized)?,
            };
            request.validate()?;
            identical_record(&self.cold_path(source_id, "restore")?, &request)?;
            send(session.channel(), request.canonical_bytes())?;
            loop {
                let packet = receive(session.channel())?;
                if let LauncherPacket::Response(response) = &packet.packet {
                    if response.request_id != request.request_id {
                        return Err(BrokerError::RequestMismatch);
                    }
                    match &response.result {
                        ResponseResult::RecoveryRestored { request: actual }
                            if actual.as_ref() == &request =>
                        {
                            break;
                        }
                        ResponseResult::Error { error } => return Err(error.clone().into()),
                        _ => return Err(BrokerError::RequestMismatch),
                    }
                }
                self.lifecycle_packet(session, packet, &mut verify)?;
            }
            let status = self.supervisor_status(session, &mut verify)?;
            check_status(&status, SessionState::Parked)?;
            if status.broker_head.as_ref() != Some(&request.head) {
                return Err(BrokerError::RequestMismatch);
            }
            self.cold_session(session, caller, elapsed(now_ms, clock), &mut verify)?;
            // Preserve required recovery for the replacement itself. The consumed
            // source allocation cannot serve as its next recoverable checkpoint.
            self.register_recovery(
                session,
                caller,
                &crate::launch_protocol::RecoveryRequest {
                    schema: crate::launch_protocol::RECOVERY_REQUEST_SCHEMA.into(),
                    protocol_version: crate::launch::PROTOCOL_VERSION,
                    launch: allocation.target.clone(),
                    head: request.head.clone(),
                    retention: crate::launch_protocol::RetentionRequest {
                        request_id: allocation.target.request_id.clone(),
                        acp_session_id: allocation.source.request.acp_session_id.clone(),
                        expires_at_ms: allocation.expires_at_ms,
                    },
                },
                elapsed(now_ms, clock),
                &mut verify,
            )?;
            identical_record(&self.cold_path(source_id, "restored")?, &request)?;
            Ok(request)
        })();
        if result.is_err() {
            session.close();
        }
        result
    }

    /// Finalizes the trusted controller's load result exactly once.
    /// Call only after explicit operator Resume and the controller's ACP load.
    /// Successful persistence enables remaining commands on this owner only.
    /// Restart/retry of an already-finalized record never reconstructs that owner.
    /// A failed load clears approvals; the controller must request Disposal or
    /// drive `step` through the exited process's terminal receipt before dropping
    /// the owner. Late completion cannot revive authority or interrupt that cleanup.
    /// # Errors
    /// Refuses foreign/expired bindings, absent restore, conflicting completion,
    /// non-running target, quarantine, audit failure or uncertain persistence.
    pub fn finish_cold_resume<F>(
        &self,
        session: &mut BrokerSession,
        caller: &LifecycleCaller,
        outcome: &ColdLoadOutcome,
        now_ms: u64,
        mut verify: F,
    ) -> Result<(), BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        let clock = std::time::Instant::now();
        let allocation = self
            .cold_target(&session.authorization().session_id)?
            .ok_or(BrokerError::InvalidGrant)?;
        check_operator(caller, allocation.controller_uid)?;
        let path = self.cold_path(&allocation.source.launch.session_id, "final")?;
        if let Some(prior) = read_record::<ColdLoadOutcome>(&path)? {
            if prior != *outcome {
                return Err(BrokerError::RequestMismatch);
            }
            if matches!(prior, ColdLoadOutcome::Loaded { .. }) {
                self.cold_session(session, caller, elapsed(now_ms, clock), &mut verify)?;
                check_status(
                    &self.supervisor_status(session, &mut verify)?,
                    SessionState::Running,
                )?;
            }
            return durable_again(&path);
        }
        let result = (|| {
            if *outcome == ColdLoadOutcome::Failed {
                session.commands = None;
                session.recovery_admitted_until = None;
                return write_new_record(&path, outcome);
            }
            self.cold_session(session, caller, now_ms, &mut verify)?;
            let ColdLoadOutcome::Loaded { acp_session_id } = outcome else {
                return Err(BrokerError::InvalidGrant);
            };
            if *acp_session_id != allocation.source.request.acp_session_id {
                return Err(BrokerError::RequestMismatch);
            }
            let restored: RecoveryRestoreRequest =
                read_record(&self.cold_path(&allocation.source.launch.session_id, "restored")?)?
                    .ok_or(BrokerError::InvalidGrant)?;
            if restored.source != allocation.source || restored.target != allocation.target {
                return Err(BrokerError::RequestMismatch);
            }
            let status = self.supervisor_status(session, &mut verify)?;
            check_status(&status, SessionState::Running)?;
            let current_ms = elapsed(now_ms, clock);
            self.cold_session(session, caller, current_ms, &mut verify)?;
            let chain = self.receipts().chain(&allocation.target.session_id)?;
            let Some(ReceiptOutcome::Start { evidence, .. }) =
                chain.get(1).map(|r| &r.payload.outcome)
            else {
                return Err(BrokerError::ReceiptUnauthorized);
            };
            let commands = allocation
                .commands
                .as_ref()
                .filter(|c| c.expires_at_ms > current_ms)
                .map(|commands| {
                    CommandAuthority::new(
                        CapabilityBinding {
                            session_id: allocation.target.session_id.clone(),
                            run_id: allocation.target.run_id.clone(),
                            channel_id: "agent-capability".into(),
                            envelope_revision: allocation.target.envelope_revision,
                            identity_slot: session.authorization().identity_slot,
                            assigned_uid: evidence.assigned_uid,
                            assigned_gid: evidence.assigned_gid,
                            agent_pid: evidence.agent_pid,
                        },
                        commands.policy(&allocation.target.authorization_id, current_ms)?,
                        self.command_audit(),
                    )
                    .map_err(|_| BrokerError::InvalidGrant)
                })
                .transpose()?;
            self.admit_recovery(&allocation.target.session_id, current_ms)?;
            if allocation.require_cold_recovery
                && session
                    .recovery_admitted_until
                    .is_none_or(|deadline| std::time::Instant::now() >= deadline)
            {
                return Err(BrokerError::InvalidGrant);
            }
            // The durable single-winner marker precedes installation of any owner.
            // Crashing here can lose availability, never duplicate command budgets.
            write_new_record(&path, outcome)?;
            session.commands = commands;
            Ok(())
        })();
        if result.is_err() {
            session.close();
        }
        result
    }

    fn cold_session<F>(
        &self,
        session: &BrokerSession,
        caller: &LifecycleCaller,
        now_ms: u64,
        verify: &mut F,
    ) -> Result<ColdResumeAllocation, BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        let allocation = self
            .cold_target(&session.authorization().session_id)?
            .ok_or(BrokerError::InvalidGrant)?;
        check_operator(caller, allocation.controller_uid)?;
        if allocation.target.authorization_id != session.authorization().authorization_id
            || allocation.target.digest().to_string() != session.authorization().request_digest
            || self
                .lifecycle
                .is_quarantined(&allocation.target.session_id)?
            || session.channel().is_closed()
        {
            return Err(BrokerError::RequestMismatch);
        }
        if now_ms >= allocation.expires_at_ms {
            return Err(BrokerError::Expired);
        }
        if self.cold_source(&allocation.source.launch.session_id, now_ms, verify)?
            != allocation.source
        {
            return Err(BrokerError::RequestMismatch);
        }
        let chain = self
            .receipts()
            .verified_chain(session.authorization(), verify)?;
        // Integration evidence includes the Session identity, so its digest must
        // change on reconstruction. Both chains verify under the installed release;
        // compare the runtime measurement, while the target supervisor independently
        // enforces its measured recovery layout before copying.
        let original = self
            .receipts()
            .chain(&allocation.source.launch.session_id)?;
        let runtime = |receipts: &[crate::launch_receipt::SignedReceipt]| {
            receipts.first().and_then(|r| match &r.payload.outcome {
                ReceiptOutcome::Launch { evidence, .. } => {
                    Some(evidence.runtime_measurement_digest.clone())
                }
                _ => None,
            })
        };
        if runtime(&chain).is_none() || runtime(&chain) != runtime(&original) {
            return Err(BrokerError::ReceiptUnauthorized);
        }
        Ok(allocation)
    }
}

fn check_status(status: &SupervisorStatus, state: SessionState) -> Result<(), BrokerError> {
    if status.state != state
        || status.broker_connection != BrokerConnection::Connected
        || status.pending_operation.is_some()
        || status.pending_receipt_count != 0
        || status.process_exit.is_some()
        || status.launcher_head != status.broker_head
        || (state == SessionState::Parked && status.channel_state != ChannelState::Revoked)
        || (state == SessionState::Running && status.channel_state != ChannelState::Enabled)
    {
        return Err(BrokerError::RequestMismatch);
    }
    Ok(())
}

fn elapsed(start: u64, clock: std::time::Instant) -> u64 {
    start.saturating_add(u64::try_from(clock.elapsed().as_millis()).unwrap_or(u64::MAX))
}
