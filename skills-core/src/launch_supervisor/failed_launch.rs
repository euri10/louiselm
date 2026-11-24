//! Startup history remains owned by the original coordinator, including signed
//! envelopes whose ACK was lost. No replacement owner may create an outcome.

use super::{
    Authorization, IdentityGuard, LaunchAuthorization, LaunchRequest, RECEIPT_SCHEMA,
    ReceiptOutcome, ReceiptPayload, SIGNED_RECEIPT_SCHEMA, SessionState, SignedReceipt,
    SupervisorError, SupervisorInner, await_signer, deliver_receipt, mpsc,
};
use crate::{
    launch_receipt::{AgentExecution, LaunchFailure},
    launcher_install::startup_receipts::StartupReceipt,
};

pub(super) struct Startup<'a> {
    inner: &'a SupervisorInner,
    request: &'a LaunchRequest,
    authorization: &'a LaunchAuthorization,
    pub(super) records: Vec<StartupReceipt>,
    pub(super) execution: AgentExecution,
    /// A failed signing callback may have registered unseen genesis bytes.
    pub(super) uncertain: bool,
}

impl<'a> Startup<'a> {
    pub(super) fn new(
        inner: &'a SupervisorInner,
        request: &'a LaunchRequest,
        authorization: &'a LaunchAuthorization,
    ) -> Self {
        Self {
            inner,
            request,
            authorization,
            records: Vec::new(),
            execution: AgentExecution::NotStarted,
            uncertain: false,
        }
    }

    pub(super) fn receipt(
        &mut self,
        payload: ReceiptPayload,
        report: Option<Vec<u8>>,
    ) -> Result<SignedReceipt, SupervisorError> {
        let inner = self.inner;
        let receipt = sign_startup_receipt(inner, self, payload, report.clone())?;
        deliver_receipt(inner, &receipt, report)?;
        Ok(receipt)
    }

    pub(super) fn finish(
        &mut self,
        identity: Box<dyn IdentityGuard>,
        original: SupervisorError,
    ) -> SupervisorError {
        if self.uncertain || original == SupervisorError::CleanupUnproven {
            // A dropped production lease keeps its durable poison marker.
            // Unseen signed bytes or uncertain cleanup cannot release this lease.
            drop(identity);
            return recovery(original);
        }
        let (inner, request, authorization) = (self.inner, self.request, self.authorization);
        let proof = (|| {
            let (sender, receiver) = mpsc::sync_channel(1);
            inner.platform.seal_failed_launch_storage(
                request,
                &inner.sessions_root,
                Box::new(move |result| {
                    let _ = sender.try_send(result);
                }),
            )?;
            let retained_storage = receiver
                .recv_timeout(inner.timeout)
                .map_err(|_| SupervisorError::CleanupUnproven)??;
            let previous = self
                .records
                .last()
                .map(|record| SignedReceipt::parse_canonical(&record.receipt_bytes))
                .transpose()
                .map_err(|_| SupervisorError::ReceiptInvalid)?;
            let (phase, code) = original.diagnostic_parts();
            let payload = ReceiptPayload {
                schema: RECEIPT_SCHEMA.into(),
                session_id: request.session_id.clone(),
                run_id: request.run_id.clone(),
                request_id: previous.as_ref().map_or_else(
                    || request.request_id.clone(),
                    |receipt| format!("failed-{}", receipt.digest().hex()),
                ),
                envelope_revision: request.envelope_revision,
                sequence: previous
                    .as_ref()
                    .map_or(0, |receipt| receipt.payload.sequence + 1),
                previous_receipt_digest: previous
                    .as_ref()
                    .map(|receipt| receipt.digest().to_string()),
                release_id: inner.signer.release_id().into(),
                signing_key_id: inner.signer.signing_key_id().into(),
                outcome: ReceiptOutcome::FailedLaunch {
                    authorization: Box::new(Authorization {
                        authorization_id: authorization.authorization_id.clone(),
                        request_id: authorization.request_id.clone(),
                        request_digest: authorization.request_digest.clone(),
                    }),
                    identity_slot: authorization.identity_slot,
                    assigned_uid: authorization.assigned_uid,
                    assigned_gid: authorization.assigned_gid,
                    failure: LaunchFailure {
                        phase: phase.into(),
                        code: code.into(),
                    },
                    agent_execution: self.execution,
                    retained_storage,
                },
                resulting_state: SessionState::Terminal,
            };
            Ok::<_, SupervisorError>(payload)
        })();
        let Ok(payload) = proof else {
            drop(identity);
            return recovery(original);
        };
        if identity.release().is_err() {
            return recovery(original);
        }
        // Local release grants no broker capacity. It must finish before any
        // saved terminal proof can permit a later receipt-only delivery.
        let Ok(receipt) = sign_startup_receipt(inner, self, payload, None) else {
            return recovery(original);
        };
        if deliver_receipt(inner, &receipt, None).is_err() {
            return SupervisorError::StartupDeliveryPending {
                failure: Box::new(original),
            };
        }
        let (sender, receiver) = mpsc::sync_channel(1);
        if inner
            .signer
            .complete_session(
                receipt.payload,
                Box::new(move |result| {
                    let _ = sender.try_send(result);
                }),
            )
            .is_err()
            || !matches!(receiver.recv_timeout(inner.timeout), Ok(Ok(())))
        {
            return SupervisorError::StartupKeyCleanupPending {
                failure: Box::new(original),
            };
        }
        original
    }
}

pub(super) fn recovery(failure: SupervisorError) -> SupervisorError {
    SupervisorError::RecoveryRequired {
        failure: Box::new(failure),
    }
}

pub(super) fn sign_startup_receipt(
    inner: &SupervisorInner,
    startup: &mut Startup<'_>,
    payload: ReceiptPayload,
    report_bytes: Option<Vec<u8>>,
) -> Result<SignedReceipt, SupervisorError> {
    payload
        .validate()
        .map_err(|_| SupervisorError::ReceiptInvalid)?;
    startup.uncertain = true;
    let signature = await_signer(inner, payload.canonical_bytes())?;
    let receipt = SignedReceipt {
        schema: SIGNED_RECEIPT_SCHEMA.into(),
        payload,
        signature,
    };
    receipt
        .validate()
        .map_err(|_| SupervisorError::ReceiptInvalid)?;
    startup.records.push(StartupReceipt {
        receipt_bytes: receipt.canonical_bytes(),
        report_bytes,
    });
    let (sender, receiver) = mpsc::sync_channel(1);
    inner.signer.save_startup_receipts(
        startup.records.clone(),
        Box::new(move |result| {
            let _ = sender.try_send(result);
        }),
    )?;
    receiver
        .recv_timeout(inner.timeout)
        .map_err(|_| SupervisorError::DurabilityUnavailable)??;
    startup.uncertain = false;
    Ok(receipt)
}
