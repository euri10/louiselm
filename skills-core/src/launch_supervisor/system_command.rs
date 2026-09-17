//! Per-packet Agent capability I/O; no broker policy or process creation.

use super::{
    AcceptedCapability, Arc, KernelProcess, LauncherPacket, ListenerState, Mutex, ProtocolMessage,
    SeqpacketChannel, SupervisorCompletion, SupervisorError, SystemCapabilityGate, lock,
};

impl SystemCapabilityGate {
    pub(super) fn receive_agent_command(
        &mut self,
        complete: SupervisorCompletion<ProtocolMessage>,
    ) -> Result<(), SupervisorError> {
        let commands = Arc::clone(
            self.commands
                .as_ref()
                .ok_or(SupervisorError::CapabilityUnavailable)?,
        );
        let complete: SupervisorCompletion<ProtocolMessage> = Box::new(move |result| {
            if result.is_err() {
                // Fail closed before publishing the callback: queued starts must
                // not survive a rejected packet or lost Agent connection. Poison
                // also denies every permit; this callback never releases a lease.
                let _ = commands.revoke();
            }
            complete(result);
        });
        let process = Arc::clone(
            self.process
                .as_ref()
                .ok_or(SupervisorError::AgentIdentityRejected)?,
        );
        let mut accepted = lock(&self.accepted);
        if accepted.closed
            || accepted.receiving
            || !matches!(self.state, Some(ListenerState::Enabled(_)))
        {
            return Err(SupervisorError::CapabilityUnavailable);
        }
        accepted.receiving = true;
        let Some(channel) = accepted.channel.clone() else {
            accepted.pending = Some(complete);
            return Ok(());
        };
        let generation = accepted.generation;
        drop(accepted);
        receive(&channel, process, &self.accepted, generation, complete);
        Ok(())
    }
}

pub(super) fn receive(
    channel: &SeqpacketChannel,
    process: Arc<KernelProcess>,
    accepted: &Arc<Mutex<AcceptedCapability>>,
    generation: u64,
    complete: SupervisorCompletion<ProtocolMessage>,
) {
    let complete = Arc::new(Mutex::new(Some(complete)));
    let callback = Arc::clone(&complete);
    let state = Arc::clone(accepted);
    let result = channel.receive(Box::new(move |result| {
        let active = {
            let mut state = lock(&state);
            let active = !state.closed && state.generation == generation;
            if state.generation == generation {
                state.receiving = false;
            }
            active
        };
        let result = result
            .map_err(|_| SupervisorError::CapabilityUnavailable)
            .and_then(|packet| {
                if !active
                    || process.valid().ok() != Some(true)
                    || packet.peer_credentials != process.credentials()
                    || packet.message_credentials != process.credentials()
                {
                    return Err(SupervisorError::AgentIdentityRejected);
                }
                match packet.packet {
                    LauncherPacket::Request(message @ ProtocolMessage::ToolExecution(_)) => {
                        Ok(message)
                    }
                    LauncherPacket::Request(ProtocolMessage::Command(message))
                        if matches!(
                            message.operation,
                            crate::launch_protocol::CommandOperation::Delegate { .. }
                                | crate::launch_protocol::CommandOperation::StatusRequest {}
                                | crate::launch_protocol::CommandOperation::SkillRequest { .. }
                                | crate::launch_protocol::CommandOperation::BeadsMutation { .. }
                                | crate::launch_protocol::CommandOperation::DependencyFetch { .. }
                        ) =>
                    {
                        Ok(ProtocolMessage::Command(message))
                    }
                    _ => Err(SupervisorError::AuthorizationRejected),
                }
            });
        let callback = lock(&callback).take();
        if let Some(callback) = callback {
            callback(result);
        }
    }));
    if result.is_err() {
        let mut state = lock(accepted);
        if state.generation == generation {
            state.receiving = false;
        }
        drop(state);
        let callback = lock(&complete).take();
        if let Some(callback) = callback {
            callback(Err(SupervisorError::CapabilityUnavailable));
        }
    }
}
