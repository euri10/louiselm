//! Exact compatibility evidence for the measured Agent integrations.
//!
//! Each contract fixes its process shape in code. A registration names the
//! contract and keeps empty arguments and environment; the launcher derives the
//! exact launch values, so no registration can widen the boundary.

use super::{AgentAuthentication, SupervisorError};
#[cfg(test)]
use crate::Digest;
use crate::{
    registry::{AgentRegistration, Registry},
    release,
    sandbox::ConfinementPlan,
};
use serde::Serialize;
use std::{collections::BTreeMap, fs, os::unix::fs::MetadataExt, path::Path};

#[cfg(test)]
use crate::runtime_configuration::CODEX_BASE_URL;
pub(super) use crate::runtime_configuration::{CODEX_CONTRACT, CONTRACT};
use crate::runtime_configuration::{
    CODEX_PROVIDER, codex_configuration, codex_configuration_digest,
};
const COMPONENT: &str = "louiselm-tool-test-agent";
/// Runtime-root-relative ACP adapter script run by the measured Node executable.
pub(super) const CODEX_ADAPTER: &str = "codex-acp.js";
/// Runtime-root-relative stock Codex executable the adapter starts.
pub(super) const CODEX_RUNTIME: &str = "codex";
/// Runtime-root-relative Code Mode helper; it never gains sender authority.
pub(super) const CODEX_HOST: &str = "codex-code-mode-host";

/// Which measured integration a registration selects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Integration {
    /// The release's deterministic test Agent.
    TestTool,
    /// Node running `codex-acp.js`, which starts the stock Codex app-server.
    CodexAcp,
}

/// Stock Codex ACP chain identity bound into the evidence.
#[derive(Clone, Debug, Serialize)]
struct CodexEvidence {
    // SHA-256 digests of the adapter, runtime, helper and fixed configuration.
    adapter: String,
    runtime: String,
    code_mode_host: String,
    configuration: String,
}

/// Checkable exact integration identity, constructed only by the trusted launcher.
///
/// This proves support for one fixed integration contract. It never
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
    #[serde(skip_serializing_if = "Option::is_none")]
    codex: Option<CodexEvidence>,
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

    /// Whether the sending runtime is a descendant rather than the first exec.
    ///
    /// The Sender guard currently enrolls only the first exec-stopped process,
    /// which for this chain is the adapter, never the sending Codex runtime.
    pub(super) fn requires_descendant_enrollment(&self) -> bool {
        self.codex.is_some()
    }

    pub(super) fn measure(
        integration: Integration,
        plan: &ConfinementPlan,
        release_root: &Path,
        release_id: &str,
        backend_digest: &str,
    ) -> Result<Self, SupervisorError> {
        if !plan.arguments.is_empty() || !plan.environment.is_empty() {
            return Err(SupervisorError::ToolIsolationUnproven);
        }
        let codex = match integration {
            Integration::TestTool => {
                measure_release_component(plan, release_root, release_id)?;
                None
            }
            Integration::CodexAcp => Some(CodexEvidence {
                adapter: hash(&plan.runtime_root.join(CODEX_ADAPTER))?,
                runtime: hash(&plan.runtime_root.join(CODEX_RUNTIME))?,
                code_mode_host: hash(&plan.runtime_root.join(CODEX_HOST))?,
                configuration: codex_configuration_digest().to_string(),
            }),
        };
        let digest = hash(&plan.executable)?;
        let metadata =
            fs::metadata(&plan.executable).map_err(|_| SupervisorError::ToolIsolationUnproven)?;
        Ok(Self {
            contract: match integration {
                Integration::TestTool => CONTRACT,
                Integration::CodexAcp => CODEX_CONTRACT,
            }
            .to_owned(),
            session_id: plan.session_id.clone(),
            release_id: release_id.to_owned(),
            executable_digest: digest,
            executable_device: metadata.dev(),
            executable_inode: metadata.ino(),
            backend_digest: backend_digest.to_owned(),
            codex,
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
        let integration = validate_registration(&agent)?;
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
        let codex_matches = match (integration, &self.codex) {
            (Integration::TestTool, None) => self.contract == CONTRACT,
            (Integration::CodexAcp, Some(codex)) => {
                let adapter = |path: &str| {
                    measured
                        .adapters
                        .iter()
                        .find(|file| file.path == path)
                        .map(|file| file.sha256.as_str())
                };
                self.contract == CODEX_CONTRACT
                    && adapter(CODEX_ADAPTER) == Some(codex.adapter.as_str())
                    && adapter(CODEX_RUNTIME) == Some(codex.runtime.as_str())
                    && adapter(CODEX_HOST) == Some(codex.code_mode_host.as_str())
                    && codex.configuration == codex_configuration_digest().to_string()
            }
            _ => false,
        };
        let executable = process.executable_identity();
        if !codex_matches
            || self.session_id != request.session_id
            || self.release_id != release_id
            || self.backend_digest != backend_digest
            || self.executable_digest != measured.executable_sha256
            || (self.executable_device, self.executable_inode) != executable
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

/// One Session process observed while the whole tree is frozen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ObservedProcess {
    pub(super) pid: u32,
    pub(super) parent: u32,
    pub(super) executable_sha256: String,
}

impl ObservedProcess {
    pub(super) fn new(pid: u32, parent: u32, executable_sha256: &str) -> Self {
        Self {
            pid,
            parent,
            executable_sha256: executable_sha256.to_owned(),
        }
    }
}

/// Which measured file a process's executable identity matched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Executable {
    /// The authenticated adapter executable.
    Adapter,
    /// The measured Codex runtime file.
    Runtime,
    /// Anything else, including unreadable executables.
    Other,
}

/// Why a frozen tree is not exactly the contract's measured chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TreeRefusal {
    /// The evidence is for a contract whose sender is its first process.
    NotDescendantContract,
    /// The authenticated adapter process is not in the tree.
    AdapterMissing,
    /// The adapter process no longer runs the measured executable.
    AdapterChanged,
    /// No process runs the measured Codex runtime.
    RuntimeMissing,
    /// More than one process runs the measured Codex runtime.
    RuntimeDuplicated,
    /// The Codex runtime is not a direct child of the adapter.
    RuntimeAncestry,
    /// A process outside the contract's chain exists.
    UnexpectedProcess,
}

impl ToolIsolationEvidence {
    /// The measured digest for an executable matched by kernel identity.
    ///
    /// Matching the pinned device/inode of already-measured files avoids
    /// rehashing a large runtime at every enrollment.
    pub(super) fn digest_of(&self, executable: Executable) -> &str {
        match (executable, &self.codex) {
            (Executable::Adapter, _) => &self.executable_digest,
            (Executable::Runtime, Some(codex)) => &codex.runtime,
            _ => "unmeasured",
        }
    }

    /// Returns the one process the Sender guard may enroll for this contract.
    ///
    /// The tree must be exactly the authenticated adapter plus one direct child
    /// running the measured Codex runtime. Anything else refuses: before the
    /// first prompt no tool can legitimately exist, so an extra process is
    /// never tolerated. Threads need no listing; the kernel grant is keyed by
    /// the enrolled process's thread-group leader.
    pub(super) fn codex_sender(
        &self,
        processes: &[ObservedProcess],
        adapter_pid: u32,
    ) -> Result<u32, TreeRefusal> {
        let codex = self
            .codex
            .as_ref()
            .ok_or(TreeRefusal::NotDescendantContract)?;
        let adapter = processes
            .iter()
            .find(|process| process.pid == adapter_pid)
            .ok_or(TreeRefusal::AdapterMissing)?;
        if adapter.executable_sha256 != self.executable_digest {
            return Err(TreeRefusal::AdapterChanged);
        }
        let mut runtimes = processes
            .iter()
            .filter(|process| process.executable_sha256 == codex.runtime);
        let runtime = runtimes.next().ok_or(TreeRefusal::RuntimeMissing)?;
        if runtimes.next().is_some() {
            return Err(TreeRefusal::RuntimeDuplicated);
        }
        if runtime.parent != adapter_pid {
            return Err(TreeRefusal::RuntimeAncestry);
        }
        if processes.len() != 2 {
            return Err(TreeRefusal::UnexpectedProcess);
        }
        Ok(runtime.pid)
    }
}

/// Accepts only a known contract with empty registered arguments/environment.
pub(super) fn validate_registration(
    agent: &AgentRegistration,
) -> Result<Integration, SupervisorError> {
    let integration = match agent.tool_integration.as_deref() {
        Some(CONTRACT) => Integration::TestTool,
        Some(CODEX_CONTRACT) => Integration::CodexAcp,
        _ => return Err(SupervisorError::ToolIsolationUnproven),
    };
    if !agent.arguments.is_empty() || !agent.environment.is_empty() {
        return Err(SupervisorError::ToolIsolationUnproven);
    }
    Ok(integration)
}

/// Launcher-derived arguments and environment for one integration.
///
/// Called only after `measure`, which requires the registration's own values
/// to be empty. The Codex values are fixed by this contract: the Provider is the
/// Session's broker listener, MCP servers are empty, and no operator Codex home
/// or configuration file is read.
pub(super) fn launch_values(
    integration: Integration,
    runtime_root: &Path,
    home: &Path,
) -> (Vec<String>, BTreeMap<String, String>) {
    match integration {
        Integration::TestTool => (Vec::new(), BTreeMap::new()),
        Integration::CodexAcp => {
            let root = runtime_root.display().to_string();
            let home = home.display().to_string();
            let (config, authentication) = codex_configuration();
            let environment = BTreeMap::from([
                ("CODEX_CONFIG".to_owned(), config),
                (
                    "CODEX_PATH".to_owned(),
                    runtime_root.join(CODEX_RUNTIME).display().to_string(),
                ),
                ("DEFAULT_AUTH_REQUEST".to_owned(), authentication),
                ("HOME".to_owned(), home.clone()),
                ("INITIAL_AGENT_MODE".to_owned(), "agent".to_owned()),
                ("MODEL_PROVIDER".to_owned(), CODEX_PROVIDER.to_owned()),
                ("PATH".to_owned(), format!("{root}:/usr/bin:/bin")),
                ("USER".to_owned(), "louiselm".to_owned()),
                ("XDG_STATE_HOME".to_owned(), format!("{home}/.local/state")),
            ]);
            let adapter = runtime_root.join(CODEX_ADAPTER).display().to_string();
            (vec![adapter], environment)
        }
    }
}

fn measure_release_component(
    plan: &ConfinementPlan,
    release_root: &Path,
    release_id: &str,
) -> Result<(), SupervisorError> {
    let manifest =
        release::read_manifest(release_root).map_err(|_| SupervisorError::ToolIsolationUnproven)?;
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
    let digest = hash(&plan.executable)?;
    let metadata =
        fs::metadata(&plan.executable).map_err(|_| SupervisorError::ToolIsolationUnproven)?;
    if digest != component.sha256 || metadata.len() != component.size {
        return Err(SupervisorError::ToolIsolationUnproven);
    }
    Ok(())
}

fn hash(path: &Path) -> Result<String, SupervisorError> {
    Ok(crate::registry::measure_file(path)
        .map_err(|_| SupervisorError::ToolIsolationUnproven)?
        .hex()
        .to_owned())
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "Test fixtures assert setup and observable contract outcomes."
    )]

    use super::*;
    use crate::{
        registry::{NetworkPolicy, Provider},
        sandbox::IdentityPlan,
    };

    fn registration(contract: Option<&str>) -> AgentRegistration {
        AgentRegistration {
            id: "codex".to_owned(),
            provider: Provider::Fixed("OpenAI".to_owned()),
            runtime_id: "runtime".to_owned(),
            arguments: vec![],
            environment: BTreeMap::new(),
            tool_integration: contract.map(str::to_owned),
        }
    }

    fn codex_plan(root: &Path) -> ConfinementPlan {
        ConfinementPlan {
            session_id: "session".to_owned(),
            runtime_root: root.to_path_buf(),
            executable: root.join("node"),
            arguments: vec![],
            environment: BTreeMap::new(),
            home: root.join("home"),
            workspace: root.join("workspace"),
            cache: None,
            beads_replica: None,
            system_roots: vec![],
            identity: IdentityPlan::NamespaceOnly,
            network: NetworkPolicy::Denied,
            channels: vec![],
        }
    }

    #[test]
    fn registration_selects_only_known_contracts_without_overrides() {
        assert_eq!(
            validate_registration(&registration(Some(CONTRACT))),
            Ok(Integration::TestTool)
        );
        assert_eq!(
            validate_registration(&registration(Some(CODEX_CONTRACT))),
            Ok(Integration::CodexAcp)
        );
        for contract in [None, Some("louiselm.codex-acp-integration/2"), Some("")] {
            assert_eq!(
                validate_registration(&registration(contract)),
                Err(SupervisorError::ToolIsolationUnproven)
            );
        }
        let mut argument = registration(Some(CODEX_CONTRACT));
        argument.arguments.push("--extra".to_owned());
        let mut environment = registration(Some(CODEX_CONTRACT));
        environment
            .environment
            .insert("CODEX_CONFIG".to_owned(), "{}".to_owned());
        for agent in [argument, environment] {
            assert_eq!(
                validate_registration(&agent),
                Err(SupervisorError::ToolIsolationUnproven)
            );
        }
    }

    #[test]
    fn codex_measurement_binds_chain_and_configuration() {
        let root = tempfile::tempdir().unwrap();
        let plan = codex_plan(root.path());
        fs::write(&plan.executable, b"node").unwrap();
        fs::write(root.path().join(CODEX_ADAPTER), b"adapter").unwrap();
        let measure = |plan: &ConfinementPlan| {
            ToolIsolationEvidence::measure(Integration::CodexAcp, plan, root.path(), "r", "b")
        };
        assert!(measure(&plan).is_err(), "a missing Codex runtime refuses");
        fs::write(root.path().join(CODEX_RUNTIME), b"codex").unwrap();
        assert!(measure(&plan).is_err(), "a missing Code Mode host refuses");
        fs::write(root.path().join("codex-code-mode-host"), b"host").unwrap();
        let evidence = measure(&plan).unwrap();
        assert!(evidence.requires_descendant_enrollment());
        let value: serde_json::Value = serde_json::from_slice(&evidence.canonical_bytes()).unwrap();
        assert_eq!(value["contract"], CODEX_CONTRACT);
        assert_eq!(value["codex"]["runtime"], Digest::of(b"codex").hex());
        assert_eq!(value["codex"]["adapter"], Digest::of(b"adapter").hex());
        assert_eq!(value["codex"]["code_mode_host"], Digest::of(b"host").hex());
        assert_eq!(
            value["codex"]["configuration"],
            codex_configuration_digest().to_string()
        );

        let mut overridden = plan.clone();
        overridden
            .environment
            .insert("LD_PRELOAD".to_owned(), "plugin.so".to_owned());
        assert_eq!(
            measure(&overridden).unwrap_err(),
            SupervisorError::ToolIsolationUnproven
        );
        fs::write(root.path().join(CODEX_RUNTIME), b"updated codex").unwrap();
        let changed = measure(&plan).unwrap().canonical_bytes();
        assert_ne!(changed, evidence.canonical_bytes());
    }

    fn tree(runtime_parent: u32, extra: &[(u32, u32, &str)]) -> Vec<ObservedProcess> {
        let mut processes = vec![
            ObservedProcess::new(10, 1, "node"),
            ObservedProcess::new(11, runtime_parent, "codex"),
        ];
        processes.extend(
            extra
                .iter()
                .map(|&(pid, parent, digest)| ObservedProcess::new(pid, parent, digest)),
        );
        processes
    }

    fn evidence(root: &Path) -> ToolIsolationEvidence {
        let plan = codex_plan(root);
        fs::write(&plan.executable, b"node").unwrap();
        fs::write(root.join(CODEX_ADAPTER), b"adapter").unwrap();
        fs::write(root.join(CODEX_RUNTIME), b"codex").unwrap();
        fs::write(root.join(CODEX_HOST), b"host").unwrap();
        ToolIsolationEvidence::measure(Integration::CodexAcp, &plan, root, "r", "b").unwrap()
    }

    #[test]
    fn codex_sender_is_the_one_measured_runtime_child_of_the_adapter() {
        let root = tempfile::tempdir().unwrap();
        let evidence = evidence(root.path());
        let node = Digest::of(b"node").hex().to_owned();
        let codex = Digest::of(b"codex").hex().to_owned();
        let digests = |processes: Vec<ObservedProcess>| {
            processes
                .into_iter()
                .map(|process| {
                    let digest = match process.executable_sha256.as_str() {
                        "node" => node.clone(),
                        "codex" => codex.clone(),
                        other => other.to_owned(),
                    };
                    ObservedProcess::new(process.pid, process.parent, &digest)
                })
                .collect::<Vec<_>>()
        };
        let sender = |processes| evidence.codex_sender(&digests(processes), 10);
        assert_eq!(sender(tree(10, &[])), Ok(11));
        assert_eq!(
            sender(vec![ObservedProcess::new(10, 1, "node")]),
            Err(TreeRefusal::RuntimeMissing)
        );
        assert_eq!(
            sender(tree(10, &[(12, 10, "codex")])),
            Err(TreeRefusal::RuntimeDuplicated)
        );
        assert_eq!(
            sender(tree(10, &[(12, 11, "codex")])),
            Err(TreeRefusal::RuntimeDuplicated)
        );
        assert_eq!(sender(tree(99, &[])), Err(TreeRefusal::RuntimeAncestry));
        assert_eq!(
            sender(tree(10, &[(12, 11, "tool")])),
            Err(TreeRefusal::UnexpectedProcess)
        );
        let mut changed = tree(10, &[]);
        changed[0] = ObservedProcess::new(10, 1, "patched node");
        assert_eq!(sender(changed), Err(TreeRefusal::AdapterChanged));
        assert_eq!(
            evidence.codex_sender(&digests(tree(10, &[])), 42),
            Err(TreeRefusal::AdapterMissing)
        );
    }

    #[test]
    fn test_contract_has_no_descendant_sender() {
        let root = tempfile::tempdir().unwrap();
        let mut evidence = evidence(root.path());
        evidence.codex = None;
        assert_eq!(
            evidence.codex_sender(&tree(10, &[]), 10),
            Err(TreeRefusal::NotDescendantContract)
        );
    }

    #[test]
    fn codex_launch_values_are_fixed_by_the_contract() {
        let root = Path::new("/runtime");
        let home = Path::new("/session/home");
        let (arguments, environment) = launch_values(Integration::CodexAcp, root, home);
        assert_eq!(arguments, ["/runtime/codex-acp.js"]);
        assert_eq!(environment["CODEX_PATH"], "/runtime/codex");
        assert_eq!(environment["HOME"], "/session/home");
        assert_eq!(environment["PATH"], "/runtime:/usr/bin:/bin");
        let config: serde_json::Value = serde_json::from_str(&environment["CODEX_CONFIG"]).unwrap();
        assert_eq!(config["model"], "gpt-5.6-luna");
        assert_eq!(config["mcp_servers"], serde_json::json!({}));
        assert_eq!(config["features"]["code_mode_host"], true);
        assert_eq!(
            config["model_providers"][CODEX_PROVIDER]["base_url"],
            CODEX_BASE_URL
        );
        let authentication: serde_json::Value =
            serde_json::from_str(&environment["DEFAULT_AUTH_REQUEST"]).unwrap();
        assert_eq!(
            authentication["_meta"]["gateway"]["headers"],
            serde_json::json!({})
        );
        assert!(
            environment
                .keys()
                .all(|key| !key.contains("KEY") && !key.contains("TOKEN")),
            "no credential enters the Session environment"
        );
        assert_eq!(
            launch_values(Integration::CodexAcp, root, home),
            (arguments, environment)
        );
        assert_eq!(
            launch_values(Integration::TestTool, root, home),
            (vec![], BTreeMap::new())
        );
    }
}
