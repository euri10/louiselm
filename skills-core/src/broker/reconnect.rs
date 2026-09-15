//! Broker restart reconciles exact signed prefixes, never command authority.
use super::{BrokerError, BrokerService, BrokerSession, receive, response, send};
use crate::{
    broker::{
        attention::{AttentionCondition, AttentionReason, AttentionSubject, ProjectionChange},
        read_record, record_name, sync_directory, write_new_record,
    },
    launch_protocol::{BrokerReconnect, ProtocolMessage, ResponseResult},
    launch_receipt::ReceiptHead,
    launch_transport::{LauncherPacket, SeqpacketChannel},
};

impl BrokerService {
    /// Reattaches an authenticated supervisor against the broker's verified prefix.
    /// Runs on the broker I/O worker; callbacks and waits are bounded. Appends only
    /// exact continuous signed suffix bytes. No launch is consumed, command/grant
    /// budget reconstructed, recovery admitted, or Park automatically Resumed.
    /// # Errors
    /// Refuses foreign, conflicting, broker-ahead or unverifiable checkpoints and
    /// unavailable storage. Refusal closes transport and durably queues Attention
    /// when the known Session's storage remains writable.
    pub fn serve_reconnect<F>(
        &self,
        now_ms: u64,
        mut verify: F,
    ) -> Result<BrokerSession, BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        let channel = self.accept()?;
        let result = (|| {
            let packet = receive(&channel)?;
            let LauncherPacket::Request(ProtocolMessage::BrokerReconnect(request)) = packet.packet
            else {
                return Err(BrokerError::InvalidGrant);
            };
            self.reconnect_on(&channel, &request, now_ms, &mut verify)
        })();
        if result.is_err() {
            channel.close();
        }
        result
    }

    /// Reattaches on a connection whose reconnect offer was already read.
    ///
    /// Split out so a running broker can accept once and route by first packet;
    /// the refusal path is identical either way.
    ///
    /// # Errors
    /// Returns the reattachment failures described on [`Self::serve_reconnect`].
    pub(in crate::broker) fn reconnect_on<F>(
        &self,
        channel: &SeqpacketChannel,
        request: &BrokerReconnect,
        now_ms: u64,
        verify: &mut F,
    ) -> Result<BrokerSession, BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        let result = self.reconnect_transaction(channel, request, verify);
        if result.is_err()
            && self
                .authorizations
                .consumed_for_session(&request.session_id)?
                .is_some()
        {
            self.reconnect_failure(request, now_ms)?;
        }
        result
    }

    fn reconnect_transaction<F>(
        &self,
        channel: &SeqpacketChannel,
        request: &BrokerReconnect,
        verify: &mut F,
    ) -> Result<BrokerSession, BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        request.validate()?;
        let pending = self
            .authorizations
            .consumed_for_session(&request.session_id)?
            .ok_or(BrokerError::UnknownAuthorization)?;
        self.check_history(&request.session_id)?;
        if request.run_id != pending.run_id
            || request.envelope_revision != pending.envelope_revision
            || self.lifecycle.is_quarantined(&request.session_id)?
        {
            return Err(BrokerError::RequestMismatch);
        }
        let authorization = pending.launch_authorization();
        let chain = self.verified_history(&authorization, verify)?;
        let start = chain.get(1).ok_or(BrokerError::ReceiptUnauthorized)?;
        let head = chain.last().ok_or(BrokerError::ReceiptUnauthorized)?;
        if request.sequence < head.payload.sequence
            || (request.sequence == head.payload.sequence
                && request.receipt_digest != head.digest().to_string())
        {
            return Err(BrokerError::RequestMismatch);
        }
        let reply = BrokerReconnect {
            sequence: head.payload.sequence,
            receipt_digest: head.digest().to_string(),
            ..request.clone()
        };
        send(
            channel,
            response(
                &request.request_id,
                ResponseResult::BrokerReconnect { reconnect: reply },
            ),
        )?;
        let mut sequence = head.payload.sequence;
        while sequence < request.sequence {
            let packet = receive(channel)?;
            let LauncherPacket::SignedReceipt(receipt) = &packet.packet else {
                return Err(BrokerError::ReceiptUnauthorized);
            };
            let next = sequence
                .checked_add(1)
                .ok_or(BrokerError::ReceiptUnauthorized)?;
            if receipt.payload.sequence != next
                || (next == request.sequence
                    && receipt.digest().to_string() != request.receipt_digest)
            {
                return Err(BrokerError::RequestMismatch);
            }
            self.lifecycle.check_receipt(receipt)?;
            let ack = self
                .receipts
                .append(&authorization, &packet.bytes, None, &mut *verify)?;
            send(channel, ack.canonical_bytes())?;
            sequence = next;
        }
        Ok(BrokerSession {
            posture_evidence: self.retain_launch_posture(&authorization, verify)?,
            require_cold_recovery: pending.require_cold_recovery,
            recovery_admitted_until: None,
            authorization,
            launch_head: ReceiptHead {
                sequence: start.payload.sequence,
                digest: start.digest().to_string(),
            },
            channel: channel.clone(),
            commands: None,
        })
    }

    fn reconnect_failure(&self, request: &BrokerReconnect, now_ms: u64) -> Result<(), BrokerError> {
        self.lifecycle.quarantine(&request.session_id)?;
        let directory = self.authorizations.root.join("reconnect-failures");
        std::fs::create_dir_all(&directory).map_err(BrokerError::Storage)?;
        sync_directory(&self.authorizations.root)?;
        let path = directory.join(record_name(&request.session_id)?);
        let condition = if let Some(prior) = read_record::<AttentionCondition>(&path)? {
            prior
        } else {
            let digest = crate::Digest::of(request.session_id.as_bytes());
            let hex = digest.hex();
            let condition = AttentionCondition {
                subject: AttentionSubject::Session(request.session_id.clone()),
                operation_id: format!(
                    "{}-{}-{}-{}-{}",
                    &hex[..8],
                    &hex[8..12],
                    &hex[12..16],
                    &hex[16..20],
                    &hex[20..32]
                ),
                created_at_ms: now_ms,
                reason: AttentionReason::SessionFailed,
            };
            write_new_record(&path, &condition)?;
            condition
        };
        self.attention.enqueue(
            &format!("reconnect-{}", request.session_id),
            ProjectionChange::Upsert(condition),
        )?;
        Ok(())
    }
}
