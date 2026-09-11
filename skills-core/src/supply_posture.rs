//! Independent supply status from materialization, measurement and signed inputs.
//!
//! Status is a checked snapshot, not a lifetime lease or permission to launch.
//! The installed launcher still owns enforcement and the isolation/network inputs.

use crate::{
    Digest, Policy, Store,
    discovery::{AuthenticatedInputs, DiscoveryProof},
    instruction_view::{self, ViewError},
    launch::{LaunchError, LaunchRequest},
    posture::{DimensionInput, DimensionName, EvidenceKind, EvidenceRef, FailureCode},
    registry::{Registry, RegistryError},
};

/// Derives managed supply, native supply, runtime and Provider disclosure inputs.
///
/// Blocking local filesystem/crypto I/O: rechecks/materializes the selected
/// Agent's current view and measures its registered runtime independently.
/// `store`, `policy` and `registry` must be protected caller authority; privileged
/// consumers use `Registry::open_trusted`. Signed inputs and discovery proofs
/// must match this exact request. Missing artifacts fail their own dimension;
/// a valid view or measured runtime does not require disclosure to succeed.
///
/// Component failures become fixed posture codes here, never raw error text,
/// paths, arguments, environment, Provider branding or instruction contents.
/// Combine these four inputs with the isolation/network owners' inputs and use
/// `Posture::evaluate` for both human and robot presentation.
///
/// # Errors
/// Returns a typed request-validation error for a malformed launch request.
pub fn derive(
    request: &LaunchRequest,
    store: &Store,
    policy: &Policy,
    registry: &Registry,
    bound: Option<&AuthenticatedInputs>,
    proof: Option<&DiscoveryProof>,
) -> Result<[DimensionInput; 4], LaunchError> {
    request.validate()?;
    let bound = bound.filter(|inputs| inputs.request() == request);
    Ok([
        input(
            DimensionName::ManagedSupply,
            managed(request, store, policy, registry, bound),
        ),
        input(DimensionName::NativeSupply, native(request, bound, proof)),
        input(DimensionName::Runtime, runtime(request, registry, bound)),
        input(DimensionName::ProviderDisclosure, disclosure(bound)),
    ])
}

fn input(dimension: DimensionName, evidence: Result<EvidenceRef, FailureCode>) -> DimensionInput {
    match evidence {
        Ok(evidence) => DimensionInput::verified(dimension, vec![evidence]),
        Err(code) => DimensionInput::failed(dimension, code, vec![]),
    }
}

fn reference(kind: EvidenceKind, id: &str) -> Result<EvidenceRef, FailureCode> {
    EvidenceRef::new(kind, id).map_err(|_| FailureCode::EvidenceMissing)
}

fn managed(
    request: &LaunchRequest,
    store: &Store,
    policy: &Policy,
    registry: &Registry,
    bound: Option<&AuthenticatedInputs>,
) -> Result<EvidenceRef, FailureCode> {
    let views =
        instruction_view::materialize(store, policy, registry).map_err(|error| match error {
            ViewError::Refused("no_current_generation") => FailureCode::EvidenceMissing,
            ViewError::Refused("generation_not_witnessed") => FailureCode::WitnessMissing,
            _ => FailureCode::RootTrustFailed,
        })?;
    let view = views
        .get(&request.agent_id)
        .ok_or(FailureCode::EvidenceMissing)?;
    if view.generation() != Some(request.skill_generation_id.as_str()) {
        return Err(FailureCode::RootTrustFailed);
    }
    if let Some(bound) = bound {
        let manifest = bound.manifest();
        if manifest.skill_generation.view_digest != view.digest().to_string()
            || manifest.policy_digest != policy.digest().to_string()
        {
            return Err(FailureCode::RootTrustFailed);
        }
    }
    reference(EvidenceKind::SkillGeneration, &request.skill_generation_id)
}

fn runtime(
    request: &LaunchRequest,
    registry: &Registry,
    bound: Option<&AuthenticatedInputs>,
) -> Result<EvidenceRef, FailureCode> {
    let agent = registry
        .agent(&request.agent_id)
        .map_err(|error| runtime_failure(&error))?;
    let package = registry
        .runtime(&agent.runtime_id)
        .map_err(|error| runtime_failure(&error))?;
    let mut measurement = package.measure().map_err(|error| runtime_failure(&error))?;
    if let Some(bound) = bound {
        bound
            .check_runtime_controls()
            .map_err(|error| error.failure_code())?;
    }
    measurement.adapters.sort_by(|a, b| a.path.cmp(&b.path));
    measurement.library_baseline.sort();
    if let Some(bound) = bound
        && (bound.manifest().runtime != measurement || bound.manifest().agent != agent)
    {
        return Err(FailureCode::RuntimeDrift);
    }
    let bytes = serde_json::to_vec(&measurement).map_err(|_| FailureCode::UnknownFailure)?;
    reference(
        EvidenceKind::RuntimeMeasurement,
        &Digest::of(&bytes).to_string(),
    )
}

fn runtime_failure(error: &RegistryError) -> FailureCode {
    match error {
        RegistryError::RuntimeChanged { .. } => FailureCode::RuntimeDrift,
        RegistryError::Untrusted { .. } => FailureCode::RootTrustFailed,
        _ => FailureCode::EvidenceMissing,
    }
}

fn native(
    request: &LaunchRequest,
    bound: Option<&AuthenticatedInputs>,
    proof: Option<&DiscoveryProof>,
) -> Result<EvidenceRef, FailureCode> {
    let proof = proof.ok_or(FailureCode::NativeSupplyUncertain)?;
    if !proof.matches_request(request)
        || bound.is_some_and(|inputs| inputs.receipt_id() != proof.receipt_id())
    {
        return Err(FailureCode::NativeSupplyUncertain);
    }
    reference(EvidenceKind::SessionInputManifest, proof.manifest_id())
}

fn disclosure(bound: Option<&AuthenticatedInputs>) -> Result<EvidenceRef, FailureCode> {
    let bound = bound.ok_or(FailureCode::ProviderDisclosureMissing)?;
    // Authentication already validates the complete fixed/reachable Provider set
    // and fixed notice. Do not copy their configured names into posture output.
    reference(
        EvidenceKind::SessionInputManifest,
        &bound.request().session_input_manifest_id,
    )
}
