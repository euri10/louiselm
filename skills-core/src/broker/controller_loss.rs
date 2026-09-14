//! Durable controller-loss decisions precede authenticated disposal settlement.
use super::{BrokerError, BrokerService, BrokerSession, receive, response, send};
use crate::{
    broker::{
        attention::{AttentionCondition, AttentionReason, AttentionSubject, ProjectionChange},
        read_record, record_name, sync_directory, write_new_record,
    },
    launch::PROTOCOL_VERSION,
    launch_protocol::{
        CONTROLLER_LOSS_ACK_SCHEMA, ControllerLossAcknowledgement, ControllerLossDisposition,
        ControllerLossSettlement, ProtocolMessage, RecoveryReadiness, ResponseResult,
    },
    launch_receipt::{ReceiptHead, SessionState},
    launch_transport::{AuthenticatedPacket, LauncherPacket},
};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LossDecision {
    request: ControllerLossSettlement,
    acknowledgement: ControllerLossAcknowledgement,
    attention: AttentionCondition,
}

impl BrokerService {
    pub(in crate::broker) fn recoverable_loss(
        &self,
        session_id: &str,
        now_ms: u64,
    ) -> Result<(), BrokerError> {
        let path = self
            .authorizations
            .root
            .join("controller-loss")
            .join(record_name(session_id)?);
        let decision: LossDecision = read_record(&path)?.ok_or(BrokerError::InvalidGrant)?;
        validate_decision(&decision)?;
        if decision.request.session_id != session_id {
            return Err(BrokerError::RequestMismatch);
        }
        let ControllerLossDisposition::Recoverable {
            acp_recovery_reference,
            ..
        } = &decision.acknowledgement.disposition
        else {
            return Err(BrokerError::InvalidGrant);
        };
        if !matches!(self.recovery_readiness(session_id, now_ms)?, RecoveryReadiness::Ready { operation_id, .. } if operation_id == *acp_recovery_reference)
        {
            return Err(BrokerError::Expired);
        }
        Ok(())
    }
    /// Processes one authenticated command, receipt or controller-loss settlement.
    /// Runs on the serialized broker worker; true means a terminal receipt is
    /// durable, not that a future reconstruction is active. Projection delivery
    /// is independent; only durable local enqueue permits a loss acknowledgement.
    /// # Errors
    /// Closes transport on invalid binding, signature, storage or uncertain I/O.
    pub fn step<F>(
        &self,
        session: &mut BrokerSession,
        now_ms: u64,
        mut verify: F,
    ) -> Result<bool, BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        let clock = std::time::Instant::now();
        let result = (|| {
            let packet = receive(session.channel())?;
            self.control_packet(session, packet, elapsed_ms(now_ms, clock), &mut verify)
        })();
        if result.is_err() {
            session.close();
        }
        result
    }

    pub(in crate::broker) fn control_packet<F>(
        &self,
        session: &mut BrokerSession,
        packet: AuthenticatedPacket,
        now_ms: u64,
        verify: &mut F,
    ) -> Result<bool, BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        match &packet.packet {
            LauncherPacket::SignedReceipt(receipt) => {
                self.lifecycle.check_receipt(receipt)?;
                let ack = self
                    .receipts
                    .append(&session.authorization, &packet.bytes, verify)?;
                send(session.channel(), ack.canonical_bytes())?;
                Ok(receipt.payload.resulting_state == SessionState::Terminal)
            }
            LauncherPacket::Request(ProtocolMessage::ControllerLossSettlement(request)) => {
                let ack = self.settle_loss(session, request, now_ms, verify)?;
                send(
                    session.channel(),
                    response(
                        &request.request_id,
                        ResponseResult::ControllerLossAcknowledgement {
                            acknowledgement: ack,
                        },
                    ),
                )?;
                Ok(false)
            }
            LauncherPacket::Request(ProtocolMessage::Command(_)) => {
                session.handle_command(packet)?;
                Ok(false)
            }
            // A status read arriving mid-operation cannot be served here without
            // arming a receive against the one already waiting. Refuse, retryably.
            LauncherPacket::Request(ProtocolMessage::Status(query)) => {
                super::lifecycle_service::refuse_nested_status(session, query)?;
                Ok(false)
            }
            _ => Err(BrokerError::InvalidGrant),
        }
    }

    fn settle_loss<F>(
        &self,
        session: &mut BrokerSession,
        request: &ControllerLossSettlement,
        now_ms: u64,
        verify: &mut F,
    ) -> Result<ControllerLossAcknowledgement, BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        let clock = std::time::Instant::now();
        request.validate()?;
        let authorization = session.authorization();
        if request.session_id != authorization.session_id
            || request.run_id != authorization.run_id
            || request.envelope_revision != authorization.envelope_revision
        {
            return Err(BrokerError::RequestMismatch);
        }
        let chain = self.receipts.verified_chain(authorization, verify)?;
        let head = chain.last().ok_or(BrokerError::ReceiptUnauthorized)?;
        if head.payload.resulting_state != SessionState::Parked
            || request.parked_head
                != (ReceiptHead {
                    sequence: head.payload.sequence,
                    digest: head.digest().to_string(),
                })
        {
            return Err(BrokerError::ReceiptUnauthorized);
        }
        // A signed Park proves mechanical freeze/revocation. Forget the old
        // broker approval owner too; settlement can never issue new work.
        session.commands = None;
        session.recovery_admitted_until = None;
        let directory = self.authorizations.root.join("controller-loss");
        std::fs::create_dir_all(&directory).map_err(BrokerError::Storage)?;
        sync_directory(&self.authorizations.root)?;
        let path = directory.join(record_name(&request.session_id)?);
        let decision = if let Some(prior) = read_record::<LossDecision>(&path)? {
            if prior.request != *request {
                return Err(BrokerError::RequestMismatch);
            }
            prior
        } else {
            let decision = self.loss_decision(request, elapsed_ms(now_ms, clock))?;
            write_new_record(&path, &decision)?;
            decision
        };
        validate_decision(&decision)?;
        // A previous call may have failed after file creation but before fsync.
        std::fs::File::open(&path)
            .and_then(|file| file.sync_all())
            .map_err(BrokerError::Storage)?;
        sync_directory(&directory)?;
        self.attention.enqueue(
            &format!("controller-loss-{}", decision.attention.operation_id),
            ProjectionChange::Upsert(decision.attention),
        )?;
        if let ControllerLossDisposition::Recoverable {
            acp_recovery_reference,
            ..
        } = &decision.acknowledgement.disposition
            && !matches!(self.recovery_readiness(&request.session_id, elapsed_ms(now_ms, clock))?,
                RecoveryReadiness::Ready { operation_id, .. } if operation_id == *acp_recovery_reference)
        {
            return Err(BrokerError::Expired);
        }
        Ok(decision.acknowledgement)
    }

    fn loss_decision(
        &self,
        request: &ControllerLossSettlement,
        now_ms: u64,
    ) -> Result<LossDecision, BrokerError> {
        let operation_id = condition_id(&request.canonical_bytes());
        let readiness = self.recovery_readiness(&request.session_id, now_ms)?;
        let (disposition, reason) = match readiness {
            RecoveryReadiness::Ready {
                operation_id: recovery,
                ..
            } => (
                ControllerLossDisposition::Recoverable {
                    acp_recovery_reference: recovery,
                    attention_projection_id: operation_id.clone(),
                },
                AttentionReason::RunParked,
            ),
            _ => (
                ControllerLossDisposition::NoRecovery {
                    attention_projection_id: operation_id.clone(),
                },
                AttentionReason::SessionFailed,
            ),
        };
        let acknowledgement = ControllerLossAcknowledgement {
            schema: CONTROLLER_LOSS_ACK_SCHEMA.into(),
            protocol_version: PROTOCOL_VERSION,
            request_id: request.request_id.clone(),
            session_id: request.session_id.clone(),
            run_id: request.run_id.clone(),
            envelope_revision: request.envelope_revision,
            parked_head: request.parked_head.clone(),
            disposition,
        };
        acknowledgement.validate_for(request)?;
        Ok(LossDecision {
            request: request.clone(),
            acknowledgement,
            attention: AttentionCondition {
                subject: AttentionSubject::Session(request.session_id.clone()),
                operation_id,
                created_at_ms: now_ms,
                reason,
            },
        })
    }
}

fn condition_id(bytes: &[u8]) -> String {
    let digest = crate::Digest::of(bytes);
    let hex = digest.hex();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

fn elapsed_ms(start_ms: u64, clock: std::time::Instant) -> u64 {
    start_ms.saturating_add(u64::try_from(clock.elapsed().as_millis()).unwrap_or(u64::MAX))
}

fn validate_decision(decision: &LossDecision) -> Result<(), BrokerError> {
    decision.acknowledgement.validate_for(&decision.request)?;
    let (id, reason) = match &decision.acknowledgement.disposition {
        ControllerLossDisposition::Recoverable {
            attention_projection_id,
            ..
        } => (attention_projection_id, AttentionReason::RunParked),
        ControllerLossDisposition::NoRecovery {
            attention_projection_id,
        } => (attention_projection_id, AttentionReason::SessionFailed),
    };
    if *id != condition_id(&decision.request.canonical_bytes())
        || decision.attention.operation_id != *id
        || decision.attention.subject
            != AttentionSubject::Session(decision.request.session_id.clone())
        || decision.attention.reason != reason
    {
        return Err(BrokerError::InvalidGrant);
    }
    Ok(())
}
