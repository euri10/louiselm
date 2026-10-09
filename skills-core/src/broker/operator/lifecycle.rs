//! Authenticated lifecycle exchange; the broker remains the sole receipt verifier.

use super::{InspectError, Request, exchange};
use crate::{
    launch_protocol::{LifecycleAction, LifecycleRequest, ProtocolError},
    launch_receipt::{ReceiptAuthority, ReceiptOutcome, SignedReceipt},
};
use std::{path::Path, time::Duration};

/// Validates the bounded operator Park, Resume or Disposal request before lookup.
/// Caller authority is never supplied by this record.
/// # Errors
/// Rejects malformed CAS requests, zero revisions and unsupported mechanics.
pub fn validate_lifecycle_request(request: &LifecycleRequest) -> Result<(), InspectError> {
    request
        .validate()
        .map_err(|_| InspectError::InvalidRequest)?;
    if request.envelope_revision == 0
        || request.action == LifecycleAction::Interrupt
        || request.canonical_bytes().len() > crate::launch_protocol::MAX_PROTOCOL_MESSAGE_BYTES
    {
        return Err(InspectError::InvalidRequest);
    }
    Ok(())
}

/// Submits one exact lifecycle CAS through the installed authenticated operator socket.
/// Returns the actual signed receipt or the broker's typed policy refusal; a lost
/// reply never rolls back a durable intent. Inspect before retrying uncertain work.
/// # Errors
/// Transport/authentication failures and invalid or mismatched replies are distinct
/// from the inner policy refusal. This client does not independently verify SSHSIG.
pub fn lifecycle(
    path: &Path,
    broker_uid: u32,
    request: &LifecycleRequest,
    timeout: Duration,
) -> Result<Result<SignedReceipt, ProtocolError>, InspectError> {
    validate_lifecycle_request(request)?;
    let bytes = exchange(
        path,
        broker_uid,
        &Request::Lifecycle {
            request: Box::new(request.clone()),
        },
        timeout,
    )?;
    let response: Result<SignedReceipt, ProtocolError> =
        serde_json::from_slice(&bytes).map_err(|_| InspectError::StatusUnavailable)?;
    if serde_json::to_vec(&response).map_err(|_| InspectError::StatusUnavailable)? != bytes {
        return Err(InspectError::StatusUnavailable);
    }
    match &response {
        Ok(receipt) if receipt_matches(request, receipt) => (),
        Err(error) if error.validate().is_ok() => (),
        _ => return Err(InspectError::StatusUnavailable),
    }
    Ok(response)
}

fn receipt_matches(request: &LifecycleRequest, receipt: &SignedReceipt) -> bool {
    let payload = &receipt.payload;
    if receipt.validate().is_err()
        || payload.session_id != request.session_id
        || payload.run_id != request.run_id
        || payload.request_id != request.request_id
        || payload.envelope_revision != request.envelope_revision
        || request
            .expected_receipt_sequence
            .map_or(Some(0), |sequence| sequence.checked_add(1))
            != Some(payload.sequence)
    {
        return false;
    }
    let ((
        ReceiptOutcome::Park {
            authority: ReceiptAuthority::Authorized(authorization),
        },
        LifecycleAction::Park,
    )
    | (ReceiptOutcome::Resume { authorization }, LifecycleAction::Resume)
    | (
        ReceiptOutcome::Disposal {
            authority: ReceiptAuthority::Authorized(authorization),
        },
        LifecycleAction::Disposal,
    )) = (&payload.outcome, request.action)
    else {
        return false;
    };
    authorization.authorization_id == request.authorization_id
        && authorization.request_id == request.request_id
        && authorization.request_digest == request.digest().to_string()
}
