//! Resolve identities from the existing registry and locked view producer.

use thiserror::Error;

use crate::{
    Policy, Store,
    instruction_view::{self, ViewError},
    posture::{DimensionName, FailureCode},
    registry::{Registry, RegistryError},
};

use super::{SessionInputs, SkillInputs, SkillsMode};

/// Trusted component resolution failed; original causes are retained internally.
#[derive(Debug, Error)]
pub enum InputResolutionError {
    /// Agent lookup or runtime measurement failed.
    #[error("runtime: registry resolution failed")]
    Runtime(#[from] RegistryError),
    /// Current witnessed Generation or materialized view failed verification.
    #[error("managed_supply: Generation or view resolution failed")]
    Supply(#[from] ViewError),
}

impl InputResolutionError {
    /// Independent dimension that failed.
    #[must_use]
    pub fn dimension(&self) -> DimensionName {
        match self {
            Self::Runtime(_) => DimensionName::Runtime,
            Self::Supply(_) => DimensionName::ManagedSupply,
        }
    }

    /// Stable code without source paths or runtime content.
    #[must_use]
    pub fn failure_code(&self) -> FailureCode {
        match self {
            Self::Runtime(RegistryError::RuntimeChanged { .. }) => FailureCode::RuntimeDrift,
            Self::Runtime(_) => FailureCode::EvidenceMissing,
            Self::Supply(_) => FailureCode::RootTrustFailed,
        }
    }
}

impl SessionInputs {
    /// Resolves the registered Agent, remeasures its runtime, and materializes
    /// the explicitly selected skills mode. Blocking filesystem/crypto I/O.
    ///
    /// Enabled mode requires current signed/witnessed supply; off mode uses only
    /// the canonical empty view, with no Generation or trust enrollment.
    /// The enabled Generation binding comes from the same locked operation as the view;
    /// no second current-Generation read can race activation. Snapshot, envelope,
    /// MCP and isolation fields remain `None` until their owners supply them.
    /// Call immediately before binding; this is not a currency or launch lease.
    ///
    /// # Errors
    /// Refuses missing registrations, changed/unmeasurable runtimes, unresolved
    /// or invalid supply, and failed view publication/verification.
    pub fn resolve(
        store: &Store,
        policy: &Policy,
        registry: &Registry,
        agent_id: &str,
        mode: SkillsMode,
    ) -> Result<Self, InputResolutionError> {
        let agent = registry.agent(agent_id)?;
        let runtime = registry.runtime(&agent.runtime_id)?.measure()?;
        let skills = match mode {
            SkillsMode::Off => SkillInputs::Off {
                view_digest: instruction_view::empty(store)?.digest().to_string(),
            },
            SkillsMode::Enabled => {
                let mut views = instruction_view::materialize(store, policy, registry)?;
                let view = views
                    .remove(agent_id)
                    .ok_or(ViewError::Refused("unresolved_agent_view"))?;
                SkillInputs::Enabled {
                    generation_digest: view
                        .generation()
                        .ok_or(ViewError::Refused("unresolved_generation"))?
                        .to_owned(),
                    view_digest: view.digest().to_string(),
                }
            }
        };
        Ok(Self {
            agent: Some(agent),
            runtime: Some(runtime),
            runtime_configuration_digest: None,
            skills: Some(skills),
            policy_digest: Some(policy.digest().to_string()),
            project_instructions: None,
            tool_schemas: None,
            plugin_schemas: None,
            source_snapshot_digest: None,
            source_base_digest: None,
            cache_base_digest: None,
            acp_mcp_servers: None,
            isolation: None,
            envelope_id: None,
            envelope_revision: None,
        })
    }
}
