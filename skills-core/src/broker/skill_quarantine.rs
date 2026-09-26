//! Carries the operator's skill quarantine to running Sessions
//! (`louiselm-d6fv.6.5.1`).
//!
//! Skill quarantine (`crate::quarantine`) excludes packages from the current
//! Generation, but a running Session keeps the Generation it was launched with.
//! Each Session worker therefore checks, on its idle tick, whether the
//! quarantine reaches its pinned Generation. When it does, the broker writes the
//! Session's durable quarantine marker (which every capability service already
//! refuses on), revokes command and tool authority, then Parks the Session. The
//! marker is write-once and withdraws Resume.
//!
//! Evidence is read only through the installed [`AdmissionSource`]. Without
//! one, no skill quarantine is observable here. Unreadable evidence
//! reaches the Session immediately: the broker cannot prove
//! it unaffected, and a narrowing is never skipped silently.

use super::{
    BrokerError, BrokerService, BrokerSession,
    admission_source::{AdmissionSource, QuarantineReach},
    corrupt, is_record_identifier,
    lifecycle::LifecycleCaller,
};
use crate::{
    Digest,
    launch::PROTOCOL_VERSION,
    launch_protocol::{
        LIFECYCLE_REQUEST_SCHEMA, LaunchAuthorization, LifecycleAction, LifecycleRequest,
    },
    launch_receipt::{ReceiptHead, ReceiptOutcome, SessionState, SignedReceipt},
};
use serde::{Deserialize, Serialize};

pub(super) const SESSION_TAINT_SCHEMA: &str = "louiselm.session-taint/1";

/// Closed, safe source of a Session output taint. No operator reason is retained.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TaintSource {
    /// Exact operator quarantine bytes that reached the pinned Generation.
    Quarantine {
        /// SHA-256 digest of the exact trusted quarantine file bytes.
        digest: String,
    },
    /// Source evidence was unavailable or invalid, so reach could not be disproved.
    EvidenceUnreadable,
}

/// Durable, immutable broker provenance for a quarantined Session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SessionTaint {
    schema: String,
    session_id: String,
    run_id: String,
    skill_generation_id: String,
    source: TaintSource,
    detected_at_ms: u64,
    digest: String,
}

/// Secret-free taint projection returned by broker inspection for one subject.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionTaintProjection {
    /// Digest of the immutable canonical taint body.
    pub digest: String,
    /// Generation pinned by the Session's signed launch receipt.
    pub skill_generation_id: String,
    /// Exact quarantine digest or a closed unreadable-evidence code.
    pub source: TaintSource,
    /// First broker detection time in Unix milliseconds.
    pub detected_at_ms: u64,
    /// Later signed quarantine Park, when it was durably stored.
    pub park_receipt: Option<ReceiptHead>,
}

impl SessionTaint {
    pub(super) fn session_id(&self) -> &str {
        &self.session_id
    }

    pub(super) fn run_id(&self) -> &str {
        &self.run_id
    }

    pub(super) fn generation(&self) -> &str {
        &self.skill_generation_id
    }

    pub(super) fn digest(&self) -> &str {
        &self.digest
    }

    pub(super) fn new(
        authorization: &LaunchAuthorization,
        generation: &str,
        source: TaintSource,
        detected_at_ms: u64,
    ) -> Result<Self, BrokerError> {
        let mut record = Self {
            schema: SESSION_TAINT_SCHEMA.into(),
            session_id: authorization.session_id.clone(),
            run_id: authorization.run_id.clone(),
            skill_generation_id: generation.into(),
            source,
            detected_at_ms,
            digest: String::new(),
        };
        record.digest = record.body_digest()?.to_string();
        record.validate(&authorization.session_id)?;
        Ok(record)
    }

    fn body_digest(&self) -> Result<Digest, BrokerError> {
        let bytes = serde_json::to_vec(&(
            &self.schema,
            &self.session_id,
            &self.run_id,
            &self.skill_generation_id,
            &self.source,
            self.detected_at_ms,
        ))
        .map_err(|_| BrokerError::InvalidGrant)?;
        Ok(Digest::of(&bytes))
    }

    pub(super) fn validate(&self, session_id: &str) -> Result<(), BrokerError> {
        if self.schema != SESSION_TAINT_SCHEMA
            || self.session_id != session_id
            || !is_record_identifier(&self.session_id)
            || !is_record_identifier(&self.run_id)
            || Digest::parse(&self.skill_generation_id).is_err()
            || self.detected_at_ms == 0
            || match &self.source {
                TaintSource::Quarantine { digest } => Digest::parse(digest).is_err(),
                TaintSource::EvidenceUnreadable => false,
            }
            || self.digest != self.body_digest()?.to_string()
        {
            return Err(corrupt("Session output taint is invalid"));
        }
        Ok(())
    }

    pub(super) fn projection(&self, park_receipt: Option<ReceiptHead>) -> SessionTaintProjection {
        SessionTaintProjection {
            digest: self.digest.clone(),
            skill_generation_id: self.skill_generation_id.clone(),
            source: self.source.clone(),
            detected_at_ms: self.detected_at_ms,
            park_receipt,
        }
    }
}

/// Per-Session worker state; lost on restart, which only repeats a check.
#[derive(Debug, Default)]
pub(super) struct SkillQuarantineWatch {
    /// Generation pinned by the Session's signed launch receipt.
    generation: Option<String>,
    /// Exact quarantine bytes already found not to reach this Session.
    clear: Option<Digest>,
    /// The Session is quarantined and no longer Running; nothing left to do.
    settled: bool,
}

impl BrokerService {
    /// Quarantines and Parks this Session when the operator's skill quarantine
    /// reaches its pinned Generation.
    ///
    /// Run on the Session's broker worker, on its idle tick. Order: durable
    /// quarantine marker, then command/tool revocation, then Park through the
    /// ordinary lifecycle owner as [`LifecycleCaller::SkillQuarantine`]. Returns
    /// the Park receipt when this call parked the Session.
    ///
    /// # Errors
    /// Returns storage, revocation, transport, verification or lifecycle
    /// failures. Unreadable evidence narrows authority immediately. A failed
    /// revocation closes the Session's channel.
    pub fn settle_skill_quarantine<F>(
        &self,
        session: &mut BrokerSession,
        source: Option<&AdmissionSource>,
        now_ms: u64,
        mut verify: F,
    ) -> Result<Option<SignedReceipt>, BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        let Some(source) = source else {
            return Ok(None);
        };
        if session.skill_quarantine.settled {
            return Ok(None);
        }
        let authorization = session.authorization().clone();
        // Reach is decided first even for an already-marked Session: this path
        // acts only on Sessions the skill quarantine reaches, leaving markers
        // written for other causes (history, reconnect) to their owners.
        let reach = self.skill_quarantine_reach(session, source);
        let taint_source = match reach {
            Ok(QuarantineReach::Unaffected { quarantine }) => {
                session.skill_quarantine.clear = quarantine;
                return Ok(None);
            }
            Ok(QuarantineReach::Affected { quarantine }) => TaintSource::Quarantine {
                digest: quarantine.to_string(),
            },
            // An evidence failure cannot prove this Session unaffected.
            Err(_) => TaintSource::EvidenceUnreadable,
        };
        if let Some(generation) = session.skill_quarantine.generation.as_deref() {
            let taint = SessionTaint::new(&authorization, generation, taint_source, now_ms)?;
            // This durable record is the capability refusal, even when Park fails.
            self.lifecycle.record_skill_taint(&taint)?;
            self.audit_session_taint(&authorization, now_ms)?;
        } else {
            // An unreadable launch chain cannot supply a trustworthy pinned
            // Generation. Keep the earlier fail-closed quarantine behavior.
            self.lifecycle.quarantine(&authorization.session_id)?;
        }
        // Capabilities close before the Park; a second request would be refused.
        if session.commands.is_some() && !session.command_revocation_requested() {
            session.revoke_commands(&format!(
                "skill-quarantine-{}",
                &Digest::of(authorization.session_id.as_bytes()).hex()[..48]
            ))?;
        }
        let status = self.supervisor_status(session, &mut verify)?;
        if status.pending_operation.is_some() {
            return Ok(None);
        }
        if status.state != SessionState::Running {
            session.skill_quarantine.settled = true;
            return Ok(None);
        }
        let observed = status.broker_head.as_ref().map(|head| head.sequence);
        // One identity per Session and observed head: a retry after an uncertain
        // send repeats the same bytes, a changed head is a new request.
        let request_id = format!(
            "skill-quarantine-{}",
            &Digest::of(format!("{}:{observed:?}", authorization.session_id).as_bytes()).hex()
                [..48]
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
        let caller = LifecycleCaller::SkillQuarantine {
            run_id: authorization.run_id,
        };
        let receipt = self.request_lifecycle(session, &caller, &request, now_ms, verify)?;
        session.skill_quarantine.settled = true;
        Ok(Some(receipt))
    }

    fn skill_quarantine_reach(
        &self,
        session: &mut BrokerSession,
        source: &AdmissionSource,
    ) -> Result<QuarantineReach, BrokerError> {
        if session.skill_quarantine.generation.is_none() {
            let chain = self.receipts().chain(&session.authorization().session_id)?;
            let generation = chain
                .first()
                .and_then(|receipt| match &receipt.payload.outcome {
                    ReceiptOutcome::Launch { evidence, .. } => {
                        Some(evidence.skill_generation_id.clone())
                    }
                    _ => None,
                })
                .ok_or(BrokerError::InvalidGrant)?;
            session.skill_quarantine.generation = Some(generation);
        }
        let generation = session
            .skill_quarantine
            .generation
            .as_deref()
            .ok_or(BrokerError::InvalidGrant)?;
        source.skill_quarantine(generation, session.skill_quarantine.clear.as_ref())
    }
}
