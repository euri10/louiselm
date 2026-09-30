//! Bounded local operator inspection and skill decisions, authenticated before lookup.

mod wire;

use super::conformance_inspection::{ConformanceInspection, MAX_INSPECTION_BYTES};
use crate::Digest;
use crate::beads_mutation::{
    BeadFailureRequest, BeadsControlDecision, BeadsInspection, BeadsInspectionDetail,
    BeadsMutationStatus,
};
use crate::broker::verification::{VerificationRecord, VerificationStatus};
use crate::broker::{
    GrantRequest,
    run_envelope::{ChildAuthorization, RunAuthorization, RunEnvelope},
};
use crate::launch_protocol::SessionStatus;
use crate::launch_protocol::{LifecycleRequest, VerificationRequest};
use crate::skill_request::{SkillRequestOutcome, SkillRequestStatus};
use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    os::unix::{
        fs::{FileTypeExt, MetadataExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// Fixed installed operator endpoint, distinct from the supervisor rendezvous.
pub const SOCKET: &str = "/run/louiselm-operator/inspect.sock";
/// Total budget for one authenticated inspection exchange.
pub const TIMEOUT: Duration = Duration::from_secs(35);
/// Upper bound for an exact installed verification exchange.
pub const VERIFICATION_TIMEOUT: Duration = Duration::from_mins(10);
const READY: &[u8] = b"louiselm.operator/1";

/// Stable, redacted operator failures; no OS paths or external prose are included.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InspectError {
    /// Invalid command or request fields.
    InvalidRequest,
    /// No trusted broker endpoint could be reached.
    BrokerUnavailable,
    /// The client or server kernel UID is not the installed identity.
    AuthenticationRefused,
    /// The authenticated operator named no known Session.
    UnknownSession,
    /// Current status cannot be obtained without guessing from durable history.
    StatusUnavailable,
}

impl InspectError {
    /// Stable CLI exit status; zero is reserved for canonical success.
    #[must_use]
    pub const fn exit_code(self) -> u8 {
        match self {
            Self::InvalidRequest => 2,
            Self::BrokerUnavailable => 3,
            Self::AuthenticationRefused => 4,
            Self::UnknownSession => 5,
            Self::StatusUnavailable => 6,
        }
    }

    /// Canonical typed diagnostic with a fixed safe next action.
    #[must_use]
    pub fn canonical_bytes(self) -> Vec<u8> {
        let (code, action) = match self {
            Self::InvalidRequest => ("invalid_request", "check_request"),
            Self::BrokerUnavailable => ("broker_unavailable", "check_broker_service"),
            Self::AuthenticationRefused => ("authentication_refused", "use_configured_operator"),
            Self::UnknownSession => ("unknown_session", "check_session_id"),
            Self::StatusUnavailable => ("status_unavailable", "retry_inspection"),
        };
        format!("{{\"schema\":\"louiselm.operator-error/1\",\"error\":\"{code}\",\"next_action\":\"{action}\"}}").into_bytes()
    }

    fn parse(bytes: &[u8]) -> Option<Self> {
        [
            Self::InvalidRequest,
            Self::BrokerUnavailable,
            Self::AuthenticationRefused,
            Self::UnknownSession,
            Self::StatusUnavailable,
        ]
        .into_iter()
        .find(|error| error.canonical_bytes() == bytes)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "schema", deny_unknown_fields)]
enum Request {
    #[serde(rename = "louiselm.operator-verification/1")]
    Verification {
        request: Box<VerificationControlRequest>,
    },
    #[serde(rename = "louiselm.operator-authorization/1")]
    Authorization {
        authorization: Box<AuthorizationRequest>,
    },
    #[serde(rename = "louiselm.operator-conformance-waiver/1")]
    Waiver {
        session_id: String,
        request: super::waiver::Request,
    },
    #[serde(rename = "louiselm.operator-provider-extension/1")]
    ProviderExtension {
        session_id: String,
        request: super::provider_extension::ExtensionRequest,
    },
    #[serde(rename = "louiselm.operator-dependencies/1")]
    Dependencies {
        session_id: String,
        approve: Option<Vec<String>>,
    },
    #[serde(rename = "louiselm.operator-workspace-retention/1")]
    WorkspaceRetention {
        session_id: String,
        pin: Option<bool>,
    },
    #[serde(rename = "louiselm.operator-beads/1")]
    Beads {
        operation_id: String,
        decision: Option<BeadsControlDecision>,
    },
    #[serde(rename = "louiselm.operator-inspect/1")]
    Inspect { session_id: String },
    #[serde(rename = "louiselm.operator-conformance/1")]
    Conformance { session_id: String },
    #[serde(rename = "louiselm.operator-skill-request/1")]
    Skill {
        operation_id: String,
        outcome: Option<SkillRequestOutcome>,
    },
}

/// Operator-owned verification operations. Session channel operations are routed
/// to the one broker worker that owns that channel.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum VerificationControlRequest {
    /// Copy the selected baseline and exact plan into private broker storage.
    Stage {
        /// Identifier of the private staged input.
        input_id: String,
        /// Trusted baseline snapshot directory.
        snapshot: PathBuf,
        /// Exact baseline snapshot digest.
        snapshot_digest: String,
        /// Selected fixed plan file.
        plan: PathBuf,
        /// Exact plan digest approved in the Run envelope.
        plan_digest: String,
    },
    /// Park the producer through its authenticated supervisor.
    Park {
        /// Exact lifecycle CAS request.
        request: LifecycleRequest,
    },
    /// Export the producer's actual frozen workspace.
    Export {
        /// Exact export request.
        request: VerificationRequest,
    },
    /// Execute the exact job in a distinct verifier Session.
    Run {
        /// Exact job request.
        request: VerificationRequest,
    },
    /// Inspect the durable result, including an unknown spent intent.
    Status {
        /// Verifier Session identifier.
        session_id: String,
    },
}

/// Redacted result of one verification control operation.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum VerificationControlResponse {
    /// Exact private input digest.
    Staged {
        /// Digest of staged snapshot and plan.
        input_digest: String,
    },
    /// Signed producer Park receipt head.
    Parked {
        /// Broker-acknowledged head.
        head: crate::launch_receipt::ReceiptHead,
    },
    /// Actual producer export evidence.
    Exported {
        /// Broker-validated export.
        evidence: Box<crate::launch_protocol::VerificationExport>,
        /// Canonical identity of the export evidence.
        export_digest: String,
    },
    /// Actual command outcomes and cleanup evidence.
    Executed {
        /// Completed record.
        record: Box<VerificationRecord>,
        /// Whether every required command actually passed.
        commands_passed: bool,
    },
    /// Current durable applicability.
    Status {
        /// Verification status.
        status: VerificationStatus,
        /// Whether the completed record has a fully passing plan.
        commands_passed: bool,
    },
}

/// Submit one exact verification operation through the authenticated operator socket.
/// # Errors
/// Refuses malformed input, untrusted broker replies, or unavailable ownership.
pub fn verification(
    path: &Path,
    broker_uid: u32,
    request: &VerificationControlRequest,
    timeout: Duration,
) -> Result<VerificationControlResponse, InspectError> {
    let bytes = exchange(
        path,
        broker_uid,
        &Request::Verification {
            request: Box::new(request.clone()),
        },
        timeout,
    )?;
    let response: VerificationControlResponse =
        serde_json::from_slice(&bytes).map_err(|_| InspectError::StatusUnavailable)?;
    if serde_json::to_vec(&response).map_err(|_| InspectError::StatusUnavailable)? != bytes {
        return Err(InspectError::StatusUnavailable);
    }
    let valid = match (request, &response) {
        (
            VerificationControlRequest::Stage { plan_digest, .. },
            VerificationControlResponse::Staged { input_digest },
        ) => Digest::parse(plan_digest).is_ok() && Digest::parse(input_digest).is_ok(),
        (
            VerificationControlRequest::Park { request },
            VerificationControlResponse::Parked { head },
        ) => request
            .expected_receipt_sequence
            .and_then(|sequence| sequence.checked_add(1))
            .is_some_and(|sequence| head.sequence == sequence),
        (
            VerificationControlRequest::Export { request },
            VerificationControlResponse::Exported {
                evidence,
                export_digest,
            },
        ) => {
            evidence.request == *request
                && evidence
                    .digest()
                    .is_ok_and(|digest| digest.to_string() == *export_digest)
        }
        (
            VerificationControlRequest::Run { request },
            VerificationControlResponse::Executed {
                record,
                commands_passed,
            },
        ) => {
            record.execution.request == *request
                && record.execution.validate().is_ok()
                && record.execution.commands_passed() == *commands_passed
        }
        (
            VerificationControlRequest::Status { session_id },
            VerificationControlResponse::Status {
                status,
                commands_passed,
            },
        ) => {
            let valid_record = match status {
                VerificationStatus::Completed(record) => {
                    record.execution.request.launch.session_id == *session_id
                        && record.execution.validate().is_ok()
                }
                _ => true,
            };
            valid_record
                && matches!(status, VerificationStatus::Completed(record) if record.execution.commands_passed())
                    == *commands_passed
        }
        _ => false,
    };
    if !valid {
        return Err(InspectError::StatusUnavailable);
    }
    Ok(response)
}

/// One operator-approved Run or one controller-requested child launch.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AuthorizationRequest {
    /// Persist the exact Run policy as the authenticated operator.
    Run {
        /// Exact policy the operator approves.
        envelope: Box<RunEnvelope>,
    },
    /// Persist a child grant under the exact previously returned Run digest.
    Session {
        /// Exact single-use child launch authority.
        grant: Box<GrantRequest>,
        /// Digest returned by the Run approval the controller selected.
        expected_envelope_digest: String,
    },
    /// Report one failed Bead under the approved Run's exact comment authority.
    BeadFailure {
        /// Bounded identifiers; the broker constructs the canonical comment.
        report: BeadFailureRequest,
    },
}

/// The distinct Run and Session digests a controller must retain.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AuthorizationResponse {
    /// The immutable approved Run record.
    Run {
        /// Durable Run approval.
        receipt: RunAuthorization,
    },
    /// The exact child Session launch record.
    Session {
        /// Durable child Session authorization.
        receipt: ChildAuthorization,
    },
    /// At-most-once canonical Beads comment outcome.
    BeadFailure {
        /// Durable broker mutation status.
        receipt: BeadsMutationStatus,
    },
}

/// Sends one authenticated authorization request to the installed broker.
/// The caller may run this blocking exchange on a Lua `vim.system` worker.
/// # Errors
/// Refuses a foreign broker, malformed response, mismatched digest or policy denial.
pub fn authorization(
    path: &Path,
    broker_uid: u32,
    authorization: &AuthorizationRequest,
    timeout: Duration,
) -> Result<AuthorizationResponse, InspectError> {
    let bytes = exchange(
        path,
        broker_uid,
        &Request::Authorization {
            authorization: Box::new(authorization.clone()),
        },
        timeout,
    )?;
    let response: AuthorizationResponse =
        serde_json::from_slice(&bytes).map_err(|_| InspectError::StatusUnavailable)?;
    if serde_json::to_vec(&response).map_err(|_| InspectError::StatusUnavailable)? != bytes {
        return Err(InspectError::StatusUnavailable);
    }
    let matches = match (authorization, &response) {
        (AuthorizationRequest::Run { envelope }, AuthorizationResponse::Run { receipt }) => {
            receipt.schema == "louiselm.broker.run-authorization/1"
                && receipt.run_id == envelope.run_id
                && receipt.envelope_revision == envelope.envelope_revision
                && receipt.envelope_digest
                    == crate::Digest::of(&envelope.canonical_bytes()).to_string()
        }
        (
            AuthorizationRequest::Session {
                grant,
                expected_envelope_digest,
            },
            AuthorizationResponse::Session { receipt },
        ) => {
            receipt.schema == "louiselm.broker.child-authorization/1"
                && receipt.run_id == grant.request.run_id
                && receipt.session_id == grant.request.session_id
                && receipt.authorization_id == grant.request.authorization_id
                && receipt.envelope_revision == grant.request.envelope_revision
                && receipt.request_digest == grant.request.digest().to_string()
                && receipt.envelope_digest == *expected_envelope_digest
        }
        (
            AuthorizationRequest::BeadFailure { report },
            AuthorizationResponse::BeadFailure { receipt },
        ) => report.valid() && receipt.valid() && receipt.request_id == report.request_id,
        _ => false,
    };
    if !matches {
        return Err(InspectError::StatusUnavailable);
    }
    Ok(response)
}

/// A bounded authenticated Provider budget extension exchange.
///
/// # Errors
/// Transport errors are distinct from typed broker policy refusals in the result.
pub fn provider_extension(
    path: &Path,
    broker_uid: u32,
    session_id: &str,
    request: &super::provider_extension::ExtensionRequest,
    timeout: Duration,
) -> Result<
    Result<super::provider_extension::ExtensionOutcome, super::provider_extension::ExtensionError>,
    InspectError,
> {
    validate_subject(session_id)?;
    let bytes = exchange(
        path,
        broker_uid,
        &Request::ProviderExtension {
            session_id: session_id.into(),
            request: request.clone(),
        },
        timeout,
    )?;
    let reply: Result<
        super::provider_extension::ExtensionOutcome,
        super::provider_extension::ExtensionError,
    > = serde_json::from_slice(&bytes).map_err(|_| InspectError::StatusUnavailable)?;
    if serde_json::to_vec(&reply).map_err(|_| InspectError::StatusUnavailable)? != bytes {
        return Err(InspectError::StatusUnavailable);
    }
    if let Ok(outcome) = &reply
        && (outcome.schema != super::provider_extension::OUTCOME_SCHEMA
            || outcome.session_id != session_id
            || outcome.extension.request != *request)
    {
        return Err(InspectError::StatusUnavailable);
    }
    Ok(reply)
}

/// A bounded authenticated conformance-waiver exchange.
/// # Errors
/// Transport errors are distinct from typed broker policy refusals in the result.
pub fn conformance_waiver(
    path: &Path,
    broker_uid: u32,
    session_id: &str,
    request: &super::waiver::Request,
    timeout: Duration,
) -> Result<Result<super::waiver::Outcome, super::waiver::WaiverError>, InspectError> {
    validate_subject(session_id)?;
    let bytes = exchange(
        path,
        broker_uid,
        &Request::Waiver {
            session_id: session_id.into(),
            request: request.clone(),
        },
        timeout,
    )?;
    let reply: Result<super::waiver::Outcome, super::waiver::WaiverError> =
        serde_json::from_slice(&bytes).map_err(|_| InspectError::StatusUnavailable)?;
    if serde_json::to_vec(&reply).map_err(|_| InspectError::StatusUnavailable)? != bytes {
        return Err(InspectError::StatusUnavailable);
    }
    if let Ok(outcome) = &reply {
        if outcome.schema != "louiselm.conformance-waiver-outcome/1"
            || outcome.session_id != session_id
            || outcome
                .plan
                .as_ref()
                .is_some_and(|plan| plan.session_id != session_id)
            || outcome
                .receipt
                .as_ref()
                .is_some_and(|receipt| receipt.plan.session_id != session_id)
            || outcome.active && outcome.receipt.is_none()
        {
            return Err(InspectError::StatusUnavailable);
        }
        if let Some(receipt) = &outcome.receipt {
            let bytes = serde_json::to_vec(&(&receipt.plan, receipt.approved_at_ms))
                .map_err(|_| InspectError::StatusUnavailable)?;
            if outcome.plan.as_ref() != Some(&receipt.plan)
                || receipt.digest != crate::Digest::of(&bytes).to_string()
                || receipt.approved_at_ms >= receipt.plan.proposal.expires_at_ms
            {
                return Err(InspectError::StatusUnavailable);
            }
        }
        let matches = match request {
            super::waiver::Request::Inspect => true,
            super::waiver::Request::Plan { proposal } => outcome
                .plan
                .as_ref()
                .is_some_and(|plan| plan.proposal == *proposal),
            super::waiver::Request::Apply { plan_digest } => {
                outcome.receipt.is_some()
                    && outcome
                        .plan
                        .as_ref()
                        .is_some_and(|plan| plan.digest == *plan_digest)
            }
            super::waiver::Request::Result { plan_digest } => outcome
                .plan
                .as_ref()
                .is_some_and(|plan| plan.digest == *plan_digest),
            super::waiver::Request::Revoke { receipt_digest } => {
                !outcome.active
                    && outcome
                        .receipt
                        .as_ref()
                        .is_some_and(|receipt| receipt.digest == *receipt_digest)
            }
        };
        if !matches {
            return Err(InspectError::StatusUnavailable);
        }
    }
    Ok(reply)
}

/// Inspects retained workspace evidence or changes its explicit pin. Available
/// after supervisor exit; blocking and authenticated as the installed operator.
/// # Errors
/// Refuses malformed requests, foreign peers, corrupt state or pins after deletion.
pub fn workspace_retention(
    path: &Path,
    broker_uid: u32,
    session_id: &str,
    pin: Option<bool>,
    timeout: Duration,
) -> Result<crate::workspace::retention::RetentionInspection, InspectError> {
    validate_subject(session_id)?;
    let bytes = exchange(
        path,
        broker_uid,
        &Request::WorkspaceRetention {
            session_id: session_id.into(),
            pin,
        },
        timeout,
    )?;
    let inspection: crate::workspace::retention::RetentionInspection =
        serde_json::from_slice(&bytes).map_err(|_| InspectError::StatusUnavailable)?;
    inspection
        .record
        .validate()
        .map_err(|_| InspectError::StatusUnavailable)?;
    if inspection.record.launch.session_id != session_id
        || pin.is_some_and(|pin| inspection.record.pinned != pin)
        || serde_json::to_vec(&inspection).map_err(|_| InspectError::StatusUnavailable)? != bytes
    {
        return Err(InspectError::StatusUnavailable);
    }
    Ok(inspection)
}

/// Inspects or reconciles an existing Beads operation as the authenticated operator.
/// Blocks for one bounded exchange; no decision invokes `br`, grants authority,
/// refunds budget or changes the original outcome. Evidence digests identify
/// independently retained operator evidence, not machine-verified proof of effect.
/// # Errors
/// Refuses malformed identity/evidence, foreign peers, unavailable state or conflicting decisions.
pub fn beads_mutation(
    path: &Path,
    broker_uid: u32,
    operation_id: &str,
    decision: Option<&BeadsControlDecision>,
    timeout: Duration,
) -> Result<BeadsInspection, InspectError> {
    if !super::attention::canonical_uuid(operation_id)
        || decision.as_ref().is_some_and(|value| !value.valid())
    {
        return Err(InspectError::InvalidRequest);
    }
    let bytes = exchange(
        path,
        broker_uid,
        &Request::Beads {
            operation_id: operation_id.into(),
            decision: decision.cloned(),
        },
        timeout,
    )?;
    let result: BeadsInspection =
        serde_json::from_slice(&bytes).map_err(|_| InspectError::StatusUnavailable)?;
    let confirmed = match (&decision, &result.detail) {
        (None, _)
        | (
            Some(BeadsControlDecision::Dismiss),
            BeadsInspectionDetail::Escalation {
                dismissed: true, ..
            },
        ) => true,
        (
            Some(BeadsControlDecision::Reconcile {
                outcome,
                evidence_digest,
            }),
            BeadsInspectionDetail::Mutation {
                resolution: Some(value),
                ..
            },
        ) => value.outcome == *outcome && value.evidence_digest == *evidence_digest,
        _ => false,
    };
    if result.operation_id != operation_id || !result.valid() || !confirmed {
        return Err(InspectError::StatusUnavailable);
    }
    Ok(result)
}

/// Checks the same bounded Session identifier accepted by durable broker records.
///
/// # Errors
/// Returns `InvalidRequest` without connecting or reading installed authority.
pub fn validate_subject(session_id: &str) -> Result<(), InspectError> {
    if super::is_record_identifier(session_id) {
        Ok(())
    } else {
        Err(InspectError::InvalidRequest)
    }
}

fn validate_dependency_approval(
    session_id: &str,
    approve: Option<&[String]>,
) -> Result<(), InspectError> {
    validate_subject(session_id)?;
    if approve.is_some_and(|ids| {
        ids.is_empty()
            || ids.len() > 32
            || ids.iter().collect::<std::collections::BTreeSet<_>>().len() != ids.len()
            || ids
                .iter()
                .any(|id| !crate::Digest::parse(id).is_ok_and(|digest| digest.to_string() == *id))
    }) {
        return Err(InspectError::InvalidRequest);
    }
    Ok(())
}

/// Inspects or approves a bounded batch of exact dependency candidates locally.
/// No fetch is started; unattended Runs reject approval requests.
/// # Errors
/// Refuses invalid input, unauthenticated peers, unknown subjects or malformed replies.
pub fn dependencies(
    path: &Path,
    broker_uid: u32,
    session_id: &str,
    approve: Option<Vec<String>>,
    timeout: Duration,
) -> Result<super::DependencyInspection, InspectError> {
    validate_dependency_approval(session_id, approve.as_deref())?;
    let expected = approve.clone().unwrap_or_default();
    let bytes = exchange(
        path,
        broker_uid,
        &Request::Dependencies {
            session_id: session_id.into(),
            approve,
        },
        timeout,
    )?;
    let view: super::DependencyInspection =
        serde_json::from_slice(&bytes).map_err(|_| InspectError::StatusUnavailable)?;
    if view.session_id != session_id
        || view.envelope_revision == 0
        || view.pending.len() > 32
        || view.approved != expected
        || view.pending.iter().any(|entry| {
            !entry
                .candidate
                .id()
                .is_ok_and(|id| id == entry.candidate_id)
        })
    {
        return Err(InspectError::StatusUnavailable);
    }
    Ok(view)
}

/// Performs one blocking, bounded read-only query as the current operator.
/// Authenticates the broker kernel UID and validates exact canonical output.
///
/// # Errors
/// Returns a typed identity, request, availability or broker refusal.
pub fn inspect(
    path: &Path,
    broker_uid: u32,
    session_id: &str,
    timeout: Duration,
) -> Result<SessionStatus, InspectError> {
    validate_subject(session_id)?;
    let bytes = exchange(
        path,
        broker_uid,
        &Request::Inspect {
            session_id: session_id.into(),
        },
        timeout,
    )?;
    let status =
        SessionStatus::parse_canonical(&bytes).map_err(|_| InspectError::StatusUnavailable)?;
    if status.session_id != session_id {
        return Err(InspectError::StatusUnavailable);
    }
    Ok(status)
}

/// Read exact historical conformance evidence as the authenticated operator.
/// This blocking bounded exchange runs no probes and grants no authority.
/// # Errors
/// Returns typed authentication, subject, transport or evidence refusal.
pub fn inspect_conformance(
    path: &Path,
    broker_uid: u32,
    session_id: &str,
    timeout: Duration,
) -> Result<ConformanceInspection, InspectError> {
    validate_subject(session_id)?;
    let bytes = exchange(
        path,
        broker_uid,
        &Request::Conformance {
            session_id: session_id.into(),
        },
        timeout,
    )?;
    let evidence = ConformanceInspection::parse_canonical(&bytes)
        .map_err(|_| InspectError::StatusUnavailable)?;
    if evidence.session_id != session_id {
        return Err(InspectError::StatusUnavailable);
    }
    Ok(evidence)
}

/// Inspects, rejects or cancels one durable request through the operator endpoint.
/// Retries name the same operation; no decision grants Admission or Resume.
/// # Errors
/// Returns typed authentication, request, storage or transport refusal.
pub fn skill_request(
    path: &Path,
    broker_uid: u32,
    operation_id: &str,
    outcome: Option<SkillRequestOutcome>,
    timeout: Duration,
) -> Result<SkillRequestStatus, InspectError> {
    if !super::attention::canonical_uuid(operation_id)
        || outcome.is_some_and(|value| {
            !matches!(
                value,
                SkillRequestOutcome::Rejected | SkillRequestOutcome::Cancelled
            )
        })
    {
        return Err(InspectError::InvalidRequest);
    }
    let bytes = exchange(
        path,
        broker_uid,
        &Request::Skill {
            operation_id: operation_id.into(),
            outcome,
        },
        timeout,
    )?;
    let status: SkillRequestStatus =
        serde_json::from_slice(&bytes).map_err(|_| InspectError::StatusUnavailable)?;
    if !status.valid()
        || status.operation_id != operation_id
        || outcome.is_some_and(|expected| status.outcome != expected)
    {
        return Err(InspectError::StatusUnavailable);
    }
    Ok(status)
}

fn exchange(
    path: &Path,
    broker_uid: u32,
    request: &Request,
    timeout: Duration,
) -> Result<Vec<u8>, InspectError> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or(InspectError::InvalidRequest)?;
    let mut stream = wire::connect(path).map_err(|error| {
        if error.kind() == io::ErrorKind::PermissionDenied {
            InspectError::AuthenticationRefused
        } else {
            InspectError::BrokerUnavailable
        }
    })?;
    if peer_uid(&stream)? != broker_uid {
        return Err(InspectError::AuthenticationRefused);
    }
    let ready = wire::read(&mut stream, deadline).map_err(|_| InspectError::BrokerUnavailable)?;
    if let Some(error) = InspectError::parse(&ready) {
        return Err(error);
    }
    if ready != READY {
        return Err(InspectError::BrokerUnavailable);
    }
    let bytes = serde_json::to_vec(&request).map_err(|_| InspectError::InvalidRequest)?;
    wire::write(&mut stream, &bytes, deadline).map_err(|_| InspectError::BrokerUnavailable)?;
    let max = if matches!(request, Request::Conformance { .. }) {
        MAX_INSPECTION_BYTES
    } else {
        crate::launch_protocol::MAX_PROTOCOL_MESSAGE_BYTES
    };
    let bytes = wire::read_bounded(&mut stream, deadline, max)
        .map_err(|_| InspectError::BrokerUnavailable)?;
    if let Some(error) = InspectError::parse(&bytes) {
        return Err(error);
    }
    Ok(bytes)
}

fn peer_uid(stream: &UnixStream) -> Result<u32, InspectError> {
    rustix::net::sockopt::socket_peercred(stream)
        .map(|peer| peer.uid.as_raw())
        .map_err(|_| InspectError::AuthenticationRefused)
}

/// Owns the daemon-created operator listener. The caller owns its serving thread.
/// The installed caller must hold the broker state lock, validate trusted path
/// ancestors and use the fixed endpoint. No Session is read during construction.
pub struct OperatorServer {
    listener: UnixListener,
    operator_uid: u32,
    path: PathBuf,
    inode: u64,
}

impl OperatorServer {
    /// Binds below an existing directory owned by this broker and not writable
    /// by other identities. Under the caller's singleton state lock, a refused
    /// stale socket may be removed; live listeners and foreign paths are preserved.
    ///
    /// # Errors
    /// Refuses foreign/linked paths, insecure directories or unavailable sockets.
    pub fn bind(path: &Path, operator_uid: u32) -> io::Result<Self> {
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("missing endpoint parent"))?;
        let uid = rustix::process::geteuid().as_raw();
        let metadata = fs::symlink_metadata(parent)?;
        if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o022 != 0 {
            return Err(io::Error::other("untrusted endpoint directory"));
        }
        match fs::symlink_metadata(path) {
            Ok(metadata) => {
                if !metadata.file_type().is_socket() || metadata.uid() != uid {
                    return Err(io::Error::other("untrusted endpoint path"));
                }
                match wire::connect(path) {
                    Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => {
                        fs::remove_file(path)?;
                    }
                    _ => return Err(io::Error::other("endpoint already in use")),
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let listener = UnixListener::bind(path)?;
        // Anyone may connect to receive a typed refusal; only the kernel-pinned
        // operator reaches a request read or Session lookup.
        fs::set_permissions(path, fs::Permissions::from_mode(0o666))?;
        let inode = fs::symlink_metadata(path)?.ino();
        Ok(Self {
            listener,
            operator_uid,
            path: path.to_owned(),
            inode,
        })
    }

    /// Serves one peer synchronously, authenticating before reading its request.
    /// Client failures do not terminate the listener. The handler must finish
    /// within the supplied absolute deadline and serialize with the Session owner.
    ///
    /// # Errors
    /// Returns listener failure; refused or disconnected clients are isolated.
    #[expect(
        clippy::too_many_arguments,
        reason = "Each existing operator operation has a distinct typed handler; keep explicit authentication routing without a mutable handler registry."
    )]
    pub fn serve_once(
        &self,
        verification: impl FnOnce(
            &VerificationControlRequest,
            Instant,
        ) -> Result<VerificationControlResponse, InspectError>,
        authorization: impl FnOnce(&AuthorizationRequest) -> Result<AuthorizationResponse, InspectError>,
        dependencies: impl FnOnce(
            &str,
            Option<&[String]>,
        ) -> Result<super::DependencyInspection, InspectError>,
        lookup: impl FnOnce(&str, Instant) -> Result<SessionStatus, InspectError>,
        conformance: impl FnOnce(&str) -> Result<ConformanceInspection, InspectError>,
        skill: impl FnOnce(
            &str,
            Option<SkillRequestOutcome>,
        ) -> Result<SkillRequestStatus, InspectError>,
        beads: impl FnOnce(&str, Option<&BeadsControlDecision>) -> Result<BeadsInspection, InspectError>,
        retention: impl FnOnce(
            &str,
            Option<bool>,
        )
            -> Result<crate::workspace::retention::RetentionInspection, InspectError>,
        waiver: impl FnOnce(
            &str,
            &super::waiver::Request,
            Instant,
        ) -> Result<super::waiver::Outcome, super::waiver::WaiverError>,
        extension: impl FnOnce(
            &str,
            &super::provider_extension::ExtensionRequest,
            Instant,
        ) -> Result<
            super::provider_extension::ExtensionOutcome,
            super::provider_extension::ExtensionError,
        >,
    ) -> io::Result<()> {
        let (mut stream, _) = self.listener.accept()?;
        let mut deadline = Instant::now() + TIMEOUT;
        let result = (|| {
            if peer_uid(&stream)? != self.operator_uid {
                return Err(InspectError::AuthenticationRefused);
            }
            wire::write(&mut stream, READY, deadline).map_err(|_| InspectError::InvalidRequest)?;
            let bytes =
                wire::read(&mut stream, deadline).map_err(|_| InspectError::InvalidRequest)?;
            let request: Request =
                serde_json::from_slice(&bytes).map_err(|_| InspectError::InvalidRequest)?;
            if matches!(request, Request::Verification { .. }) {
                deadline = Instant::now() + VERIFICATION_TIMEOUT;
            }
            if Instant::now() >= deadline {
                return Err(InspectError::StatusUnavailable);
            }
            match request {
                Request::Verification { request } => {
                    serde_json::to_vec(&verification(&request, deadline)?)
                        .map_err(|_| InspectError::StatusUnavailable)
                }
                Request::Authorization {
                    authorization: request,
                } => serde_json::to_vec(&authorization(&request)?)
                    .map_err(|_| InspectError::StatusUnavailable),
                Request::Waiver {
                    session_id,
                    request,
                } => {
                    validate_subject(&session_id)?;
                    serde_json::to_vec(&waiver(&session_id, &request, deadline))
                        .map_err(|_| InspectError::StatusUnavailable)
                }
                Request::ProviderExtension {
                    session_id,
                    request,
                } => {
                    validate_subject(&session_id)?;
                    serde_json::to_vec(&extension(&session_id, &request, deadline))
                        .map_err(|_| InspectError::StatusUnavailable)
                }
                Request::Dependencies {
                    session_id,
                    approve,
                } => {
                    validate_dependency_approval(&session_id, approve.as_deref())?;
                    serde_json::to_vec(&dependencies(&session_id, approve.as_deref())?)
                        .map_err(|_| InspectError::StatusUnavailable)
                }
                Request::WorkspaceRetention { session_id, pin } => {
                    validate_subject(&session_id)?;
                    serde_json::to_vec(&retention(&session_id, pin)?)
                        .map_err(|_| InspectError::StatusUnavailable)
                }
                Request::Beads {
                    operation_id,
                    decision,
                } => {
                    if !super::attention::canonical_uuid(&operation_id)
                        || decision.as_ref().is_some_and(|value| !value.valid())
                    {
                        return Err(InspectError::InvalidRequest);
                    }
                    serde_json::to_vec(&beads(&operation_id, decision.as_ref())?)
                        .map_err(|_| InspectError::StatusUnavailable)
                }
                Request::Inspect { session_id } => {
                    validate_subject(&session_id)?;
                    Ok(lookup(&session_id, deadline)?.canonical_bytes())
                }
                Request::Conformance { session_id } => {
                    validate_subject(&session_id)?;
                    conformance(&session_id)?
                        .canonical_bytes()
                        .map_err(|_| InspectError::StatusUnavailable)
                }
                Request::Skill {
                    operation_id,
                    outcome,
                } => {
                    if !super::attention::canonical_uuid(&operation_id)
                        || outcome.is_some_and(|value| {
                            !matches!(
                                value,
                                SkillRequestOutcome::Rejected | SkillRequestOutcome::Cancelled
                            )
                        })
                    {
                        return Err(InspectError::InvalidRequest);
                    }
                    serde_json::to_vec(&skill(&operation_id, outcome)?)
                        .map_err(|_| InspectError::StatusUnavailable)
                }
            }
        })();
        let bytes = result.unwrap_or_else(InspectError::canonical_bytes);
        // A vanished reader never rolls back a durable decision. Retrying names
        // the same operation and cannot reopen a terminal request.
        let _ = wire::write(&mut stream, &bytes, deadline);
        Ok(())
    }
}

impl Drop for OperatorServer {
    fn drop(&mut self) {
        if let Ok(metadata) = fs::symlink_metadata(&self.path)
            && metadata.file_type().is_socket()
            && metadata.ino() == self.inode
        {
            // Best-effort name cleanup only. Startup validates stale sockets;
            // failure cannot authorize a peer or permit concurrent state ownership.
            let _ = fs::remove_file(&self.path);
        }
    }
}
