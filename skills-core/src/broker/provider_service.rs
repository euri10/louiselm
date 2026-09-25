//! Per-request admission of brokered Provider calls on the Session owner.
//!
//! Follows the `louiselm-qbr.5.1.3.8` boundary: every complete request is
//! checked against current authority on the worker that owns the
//! `BrokerSession`, one durable Run unit is spent, time and channel are
//! rechecked after that storage I/O, and only then does the upstream attempt
//! start. No permit leaves the owner turn.

use std::{
    io::{self, Read},
    time::{Duration, Instant},
};

use super::{
    BrokerError, BrokerService, BrokerSession,
    attention::{
        AttentionCondition, AttentionReason, AttentionSubject, ProjectionChange, canonical_uuid,
        condition_id,
    },
    is_record_identifier,
    lifecycle::LifecycleCaller,
    provider_credentials::ProviderCredentialStore,
    provider_extension::{ExtensionError, ExtensionOutcome, ExtensionRequest, OUTCOME_SCHEMA},
    provider_requests::{HoldReason, ProviderHold, total},
};
use crate::{
    broker::provider_transport::{ProviderTransport, UpstreamResponse},
    conformance::admission::Attendance,
    launch::PROTOCOL_VERSION,
    launch_protocol::{
        ChannelState, ErrorCode, LIFECYCLE_REQUEST_SCHEMA, LifecycleAction, LifecycleRequest,
        ProtocolError,
    },
    launch_receipt::{SessionState, SignedReceipt},
    provider_request::{ApprovedProviderRequests, MAX_RUN_REQUESTS, ProviderRequest},
};

impl BrokerService {
    /// Admits one complete Provider request and starts its upstream attempt.
    ///
    /// Run on the Session's broker worker. The broker-held credential for the
    /// granted Provider authenticates the attempt; it never reaches the Session.
    ///
    /// # Errors
    /// Refuses without spending a unit when the launch grants no valid Provider
    /// permission ([`BrokerError::InvalidGrant`], [`BrokerError::Expired`]), the
    /// request names a Model or effort outside the grant
    /// ([`ErrorCode::CapabilityDenied`]), the approved metadata profile differs
    /// or request metadata is unreviewed ([`ErrorCode::ProviderDisclosureDenied`]),
    /// the credential is not configured, or
    /// the Session is not running with an
    /// enabled channel. Refuses with [`BrokerError::ProviderBudgetExhausted`]
    /// once the Run's total is spent. After a unit is spent, an expiry or channel
    /// loss discovered before the attempt, an unreachable upstream, or a rejected
    /// key ([`ErrorCode::CredentialUnavailable`]) keeps that unit spent; nothing
    /// is retried. The returned body fails every read from the earliest
    /// permission or launch expiry on, cutting a stream still in flight.
    pub fn serve_provider_request<F>(
        &self,
        session: &mut BrokerSession,
        credentials: &ProviderCredentialStore,
        transport: &dyn ProviderTransport,
        request: &ProviderRequest,
        now_ms: u64,
        mut verify: F,
    ) -> Result<UpstreamResponse, BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        let clock = Instant::now();
        let elapsed = || {
            now_ms.saturating_add(u64::try_from(clock.elapsed().as_millis()).unwrap_or(u64::MAX))
        };
        let authorization = session.authorization().clone();
        let approved = self
            .authorizations()
            .consumed_for_session(&authorization.session_id)?
            .ok_or(BrokerError::UnknownAuthorization)?;
        if approved.run_id != authorization.run_id
            || approved.envelope_revision != authorization.envelope_revision
        {
            return Err(BrokerError::RequestMismatch);
        }
        let permission = &self.extended(
            &authorization.run_id,
            approved
                .provider_requests
                .as_ref()
                .ok_or(BrokerError::InvalidGrant)?,
        )?;
        let live = |at: u64| {
            permission.valid(at) && at < authorization.expires_at_ms && at < approved.expires_at_ms
        };
        if permission.disclosure_profile != crate::provider_request::disclosure::profile_digest() {
            return Err(ProtocolError::new(ErrorCode::ProviderDisclosureDenied, None, None).into());
        }
        request.validate_disclosure()?;
        let run_id = authorization.run_id.as_str();
        if let Some(hold) = self.provider_requests.held(run_id)? {
            return Err(hold_refusal(hold.reason));
        }
        if !live(now_ms) {
            return Err(self.expired(run_id, permission, now_ms));
        }
        if !permission.permits(&request.model, request.effort.as_deref()) {
            return Err(ProtocolError::new(ErrorCode::CapabilityDenied, None, None).into());
        }
        let handle = credentials.handle(&permission.provider)?;
        let status = self.supervisor_status(session, &mut verify)?;
        if status.state != SessionState::Running
            || status.channel_state != ChannelState::Enabled
            || status.pending_operation.is_some()
            || self.lifecycle.is_quarantined(&authorization.session_id)?
        {
            return Err(BrokerError::InvalidGrant);
        }
        if !live(elapsed()) || session.channel().is_closed() {
            return Err(self.expired(run_id, permission, elapsed()));
        }
        match self.provider_requests.reserve(
            run_id,
            &authorization.session_id,
            permission.max_run_requests,
            elapsed(),
        ) {
            Err(BrokerError::ProviderBudgetExhausted) => {
                self.provider_requests
                    .hold(run_id, HoldReason::Exhausted, elapsed())?;
                return Err(BrokerError::ProviderBudgetExhausted);
            }
            other => other?,
        };
        // The unit is spent. Storage I/O cannot extend expiry or a lost channel.
        if !live(elapsed()) || session.channel().is_closed() {
            return Err(self.expired(run_id, permission, elapsed()));
        }
        // The earliest expiry, as a monotonic instant: wall-clock steps cannot
        // stretch an admitted stream past its permission.
        let expires_at_ms = permission
            .expires_at_ms
            .min(authorization.expires_at_ms)
            .min(approved.expires_at_ms);
        let deadline = clock + Duration::from_millis(expires_at_ms.saturating_sub(now_ms));
        let mut response = credentials.with_secret(&handle, |bearer| {
            transport.send(permission, bearer, request, deadline)
        })??;
        if matches!(response.status, 401 | 403) {
            return Err(ProtocolError::new(ErrorCode::CredentialUnavailable, None, None).into());
        }
        response.body = Box::new(ExpiringBody {
            inner: response.body,
            deadline,
        });
        Ok(response)
    }

    /// Parks this Session and raises the Run's Attention item once its Provider
    /// budget is held, recording an expiry hold first when the permission lapsed.
    ///
    /// Run on the Session's broker worker, on its idle tick and after a refused
    /// Provider request, so every Session of a held Run parks itself without any
    /// cross-worker signal. Returns the Park receipt when this call parked the
    /// Session, and `None` when there is no hold, no Provider permission, or the
    /// Session is not Running. Idempotent: the Attention item and the Park
    /// request derive their identities from the durable hold.
    ///
    /// # Errors
    /// Returns storage, Attention, transport, verification or lifecycle policy
    /// failures; the hold stays recorded and the next tick retries.
    pub fn settle_provider_hold<F>(
        &self,
        session: &mut BrokerSession,
        now_ms: u64,
        mut verify: F,
    ) -> Result<Option<SignedReceipt>, BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        let authorization = session.authorization().clone();
        let Some(permission) = self
            .authorizations()
            .consumed_for_session(&authorization.session_id)?
            .and_then(|approved| approved.provider_requests)
        else {
            return Ok(None);
        };
        let permission = self.extended(&authorization.run_id, &permission)?;
        let hold = match self.provider_requests.held(&authorization.run_id)? {
            Some(hold) => hold,
            None if now_ms >= permission.expires_at_ms => {
                self.provider_requests
                    .hold(&authorization.run_id, HoldReason::Expired, now_ms)?
            }
            None => return Ok(None),
        };
        self.raise_hold_attention(&authorization.session_id, &hold)?;
        let status = self.supervisor_status(session, &mut verify)?;
        if status.state != SessionState::Running || status.pending_operation.is_some() {
            return Ok(None);
        }
        let observed = status.broker_head.as_ref().map(|head| head.sequence);
        // One request identity per hold and observed head: a retry after an
        // uncertain send repeats the same bytes, a changed head is a new request.
        let request_id = format!(
            "provider-hold-{}",
            &crate::Digest::of(
                format!(
                    "{}:{}:{}:{observed:?}",
                    hold.run_id, authorization.session_id, hold.held_at_ms
                )
                .as_bytes()
            )
            .hex()[..48]
        );
        let request = LifecycleRequest {
            schema: LIFECYCLE_REQUEST_SCHEMA.into(),
            protocol_version: PROTOCOL_VERSION,
            authorization_id: request_id.clone(),
            request_id,
            session_id: authorization.session_id.clone(),
            run_id: authorization.run_id.clone(),
            action: LifecycleAction::Park,
            expected_state: SessionState::Running,
            expected_receipt_sequence: observed,
            envelope_revision: authorization.envelope_revision,
        };
        let caller = LifecycleCaller::ProviderBudget {
            run_id: authorization.run_id,
        };
        self.request_lifecycle(session, &caller, &request, now_ms, verify)
            .map(Some)
    }

    /// The Run's durable Provider budget hold, when one was recorded.
    ///
    /// # Errors
    /// Returns [`BrokerError::Storage`] or [`BrokerError::InvalidGrant`] when
    /// the hold record is unreadable or names another Run.
    pub fn provider_hold(&self, run_id: &str) -> Result<Option<ProviderHold>, BrokerError> {
        self.provider_requests.held(run_id)
    }

    /// One Attention item per held Run, identical from every Session.
    fn raise_hold_attention(
        &self,
        session_id: &str,
        hold: &ProviderHold,
    ) -> Result<(), BrokerError> {
        let (mutation_id, condition) = hold_condition(session_id, hold);
        self.attention
            .enqueue(&mutation_id, ProjectionChange::Upsert(condition))?;
        Ok(())
    }

    /// Lifts the Run's Provider budget hold with an operator extension.
    ///
    /// Run on the Session's broker worker. Only the Session's controller may
    /// extend, and only for an interactive Session. The extension adds units to
    /// the Run's shared total and may move the Provider permission's expiry
    /// later, never past the launch's own expiry; afterwards the Run must have
    /// a unit left and an expiry in the future, or nothing is recorded. The
    /// Attention item is cleared. The Session stays Parked until the operator
    /// Resumes it. Retrying the same request returns the recorded extension.
    ///
    /// # Errors
    /// Returns [`BrokerError::ProviderExtension`] for policy refusals, or
    /// storage and Attention failures.
    pub fn extend_provider_budget(
        &self,
        session: &BrokerSession,
        operator_uid: u32,
        request: &ExtensionRequest,
        now_ms: u64,
    ) -> Result<ExtensionOutcome, BrokerError> {
        let authorization = session.authorization();
        if operator_uid != authorization.controller_uid {
            return Err(ExtensionError::WrongOperator.into());
        }
        if authorization.conformance.attendance != Attendance::Interactive {
            return Err(ExtensionError::Unattended.into());
        }
        if !is_record_identifier(&request.request_id)
            || request.additional_requests > MAX_RUN_REQUESTS
        {
            return Err(ExtensionError::InvalidRequest.into());
        }
        let approved = self
            .authorizations()
            .consumed_for_session(&authorization.session_id)?
            .ok_or(ExtensionError::Unknown)?;
        let granted = approved
            .provider_requests
            .as_ref()
            .ok_or(ExtensionError::NotHeld)?;
        let launch_expiry = authorization.expires_at_ms.min(approved.expires_at_ms);
        let run_id = authorization.run_id.as_str();
        let extension = self.provider_requests.extend(
            run_id,
            request,
            operator_uid,
            now_ms,
            |_hold, prior, spent| {
                let total = total(granted.max_run_requests, prior)
                    .ok()
                    .and_then(|total| total.checked_add(request.additional_requests))
                    .filter(|total| *total <= MAX_RUN_REQUESTS)
                    .ok_or(ExtensionError::InvalidRequest)?;
                if let Some(expiry) = request.expires_at_ms
                    && (expiry <= now_ms || expiry > launch_expiry)
                {
                    return Err(ExtensionError::ExpiryOutOfRange);
                }
                let expiry = request
                    .expires_at_ms
                    .or_else(|| prior.iter().rev().find_map(|e| e.request.expires_at_ms))
                    .unwrap_or(granted.expires_at_ms);
                // Lifting a hold the Run would immediately hit again is not an extension.
                if spent >= total || expiry <= now_ms {
                    return Err(ExtensionError::Insufficient);
                }
                Ok(())
            },
        )?;
        let (mutation_id, condition) = hold_condition(&authorization.session_id, &extension.lifts);
        self.attention.enqueue(
            &format!("{mutation_id}-lifted"),
            ProjectionChange::Clear(condition),
        )?;
        let extensions = self.provider_requests.extensions(run_id)?;
        Ok(ExtensionOutcome {
            schema: OUTCOME_SCHEMA.into(),
            session_id: authorization.session_id.clone(),
            total_requests: total(granted.max_run_requests, &extensions)?,
            spent: self.provider_requests.spent(run_id)?,
            expires_at_ms: self.extended(run_id, granted)?.expires_at_ms,
            extension,
        })
    }

    /// The grant with the latest operator-extended expiry applied.
    fn extended(
        &self,
        run_id: &str,
        granted: &ApprovedProviderRequests,
    ) -> Result<ApprovedProviderRequests, BrokerError> {
        let mut permission = granted.clone();
        if let Some(expiry) = self
            .provider_requests
            .extensions(run_id)?
            .iter()
            .rev()
            .find_map(|extension| extension.request.expires_at_ms)
        {
            permission.expires_at_ms = expiry;
        }
        Ok(permission)
    }

    /// Records an expiry hold when the Provider permission itself lapsed.
    ///
    /// Launch-authorization expiry belongs to the lifecycle owners, not this
    /// budget, so it refuses without holding the Run.
    fn expired(
        &self,
        run_id: &str,
        permission: &ApprovedProviderRequests,
        at_ms: u64,
    ) -> BrokerError {
        if at_ms >= permission.expires_at_ms
            && let Err(error) = self
                .provider_requests
                .hold(run_id, HoldReason::Expired, at_ms)
        {
            return error;
        }
        BrokerError::Expired
    }

    /// Units the Run has spent, including attempts whose outcome is unknown.
    ///
    /// # Errors
    /// Returns [`BrokerError::Storage`] or [`BrokerError::InvalidGrant`] when
    /// the durable ledger is unreadable or holds unexpected entries.
    pub fn provider_requests_spent(&self, run_id: &str) -> Result<u32, BrokerError> {
        self.provider_requests.spent(run_id)
    }
}

fn hold_refusal(reason: HoldReason) -> BrokerError {
    match reason {
        HoldReason::Exhausted => BrokerError::ProviderBudgetExhausted,
        HoldReason::Expired => BrokerError::Expired,
    }
}

/// Upstream body cut locally at the permission's expiry.
///
/// Bytes read before the deadline were already relayed and stay with the
/// runtime; anything arriving later is dropped and the read fails, so the relay
/// ends without its terminating chunk. A local cut is not proof the Provider
/// stopped, and the spent unit is never refunded.
struct ExpiringBody {
    inner: Box<dyn Read + Send>,
    deadline: Instant,
}

impl Read for ExpiringBody {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let expired = || io::Error::new(io::ErrorKind::TimedOut, "Provider permission expired");
        if Instant::now() >= self.deadline {
            return Err(expired());
        }
        let count = self.inner.read(buffer)?;
        if Instant::now() >= self.deadline {
            return Err(expired());
        }
        Ok(count)
    }
}

/// The Attention identity and condition of one hold, the same from every Session.
fn hold_condition(session_id: &str, hold: &ProviderHold) -> (String, AttentionCondition) {
    // Legacy Run names cannot be an Attention subject; the Session carries it.
    let subject = if canonical_uuid(&hold.run_id) {
        AttentionSubject::Run(hold.run_id.clone())
    } else {
        AttentionSubject::Session(session_id.to_owned())
    };
    let subject_key = match &subject {
        AttentionSubject::Run(id) => format!("run:{id}"),
        AttentionSubject::Session(id) => format!("session:{id}"),
    };
    let identity = format!(
        "provider-hold:{}:{}:{subject_key}",
        hold.run_id, hold.held_at_ms
    );
    (
        format!(
            "provider-hold-{}",
            &crate::Digest::of(identity.as_bytes()).hex()[..48]
        ),
        AttentionCondition {
            linked_run_id: None,
            subject,
            operation_id: condition_id(identity.as_bytes()),
            created_at_ms: hold.held_at_ms,
            reason: AttentionReason::RunParked,
        },
    )
}
