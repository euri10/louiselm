//! Installed authority withdrawal is independent of broker/controller liveness.

use super::super::{SupervisorTimer, ThreadSupervisorTimer};
use super::{OwnerEvent, ParkResult, SessionOwner};
use crate::{launch_supervisor::SupervisorError, launcher_install::KeyContainment};
use std::time::Duration;

#[derive(Default)]
pub(super) struct KeyAuthority {
    epoch: u64,
    pending: bool,
    pub(super) withdrawn: bool,
}

impl SessionOwner {
    pub(super) fn check_key_authority(&mut self) {
        if self.key_authority.withdrawn || self.key_authority.pending {
            return;
        }
        self.key_authority.epoch = self.key_authority.epoch.saturating_add(1);
        let epoch = self.key_authority.epoch;
        self.key_authority.pending = true;
        let sender = self.sender.clone();
        let deadline = ThreadSupervisorTimer.schedule(
            self.timeout,
            Box::new(move || {
                let _ = sender.send(OwnerEvent::KeyAuthorityDeadline { epoch });
            }),
        );
        let sender = self.sender.clone();
        let admitted = self.signer.check_authority(Box::new(move |result| {
            let _ = sender.send(OwnerEvent::KeyAuthorityChecked { epoch, result });
        }));
        if deadline.is_err() || admitted.is_err() {
            self.key_authority_checked(epoch, &Err(SupervisorError::SigningUnavailable));
        }
    }

    pub(super) fn key_authority_checked(
        &mut self,
        epoch: u64,
        result: &Result<(), SupervisorError>,
    ) {
        if epoch != self.key_authority.epoch
            || !self.key_authority.pending
            || self.key_authority.withdrawn
        {
            return;
        }
        self.key_authority.pending = false;
        if result.is_err() {
            self.withdraw_key_authority();
            return;
        }
        let sender = self.sender.clone();
        // Key authority has a fixed host clock, independent of the signed
        // broker-loss grace and its lifecycle scheduler.
        if ThreadSupervisorTimer
            .schedule(
                Duration::from_millis(250),
                Box::new(move || {
                    let _ = sender.send(OwnerEvent::KeyAuthorityPoll);
                }),
            )
            .is_err()
        {
            self.withdraw_key_authority();
        }
    }

    fn withdraw_key_authority(&mut self) {
        self.key_authority.withdrawn = true;
        self.widening_blocked = true;
        self.commands.closed = true;
        // Withdraw the shared enforcer before requesting whole-tree freeze.
        // Pending callbacks remain owned but cannot enable or sign afterward.
        let revoked = self
            .resources
            .capability
            .as_mut()
            .is_some_and(|gate| gate.revoke().is_ok());
        self.channel_state = crate::launch_protocol::ChannelState::Revoked;
        let parked = matches!(self.attempt_park(), ParkResult::Parked);
        if let Some(broker) = &self.resources.broker {
            broker.close();
        }
        self.broker_connection = crate::launch_protocol::BrokerConnection::Disconnected;
        let observation = if revoked && parked {
            KeyContainment::Frozen
        } else {
            KeyContainment::Failed
        };
        self.last_failure = Some(crate::launch_protocol::ProtocolError::new(
            crate::launch_protocol::ErrorCode::ReceiptChainInvalid,
            parked.then_some(crate::launch_receipt::SessionState::Parked),
            Some(self.broker_head.sequence),
        ));
        let sender = self.sender.clone();
        if let Err(error) = self.signer.record_containment(
            self.binding.session_id.clone(),
            observation,
            Box::new(move |result| {
                let _ = sender.send(OwnerEvent::KeyContainmentRecorded(result));
            }),
        ) {
            self.key_containment_recorded(&Err(error));
        }
        if !revoked || !parked {
            // Failed narrowing cannot leave a runnable tree. Existing cleanup
            // poisons the retained identity if zero survivors cannot be proved.
            self.quarantine_mechanic(None);
        }
    }

    pub(super) fn key_containment_recorded(&mut self, result: &Result<(), SupervisorError>) {
        if result.is_err() {
            self.last_failure = Some(crate::launch_protocol::ProtocolError::new(
                crate::launch_protocol::ErrorCode::DurabilityUnavailable,
                None,
                Some(self.broker_head.sequence),
            ));
        }
    }

    /// After withdrawal, no late signature, Resume, reconnect or tool result may
    /// re-enter the normal state machine. Controller disposal still owns cleanup.
    pub(super) fn handle_withdrawn_key(&mut self, event: &OwnerEvent) {
        match event {
            OwnerEvent::ControllerDetached | OwnerEvent::RunningAgent { .. } => {
                let result = self.resources.cleanup();
                self.finished = Some(result.and(Err(SupervisorError::SigningUnavailable)));
            }
            OwnerEvent::KeyContainmentRecorded(result) => {
                self.key_containment_recorded(result);
            }
            _ => {}
        }
    }
}
