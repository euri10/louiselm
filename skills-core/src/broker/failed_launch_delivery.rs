//! Receipt-only delivery creates neither a Session owner nor launch authority.

use super::{BrokerService, STEP_TIMEOUT, conformance_transfer, receive, send};
use crate::{
    broker::{AuditDecision, BrokerError},
    launch_receipt::ReceiptOutcome,
    launch_transport::{AuthenticatedPacket, LauncherPacket, SeqpacketChannel},
};
use std::time::Instant;

impl BrokerService {
    pub(super) fn deliver_failed_on<F>(
        &self,
        channel: &SeqpacketChannel,
        mut packet: AuthenticatedPacket,
        now_ms: u64,
        verify: &mut F,
    ) -> Result<(), BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        let LauncherPacket::SignedReceipt(first) = &packet.packet else {
            return Err(BrokerError::ReceiptUnauthorized);
        };
        let authorization = self
            .authorizations()
            .consumed_for_session(&first.payload.session_id)?
            .ok_or(BrokerError::UnknownAuthorization)?
            .launch_authorization();
        for sequence in 0..3 {
            let LauncherPacket::SignedReceipt(receipt) = &packet.packet else {
                return Err(BrokerError::ReceiptUnauthorized);
            };
            if receipt.payload.sequence != sequence
                || (!receipt.payload.outcome.is_failed_launch()
                    && !matches!(
                        receipt.payload.outcome,
                        ReceiptOutcome::Launch { .. } | ReceiptOutcome::Start { .. }
                    ))
            {
                return Err(BrokerError::ReceiptUnauthorized);
            }
            self.check_history(&authorization.session_id)?;
            let history = self.receipts().verified_chain(&authorization, &mut *verify);
            self.history_result(&authorization.session_id, history)?;
            let report = conformance_transfer::receive_report(
                channel,
                receipt,
                Instant::now() + STEP_TIMEOUT,
            )?;
            let ack = self.receipts().append_saved(
                &authorization,
                &packet.bytes,
                report.as_deref(),
                verify,
            )?;
            self.record_for(
                &authorization,
                now_ms,
                AuditDecision::ReceiptStored {
                    sequence: ack.sequence,
                },
            )?;
            send(channel, ack.canonical_bytes())?;
            if receipt.payload.outcome.is_failed_launch() {
                return Ok(());
            }
            packet = receive(channel)?;
        }
        Err(BrokerError::ReceiptUnauthorized)
    }
}
