//! A launch's supplemental bytes share its authenticated peer and deadline.

use super::{BrokerError, LauncherPacket, SeqpacketChannel, TransportError, receive_for};
use crate::launch_receipt::{ConformanceEvidence, ReceiptOutcome, SignedReceipt};
use std::time::Instant;

pub(super) fn receive_report(
    channel: &SeqpacketChannel,
    receipt: &SignedReceipt,
    deadline: Instant,
) -> Result<Option<Vec<u8>>, BrokerError> {
    let ReceiptOutcome::Launch { evidence, .. } = &receipt.payload.outcome else {
        return Ok(None);
    };
    if !matches!(
        evidence.conformance,
        ConformanceEvidence::Certified { .. }
            | ConformanceEvidence::Waived {
                report_digest: Some(_),
                ..
            }
    ) {
        return Ok(None);
    }
    let digest = receipt.digest().to_string();
    let mut bytes = Vec::new();
    let mut total = None;
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|duration| !duration.is_zero())
            .ok_or(BrokerError::Transport(TransportError::Closed))?;
        let packet = receive_for(channel, remaining)?;
        let LauncherPacket::ConformanceReportChunk(chunk) = packet.packet else {
            return Err(BrokerError::ReceiptUnauthorized);
        };
        if chunk.receipt_digest != digest
            || chunk.offset != bytes.len()
            || total.is_some_and(|total| chunk.total_bytes != total)
        {
            return Err(BrokerError::ReceiptUnauthorized);
        }
        // The decoder bounded both allocation and fixed fragment count. No
        // bytes reach disk until ReceiptStore verifies signature and contents.
        total = Some(chunk.total_bytes);
        bytes.extend_from_slice(&chunk.bytes);
        if bytes.len() == chunk.total_bytes {
            return Ok(Some(bytes));
        }
    }
}
