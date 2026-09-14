//! Bounded producer facts for the broker, distinct from prospective previews.

use super::{disclosure, managed, native};
use crate::{
    Policy, Store,
    discovery::{AuthenticatedInputs, DiscoveryError, DiscoveryProof},
    launch::LaunchRequest,
    posture::{EvidenceRef, FailureCode},
    registry::Registry,
};

/// Validated supply facts for exactly one authenticated launch.
///
/// Construction performs blocking producer I/O. No raw manifests, paths,
/// configuration or instruction contents are retained, and status cannot
/// deserialize this type. Runtime authority stays with the launch-proof owner.
///
/// ```compile_fail
/// let _: louiselm_skills::supply_posture::SupplyEvidence = serde_json::from_str("{}").unwrap();
/// ```
pub struct SupplyEvidence {
    pub(crate) request_digest: String,
    pub(crate) receipt_id: Option<String>,
    pub(crate) checked_at_ms: u64,
    pub(crate) dimensions: [Result<EvidenceRef, FailureCode>; 3],
}

impl SupplyEvidence {
    /// Checks admitted inputs using the existing supply validators.
    ///
    /// Run on the trusted producer worker, using protected store/policy/registry
    /// authority and its observation time. This is an admission/update boundary,
    /// never a status-read operation. Missing authenticated inputs cannot verify
    /// managed supply or disclosure; native supply needs its matching proof.
    /// Ordinary future-Session updates must not recheck a retained admission
    /// against the newly selected Generation.
    ///
    /// # Errors
    /// Refuses malformed requests, a different request/receipt, and unavailable
    /// or mismatched protected Agent/runtime registrations.
    /// Component failures remain independent typed dimension results.
    pub fn derive(
        request: &LaunchRequest,
        store: &Store,
        policy: &Policy,
        registry: &Registry,
        bound: Option<&AuthenticatedInputs>,
        proof: Option<&DiscoveryProof>,
        checked_at_ms: u64,
    ) -> Result<Self, DiscoveryError> {
        request
            .validate()
            .map_err(|_| DiscoveryError::Refused("invalid_launch_request"))?;
        if bound.is_some_and(|bound| bound.request() != request)
            || proof.is_some_and(|proof| !proof.matches_request(request))
            || bound
                .zip(proof)
                .is_some_and(|(bound, proof)| bound.receipt_id() != proof.receipt_id())
        {
            return Err(DiscoveryError::Refused("launch_binding_mismatch"));
        }
        if let Some(bound) = bound {
            registered_inputs(bound, registry)?;
        }
        let managed = bound.map_or(Err(FailureCode::EvidenceMissing), |bound| {
            managed(request, store, policy, registry, Some(bound.manifest()))
        });
        Ok(Self {
            request_digest: request.digest().to_string(),
            receipt_id: bound
                .map(AuthenticatedInputs::receipt_id)
                .or_else(|| proof.map(DiscoveryProof::receipt_id))
                .map(str::to_owned),
            checked_at_ms,
            dimensions: [managed, native(request, bound, proof), disclosure(bound)],
        })
    }
}

// Compare the authorized registration, without claiming a new runtime
// measurement. DiscoveryProof and the launch runtime owner perform their own
// existing measurement checks.
fn registered_inputs(
    bound: &AuthenticatedInputs,
    registry: &Registry,
) -> Result<(), DiscoveryError> {
    let manifest = bound.manifest();
    let agent = registry.agent(&manifest.agent.id)?;
    let mut runtime = registry.runtime(&agent.runtime_id)?;
    runtime.adapters.sort_by(|a, b| a.path.cmp(&b.path));
    if agent != manifest.agent
        || runtime.id != manifest.runtime.runtime_id
        || runtime.executable_sha256 != manifest.runtime.executable_sha256
        || runtime.adapters != manifest.runtime.adapters
        || runtime.version != manifest.runtime.version
        || runtime.origin != manifest.runtime.origin
    {
        return Err(DiscoveryError::Refused("runtime_registration_mismatch"));
    }
    Ok(())
}
