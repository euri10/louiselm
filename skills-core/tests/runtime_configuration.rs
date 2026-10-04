//! Fixed configuration is resolved explicitly, never inferred for unknown Agents.
#![allow(
    clippy::unwrap_used,
    reason = "Fixtures assert public success and refusal values."
)]

use louiselm_skills::{
    registry::{AgentRegistration, Provider},
    runtime_configuration::{self, ConfigurationError},
};
use std::collections::BTreeMap;

fn agent() -> AgentRegistration {
    AgentRegistration {
        id: "codex".into(),
        provider: Provider::Fixed("openai".into()),
        runtime_id: "runtime".into(),
        arguments: vec![],
        environment: BTreeMap::new(),
        tool_integration: Some("louiselm.codex-acp-integration/1".into()),
    }
}

#[test]
fn supported_configuration_is_stable_and_integration_specific() {
    let mut agent = agent();
    let codex = runtime_configuration::resolve(&agent).unwrap();
    assert_eq!(codex, runtime_configuration::resolve(&agent).unwrap());
    agent.tool_integration = Some("louiselm.test-tool-integration/1".into());
    assert_ne!(codex, runtime_configuration::resolve(&agent).unwrap());
}

#[test]
fn unset_unknown_or_overridden_configuration_never_defaults_to_empty_inputs() {
    for contract in [
        None,
        Some("unknown"),
        Some("louiselm.codex-acp-integration/99"),
    ] {
        let mut agent = agent();
        agent.tool_integration = contract.map(str::to_owned);
        assert_eq!(
            runtime_configuration::resolve(&agent),
            Err(ConfigurationError::UnsupportedIntegration)
        );
    }
    let mut arguments = agent();
    arguments.arguments.push("--extra-plugin".into());
    let mut environment = agent();
    environment
        .environment
        .insert("CODEX_CONFIG".into(), "private override".into());
    for agent in [arguments, environment] {
        assert_eq!(
            runtime_configuration::resolve(&agent),
            Err(ConfigurationError::RegistrationOverrides)
        );
    }
}
