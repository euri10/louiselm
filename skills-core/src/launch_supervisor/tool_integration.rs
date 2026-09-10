//! Exact compatibility evidence for the deterministic integration only.

use super::{AgentAuthentication, SupervisorError};
use crate::{
    registry::{AgentRegistration, Registry},
    release,
    sandbox::ConfinementPlan,
};
use serde::Serialize;
use std::{fs, os::unix::fs::MetadataExt, path::Path};

pub(super) const CONTRACT: &str = "louiselm.test-tool-integration/1";
const COMPONENT: &str = "louiselm-tool-test-agent";

/// Checkable exact integration identity, constructed only by the trusted launcher.
///
/// This proves support for the release's deterministic test Agent. It never
/// establishes compatibility of another executable, adapter or vendor Agent.
#[derive(Clone, Debug, Serialize)]
pub struct ToolIsolationEvidence {
    contract: String,
    session_id: String,
    release_id: String,
    executable_digest: String,
    executable_device: u64,
    executable_inode: u64,
    backend_digest: String,
}

impl ToolIsolationEvidence {
    /// Deterministic compatibility evidence, containing no command or tool output.
    ///
    /// # Panics
    /// Only a future fallible custom serializer could make this derived schema fail.
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "Derived JSON-native fields have no custom serializer."
    )]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("integration evidence is serializable")
    }

    pub(super) fn measure(
        plan: &ConfinementPlan,
        release_root: &Path,
        release_id: &str,
        backend_digest: &str,
    ) -> Result<Self, SupervisorError> {
        if !plan.arguments.is_empty() || !plan.environment.is_empty() {
            return Err(SupervisorError::ToolIsolationUnproven);
        }
        let manifest = release::read_manifest(release_root)
            .map_err(|_| SupervisorError::ToolIsolationUnproven)?;
        let component = manifest
            .component(COMPONENT)
            .ok_or(SupervisorError::ToolIsolationUnproven)?;
        if manifest.release_id != release_id
            || manifest.digest().to_string() != release_id
            || component.path != format!("bin/{COMPONENT}")
            || !component.executable
        {
            return Err(SupervisorError::ToolIsolationUnproven);
        }
        let digest = crate::registry::measure_file(&plan.executable)
            .map_err(|_| SupervisorError::ToolIsolationUnproven)?
            .hex()
            .to_owned();
        let metadata =
            fs::metadata(&plan.executable).map_err(|_| SupervisorError::ToolIsolationUnproven)?;
        if digest != component.sha256 || metadata.len() != component.size {
            return Err(SupervisorError::ToolIsolationUnproven);
        }
        Ok(Self {
            contract: CONTRACT.to_owned(),
            session_id: plan.session_id.clone(),
            release_id: release_id.to_owned(),
            executable_digest: digest,
            executable_device: metadata.dev(),
            executable_inode: metadata.ino(),
            backend_digest: backend_digest.to_owned(),
        })
    }

    pub(super) fn verify(
        &self,
        request: &crate::launch::LaunchRequest,
        authentication: &AgentAuthentication,
        registry: &Registry,
        release_id: &str,
        backend_digest: &str,
    ) -> Result<(), SupervisorError> {
        let agent = registry
            .agent(&request.agent_id)
            .map_err(|_| SupervisorError::ToolIsolationUnproven)?;
        validate_registration(&agent)?;
        let runtime = registry
            .runtime(&agent.runtime_id)
            .map_err(|_| SupervisorError::ToolIsolationUnproven)?;
        let measured = runtime
            .measure()
            .map_err(|_| SupervisorError::ToolIsolationUnproven)?;
        let process = authentication
            .process
            .as_ref()
            .ok_or(SupervisorError::AgentIdentityRejected)?;
        if !process
            .valid()
            .map_err(|_| SupervisorError::AgentIdentityRejected)?
        {
            return Err(SupervisorError::AgentIdentityRejected);
        }
        let executable = fs::metadata(format!("/proc/{}/exe", process.credentials().pid))
            .map_err(|_| SupervisorError::AgentIdentityRejected)?;
        if self.contract != CONTRACT
            || self.session_id != request.session_id
            || self.release_id != release_id
            || self.backend_digest != backend_digest
            || self.executable_digest != measured.executable_sha256
            || self.executable_device != executable.dev()
            || self.executable_inode != executable.ino()
            || authentication.credentials != process.credentials()
            || !process
                .valid()
                .map_err(|_| SupervisorError::AgentIdentityRejected)?
        {
            return Err(SupervisorError::ToolIsolationUnproven);
        }
        Ok(())
    }
}

fn validate_registration(agent: &AgentRegistration) -> Result<(), SupervisorError> {
    if agent.tool_integration.as_deref() != Some(CONTRACT)
        || !agent.arguments.is_empty()
        || !agent.environment.is_empty()
    {
        return Err(SupervisorError::ToolIsolationUnproven);
    }
    Ok(())
}
