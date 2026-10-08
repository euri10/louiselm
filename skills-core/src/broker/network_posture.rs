//! Current network evidence from the authenticated owner of active kernel policy.

use super::{BrokerError, BrokerService, BrokerSession};
use crate::{
    launch_protocol::{
        BrokerConnection, ChannelState, EvidenceFreshness, FreshnessBasis, GuardEnrollment,
        SupervisorStatus,
    },
    launch_receipt::{ReceiptOutcome, SessionState},
    launch_transport::{AuthenticatedPacket, LauncherPacket},
    posture::{DimensionInput, DimensionName, EvidenceKind, EvidenceRef, FailureCode},
};
use std::sync::{
    Arc, Weak,
    atomic::{AtomicBool, Ordering},
};

/// Neither a wire record nor durable state can recreate this process-owned proof.
pub(super) struct RetainedNetwork {
    enrollment: GuardEnrollment,
    expires_at_ms: u64,
    checked_at_ms: u64,
    receipt: EvidenceRef,
    cancelled: Arc<AtomicBool>,
    listener: Weak<()>,
    provider_generation: usize,
}

impl BrokerService {
    pub(super) fn network_provider_invalidated(
        &self,
        session: &BrokerSession,
    ) -> Result<bool, BrokerError> {
        let run = &session.authorization().run_id;
        Ok(self.provider_requests.held(run)?.is_some()
            || match &session.posture_evidence.network {
                Some(network) => {
                    self.provider_requests.extensions(run)?.len() != network.provider_generation
                }
                None => false,
            })
    }

    pub(super) fn retain_network_activation(
        &self,
        session: &mut BrokerSession,
        packet: &AuthenticatedPacket,
        enrollment: &GuardEnrollment,
        now_ms: u64,
        verify: &mut impl FnMut(&str, &[u8], &str) -> bool,
    ) -> Result<(), BrokerError> {
        if packet.peer_credentials != session.channel().peer_credentials()
            || packet.message_credentials != packet.peer_credentials
            || packet.descriptors.is_some()
            || !matches!(packet.packet, LauncherPacket::Response(_))
        {
            return Err(BrokerError::InvalidGrant);
        }
        let listener = session
            .provider_listener
            .as_ref()
            .ok_or(BrokerError::InvalidGrant)?;
        if listener.enrollment != *enrollment
            || session.provider_revision != Some(enrollment.scope.revision)
            || session.channel().is_closed()
            || session.provider_work.cancelled.load(Ordering::Acquire)
        {
            return Err(BrokerError::RequestMismatch);
        }
        let authorization = session.authorization();
        let approved = self
            .authorizations()
            .consumed_for_session(&authorization.session_id)?
            .ok_or(BrokerError::UnknownAuthorization)?;
        let permission = approved
            .provider_requests
            .as_ref()
            .ok_or(BrokerError::InvalidGrant)?;
        let expires_at_ms = permission
            .expires_at_ms
            .min(approved.expires_at_ms)
            .min(authorization.expires_at_ms);
        if enrollment.scope.session_id != authorization.session_id
            || enrollment.scope.run_id != authorization.run_id
            || enrollment.scope.envelope_revision != authorization.envelope_revision
            || now_ms >= expires_at_ms
            || monotonic_ns().is_none_or(|now| now >= enrollment.scope.deadline_ns)
            || self.lifecycle.is_quarantined(&authorization.session_id)?
            || self
                .provider_requests
                .held(&authorization.run_id)?
                .is_some()
        {
            return Err(BrokerError::InvalidGrant);
        }
        if let Some(previous) = &session.posture_evidence.network {
            if previous.enrollment == *enrollment {
                // Exact duplicates do not renew the original successful observation.
                return Ok(());
            }
            if previous.enrollment.scope.revision >= enrollment.scope.revision {
                return Err(BrokerError::RequestMismatch);
            }
        }
        let history = self.verified_history(authorization, verify)?;
        let start = history.iter().find(|receipt| matches!(&receipt.payload.outcome,
            ReceiptOutcome::Start { evidence, .. } if evidence.sender_guard_required && evidence.agent_pid == enrollment.runtime_pid
        )).ok_or(BrokerError::ReceiptUnauthorized)?;
        session.posture_evidence.network = Some(RetainedNetwork {
            enrollment: enrollment.clone(),
            expires_at_ms,
            checked_at_ms: now_ms,
            receipt: EvidenceRef::new(EvidenceKind::BrokerReceipt, &start.digest().to_string())?,
            cancelled: Arc::clone(&session.provider_work.cancelled),
            listener: listener.owner_lease(),
            provider_generation: self
                .provider_requests
                .extensions(&authorization.run_id)?
                .len(),
        });
        Ok(())
    }
}

impl RetainedNetwork {
    pub(super) fn dimension(
        &self,
        status: &SupervisorStatus,
        quarantined: bool,
        held: bool,
        now_ms: u64,
    ) -> (DimensionInput, EvidenceFreshness) {
        let scope = &self.enrollment.scope;
        let current = !quarantined
            && !held
            && status.session_id == scope.session_id
            && status.run_id == scope.run_id
            && status.envelope_revision == scope.envelope_revision
            && status.state == SessionState::Running
            && status.channel_state == ChannelState::Enabled
            && status.broker_connection == BrokerConnection::Connected
            && status.pending_operation.is_none()
            && self.checked_at_ms <= now_ms
            && now_ms < self.expires_at_ms
            && monotonic_ns().is_some_and(|now| now < scope.deadline_ns)
            && !self.cancelled.load(Ordering::Acquire)
            && self.listener.strong_count() > 0;
        let evidence = vec![self.receipt.clone()];
        let input = if current {
            DimensionInput::verified(DimensionName::Network, evidence)
        } else {
            DimensionInput::failed(
                DimensionName::Network,
                if quarantined {
                    FailureCode::Quarantined
                } else {
                    FailureCode::EvidenceInvalidated
                },
                evidence,
            )
        };
        (
            input,
            EvidenceFreshness {
                basis: if current {
                    FreshnessBasis::Launch
                } else {
                    FreshnessBasis::Invalidated
                },
                last_verified_at_ms: Some(self.checked_at_ms),
            },
        )
    }
}

fn monotonic_ns() -> Option<u64> {
    let now = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    u64::try_from(now.tv_sec)
        .ok()?
        .checked_mul(1_000_000_000)?
        .checked_add(u64::try_from(now.tv_nsec).ok()?)
}
