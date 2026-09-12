//! Closed recovery-retention exchange on the authenticated supervisor channel.

use serde::{Deserialize, Serialize};

use super::{
    ErrorCode, ProtocolError, validate_digest, validate_identifier, validate_schema,
    validate_version,
};
use crate::{launch::LaunchRequest, launch_receipt::ReceiptHead};

/// Version of the controller-to-supervisor retention operation.
pub const RECOVERY_REQUEST_SCHEMA: &str = "louiselm.launch.recovery-request/1";
/// Version of durable mechanical retention evidence.
pub const RETENTION_EVIDENCE_SCHEMA: &str = "louiselm.launch.recovery-retention/1";
/// Version of a broker-authorized copy into a distinct frozen Session.
pub const RECOVERY_RESTORE_SCHEMA: &str = "louiselm.launch.recovery-restore/1";

/// Exact protected recovery point and fresh target selected by the operator.
/// Contains no caller-selected filesystem paths or transferred capabilities.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryRestoreRequest {
    /// Closed operation schema.
    pub schema: String,
    /// Launch protocol version.
    pub protocol_version: u32,
    /// Immutable reconstruction operation identity.
    pub request_id: String,
    /// Broker-owned original retained point, including its unchanged expiry.
    pub source: RetentionEvidence,
    /// Distinct already-authorized target with the same envelope and inputs.
    pub target: LaunchRequest,
    /// Target's exact durable Park head before copying.
    pub head: ReceiptHead,
}

impl RecoveryRestoreRequest {
    /// Validates exact source/target invariants without granting restore authority.
    /// # Errors
    /// Refuses malformed fields, reused launch identity or changed Agent/envelope/inputs.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_schema(&self.schema, RECOVERY_RESTORE_SCHEMA)?;
        validate_version(self.protocol_version)?;
        validate_identifier(&self.request_id)?;
        validate_digest(&self.head.digest)?;
        self.source.validate()?;
        self.target.validate().map_err(|_| invalid())?;
        let source = &self.source.launch;
        let target = &self.target;
        if source.session_id == target.session_id
            || source.authorization_id == target.authorization_id
            || source.request_id == target.request_id
            || source.run_id != target.run_id
            || source.agent_id != target.agent_id
            || source.envelope_id != target.envelope_id
            || source.envelope_revision != target.envelope_revision
            || source.skill_generation_id != target.skill_generation_id
            || source.session_input_manifest_id != target.session_input_manifest_id
        {
            return Err(invalid());
        }
        Ok(())
    }

    /// Deterministic bytes used for immutable operation binding.
    /// # Panics
    /// Only a future fallible serializer could fail for this JSON-native schema.
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "Derived JSON-native fields have no fallible serializer."
    )]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("restore request is serializable")
    }
}

/// A bounded retention operation, with no caller-selected filesystem paths.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetentionRequest {
    /// Immutable operation identity.
    pub request_id: String,
    /// ACP identity observed by the authorized controller.
    pub acp_session_id: String,
    /// Original exclusive absolute Park expiry in milliseconds.
    pub expires_at_ms: u64,
}

/// Mechanical proof returned only after selected recovery bytes are durable.
/// A serialized copy has no authority without its authenticated supervisor source.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetentionEvidence {
    /// Closed evidence version.
    pub schema: String,
    /// Exact launch whose storage was retained.
    pub launch: LaunchRequest,
    /// Original operation, ACP identity and expiry.
    pub request: RetentionRequest,
    /// Measured Agent recovery layout contract.
    pub contract: String,
    /// Digest of the measured Agent/tool integration.
    pub integration_digest: String,
    /// Digest of checkpoint and required workspace material.
    pub material_digest: String,
}

impl RetentionEvidence {
    /// Checks shape only; the broker must also authenticate and bind the evidence.
    /// # Errors
    /// Refuses unknown schemas/contracts, malformed identities, digests or launch.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_schema(&self.schema, RETENTION_EVIDENCE_SCHEMA)?;
        self.launch.validate().map_err(|_| invalid())?;
        self.request.validate()?;
        if self.contract != "louiselm.test-recovery/1" {
            return Err(invalid());
        }
        validate_digest(&self.integration_digest)?;
        validate_digest(&self.material_digest)
    }
}

impl RetentionRequest {
    /// Checks bounded identifiers and nonzero expiry, without reading the clock.
    /// # Errors
    /// Refuses malformed identities or a zero deadline.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_identifier(&self.request_id)?;
        validate_identifier(&self.acp_session_id)?;
        if self.expires_at_ms == 0 {
            return Err(invalid());
        }
        Ok(())
    }
}

/// Exact launch and durable Park checkpoint selected by the broker.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryRequest {
    /// Closed request version.
    pub schema: String,
    /// Launch protocol version.
    pub protocol_version: u32,
    /// Exact authorized launch, including configured Agent.
    pub launch: LaunchRequest,
    /// Durable Park head that must still be current when retention completes.
    pub head: ReceiptHead,
    /// Bounded immutable retention operation.
    pub retention: RetentionRequest,
}

impl RecoveryRequest {
    /// Validates the closed operation before process or storage effects.
    /// # Errors
    /// Refuses malformed versions, launch identity, receipt digest or metadata.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_schema(&self.schema, RECOVERY_REQUEST_SCHEMA)?;
        validate_version(self.protocol_version)?;
        self.launch.validate().map_err(|_| invalid())?;
        validate_digest(&self.head.digest)?;
        self.retention.validate()
    }

    /// Returns deterministic request bytes.
    /// # Panics
    /// Only a future fallible serializer could fail for this JSON-native schema.
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "Derived JSON-native closed records have no fallible serializer."
    )]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("recovery request is serializable")
    }
}

fn invalid() -> ProtocolError {
    ProtocolError::new(ErrorCode::InvalidRequest, None, None)
}
