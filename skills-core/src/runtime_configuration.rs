//! Session-independent configuration shared by trusted preparation and launch.
//!
//! Built-in tools belong to the measured runtime plus this configuration. Both
//! supported integrations explicitly supply no added tool/plugin schemas or ACP
//! MCP servers. This describes intended configuration, not native-discovery proof.

use crate::{Digest, registry::AgentRegistration};
use thiserror::Error;

pub(crate) const CONTRACT: &str = "louiselm.test-tool-integration/1";
pub(crate) const CODEX_CONTRACT: &str = "louiselm.codex-acp-integration/1";
pub(crate) const CODEX_BASE_URL: &str = "http://127.0.0.1:40773/v1";
pub(crate) const CODEX_PROVIDER: &str = "louiselm-broker";

/// A registration has no supported, fixed configuration producer.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ConfigurationError {
    /// No known integration was selected.
    #[error("runtime configuration requires a supported Agent integration")]
    UnsupportedIntegration,
    /// Registered overrides could widen the integration's fixed inputs.
    #[error("fixed runtime configuration requires empty registered arguments and environment")]
    RegistrationOverrides,
}

/// Resolves the exact installed configuration for a known integration.
///
/// Pure and payload-free. Success explicitly selects no added tool/plugin
/// schemas or ACP MCP servers; it never guesses empty inputs for an unknown
/// integration. Session paths are derived by the launcher, not caller settings.
/// This digest grants no authority and does not prove native-source masking.
///
/// # Errors
/// Refuses unsupported integrations and any registered argument/environment override.
pub fn resolve(agent: &AgentRegistration) -> Result<Digest, ConfigurationError> {
    if !agent.arguments.is_empty() || !agent.environment.is_empty() {
        return Err(ConfigurationError::RegistrationOverrides);
    }
    match agent.tool_integration.as_deref() {
        Some(CONTRACT) => Ok(Digest::of(CONTRACT.as_bytes())),
        Some(CODEX_CONTRACT) => Ok(codex_configuration_digest()),
        _ => Err(ConfigurationError::UnsupportedIntegration),
    }
}

/// Exact configuration and credential-free gateway request used at launch.
pub(crate) fn codex_configuration() -> (String, String) {
    let config = serde_json::json!({
        "features": {
            "code_mode_host": true,
            "apps": false,
            "plugins": false,
            "remote_plugin": false,
        },
        "mcp_servers": {},
        "model": "gpt-5.6-luna",
        "model_provider": CODEX_PROVIDER,
        "model_reasoning_effort": "low",
        "model_providers": {
            CODEX_PROVIDER: {
                "base_url": CODEX_BASE_URL,
                "name": CODEX_PROVIDER,
                "request_max_retries": 0,
                "requires_openai_auth": false,
                "stream_max_retries": 0,
                "wire_api": "responses",
            },
        },
    });
    let authentication = serde_json::json!({
        "_meta": {
            "gateway": {
                "baseUrl": CODEX_BASE_URL,
                "headers": {},
                "providerName": CODEX_PROVIDER,
            },
        },
        "methodId": "gateway",
    });
    (config.to_string(), authentication.to_string())
}

pub(crate) fn codex_configuration_digest() -> Digest {
    let (config, authentication) = codex_configuration();
    Digest::of(format!("{config}\n{authentication}").as_bytes())
}
