//! Resolve identities from the existing registry and locked view producer.

use thiserror::Error;

use crate::{
    Policy, Store,
    instruction_view::{self, ViewError},
    posture::{DimensionName, FailureCode},
    registry::{Registry, RegistryError},
};

use super::SessionInputs;

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
    /// its view from current witnessed supply. Blocking filesystem/crypto I/O.
    ///
    /// The Generation binding comes from the same locked operation as the view;
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
    ) -> Result<Self, InputResolutionError> {
        let agent = registry.agent(agent_id)?;
        let runtime = registry.runtime(&agent.runtime_id)?.measure()?;
        let mut views = instruction_view::materialize(store, policy, registry)?;
        let view = views
            .remove(agent_id)
            .ok_or(ViewError::Refused("unresolved_agent_view"))?;
        let generation = view
            .generation()
            .ok_or(ViewError::Refused("unresolved_generation"))?;
        Ok(Self {
            agent: Some(agent),
            runtime: Some(runtime),
            skill_generation_id: Some(generation.to_owned()),
            view_digest: Some(view.digest().to_string()),
            policy_digest: Some(policy.digest().to_string()),
            project_instructions: None,
            tool_schemas: None,
            plugin_schemas: None,
            cache_base_digest: None,
            acp_mcp_servers: None,
            isolation_receipt: None,
            envelope_id: None,
            envelope_revision: None,
        })
    }
}
