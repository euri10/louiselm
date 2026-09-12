//! Exact broker-directed verification operations, independent of Agent tool requests.

use super::{
    ErrorCode, ProtocolError, validate_digest, validate_identifier, validate_schema,
    validate_version,
};
use crate::{
    launch::LaunchRequest, launch_receipt::ReceiptHead, workspace::verification::JobPreview,
};
use serde::{Deserialize, Serialize};

/// Version of the closed verification operation.
pub const VERIFICATION_SCHEMA: &str = "louiselm.launch.verification/1";

/// One operation selected by the authenticated broker; no host paths or commands.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum VerificationOperation {
    /// Export the actual frozen producer workspace against staged approved inputs.
    Export {
        /// Broker-owned staging directory identifier.
        input_id: String,
        /// Exact staged snapshot and plan identity.
        input_digest: String,
    },
    /// Execute a retained job in this distinct fresh Session.
    Run {
        /// Supervisor whose actual export produced the job.
        producer_session_id: String,
        /// Producer-owned immutable export operation.
        export_request_id: String,
        /// Digest of the exact authenticated export evidence.
        export_digest: String,
        /// Independently approved prepared job digest.
        job_digest: String,
    },
}

/// Single-use job authority conveyed only on the retained broker connection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationRequest {
    /// Fixed schema.
    pub schema: String,
    /// Shared broker/supervisor wire version.
    pub protocol_version: u32,
    /// Unique operation, never renewed or automatically replayed.
    pub request_id: String,
    /// Exact launch and configured Agent whose supervisor owns the operation.
    pub launch: LaunchRequest,
    /// Required durable lifecycle head before the operation starts.
    pub head: ReceiptHead,
    /// Exclusive absolute job authorization deadline in milliseconds.
    pub expires_at_ms: u64,
    /// Bounded export or execution scope.
    pub operation: VerificationOperation,
}

impl VerificationRequest {
    /// Validates shape; authentication, currency and one-use admission remain owner checks.
    /// # Errors
    /// Refuses malformed identities, digests, schema, same-Session execution or zero expiry.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_schema(&self.schema, VERIFICATION_SCHEMA)?;
        validate_version(self.protocol_version)?;
        validate_identifier(&self.request_id)?;
        self.launch.validate().map_err(|_| invalid())?;
        validate_digest(&self.head.digest)?;
        if self.expires_at_ms == 0 {
            return Err(invalid());
        }
        match &self.operation {
            VerificationOperation::Export {
                input_id,
                input_digest,
            } => {
                validate_identifier(input_id)?;
                validate_digest(input_digest)?;
            }
            VerificationOperation::Run {
                producer_session_id,
                export_request_id,
                export_digest,
                job_digest,
            } => {
                validate_identifier(producer_session_id)?;
                validate_identifier(export_request_id)?;
                validate_digest(export_digest)?;
                validate_digest(job_digest)?;
                if producer_session_id == &self.launch.session_id || self.head.sequence != 1 {
                    return Err(invalid());
                }
            }
        }
        Ok(())
    }

    /// Canonical encoding with no command or source payload.
    /// # Panics
    /// Only a future fallible serializer could fail for these derived JSON-native fields.
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "Derived JSON-native fields have no fallible serializer."
    )]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("verification request is serializable")
    }
}

/// Supervisor-observed export, made durable alongside the exact prepared job.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationExport {
    /// Exact operation and producer launch at the frozen checkpoint.
    pub request: VerificationRequest,
    /// Exact immutable job identities; preparation remains distinct from execution.
    pub job: JobPreview,
    /// Actual launched Agent/tool isolation measurement.
    pub integration_digest: String,
}

impl VerificationExport {
    /// Checks bounded normalized structure, not provenance of serialized bytes.
    /// # Errors
    /// Refuses contradictory operation, malformed measurements or job identities.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        self.request.validate()?;
        if !matches!(self.request.operation, VerificationOperation::Export { .. }) {
            return Err(invalid());
        }
        validate_digest(&self.integration_digest)?;
        validate_job(&self.job)
    }

    /// Exact evidence identity, authenticated by its original supervisor transport.
    /// # Errors
    /// Refuses invalid records or serialization failure.
    pub fn digest(&self) -> Result<crate::Digest, ProtocolError> {
        self.validate()?;
        Ok(crate::Digest::of(
            &serde_json::to_vec(self).map_err(|_| invalid())?,
        ))
    }
}

/// Normalized command observation; no stdout, stderr, arguments or environment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum VerificationStep {
    /// Actual exit observed, followed by proven descendant cleanup.
    Completed {
        /// Actual exit or negative signal status.
        exit_code: i32,
        /// Whether the command exceeded its own deadline.
        timed_out: bool,
    },
    /// The command may have started but no complete result was obtained.
    Unknown,
}

/// Actual plan outcomes, including any partial execution and cleanup uncertainty.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationExecution {
    /// Exact spent job authorization and verifier launch.
    pub request: VerificationRequest,
    /// Remeasured input job before making the independent execution copy.
    pub job: JobPreview,
    /// Actual verifier Agent/tool isolation measurement.
    pub integration_digest: String,
    /// Ordered observations; absent trailing commands were never started.
    pub steps: Vec<VerificationStep>,
    /// True only after every started command tree was proved terminated.
    pub cleanup_proven: bool,
    /// Cancellation, expiry or loss of the verifier lifetime precludes passing.
    pub interrupted: bool,
}

impl VerificationExecution {
    /// Checks shape only; the broker must authenticate and revalidate applicability.
    /// # Errors
    /// Refuses a foreign job, excess steps or malformed launch/integration evidence.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        self.request.validate()?;
        let VerificationOperation::Run { job_digest, .. } = &self.request.operation else {
            return Err(invalid());
        };
        validate_job(&self.job)?;
        validate_digest(&self.integration_digest)?;
        if job_digest != &self.job.job_digest || self.steps.len() > self.job.command_count {
            return Err(invalid());
        }
        Ok(())
    }

    /// Mechanical success only; authority still requires the broker's durable admission.
    #[must_use]
    pub fn commands_passed(&self) -> bool {
        self.validate().is_ok()
            && self.cleanup_proven
            && !self.interrupted
            && self.steps.len() == self.job.command_count
            && self.steps.iter().all(|step| {
                matches!(
                    step,
                    VerificationStep::Completed {
                        exit_code: 0,
                        timed_out: false
                    }
                )
            })
    }
}

fn validate_job(job: &JobPreview) -> Result<(), ProtocolError> {
    if job.schema != "louiselm.workspace.verification-preview/1"
        || job.state != "prepared"
        || !(1..=32).contains(&job.command_count)
    {
        return Err(invalid());
    }
    for digest in [
        &job.job_digest,
        &job.snapshot_digest,
        &job.bundle_digest,
        &job.base_digest,
        &job.result_digest,
        &job.plan_digest,
    ] {
        validate_digest(digest)?;
    }
    Ok(())
}

fn invalid() -> ProtocolError {
    ProtocolError::new(ErrorCode::InvalidRequest, None, None)
}
