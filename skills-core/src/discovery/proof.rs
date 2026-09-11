//! Verify complete source controls against authenticated inputs and runtime bytes.

use super::{
    AuthenticatedInputs, DiscoveryError, Inventory, MAX_DISCOVERY_BYTES, SOURCE_EVIDENCE_SCHEMA,
    json_digest,
};
use crate::discovery_source::{SourceControl, SourceKind, SourceObservation};
use crate::registry::RuntimePackage;
use std::collections::BTreeMap;

/// Checked native-source controls, bound to one authenticated launch.
///
/// This is evidence about the signed launch, not a liveness or mount lease.
/// Only verification constructs it; no deserialization or raw evidence setter.
#[derive(Clone, Debug)]
pub struct DiscoveryProof {
    request_digest: crate::Digest,
    manifest_id: String,
    receipt_id: String,
    sources: Vec<SourceObservation>,
}

impl DiscoveryProof {
    /// Remeasures the runtime and verifies every signed source observation.
    ///
    /// Blocking local I/O. `runtime` must come from the protected registry used
    /// for the request. A supported backend signs these observations only after
    /// establishing immutable snapshot routing and masks for the Session lifetime.
    /// A generic sandbox receipt with no source observations always fails here.
    ///
    /// # Errors
    /// Refuses runtime drift, absent or unregistered inventory, incomplete or
    /// unknown sources, contradictory controls, mutable rediscovery, native MCP,
    /// live executable lookup, self-update, or unbound snapshots.
    pub fn verify(
        bound: &AuthenticatedInputs,
        runtime: &RuntimePackage,
    ) -> Result<Self, DiscoveryError> {
        let mut measured = runtime.measure()?;
        measured.adapters.sort_by(|a, b| a.path.cmp(&b.path));
        measured.library_baseline.sort();
        if measured != bound.manifest.runtime {
            return Err(DiscoveryError::Refused("runtime_mismatch"));
        }
        let inventory = Inventory::load(runtime)?;
        bound.isolation.check()?;
        let observations = bound
            .isolation
            .native_sources
            .as_ref()
            .ok_or(DiscoveryError::Refused("source_evidence_missing"))?;
        if serde_json::to_vec(observations)?.len() > MAX_DISCOVERY_BYTES {
            return Err(DiscoveryError::Refused("source_evidence_too_large"));
        }
        if observations.schema != SOURCE_EVIDENCE_SCHEMA
            || observations.inventory_digest
                != crate::Digest::of(&inventory.canonical_bytes()).to_string()
            || observations.evidence_id != bound.manifest.isolation_receipt
        {
            return Err(DiscoveryError::Refused("source_evidence_mismatch"));
        }
        bound.check_runtime_controls()?;
        if !observations.workspace_rediscovery_disabled {
            return Err(DiscoveryError::Refused("workspace_rediscovery_enabled"));
        }
        let mut by_id = BTreeMap::new();
        for observation in &observations.sources {
            if by_id
                .insert(observation.source.id.as_str(), observation)
                .is_some()
            {
                return Err(DiscoveryError::Refused("source_set_mismatch"));
            }
        }
        if by_id.len() != inventory.sources.len() {
            return Err(DiscoveryError::Refused("source_set_mismatch"));
        }
        for source in &inventory.sources {
            let observation = by_id
                .get(source.id.as_str())
                .ok_or(DiscoveryError::Refused("source_set_mismatch"))?;
            if observation.source != *source {
                return Err(DiscoveryError::Refused("source_set_mismatch"));
            }
            verify_control(source.kind, &observation.control, bound)?;
        }
        Ok(Self {
            request_digest: bound.request.digest(),
            manifest_id: bound.request.session_input_manifest_id.clone(),
            receipt_id: bound.receipt_id.clone(),
            sources: by_id.values().map(|s| (*s).clone()).collect(),
        })
    }

    /// Whether this proof belongs to this exact launch, including Session and Run.
    #[must_use]
    pub fn matches_request(&self, request: &crate::launch::LaunchRequest) -> bool {
        self.request_digest == request.digest()
    }

    /// Exact Session input manifest whose sources passed verification.
    #[must_use]
    pub fn manifest_id(&self) -> &str {
        &self.manifest_id
    }
    /// Authenticated source observation receipt identity.
    #[must_use]
    pub fn receipt_id(&self) -> &str {
        &self.receipt_id
    }
    /// Verified sources in canonical source-ID order; no input file content.
    #[must_use]
    pub fn sources(&self) -> &[SourceObservation] {
        &self.sources
    }
    /// Fixed disclosure distinguishing executable trust from managed supply.
    #[must_use]
    pub fn embedded_instructions_notice(&self) -> &'static str {
        crate::posture::EMBEDDED_INSTRUCTIONS_NOTICE
    }
}

fn verify_control(
    kind: SourceKind,
    control: &SourceControl,
    bound: &AuthenticatedInputs,
) -> Result<(), DiscoveryError> {
    let manifest = &bound.manifest;
    let snapshot = match kind {
        SourceKind::ProjectInstructions => {
            Some(json_digest(&manifest.project_instructions)?.to_string())
        }
        SourceKind::ToolSchemas => Some(json_digest(&manifest.tool_schemas)?.to_string()),
        SourceKind::PluginSchemas => Some(json_digest(&manifest.plugin_schemas)?.to_string()),
        SourceKind::ManagedSkills => Some(manifest.skill_generation.view_digest.clone()),
        _ => None,
    };
    match (snapshot, control) {
        (Some(expected), SourceControl::FrozenSnapshot { digest }) if expected == *digest => Ok(()),
        (Some(_), _) => Err(DiscoveryError::Refused("snapshot_mismatch")),
        (None, SourceControl::Masked { evidence_id })
            if *evidence_id == manifest.isolation_receipt =>
        {
            Ok(())
        }
        (None, _) => Err(DiscoveryError::Refused("native_source_unmasked")),
    }
}
