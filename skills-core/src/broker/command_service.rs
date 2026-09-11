//! Command policy attached once to the original authenticated broker Session.

use super::{BrokerError, BrokerSession, receive, send};
use crate::{
    broker::{
        commands::CommandAuthority,
        delegation::{DelegationError, DelegationPolicy},
    },
    launch_protocol::{CommandOperation, ErrorCode, ProtocolMessage},
    launch_supervisor::CapabilityBinding,
    launch_transport::LauncherPacket,
};

impl BrokerSession {
    /// Installs the already approved Agent policy once for this launch lifetime.
    /// `binding` comes from trusted supervisor launch context, not Agent payloads.
    /// No kernel handle is reconstructed here and no operator prompt is introduced.
    ///
    /// # Errors
    /// Refuses contradictory launch context, policy replacement or unavailable audit.
    pub fn enable_commands(
        &mut self,
        binding: CapabilityBinding,
        policy: DelegationPolicy,
    ) -> Result<(), DelegationError> {
        let launch = &self.authorization;
        if self.commands.is_some()
            || self.channel.is_closed()
            || binding.session_id != launch.session_id
            || binding.run_id != launch.run_id
            || binding.envelope_revision != launch.envelope_revision
            || binding.identity_slot != launch.identity_slot
            || binding.assigned_uid != launch.assigned_uid
            || binding.assigned_gid != launch.assigned_gid
            || binding.channel_id != "agent-capability"
        {
            return Err(DelegationError::ScopeMismatch);
        }
        self.commands = Some(CommandAuthority::new(
            binding,
            policy,
            std::sync::Arc::clone(&self.audit),
        )?);
        Ok(())
    }

    /// Processes one command packet on the retained supervisor connection.
    ///
    /// Blocks only on the broker worker, with the launch transport's fixed step
    /// deadline. The worker must not arm a competing receive. Policy refusals
    /// are typed replies; uncertain transport/audit failures close the connection
    /// without refunding spent authority. Revocation acknowledgements get no echo.
    ///
    /// # Errors
    /// Returns transport, protocol or durable-audit failure; cleanup is not inferred.
    pub fn serve_command(&mut self) -> Result<(), BrokerError> {
        let result = self.command_step();
        if result.is_err() {
            self.close();
        }
        result
    }

    fn command_step(&mut self) -> Result<(), BrokerError> {
        let packet = receive(&self.channel)?;
        // CredentialPin also checks each packet; retain this exact-process
        // constraint even if installation accepted a supervisor UID initially.
        if packet.peer_credentials != self.channel.peer_credentials()
            || packet.message_credentials != packet.peer_credentials
        {
            return Err(BrokerError::InvalidGrant);
        }
        let LauncherPacket::Request(ProtocolMessage::Command(mut message)) = packet.packet else {
            return Err(BrokerError::InvalidGrant);
        };
        let authority = self.commands.as_mut().ok_or(BrokerError::InvalidGrant)?;
        match authority.handle(&message) {
            Ok(reply) => {
                if !matches!(
                    message.operation,
                    CommandOperation::Revoked { .. } | CommandOperation::GrantRevoked { .. }
                ) {
                    send(&self.channel, reply.canonical_bytes())?;
                }
                Ok(())
            }
            Err(DelegationError::Audit(error)) => Err(error),
            Err(DelegationError::OwnerUnavailable) => Err(BrokerError::InvalidGrant),
            Err(_)
                if matches!(
                    message.operation,
                    CommandOperation::Request { .. } | CommandOperation::DelegationRequest { .. }
                ) =>
            {
                message.operation = CommandOperation::Reject {
                    error: ErrorCode::InvalidRequest,
                };
                send(&self.channel, message.canonical_bytes())
            }
            Err(_) => Err(BrokerError::InvalidGrant),
        }
    }

    /// Stops approvals before sending cancellation to the supervisor.
    /// A successful send is not successful enforcement; keep serving the same
    /// connection until [`Self::command_revocation_complete`] proves the ACK.
    ///
    /// # Errors
    /// Refuses missing policy or failed durable intent/transport, then closes.
    pub fn revoke_commands(&mut self, request_id: &str) -> Result<(), BrokerError> {
        let result = self
            .commands
            .as_mut()
            .ok_or(BrokerError::InvalidGrant)
            .and_then(|authority| {
                authority.revoke(request_id).map_err(|error| match error {
                    DelegationError::Audit(error) => error,
                    _ => BrokerError::InvalidGrant,
                })
            })
            .and_then(|message| send(&self.channel, message.canonical_bytes()));
        if result.is_err() {
            self.close();
        }
        result
    }

    /// True only after the supervisor proved cancellation and its ACK was audited.
    #[must_use]
    pub fn command_revocation_complete(&self) -> bool {
        self.commands
            .as_ref()
            .is_some_and(CommandAuthority::revocation_complete)
    }

    /// Stops one grant's approvals, then requests confirmed supervisor cancellation.
    ///
    /// # Errors
    /// Refuses missing/unknown authority, audit failure or lost transport. Errors close the connection.
    pub fn revoke_tool_grant(&mut self, request_id: &str, grant: u64) -> Result<(), BrokerError> {
        let result = self
            .commands
            .as_mut()
            .ok_or(BrokerError::InvalidGrant)
            .and_then(|owner| {
                owner
                    .revoke_grant(request_id, grant)
                    .map_err(|error| match error {
                        DelegationError::Audit(error) => error,
                        _ => BrokerError::InvalidGrant,
                    })
            })
            .and_then(|message| send(&self.channel, message.canonical_bytes()));
        if result.is_err() {
            self.close();
        }
        result
    }

    /// True only after authenticated enforcement of this grant became durable.
    #[must_use]
    pub fn tool_grant_revocation_complete(&self, grant: u64) -> bool {
        self.commands
            .as_ref()
            .is_some_and(|owner| owner.grant_revocation_complete(grant))
    }
}
