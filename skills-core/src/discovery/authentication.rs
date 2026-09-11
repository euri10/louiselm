//! Bind raw input records to the existing authenticated sequence-zero receipt.

use super::{DiscoveryError, json_digest};
use crate::{
    isolation::IsolationEvidence,
    launch::LaunchRequest,
    launch_receipt::{self, ChainAnchor, ReceiptOutcome, SignedReceipt},
    session_manifest::SessionInputManifest,
};
use std::fmt;

/// Immutable authenticated launch inputs, not a claim of current live state.
///
/// Only signature verification constructs this type. Cloning or deserializing
/// raw manifests cannot manufacture it. Debug output omits sensitive input data.
#[derive(Clone)]
pub struct AuthenticatedInputs {
    pub(super) request: LaunchRequest,
    pub(super) manifest: SessionInputManifest,
    pub(super) isolation: IsolationEvidence,
    pub(super) receipt_id: String,
}

impl fmt::Debug for AuthenticatedInputs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthenticatedInputs")
            .field("session_id", &self.request.session_id)
            .field("manifest_id", &self.request.session_input_manifest_id)
            .finish_non_exhaustive()
    }
}

impl AuthenticatedInputs {
    /// Authenticates one exact requested launch and all its input evidence.
    ///
    /// `request`, `anchor`, and the signature verifier are trusted caller inputs,
    /// never selected by an Agent or read from the presented receipt. The verifier
    /// checks signatures in `launch_receipt::RECEIPT_SCHEMA` against the installed
    /// launcher key. Native controls are evaluated separately, so their failure
    /// does not erase authenticated disclosure or runtime evidence.
    ///
    /// # Errors
    /// Refuses invalid signatures, schemas, subjects, canonical records, request
    /// bindings, envelope revisions or any substituted evidence digest.
    pub fn verify<F>(
        request: &LaunchRequest,
        manifest: &SessionInputManifest,
        isolation: &IsolationEvidence,
        receipt: &SignedReceipt,
        anchor: &ChainAnchor,
        verify_signature: F,
    ) -> Result<Self, DiscoveryError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        request
            .validate()
            .map_err(|_| DiscoveryError::Refused("invalid_launch_request"))?;
        let manifest = SessionInputManifest::parse(&manifest.canonical_bytes())?;
        launch_receipt::verify_chain(std::slice::from_ref(receipt), anchor, verify_signature)?;
        let ReceiptOutcome::Launch {
            authorization,
            evidence,
        } = &receipt.payload.outcome
        else {
            return Err(DiscoveryError::Refused("not_launch_evidence"));
        };
        if anchor.session_id != request.session_id
            || anchor.run_id != request.run_id
            || receipt.payload.request_id != request.request_id
            || authorization.authorization_id != request.authorization_id
            || authorization.request_id != request.request_id
            || authorization.request_digest != request.digest().to_string()
            || evidence.launch_request_digest != request.digest().to_string()
            || request.agent_id != manifest.agent.id
            || request.envelope_id != manifest.envelope.id
            || request.envelope_revision != manifest.envelope.revision
            || receipt.payload.envelope_revision != request.envelope_revision
            || request.skill_generation_id != manifest.skill_generation.generation_digest
            || evidence.skill_generation_id != request.skill_generation_id
            || request.session_input_manifest_id != manifest.digest().to_string()
            || evidence.session_input_manifest_id != request.session_input_manifest_id
            || evidence.runtime_measurement_digest != json_digest(&manifest.runtime)?.to_string()
            || evidence.isolation_evidence_digest != json_digest(isolation)?.to_string()
            || evidence.isolation_contract != isolation.contract_version
        {
            return Err(DiscoveryError::Refused("launch_binding_mismatch"));
        }
        Ok(Self {
            request: request.clone(),
            manifest,
            isolation: isolation.clone(),
            receipt_id: receipt.digest().to_string(),
        })
    }

    /// Exact request authenticated for this Session.
    #[must_use]
    pub fn request(&self) -> &LaunchRequest {
        &self.request
    }
    /// Exact bound inputs. May contain sensitive configuration; never log them.
    #[must_use]
    pub fn manifest(&self) -> &SessionInputManifest {
        &self.manifest
    }
    /// Signed launcher receipt identity.
    #[must_use]
    pub fn receipt_id(&self) -> &str {
        &self.receipt_id
    }

    pub(crate) fn check_runtime_controls(&self) -> Result<(), DiscoveryError> {
        // Absent source evidence prevents native verification independently.
        // Known unsafe controls also fail runtime, even if bytes still match.
        if let Some(sources) = &self.isolation.native_sources {
            if !sources.fixed_executable {
                return Err(DiscoveryError::Refused("live_executable_lookup"));
            }
            if !sources.self_update_disabled {
                return Err(DiscoveryError::Refused("self_update_enabled"));
            }
        }
        Ok(())
    }
}
