//! Rebuilds a reattached Session's command authority on an authorized Resume.
//!
//! A warm broker restart reattaches a running Session without command
//! authority (`reconnect.rs`). Only an explicit operator Park and Resume may
//! restore it, and then only the uses the durable audit says remain
//! (louiselm-d6fv.12.5.5.2). Nothing is refunded and no grant is revived.

use super::{
    BrokerError, BrokerService, BrokerSession,
    command_replay::{ReplayRefusal, replay},
    commands::CommandAuthority,
};
use crate::{
    launch_protocol::{ErrorCode, ProtocolError, SupervisorStatus},
    launch_receipt::ReceiptOutcome,
    launch_supervisor::CapabilityBinding,
};

impl BrokerService {
    /// Returns the restored authority, `None` when the Session has no live
    /// command approval, or a typed `CapabilityDenied` refusal when the durable
    /// record is missing, contradictory or revoked.
    pub(super) fn restore_commands_for_resume(
        &self,
        session: &BrokerSession,
        status: &SupervisorStatus,
        now_ms: u64,
    ) -> Result<Option<CommandAuthority>, BrokerError> {
        let denied = || {
            BrokerError::Policy(ProtocolError::new(
                ErrorCode::CapabilityDenied,
                Some(status.state),
                status.broker_head.as_ref().map(|head| head.sequence),
            ))
        };
        let authorization = session.authorization();
        let source = self
            .authorizations()
            .consumed_for_session(&authorization.session_id)?
            .ok_or_else(denied)?;
        let state = match replay(&source, &self.command_audit().entries()?, now_ms) {
            Ok(state) => state,
            Err(ReplayRefusal::NotApproved | ReplayRefusal::Expired) => return Ok(None),
            Err(ReplayRefusal::NotConsumed | ReplayRefusal::Contradictory) => {
                return Err(denied());
            }
        };
        let chain = self.receipts().chain(&authorization.session_id)?;
        let Some(ReceiptOutcome::Start { evidence, .. }) =
            chain.get(1).map(|receipt| &receipt.payload.outcome)
        else {
            return Err(BrokerError::ReceiptUnauthorized);
        };
        let policy = match state
            .commands
            .policy(&authorization.authorization_id, now_ms)
        {
            Ok(policy) => policy,
            Err(BrokerError::Expired) => return Ok(None),
            Err(error) => return Err(error),
        };
        CommandAuthority::restore(
            CapabilityBinding {
                session_id: authorization.session_id.clone(),
                run_id: authorization.run_id.clone(),
                channel_id: "agent-capability".into(),
                envelope_revision: authorization.envelope_revision,
                identity_slot: authorization.identity_slot,
                assigned_uid: evidence.assigned_uid,
                assigned_gid: evidence.assigned_gid,
                agent_pid: evidence.agent_pid,
            },
            policy,
            self.command_audit(),
            &state,
        )
        .map(Some)
        .map_err(|_| denied())
    }
}
