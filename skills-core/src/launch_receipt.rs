//! Canonical, signed receipts for privileged Session lifecycle outcomes.
//!
//! The payload is the statement the Launch supervisor signs. Chain links hash
//! the complete signed envelope, so neither a signature nor its payload can be
//! replaced without changing every later predecessor. This module owns bytes
//! and pure verification only; signing, persistence, policy, and transport are
//! deliberately left to narrow caller-provided boundaries.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    canonical::Digest, conformance::admission::Condition, isolation::CONTRACT_VERSION,
    launch::MAX_BROKER_LOSS_GRACE_MS,
};

/// SSHSIG namespace and schema for the bytes a Launch supervisor signs.
pub const RECEIPT_SCHEMA: &str = "louiselm.launch.receipt/5";

/// Schema for the payload plus its launcher signature.
pub const SIGNED_RECEIPT_SCHEMA: &str = "louiselm.launch.signed-receipt/5";

/// Largest canonical payload or signed envelope accepted at the trust boundary.
pub const MAX_RECEIPT_BYTES: usize = 64 * 1024;

const MAX_IDENTIFIER_BYTES: usize = 128;
const MAX_SIGNATURE_BYTES: usize = 16 * 1024;

/// Public lifecycle state reported by the launcher contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    /// The process tree is prepared and authorized, but its workload is blocked.
    Starting,
    /// The Session process tree may execute.
    Running,
    /// The whole Session process tree is frozen.
    Parked,
    /// The process tree has ended and cannot resume.
    Terminal,
}

/// Bounded classification of a confirmed supervised process exit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessExitClassification {
    /// The supervised process reported successful completion.
    Success,
    /// The supervised process reported a non-zero completion status.
    Failure,
    /// The supervised process ended because of a signal.
    Signaled,
}

/// Durable broker authorization bound into a privileged result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Authorization {
    /// Broker-generated authorization identity.
    pub authorization_id: String,
    /// Idempotency identity of the authorized request.
    pub request_id: String,
    /// Digest of that request's exact canonical bytes.
    pub request_digest: String,
}

/// Closed mechanical causes that can require an outcome without a new authorization.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptCause {
    /// The durable sequence-zero launch receipt was acknowledged.
    LaunchAcknowledged,
    /// The authenticated Control broker disconnected.
    BrokerLost,
    /// The owning controller disappeared.
    ControllerLost,
    /// A success receipt was not durably acknowledged in time.
    AcknowledgementFailed,
    /// The supervisor's ACP relay failed; no raw I/O detail is retained.
    RelayFailed,
    /// The authenticated Agent lifetime or executable proof ended without an exit status.
    AgentIdentityLost,
}

/// Why the supervisor performed one non-launch lifecycle action.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReceiptAuthority {
    /// A Control broker authorization.
    Authorized(Authorization),
    /// A closed supervisor-observed cause.
    Cause {
        /// Cause that required the action.
        cause: ReceiptCause,
    },
    /// The supervised process ended independently of a lifecycle request.
    ProcessExited {
        /// Sanitized outcome; raw status and signal values never enter receipts.
        classification: ProcessExitClassification,
    },
}

/// Host conformance evidence this launch was actually admitted under.
///
/// Only admitting decisions reach a receipt: a refusal produces no launch.
/// A waived launch records the waiver and the evidence it rode past, so an
/// auditor can tell a certified Session from a waived one without inferring it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ConformanceEvidence {
    /// Current passing host evidence admitted this launch.
    Certified {
        /// Digest of the exact observation report admission relied on.
        report_digest: String,
    },
    /// An operator waived one condition; isolation stays unverified.
    Waived {
        /// The exact condition approved for this Session.
        condition: Condition,
        /// Digest of the evidence the waiver rode past, where any exists.
        report_digest: Option<String>,
    },
    /// Admission consulted no host evidence, so this launch claims nothing.
    ///
    /// The pre-cutover state: the gate of `louiselm-d6fv.9` is not in force, and
    /// a receipt that said anything else would assert a property never checked.
    /// Presentation must report the conformance dimension unverified for these
    /// Sessions, which is what makes the state visible rather than comfortable.
    Unevaluated,
}

/// Exact launch inputs and measured evidence established by sequence zero.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchEvidence {
    /// Host conformance evidence admission relied on. Nested under its own
    /// name: `deny_unknown_fields` and `flatten` cannot be combined, because
    /// flattening deserializes through a map the deny rejects.
    pub conformance: ConformanceEvidence,
    /// Digest of the closed [`crate::launch::LaunchRequest`].
    pub launch_request_digest: String,
    /// Digest of the runtime measurement used for this launch.
    pub runtime_measurement_digest: String,
    /// Skill Generation fixed for the Session.
    pub skill_generation_id: String,
    /// Complete Session-input manifest fixed for the Session.
    pub session_input_manifest_id: String,
    /// Isolation contract the evidence answers.
    pub isolation_contract: String,
    /// Measured isolation backend identity.
    pub isolation_backend_id: String,
    /// Kernel identity against which prerequisites were checked.
    pub kernel_identity: String,
    /// Digest of the complete isolation evidence.
    pub isolation_evidence_digest: String,
    /// Signed interval allowed for authenticated broker reattachment.
    pub broker_loss_grace_ms: u32,
    /// Capability channel identities, in ascending byte order.
    pub capability_channel_ids: Vec<String>,
}

/// Supervisor-established authority after restricted initialization.
///
/// The broker uses these signed identifiers for attribution only. Kernel handles
/// and per-packet identity/lifetime checks remain owned by the supervisor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartEvidence {
    /// Actual Agent runtime PID in the supervisor's host namespace, not its reaper.
    pub agent_pid: u32,
    /// Kernel-confirmed installed Session UID.
    pub assigned_uid: u32,
    /// Kernel-confirmed installed Session GID.
    pub assigned_gid: u32,
    /// Digest of the exact integration evidence verified before channel enablement.
    pub tool_isolation_digest: String,
}

/// Privileged lifecycle result recorded by one receipt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReceiptOutcome {
    /// The initial authorized launch and all evidence it established.
    Launch {
        /// Broker authorization for the launch.
        authorization: Authorization,
        /// Measurements and fixed inputs used to launch.
        evidence: Box<LaunchEvidence>,
    },
    /// The prepared process tree was released after launch acknowledgement.
    Start {
        /// Closed supervisor-observed launch acknowledgement.
        authority: ReceiptAuthority,
        /// Actual Agent identity and enforced tool-isolation proof.
        evidence: StartEvidence,
    },
    /// The process tree was frozen.
    Park {
        /// Authorization or supervisor-observed cause.
        authority: ReceiptAuthority,
    },
    /// A parked process tree was allowed to execute again.
    Resume {
        /// Durable authorization required for a widening action.
        authorization: Authorization,
    },
    /// The Agent was interrupted without ending the Session.
    Interrupt {
        /// Durable authorization for the interrupt.
        authorization: Authorization,
    },
    /// The process tree and its owned resources were disposed.
    Disposal {
        /// Authorization or supervisor-observed cause.
        authority: ReceiptAuthority,
    },
}

impl ReceiptOutcome {
    fn authorization(&self) -> Option<&Authorization> {
        match self {
            Self::Launch { authorization, .. }
            | Self::Park {
                authority: ReceiptAuthority::Authorized(authorization),
            }
            | Self::Disposal {
                authority: ReceiptAuthority::Authorized(authorization),
            }
            | Self::Resume { authorization }
            | Self::Interrupt { authorization } => Some(authorization),
            _ => None,
        }
    }

    fn result_matches(&self, state: SessionState) -> bool {
        match self {
            Self::Launch { .. } => state == SessionState::Starting,
            Self::Start { .. } | Self::Resume { .. } => state == SessionState::Running,
            Self::Interrupt { .. } => matches!(state, SessionState::Running | SessionState::Parked),
            Self::Park { .. } => state == SessionState::Parked,
            Self::Disposal { .. } => state == SessionState::Terminal,
        }
    }
}

/// Exact statement signed for one privileged lifecycle outcome.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptPayload {
    /// Payload schema identifier.
    pub schema: String,
    /// Session whose process tree changed.
    pub session_id: String,
    /// Run that owns the Session.
    pub run_id: String,
    /// Request or internally generated event identity.
    pub request_id: String,
    /// Capability envelope revision in force for the action.
    pub envelope_revision: u64,
    /// Zero-based position in this Session's receipt chain.
    pub sequence: u64,
    /// Digest of the preceding complete signed envelope.
    pub previous_receipt_digest: Option<String>,
    /// Digest of the trusted launcher release executing the action.
    pub release_id: String,
    /// Stable launcher signing-key identity.
    pub signing_key_id: String,
    /// Action and its authorization or cause.
    pub outcome: ReceiptOutcome,
    /// State after the privileged action completed.
    pub resulting_state: SessionState,
}

impl ReceiptPayload {
    /// Returns the exact bytes the launcher signs.
    ///
    /// # Panics
    /// Panics only if serialization fails after a future schema change introduces
    /// a fallible serializer. The current derived schema has only JSON-native values.
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "Derived schema has only JSON-native values and string-keyed maps, with no custom serializers."
    )]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("a receipt payload is always serializable")
    }

    /// Parses and validates exact canonical payload bytes.
    ///
    /// # Errors
    /// Rejects oversized, malformed, unsupported-schema/version, or noncanonical bytes and any failure from [`Self::validate`].
    pub fn parse_canonical(bytes: &[u8]) -> Result<Self, ReceiptError> {
        check_size(bytes)?;
        preflight_schema(bytes, RECEIPT_SCHEMA)?;
        let payload: Self = serde_json::from_slice(bytes)
            .map_err(|error| ReceiptError::Malformed(error.to_string()))?;
        if payload.canonical_bytes() != bytes {
            return Err(ReceiptError::NonCanonical);
        }
        payload.validate()?;
        Ok(payload)
    }

    /// Validates all invariants that do not require the preceding receipt.
    ///
    /// # Errors
    /// Rejects size/schema/identifier/digest violations, invalid genesis/predecessor shape, inconsistent authorization/evidence, or an outcome incompatible with the resulting state.
    pub fn validate(&self) -> Result<(), ReceiptError> {
        check_size(&self.canonical_bytes())?;
        if self.schema != RECEIPT_SCHEMA {
            return Err(ReceiptError::UnsupportedSchema(self.schema.clone()));
        }
        validate_identifier("session_id", &self.session_id)?;
        validate_identifier("run_id", &self.run_id)?;
        validate_identifier("request_id", &self.request_id)?;
        validate_digest("release_id", &self.release_id)?;
        validate_digest("signing_key_id", &self.signing_key_id)?;

        match (self.sequence, &self.previous_receipt_digest) {
            (0, None) => {}
            (0, Some(_)) => return Err(ReceiptError::GenesisHasPredecessor),
            (_, None) => return Err(ReceiptError::MissingPredecessor),
            (_, Some(predecessor)) => {
                validate_digest("previous_receipt_digest", predecessor)?;
            }
        }

        match &self.outcome {
            ReceiptOutcome::Launch {
                authorization,
                evidence,
            } => {
                if self.sequence != 0 {
                    return Err(ReceiptError::LaunchNotGenesis);
                }
                validate_authorization(authorization, &self.request_id)?;
                validate_launch_evidence(evidence)?;
                if authorization.request_digest != evidence.launch_request_digest {
                    return Err(ReceiptError::LaunchRequestMismatch);
                }
            }
            ReceiptOutcome::Start {
                authority,
                evidence,
            } => {
                if !matches!(
                    authority,
                    ReceiptAuthority::Cause {
                        cause: ReceiptCause::LaunchAcknowledged
                    }
                ) {
                    return Err(ReceiptError::ContradictoryCause);
                }
                if evidence.agent_pid == 0
                    || evidence.assigned_uid == 0
                    || evidence.assigned_gid == 0
                {
                    return Err(ReceiptError::Malformed(
                        "invalid Agent authority identity".to_owned(),
                    ));
                }
                validate_digest("tool_isolation_digest", &evidence.tool_isolation_digest)?;
                if self.sequence == 0 {
                    return Err(ReceiptError::GenesisNotLaunch);
                }
            }
            ReceiptOutcome::Park { authority }
                if !matches!(
                    authority,
                    ReceiptAuthority::Authorized(_)
                        | ReceiptAuthority::Cause {
                            cause: ReceiptCause::BrokerLost
                                | ReceiptCause::ControllerLost
                                | ReceiptCause::AcknowledgementFailed
                        }
                ) =>
            {
                return Err(ReceiptError::ContradictoryCause);
            }
            ReceiptOutcome::Disposal { authority }
                if !matches!(
                    authority,
                    ReceiptAuthority::Authorized(_)
                        | ReceiptAuthority::Cause {
                            cause: ReceiptCause::ControllerLost
                                | ReceiptCause::AcknowledgementFailed
                                | ReceiptCause::RelayFailed
                                | ReceiptCause::AgentIdentityLost
                        }
                        | ReceiptAuthority::ProcessExited { .. }
                ) =>
            {
                return Err(ReceiptError::ContradictoryCause);
            }
            _ if self.sequence == 0 => return Err(ReceiptError::GenesisNotLaunch),
            _ => {
                if let Some(authorization) = self.outcome.authorization() {
                    validate_authorization(authorization, &self.request_id)?;
                }
            }
        }

        if !self.outcome.result_matches(self.resulting_state) {
            return Err(ReceiptError::ContradictoryResult);
        }
        Ok(())
    }
}

/// Signed receipt stored by the broker and linked by every successor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedReceipt {
    /// Envelope schema identifier.
    pub schema: String,
    /// Canonical signed statement.
    pub payload: ReceiptPayload,
    /// Armored SSHSIG signature over [`ReceiptPayload::canonical_bytes`].
    pub signature: String,
}

impl SignedReceipt {
    /// Returns the exact envelope bytes stored and hashed by the next receipt.
    ///
    /// # Panics
    /// Panics only if serialization fails after a future schema change introduces
    /// a fallible serializer. The current derived schema has only JSON-native values.
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "Derived schema has only JSON-native values and string-keyed maps, with no custom serializers."
    )]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("a signed receipt is always serializable")
    }

    /// Parses and validates exact canonical signed-envelope bytes.
    ///
    /// # Errors
    /// Rejects oversized, malformed, unsupported-schema/version, or noncanonical bytes and any failure from [`Self::validate`].
    pub fn parse_canonical(bytes: &[u8]) -> Result<Self, ReceiptError> {
        check_size(bytes)?;
        preflight_schema(bytes, SIGNED_RECEIPT_SCHEMA)?;
        let receipt: Self = serde_json::from_slice(bytes)
            .map_err(|error| ReceiptError::Malformed(error.to_string()))?;
        if receipt.canonical_bytes() != bytes {
            return Err(ReceiptError::NonCanonical);
        }
        receipt.validate()?;
        Ok(receipt)
    }

    /// Returns the chain identity of the complete signed envelope.
    #[must_use]
    pub fn digest(&self) -> Digest {
        Digest::of(&self.canonical_bytes())
    }

    /// Validates envelope and payload shape without asserting cryptography.
    ///
    /// # Errors
    /// Rejects envelope size/schema, absent/oversized signatures, or any payload-shape violation.
    pub fn validate(&self) -> Result<(), ReceiptError> {
        check_size(&self.canonical_bytes())?;
        if self.schema != SIGNED_RECEIPT_SCHEMA {
            return Err(ReceiptError::UnsupportedSchema(self.schema.clone()));
        }
        if self.signature.is_empty() || self.signature.len() > MAX_SIGNATURE_BYTES {
            return Err(ReceiptError::InvalidSignatureEncoding);
        }
        self.payload.validate()
    }
}

/// Trusted identity a complete sequence-zero chain must match.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChainAnchor {
    /// Expected Session identity.
    pub session_id: String,
    /// Expected Run identity.
    pub run_id: String,
    /// Trusted release digest pinned for the chain.
    pub release_id: String,
    /// Launcher signing key pinned for the chain.
    pub signing_key_id: String,
}

/// Canonical wire summary of a signed receipt chain's current head.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptHead {
    /// Last verified sequence number.
    pub sequence: u64,
    /// Digest of the last complete signed receipt.
    pub digest: String,
}

/// Trusted state needed to verify a launcher-ahead suffix.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedReceiptHead {
    /// Session identity pinned by the verified prefix.
    session_id: String,
    /// Run identity pinned by the verified prefix.
    run_id: String,
    /// Release identity pinned by the verified prefix.
    release_id: String,
    /// Signing-key identity pinned by the verified prefix.
    signing_key_id: String,
    /// Last verified sequence number.
    sequence: u64,
    /// Digest of the last complete signed receipt.
    receipt_digest: String,
    /// State after the last receipt.
    state: SessionState,
    /// Envelope revision at the last receipt.
    envelope_revision: u64,
    request_ids: BTreeSet<String>,
}

impl VerifiedReceiptHead {
    /// Last verified sequence number.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Digest of the last complete signed receipt.
    #[must_use]
    pub fn receipt_digest(&self) -> &str {
        &self.receipt_digest
    }

    /// State established by the last verified receipt.
    #[must_use]
    pub const fn state(&self) -> SessionState {
        self.state
    }

    /// Envelope revision established by the last verified receipt.
    #[must_use]
    pub const fn envelope_revision(&self) -> u64 {
        self.envelope_revision
    }

    /// Returns the bounded wire summary of this verified head.
    #[must_use]
    pub fn wire_head(&self) -> ReceiptHead {
        ReceiptHead {
            sequence: self.sequence,
            digest: self.receipt_digest.clone(),
        }
    }
}

/// Verifies a complete chain beginning at its trusted sequence-zero anchor.
///
/// # Errors
/// Rejects an invalid anchor, empty or discontinuous chain, failed signatures, changed pinned identities, repeated request IDs, revision regression, or invalid lifecycle transitions.
pub fn verify_chain<F>(
    receipts: &[SignedReceipt],
    anchor: &ChainAnchor,
    mut verify_signature: F,
) -> Result<VerifiedReceiptHead, ReceiptError>
where
    F: FnMut(&str, &[u8], &str) -> bool,
{
    validate_anchor(anchor)?;
    let first = receipts.first().ok_or(ReceiptError::EmptyChain)?;
    if first.payload.sequence != 0 {
        return Err(ReceiptError::ExpectedSequence {
            expected: 0,
            found: first.payload.sequence,
        });
    }
    check_pinned(&first.payload, anchor)?;
    first.validate()?;
    verify_one(first, &mut verify_signature)?;

    let mut head = head_of(first);
    verify_after(&receipts[1..], &mut head, &mut verify_signature)?;
    Ok(head)
}

/// Verifies a continuous launcher-ahead suffix against a trusted prior head.
///
/// An empty suffix is an equal-prefix reconciliation and returns the unchanged
/// head. A caller must not construct `trusted_head` from unverified input.
///
/// # Errors
/// Rejects an invalid trusted head, discontinuous sequence/predecessor, failed signatures, changed pinned identities, repeated request IDs, revision regression, or invalid lifecycle transitions.
pub fn verify_suffix<F>(
    receipts: &[SignedReceipt],
    trusted_head: &VerifiedReceiptHead,
    mut verify_signature: F,
) -> Result<VerifiedReceiptHead, ReceiptError>
where
    F: FnMut(&str, &[u8], &str) -> bool,
{
    validate_head(trusted_head)?;
    let mut head = trusted_head.clone();
    verify_after(receipts, &mut head, &mut verify_signature)?;
    Ok(head)
}

fn verify_after<F>(
    receipts: &[SignedReceipt],
    head: &mut VerifiedReceiptHead,
    verify_signature: &mut F,
) -> Result<(), ReceiptError>
where
    F: FnMut(&str, &[u8], &str) -> bool,
{
    for receipt in receipts {
        receipt.validate()?;
        let sequence = head
            .sequence
            .checked_add(1)
            .ok_or(ReceiptError::SequenceOverflow)?;
        if receipt.payload.sequence != sequence {
            return Err(ReceiptError::ExpectedSequence {
                expected: sequence,
                found: receipt.payload.sequence,
            });
        }
        if receipt.payload.previous_receipt_digest.as_deref() != Some(head.receipt_digest.as_str())
        {
            return Err(ReceiptError::WrongPredecessor { sequence });
        }
        check_head_pinned(&receipt.payload, head)?;
        if receipt.payload.envelope_revision < head.envelope_revision {
            return Err(ReceiptError::EnvelopeRevisionRegressed {
                previous: head.envelope_revision,
                found: receipt.payload.envelope_revision,
                sequence,
            });
        }
        if !valid_transition(
            head.state,
            &receipt.payload.outcome,
            receipt.payload.resulting_state,
        ) {
            return Err(ReceiptError::InvalidTransition { sequence });
        }
        if !head.request_ids.insert(receipt.payload.request_id.clone()) {
            return Err(ReceiptError::DuplicateRequestId {
                request_id: receipt.payload.request_id.clone(),
            });
        }
        verify_one(receipt, verify_signature)?;
        head.sequence = receipt.payload.sequence;
        head.receipt_digest = receipt.digest().to_string();
        head.state = receipt.payload.resulting_state;
        head.envelope_revision = receipt.payload.envelope_revision;
    }
    Ok(())
}

fn verify_one<F>(receipt: &SignedReceipt, verify_signature: &mut F) -> Result<(), ReceiptError>
where
    F: FnMut(&str, &[u8], &str) -> bool,
{
    if !verify_signature(
        &receipt.payload.signing_key_id,
        &receipt.payload.canonical_bytes(),
        &receipt.signature,
    ) {
        return Err(ReceiptError::InvalidSignature {
            sequence: receipt.payload.sequence,
        });
    }
    Ok(())
}

fn valid_transition(
    state: SessionState,
    outcome: &ReceiptOutcome,
    resulting_state: SessionState,
) -> bool {
    matches!(
        (state, outcome, resulting_state),
        (
            SessionState::Starting,
            ReceiptOutcome::Start { .. },
            SessionState::Running
        ) | (
            SessionState::Running,
            ReceiptOutcome::Park { .. },
            SessionState::Parked
        ) | (
            SessionState::Parked,
            ReceiptOutcome::Resume { .. },
            SessionState::Running
        ) | (
            SessionState::Running,
            ReceiptOutcome::Interrupt { .. },
            SessionState::Running
        ) | (
            SessionState::Parked,
            ReceiptOutcome::Interrupt { .. },
            SessionState::Parked
        ) | (
            SessionState::Running | SessionState::Parked,
            ReceiptOutcome::Disposal { .. },
            SessionState::Terminal
        )
    )
}

fn head_of(receipt: &SignedReceipt) -> VerifiedReceiptHead {
    let mut request_ids = BTreeSet::new();
    request_ids.insert(receipt.payload.request_id.clone());
    VerifiedReceiptHead {
        session_id: receipt.payload.session_id.clone(),
        run_id: receipt.payload.run_id.clone(),
        release_id: receipt.payload.release_id.clone(),
        signing_key_id: receipt.payload.signing_key_id.clone(),
        sequence: receipt.payload.sequence,
        receipt_digest: receipt.digest().to_string(),
        state: receipt.payload.resulting_state,
        envelope_revision: receipt.payload.envelope_revision,
        request_ids,
    }
}

fn check_pinned(payload: &ReceiptPayload, anchor: &ChainAnchor) -> Result<(), ReceiptError> {
    if payload.session_id != anchor.session_id {
        return Err(ReceiptError::ForeignSession);
    }
    if payload.run_id != anchor.run_id {
        return Err(ReceiptError::ForeignRun);
    }
    if payload.release_id != anchor.release_id {
        return Err(ReceiptError::ReleaseChanged);
    }
    if payload.signing_key_id != anchor.signing_key_id {
        return Err(ReceiptError::SigningKeyChanged);
    }
    Ok(())
}

fn check_head_pinned(
    payload: &ReceiptPayload,
    head: &VerifiedReceiptHead,
) -> Result<(), ReceiptError> {
    check_pinned(
        payload,
        &ChainAnchor {
            session_id: head.session_id.clone(),
            run_id: head.run_id.clone(),
            release_id: head.release_id.clone(),
            signing_key_id: head.signing_key_id.clone(),
        },
    )
}

fn validate_anchor(anchor: &ChainAnchor) -> Result<(), ReceiptError> {
    validate_identifier("session_id", &anchor.session_id)?;
    validate_identifier("run_id", &anchor.run_id)?;
    validate_digest("release_id", &anchor.release_id)?;
    validate_digest("signing_key_id", &anchor.signing_key_id)
}

fn validate_head(head: &VerifiedReceiptHead) -> Result<(), ReceiptError> {
    validate_anchor(&ChainAnchor {
        session_id: head.session_id.clone(),
        run_id: head.run_id.clone(),
        release_id: head.release_id.clone(),
        signing_key_id: head.signing_key_id.clone(),
    })?;
    validate_digest("receipt_digest", &head.receipt_digest)
}

fn validate_authorization(
    authorization: &Authorization,
    receipt_request_id: &str,
) -> Result<(), ReceiptError> {
    validate_identifier("authorization_id", &authorization.authorization_id)?;
    validate_identifier("authorization.request_id", &authorization.request_id)?;
    validate_digest(
        "authorization.request_digest",
        &authorization.request_digest,
    )?;
    if authorization.request_id != receipt_request_id {
        return Err(ReceiptError::AuthorizationRequestMismatch);
    }
    Ok(())
}

fn validate_launch_evidence(evidence: &LaunchEvidence) -> Result<(), ReceiptError> {
    match &evidence.conformance {
        ConformanceEvidence::Certified { report_digest } => {
            validate_digest("conformance_report_digest", report_digest)?;
        }
        ConformanceEvidence::Waived {
            report_digest: Some(report_digest),
            ..
        } => validate_digest("conformance_report_digest", report_digest)?,
        // A waiver over absent evidence and an unevaluated launch both carry no
        // digest: there is nothing to bind, and inventing one would be a claim.
        ConformanceEvidence::Waived {
            report_digest: None,
            ..
        }
        | ConformanceEvidence::Unevaluated => (),
    }
    for (field, value) in [
        ("launch_request_digest", &evidence.launch_request_digest),
        (
            "runtime_measurement_digest",
            &evidence.runtime_measurement_digest,
        ),
        ("skill_generation_id", &evidence.skill_generation_id),
        (
            "session_input_manifest_id",
            &evidence.session_input_manifest_id,
        ),
        (
            "isolation_evidence_digest",
            &evidence.isolation_evidence_digest,
        ),
    ] {
        validate_digest(field, value)?;
    }
    if evidence.isolation_contract != CONTRACT_VERSION {
        return Err(ReceiptError::UnsupportedIsolationContract(
            evidence.isolation_contract.clone(),
        ));
    }
    if evidence.broker_loss_grace_ms > MAX_BROKER_LOSS_GRACE_MS {
        return Err(ReceiptError::InvalidBrokerLossGrace);
    }
    validate_token("isolation_backend_id", &evidence.isolation_backend_id)?;
    validate_token("kernel_identity", &evidence.kernel_identity)?;
    if evidence.capability_channel_ids.is_empty() {
        return Err(ReceiptError::MissingChannel);
    }
    let mut previous: Option<&str> = None;
    for id in &evidence.capability_channel_ids {
        validate_identifier("capability_channel_id", id)?;
        if let Some(earlier) = previous {
            if earlier == id {
                return Err(ReceiptError::DuplicateChannel(id.clone()));
            }
            if earlier > id.as_str() {
                return Err(ReceiptError::UnsortedChannels);
            }
        }
        previous = Some(id);
    }
    Ok(())
}

fn check_size(bytes: &[u8]) -> Result<(), ReceiptError> {
    if bytes.len() > MAX_RECEIPT_BYTES {
        Err(ReceiptError::Oversized)
    } else {
        Ok(())
    }
}

#[derive(Deserialize)]
struct ReceiptSchemaHeader {
    schema: String,
}

fn preflight_schema(bytes: &[u8], expected: &str) -> Result<(), ReceiptError> {
    let header: ReceiptSchemaHeader = serde_json::from_slice(bytes)
        .map_err(|error| ReceiptError::Malformed(error.to_string()))?;
    if header.schema == expected {
        Ok(())
    } else {
        Err(ReceiptError::UnsupportedSchema(header.schema))
    }
}

fn validate_identifier(field: &'static str, value: &str) -> Result<(), ReceiptError> {
    if value.is_empty()
        || value.len() > MAX_IDENTIFIER_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(ReceiptError::InvalidIdentifier { field });
    }
    Ok(())
}

fn validate_token(field: &'static str, value: &str) -> Result<(), ReceiptError> {
    if value.is_empty()
        || value.len() > MAX_IDENTIFIER_BYTES
        || !value.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err(ReceiptError::InvalidIdentifier { field });
    }
    Ok(())
}

fn validate_digest(field: &'static str, value: &str) -> Result<(), ReceiptError> {
    let digest = Digest::parse(value).map_err(|_| ReceiptError::InvalidDigest { field })?;
    if digest.to_string() != value {
        return Err(ReceiptError::InvalidDigest { field });
    }
    Ok(())
}

/// Receipt construction or verification failure.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ReceiptError {
    /// Input exceeded [`MAX_RECEIPT_BYTES`].
    #[error("receipt exceeds the size limit")]
    Oversized,
    /// Input was not valid closed-schema JSON.
    #[error("receipt is malformed: {0}")]
    Malformed(String),
    /// Parsed JSON did not reserialize to the exact input bytes.
    #[error("receipt bytes are not canonical")]
    NonCanonical,
    /// Payload or envelope named an unknown schema.
    #[error("unsupported receipt schema '{0}'")]
    UnsupportedSchema(String),
    /// A bounded opaque identifier was empty, unsafe, or too large.
    #[error("invalid receipt identifier '{field}'")]
    InvalidIdentifier {
        /// Field rejected.
        field: &'static str,
    },
    /// A digest was not exact canonical `sha256:<hex>` text.
    #[error("invalid receipt digest '{field}'")]
    InvalidDigest {
        /// Field rejected.
        field: &'static str,
    },
    /// Sequence zero named a predecessor.
    #[error("sequence zero cannot name a predecessor")]
    GenesisHasPredecessor,
    /// A later receipt omitted its predecessor.
    #[error("a non-genesis receipt requires a predecessor")]
    MissingPredecessor,
    /// A launch outcome appeared after sequence zero.
    #[error("launch must be sequence zero")]
    LaunchNotGenesis,
    /// Sequence zero did not record a launch.
    #[error("sequence zero must record launch")]
    GenesisNotLaunch,
    /// The action and its declared resulting state disagree.
    #[error("receipt action contradicts its resulting state")]
    ContradictoryResult,
    /// The declared mechanical cause cannot produce the named action.
    #[error("receipt cause contradicts its action")]
    ContradictoryCause,
    /// Authorization does not describe the receipt's request.
    #[error("authorization request does not match receipt request")]
    AuthorizationRequestMismatch,
    /// Sequence zero's authorization names different bytes from its launch evidence.
    #[error("launch authorization does not match launch request evidence")]
    LaunchRequestMismatch,
    /// Launch evidence answers an unsupported isolation contract.
    #[error("unsupported isolation contract '{0}'")]
    UnsupportedIsolationContract(String),
    /// Launch evidence grants more broker-loss grace than the launcher accepts.
    #[error("broker-loss grace exceeds the fixed limit")]
    InvalidBrokerLossGrace,
    /// Launch evidence listed no capability channel.
    #[error("launch evidence requires at least one capability channel")]
    MissingChannel,
    /// Launch evidence repeated a capability channel identity.
    #[error("duplicate capability channel '{0}'")]
    DuplicateChannel(String),
    /// Capability channel identities were not in canonical order.
    #[error("capability channels are not sorted")]
    UnsortedChannels,
    /// Signed envelope carried no usable bounded signature.
    #[error("signature encoding is empty or oversized")]
    InvalidSignatureEncoding,
    /// Complete-chain verification received no sequence-zero receipt.
    #[error("receipt chain is empty")]
    EmptyChain,
    /// Chain sequence was not the exact successor expected.
    #[error("expected receipt sequence {expected}, found {found}")]
    ExpectedSequence {
        /// Exact next sequence.
        expected: u64,
        /// Sequence supplied.
        found: u64,
    },
    /// The trusted head cannot have a successor because its sequence is maxed.
    #[error("receipt sequence overflow")]
    SequenceOverflow,
    /// A successor did not hash the exact trusted signed envelope.
    #[error("receipt {sequence} names the wrong predecessor")]
    WrongPredecessor {
        /// Offending receipt sequence.
        sequence: u64,
    },
    /// Receipt belongs to another Session.
    #[error("receipt belongs to a foreign Session")]
    ForeignSession,
    /// Receipt belongs to another Run.
    #[error("receipt belongs to a foreign Run")]
    ForeignRun,
    /// Receipt switched launcher release inside one live chain.
    #[error("receipt changes the pinned launcher release")]
    ReleaseChanged,
    /// Receipt switched signing key inside one live chain.
    #[error("receipt changes the pinned signing key")]
    SigningKeyChanged,
    /// A successor declared an older capability envelope revision.
    #[error("receipt {sequence} regresses envelope revision from {previous} to {found}")]
    EnvelopeRevisionRegressed {
        /// Revision at the trusted predecessor.
        previous: u64,
        /// Revision declared by the successor.
        found: u64,
        /// Offending receipt sequence.
        sequence: u64,
    },
    /// Action cannot follow the trusted predecessor state.
    #[error("receipt {sequence} is not a valid lifecycle transition")]
    InvalidTransition {
        /// Offending receipt sequence.
        sequence: u64,
    },
    /// A request identity appeared twice in one supplied complete chain.
    #[error("duplicate receipt request id '{request_id}'")]
    DuplicateRequestId {
        /// Reused request identity.
        request_id: String,
    },
    /// Caller-provided cryptographic verification rejected the payload.
    #[error("receipt {sequence} has an invalid signature")]
    InvalidSignature {
        /// Offending receipt sequence.
        sequence: u64,
    },
}

/// A dependency-free callback representing later completion of an IO port.
pub type Completion<T, E> = Box<dyn FnOnce(Result<T, E>) + Send + 'static>;

/// Asynchronous-shaped boundary for the root-owned launcher signer.
pub trait ReceiptSigner {
    /// Adapter-specific signing error.
    type Error;

    /// Starts signing exact canonical payload bytes in [`RECEIPT_SCHEMA`].
    ///
    /// Returning only the signature keeps canonicalization and envelope
    /// construction in this module's caller rather than delegating either to
    /// the key adapter.
    fn sign(&self, payload_bytes: Vec<u8>, complete: Completion<String, Self::Error>);
}

/// Asynchronous-shaped boundary for exact durable receipt persistence.
pub trait ReceiptAppender {
    /// Adapter-specific persistence error.
    type Error;

    /// Starts appending exact canonical envelope bytes.
    fn append(&self, receipt_bytes: Vec<u8>, complete: Completion<(), Self::Error>);
}
