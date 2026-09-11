//! Prospective artifact inspection, never authorization or live Session state.
//!
//! Proposed records only constrain independent local checks. Runtime controls
//! remain unproven. No receipt from a previous launch is reused as authority.

use std::fmt::Write as _;

use serde::Serialize;
use thiserror::Error;

use crate::{
    Digest, Policy, Store,
    isolation::CONTRACT_VERSION,
    launch::LaunchRequest,
    posture::{DimensionInput, DimensionName, FailureCode, Posture},
    registry::Registry,
    session_manifest::SessionInputManifest,
    supply_posture,
};

/// Versioned presentation record, not a launch protocol.
pub const PREFLIGHT_SCHEMA: &str = "louiselm.launch.preflight/1";
/// Always displayed, including when local artifacts check out.
pub const PREFLIGHT_NOTICE: &str = "Prospective artifact snapshot only; not authorization or live Session state. Launch must recheck and bind this exact request digest. Proposed identities do not prove enforcement.";
/// Normal configured commands, including wrappers, carry no implied protection.
pub const DIRECT_LAUNCH_NOTICE: &str = "Direct vendor launch: no LouiseLM Verified posture exists. A wrapper does not establish Verified posture.";

/// Whether supplied input bytes actually match the proposed request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ManifestState {
    /// No manifest was selected.
    Missing,
    /// Valid manifest, but its digest or bindings disagree with the request.
    Contradictory,
    /// Canonical bytes and all request bindings agree; provenance is not implied.
    Matched,
}

/// Closed set of preview identities. Values never contain instruction content,
/// runtime paths, arguments, environment, or arbitrary Provider strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityField {
    /// Configured Agent identifier.
    Agent,
    /// Requested witnessed Generation digest.
    Generation,
    /// Agent-scoped view digest.
    InstructionView,
    /// Digest of the normalized runtime measurement.
    Runtime,
    /// Complete Session input manifest digest.
    InputManifest,
    /// Project-instruction snapshot inventory digest.
    ProjectInstructions,
    /// Tool-schema snapshot inventory digest.
    ToolSchemas,
    /// Plugin-schema snapshot inventory digest.
    PluginSchemas,
    /// Governing supply policy digest.
    Policy,
    /// Recognized isolation contract version, otherwise unresolved.
    IsolationContract,
    /// Proposed isolation evidence identifier, not proof of confinement.
    IsolationReceipt,
    /// Requested capability envelope identifier.
    Envelope,
    /// Requested revision as an exact decimal string (no JSON precision loss).
    EnvelopeRevision,
    /// Unresolved: the request/manifest do not contain revision-bound network rules.
    NetworkScope,
    /// Digest of the proposed reachable service set and fixed disclosure notice.
    ProviderDisclosure,
}

impl IdentityField {
    /// Stable human/robot field label.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Generation => "generation",
            Self::InstructionView => "instruction_view",
            Self::Runtime => "runtime",
            Self::InputManifest => "input_manifest",
            Self::ProjectInstructions => "project_instructions",
            Self::ToolSchemas => "tool_schemas",
            Self::PluginSchemas => "plugin_schemas",
            Self::Policy => "policy",
            Self::IsolationContract => "isolation_contract",
            Self::IsolationReceipt => "isolation_receipt",
            Self::Envelope => "envelope",
            Self::EnvelopeRevision => "envelope_revision",
            Self::NetworkScope => "network_scope",
            Self::ProviderDisclosure => "provider_disclosure",
        }
    }
}

/// A proposed identity; `None` is unresolved, never an empty or denied scope.
#[derive(Debug, Serialize)]
pub struct ProposedIdentity {
    /// Closed identity label.
    pub field: IdentityField,
    /// Validated identifier, digest, contract version, or exact revision.
    pub value: Option<String>,
}

/// Comparability is limited to explicit, matching manifests for the same Agent.
/// It does not attest that the prior request ran, or share a workspace identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ComparisonState {
    /// No automatic history lookup is performed.
    NotRequested,
    /// Different Agent configurations are not comparable.
    DifferentAgent,
    /// One of the selected manifests is missing or contradicts its request.
    InputsUnavailable,
    /// Known identities compared; unresolved fields remain explicitly listed.
    Compared,
}

/// A change between two known proposed identities.
#[derive(Debug, Serialize)]
pub struct IdentityChange {
    /// Identity being compared.
    pub field: IdentityField,
    /// Exact prior value.
    pub before: String,
    /// Exact proposed value.
    pub after: String,
}

/// Typed comparison with the explicitly selected prior input artifact.
#[derive(Debug, Serialize)]
pub struct Comparison {
    /// Whether and why comparison was possible.
    pub state: ComparisonState,
    /// Exact selected prior request digest, not evidence of its execution.
    pub previous_request_digest: Option<String>,
    /// Changes where both values are known.
    pub changes: Vec<IdentityChange>,
    /// Fields with at least one unresolved value, never reported as unchanged.
    pub unresolved: Vec<IdentityField>,
}

/// One normalized record consumed by human CLI, robot clients, and health.
#[derive(Debug, Serialize)]
pub struct Preview {
    /// Presentation schema.
    pub schema: &'static str,
    /// Fixed limit on what this snapshot claims.
    pub notice: &'static str,
    /// Identity future launch integration must bind, not an approval token.
    pub request_digest: String,
    /// Whether proposed inputs match the request.
    pub manifest_state: ManifestState,
    /// Ordered proposed identities, explicitly separate from checked evidence.
    pub proposed: Vec<ProposedIdentity>,
    /// Explicit prior input comparison.
    pub comparison: Comparison,
    /// Independent artifact checks and unproven enforcement dimensions.
    pub posture: Posture,
}

/// Fixed errors never disclose supplied paths, schema strings or payloads.
#[derive(Debug, Error)]
pub enum PreflightError {
    /// A selected request failed validation.
    #[error("invalid preflight request")]
    Request,
    /// A selected manifest is malformed or noncanonical.
    #[error("invalid preflight input manifest")]
    Manifest,
    /// Internal normalized evidence was rejected.
    #[error("invalid preflight evidence")]
    Evidence,
}

/// Inspects local artifacts against exact proposed bytes, without spawning an
/// Agent, granting permissions, or recording approval. May materialize the
/// existing immutable view cache. Blocking filesystem/crypto I/O; run off the UI.
///
/// The caller supplies protected store/policy/registry authority; installed CLI
/// consumers use `Registry::open_trusted`. Missing readers fail independently.
/// Network scope stays unresolved: today's registry has no revision-bound
/// network record and cannot reconstruct a prior envelope's rules.
///
/// # Errors
/// Rejects malformed current/prior inputs and invalid normalized evidence.
pub fn inspect(
    request: &LaunchRequest,
    manifest: Option<&SessionInputManifest>,
    store: Option<&Store>,
    policy: &Policy,
    registry: Option<&Registry>,
    previous: Option<(&LaunchRequest, &SessionInputManifest)>,
) -> Result<Preview, PreflightError> {
    let manifest_state = binding(request, manifest)?;
    let manifest = manifest.filter(|_| manifest_state == ManifestState::Matched);
    let proposed = identities(request, manifest)?;
    let mut inputs = vec![
        failed(DimensionName::ManagedSupply, FailureCode::EvidenceMissing),
        failed(DimensionName::Runtime, FailureCode::EvidenceMissing),
        failed(
            DimensionName::NativeSupply,
            FailureCode::NativeSupplyUncertain,
        ),
        failed(DimensionName::Isolation, FailureCode::EvidenceMissing),
        failed(DimensionName::Network, FailureCode::EvidenceMissing),
        failed(
            DimensionName::ProviderDisclosure,
            FailureCode::ProviderDisclosureMissing,
        ),
    ];
    if let (Some(manifest), Some(registry)) = (manifest, registry) {
        let [managed, runtime] =
            supply_posture::artifacts(request, manifest, store, policy, registry);
        inputs[0] = managed;
        inputs[1] = runtime;
    }
    let comparison = compare(request, manifest_state, &proposed, previous)?;
    Ok(Preview {
        schema: PREFLIGHT_SCHEMA,
        notice: PREFLIGHT_NOTICE,
        request_digest: request.digest().to_string(),
        manifest_state,
        proposed,
        comparison,
        posture: Posture::evaluate(&request.session_id, &request.run_id, inputs)
            .map_err(|_| PreflightError::Evidence)?,
    })
}

fn failed(dimension: DimensionName, code: FailureCode) -> DimensionInput {
    DimensionInput::failed(dimension, code, vec![])
}

fn binding(
    request: &LaunchRequest,
    manifest: Option<&SessionInputManifest>,
) -> Result<ManifestState, PreflightError> {
    request.validate().map_err(|_| PreflightError::Request)?;
    let Some(manifest) = manifest else {
        return Ok(ManifestState::Missing);
    };
    SessionInputManifest::parse(&manifest.canonical_bytes())
        .map_err(|_| PreflightError::Manifest)?;
    Ok(
        if manifest.digest().to_string() == request.session_input_manifest_id
            && manifest.agent.id == request.agent_id
            && manifest.skill_generation.generation_digest == request.skill_generation_id
            && manifest.envelope.id == request.envelope_id
            && manifest.envelope.revision == request.envelope_revision
        {
            ManifestState::Matched
        } else {
            ManifestState::Contradictory
        },
    )
}

fn digest(value: &impl Serialize) -> Result<String, PreflightError> {
    serde_json::to_vec(value)
        .map(|bytes| Digest::of(&bytes).to_string())
        .map_err(|_| PreflightError::Manifest)
}

fn identities(
    request: &LaunchRequest,
    manifest: Option<&SessionInputManifest>,
) -> Result<Vec<ProposedIdentity>, PreflightError> {
    use IdentityField as F;
    let entries = [
        (F::Agent, Some(request.agent_id.clone())),
        (F::Generation, Some(request.skill_generation_id.clone())),
        (
            F::InstructionView,
            manifest.map(|m| m.skill_generation.view_digest.clone()),
        ),
        (
            F::Runtime,
            manifest.map(|m| digest(&m.runtime)).transpose()?,
        ),
        (
            F::InputManifest,
            Some(request.session_input_manifest_id.clone()),
        ),
        (
            F::ProjectInstructions,
            manifest
                .map(|m| digest(&m.project_instructions))
                .transpose()?,
        ),
        (
            F::ToolSchemas,
            manifest.map(|m| digest(&m.tool_schemas)).transpose()?,
        ),
        (
            F::PluginSchemas,
            manifest.map(|m| digest(&m.plugin_schemas)).transpose()?,
        ),
        (F::Policy, manifest.map(|m| m.policy_digest.clone())),
        (
            F::IsolationContract,
            manifest
                .filter(|m| m.runtime.isolation_policy_version == CONTRACT_VERSION)
                .map(|_| CONTRACT_VERSION.to_owned()),
        ),
        (
            F::IsolationReceipt,
            manifest.map(|m| m.isolation_receipt.clone()),
        ),
        (F::Envelope, Some(request.envelope_id.clone())),
        (
            F::EnvelopeRevision,
            Some(request.envelope_revision.to_string()),
        ),
        (F::NetworkScope, None),
        (
            F::ProviderDisclosure,
            manifest
                .map(|m| digest(&m.provider_disclosure))
                .transpose()?,
        ),
    ];
    Ok(entries
        .into_iter()
        .map(|(field, value)| ProposedIdentity { field, value })
        .collect())
}

fn compare(
    request: &LaunchRequest,
    manifest_state: ManifestState,
    proposed: &[ProposedIdentity],
    previous: Option<(&LaunchRequest, &SessionInputManifest)>,
) -> Result<Comparison, PreflightError> {
    let mut result = Comparison {
        state: ComparisonState::NotRequested,
        previous_request_digest: None,
        changes: vec![],
        unresolved: vec![],
    };
    let Some((prior_request, prior_manifest)) = previous else {
        return Ok(result);
    };
    let prior_state = binding(prior_request, Some(prior_manifest))?;
    result.previous_request_digest = Some(prior_request.digest().to_string());
    result.state = if request.agent_id != prior_request.agent_id {
        ComparisonState::DifferentAgent
    } else if manifest_state != ManifestState::Matched || prior_state != ManifestState::Matched {
        ComparisonState::InputsUnavailable
    } else {
        ComparisonState::Compared
    };
    if result.state == ComparisonState::Compared {
        for (current, prior) in proposed
            .iter()
            .zip(identities(prior_request, Some(prior_manifest))?)
        {
            match (&current.value, prior.value) {
                (Some(after), Some(before)) if *after != before => {
                    result.changes.push(IdentityChange {
                        field: current.field,
                        before,
                        after: after.clone(),
                    });
                }
                (Some(_), Some(_)) => {}
                _ => result.unresolved.push(current.field),
            }
        }
    }
    Ok(result)
}

/// Human rendering of exactly the same normalized record emitted to robots.
#[must_use]
pub fn render(preview: &Preview) -> String {
    let mut out = format!(
        "{}\nRequest digest: {}\nManifest: {:?}\nProposed identities (not enforcement evidence):\n",
        preview.notice, preview.request_digest, preview.manifest_state
    );
    for identity in &preview.proposed {
        let _ = writeln!(
            out,
            "  {}: {}",
            identity.field.name(),
            identity.value.as_deref().unwrap_or("unresolved")
        );
    }
    let _ = writeln!(
        out,
        "Prior input comparison: {:?} (selected artifacts only; not evidence of execution)",
        preview.comparison.state
    );
    if let Some(digest) = &preview.comparison.previous_request_digest {
        let _ = writeln!(out, "  prior request digest: {digest}");
    }
    for change in &preview.comparison.changes {
        let _ = writeln!(
            out,
            "  {}: {} -> {}",
            change.field.name(),
            change.before,
            change.after
        );
    }
    for field in &preview.comparison.unresolved {
        let _ = writeln!(out, "  {}: comparison unresolved", field.name());
    }
    out.push_str(&crate::render::posture(&preview.posture));
    out
}
