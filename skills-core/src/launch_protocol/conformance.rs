//! Bounded report fragments bound to an exact signed launch receipt.

use serde::{Deserialize, Serialize};

use super::{ErrorCode, MAX_PROTOCOL_MESSAGE_BYTES, ProtocolError, validate_digest};
use crate::conformance::{
    MAX_REPORT_BYTES,
    admission::{Attendance, Condition},
};
use crate::launch_receipt::ConformanceEvidence;

/// Checks bounded historical display facts, never their present applicability.
pub(crate) fn validate_admission_history(
    admission: &ConformanceEvidence,
) -> Result<(), ProtocolError> {
    match admission {
        ConformanceEvidence::Waived {
            condition: Condition::ContainmentFailure,
            ..
        }
        | ConformanceEvidence::Waived {
            condition: Condition::Missing,
            report_digest: Some(_),
        } => Err(invalid()),
        ConformanceEvidence::Certified { report_digest }
        | ConformanceEvidence::Waived {
            report_digest: Some(report_digest),
            ..
        } => validate_digest(report_digest),
        ConformanceEvidence::Unevaluated
        | ConformanceEvidence::Waived {
            report_digest: None,
            ..
        } => Ok(()),
    }
}

/// Broker-approved attendance and optional exact waiver, never gate activation.
///
/// The trusted controller supplies already-authorized policy. Its waiver
/// producer owns approval/audit validation; the supervisor independently checks
/// these bindings and re-evaluates the actual protected host condition.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConformanceAuthorization {
    /// Trusted Run attendance; omission from the wire is invalid, not interactive.
    pub attendance: Attendance,
    /// Validated operator decision for this exact launch, if any.
    pub waiver: Option<ConformanceWaiver>,
}

/// Exact normalized outcome from the broker's operator-waiver authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConformanceWaiver {
    /// Session explicitly approved by the operator.
    pub session_id: String,
    /// Exact request digest, binding Run, authorization and envelope revision too.
    pub request_digest: String,
    /// Authenticated operator identity that approved this decision.
    pub operator_uid: u32,
    /// Only this condition may be waived; containment failure never may.
    pub condition: Condition,
    /// Exclusive absolute expiry; transport and restart cannot renew it.
    pub expires_at_ms: u64,
    /// Digest of the broker's durable waiver receipt, not an Agent verdict.
    pub receipt_digest: String,
}

/// Authenticated broker update to a live supervisor's current waiver policy.
/// Original launch authorization and signed admission remain immutable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WaiverChange {
    /// Closed operation schema.
    pub schema: String,
    /// Launch protocol version.
    pub protocol_version: u32,
    /// Correlation identity.
    pub request_id: String,
    /// Exact existing Session.
    pub session_id: String,
    /// Immutable launch request digest.
    pub request_digest: String,
    /// Monotonic broker decision revision; zero denotes original admission.
    pub revision: u64,
    /// Current approved decision, or explicit revocation.
    pub waiver: Option<ConformanceWaiver>,
}

impl WaiverChange {
    /// Validate the closed request without authenticating its sender.
    /// # Errors
    /// Refuses malformed identifiers, unsupported schemas and invalid decisions.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.schema != "louiselm.launch.waiver-change/1"
            || self.protocol_version != crate::launch::PROTOCOL_VERSION
            || self.revision == 0
        {
            return Err(invalid());
        }
        super::validate_identifier(&self.request_id)?;
        super::validate_identifier(&self.session_id)?;
        validate_digest(&self.request_digest)?;
        if let Some(waiver) = &self.waiver {
            ConformanceAuthorization {
                attendance: Attendance::Interactive,
                waiver: Some(waiver.clone()),
            }
            .validate_for(
                &self.session_id,
                &self.request_digest,
                waiver.operator_uid,
                0,
            )?;
        }
        Ok(())
    }

    /// Encode an exact bounded request.
    /// # Errors
    /// Refuses invalid fields or serialization failure.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ProtocolError> {
        self.validate()?;
        serde_json::to_vec(self).map_err(|_| invalid())
    }
}

impl ConformanceAuthorization {
    /// Check the exact launch binding and current waiver validity.
    ///
    /// # Errors
    /// Rejects unattended waivers, foreign Session/request/operator decisions,
    /// invalid receipt digests, expired decisions or non-waivable failures.
    pub fn validate_for(
        &self,
        session_id: &str,
        request_digest: &str,
        controller_uid: u32,
        now_ms: u64,
    ) -> Result<(), ProtocolError> {
        if let Some(waiver) = &self.waiver {
            validate_digest(&waiver.receipt_digest)?;
            if self.attendance != Attendance::Interactive
                || waiver.session_id != session_id
                || waiver.request_digest != request_digest
                || waiver.operator_uid == 0
                || waiver.operator_uid != controller_uid
                || waiver.expires_at_ms <= now_ms
                || waiver.condition == Condition::ContainmentFailure
            {
                return Err(invalid());
            }
        }
        Ok(())
    }
}

/// Schema for a supervisor's supplemental observation report fragment.
pub const CONFORMANCE_REPORT_CHUNK_SCHEMA: &str = "louiselm.launch.conformance-report-chunk/1";
/// Fixed fragment size; even JSON's largest byte encoding fits one packet.
pub const CONFORMANCE_REPORT_CHUNK_BYTES: usize = 8192;

/// One exact fragment, sent after its signed receipt and before its durable ACK.
///
/// The receipt digest binds the Session, Run, authorization and report digest.
/// Fragments alone assert no authority; the receiver authenticates the producer,
/// enforces contiguous order and verifies the complete report against the receipt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConformanceReportChunk {
    /// Must be [`CONFORMANCE_REPORT_CHUNK_SCHEMA`].
    pub schema: String,
    /// Digest of the exact canonical signed receipt preceding this transfer.
    pub receipt_digest: String,
    /// Zero-based byte position, aligned to the fixed fragment size.
    pub offset: usize,
    /// Complete report length, at most the observation report bound.
    pub total_bytes: usize,
    /// Exact report bytes, never normalized or interpreted independently.
    pub bytes: Vec<u8>,
}

fn invalid() -> ProtocolError {
    ProtocolError::new(ErrorCode::MalformedMessage, None, None)
}

impl ConformanceReportChunk {
    /// Encode one closed, bounded fragment.
    ///
    /// # Errors
    /// Rejects invalid schema/digest, empty or oversized reports, and non-fixed
    /// fragment lengths or offsets. The last fragment may be shorter.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ProtocolError> {
        validate_digest(&self.receipt_digest)?;
        if self.schema != CONFORMANCE_REPORT_CHUNK_SCHEMA
            || self.total_bytes == 0
            || self.total_bytes > MAX_REPORT_BYTES
            || self.offset >= self.total_bytes
            || !self.offset.is_multiple_of(CONFORMANCE_REPORT_CHUNK_BYTES)
            || self.bytes.len()
                != (self.total_bytes - self.offset).min(CONFORMANCE_REPORT_CHUNK_BYTES)
        {
            return Err(invalid());
        }
        let bytes = serde_json::to_vec(self).map_err(|_| invalid())?;
        if bytes.len() > MAX_PROTOCOL_MESSAGE_BYTES {
            return Err(invalid());
        }
        Ok(bytes)
    }

    /// Decode exact canonical bytes without authenticating their producer.
    ///
    /// # Errors
    /// Rejects oversized, malformed, noncanonical or invalid fragments.
    pub fn parse_canonical(bytes: &[u8]) -> Result<Self, ProtocolError> {
        if bytes.len() > MAX_PROTOCOL_MESSAGE_BYTES {
            return Err(invalid());
        }
        let value: Self = serde_json::from_slice(bytes).map_err(|_| invalid())?;
        if value.canonical_bytes()? != bytes {
            return Err(invalid());
        }
        Ok(value)
    }
}
