//! Measured inactive QA-runtime configuration; never signs or registers it.

use super::WorkspaceError;
use crate::{
    Digest,
    discovery::{INVENTORY_PATH, INVENTORY_SCHEMA, Inventory},
    discovery_source::{Source, SourceKind, SourceRoot},
    registry::{
        AgentRegistration, EnvelopeRegistration, MeasuredFile, NetworkPolicy, Provider,
        RuntimePackage,
    },
    release::{self, ReleaseManifest},
};
use serde::Serialize;
use std::path::Path;
use std::{collections::BTreeMap, fs, os::unix::fs::PermissionsExt, path::PathBuf};

const EXECUTABLE: &str = "louiselm-tool-test-agent";

#[derive(Serialize)]
pub(crate) struct Proposal {
    schema: &'static str,
    state: &'static str,
    pub(crate) release_id: String,
    source_commit: String,
    agent: AgentRegistration,
    pub(crate) runtime: RuntimePackage,
    envelope: EnvelopeRegistration,
    inventory_digest: String,
}

fn invalid(message: &'static str) -> WorkspaceError {
    WorkspaceError::Invalid(message)
}

pub(crate) fn prepare(bundle: &Path, output: &Path) -> Result<Proposal, WorkspaceError> {
    let bundle = fs::canonicalize(bundle).map_err(|_| invalid("QA release bundle unavailable"))?;
    let root = crate::workspace::filesystem::open_directory(&bundle)
        .map_err(|_| invalid("QA bundle unavailable"))?;
    let manifest_bytes =
        crate::workspace::filesystem::read_source(&root, "manifest.json", 1024 * 1024)
            .map_err(|_| invalid("QA release manifest unavailable"))?
            .ok_or_else(|| invalid("QA release manifest missing"))?
            .bytes;
    let manifest: ReleaseManifest = serde_json::from_slice(&manifest_bytes)?;
    if manifest.schema != release::MANIFEST_SCHEMA
        || !manifest.source.clean
        || manifest.digest().to_string() != manifest.release_id
    {
        return Err(invalid("QA needs a content-bound clean release proposal"));
    }
    let component = manifest
        .component(EXECUTABLE)
        .ok_or_else(|| invalid("QA Agent is not a release component"))?;
    if component.path != format!("bin/{EXECUTABLE}") || !component.executable {
        return Err(invalid("QA Agent component is not the fixed executable"));
    }
    let executable =
        crate::workspace::filesystem::read_source(&root, &component.path, 128 * 1024 * 1024)
            .map_err(|_| invalid("QA Agent bytes unavailable"))?
            .ok_or_else(|| invalid("QA Agent bytes missing"))?;
    if !executable.executable
        || Digest::of(&executable.bytes).hex() != component.sha256
        || executable.bytes.len() as u64 != component.size
    {
        return Err(invalid("QA Agent bytes changed"));
    }
    let identity = format!("qa-{}", &manifest.digest().hex()[..24]);
    let inventory = inventory(&identity, &component.sha256);
    let inventory_bytes = inventory.canonical_bytes();
    Inventory::parse(&inventory_bytes).map_err(|_| invalid("QA inventory invalid"))?;
    let proposal = Proposal {
        schema: "louiselm.qa.runtime-proposal/1",
        state: "proposed",
        release_id: manifest.release_id.clone(),
        source_commit: manifest.source.commit.clone(),
        agent: AgentRegistration {
            id: identity.clone(),
            provider: Provider::Fixed("offline-test".into()),
            runtime_id: identity.clone(),
            arguments: vec![],
            environment: BTreeMap::new(),
            tool_integration: Some(crate::runtime_configuration::CONTRACT.into()),
        },
        runtime: RuntimePackage {
            id: identity,
            root: PathBuf::from("/var/lib/louiselm/qa-runtimes").join(manifest.digest().hex()),
            executable: EXECUTABLE.into(),
            executable_sha256: component.sha256.clone(),
            adapters: vec![MeasuredFile {
                path: INVENTORY_PATH.into(),
                sha256: Digest::of(&inventory_bytes).hex().into(),
            }],
            version: manifest.version.clone(),
            origin: format!("release:{}", manifest.release_id),
        },
        envelope: EnvelopeRegistration {
            id: "desktop-qa-denied".into(),
            network: NetworkPolicy::Denied,
            description: "Explicit operator-only offline desktop conformance QA".into(),
        },
        inventory_digest: Digest::of(&inventory_bytes).to_string(),
    };
    crate::workspace::filesystem::validate_output(output, &bundle)
        .map_err(|_| invalid("QA output must be fresh and outside its bundle"))?;
    crate::workspace::filesystem::publish(output, |staging| {
        let runtime = staging.join("runtime");
        fs::create_dir_all(runtime.join("builtin/skills"))?;
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700))?;
        crate::workspace::filesystem::write_file(
            &runtime.join(EXECUTABLE),
            &executable.bytes,
            0o500,
        )?;
        crate::workspace::filesystem::write_file(
            &runtime.join(INVENTORY_PATH),
            &inventory_bytes,
            0o400,
        )?;
        crate::workspace::filesystem::write_file(
            &staging.join("proposal.json"),
            &serde_json::to_vec(&proposal)?,
            0o400,
        )
    })
    .map_err(|_| invalid("QA proposal publication refused"))?;
    Ok(proposal)
}

fn inventory(runtime_id: &str, executable_sha256: &str) -> Inventory {
    Inventory {
        schema: INVENTORY_SCHEMA.into(),
        adapter_id: "offline-test-agent".into(),
        version: "1".into(),
        runtime_id: runtime_id.into(),
        executable_sha256: executable_sha256.into(),
        sources: SourceKind::ALL
            .into_iter()
            .map(|kind| {
                let (namespace, path) = match kind {
                    SourceKind::ManagedSkills => (SourceRoot::Home, ".managed/skills".into()),
                    SourceKind::NativeSkills => (SourceRoot::Home, ".agents/skills".into()),
                    SourceKind::RepositorySkills => {
                        (SourceRoot::Workspace, ".agents/skills".into())
                    }
                    SourceKind::SystemSkills => (SourceRoot::Runtime, "builtin/skills".into()),
                    SourceKind::ProjectInstructions => (SourceRoot::Workspace, "AGENTS.md".into()),
                    _ => (SourceRoot::Home, format!("other/{}", kind.name())),
                };
                Source {
                    id: kind.name().into(),
                    kind,
                    root: namespace,
                    path,
                }
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Policy,
        release::{
            AssembleRequest, ComponentInput, ComponentKind, SourceIdentity, ToolchainIdentity,
        },
    };
    #[allow(
        clippy::unwrap_used,
        reason = "Offline byte-owner fixture asserts exact refusals."
    )]
    fn bundle(root: &Path) -> PathBuf {
        let executable = root.join(EXECUTABLE);
        fs::write(&executable, b"measured deterministic fixture bytes").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let bundle = root.join("bundle");
        release::assemble(
            &AssembleRequest {
                source: SourceIdentity {
                    commit: "a".repeat(40),
                    clean: true,
                    describe: "fixture".into(),
                    dependencies_digest: Digest::of(b"lock").to_string(),
                },
                toolchain: ToolchainIdentity {
                    rustc: "fixture".into(),
                    cargo: "fixture".into(),
                    target: "fixture".into(),
                },
                policy: &Policy::embedded(),
                built_at_ms: 1,
                components: vec![ComponentInput {
                    name: EXECUTABLE.into(),
                    path: executable,
                    kind: ComponentKind::Executable,
                }],
            },
            &bundle,
        )
        .unwrap();
        bundle
    }

    #[test]
    #[allow(
        clippy::unwrap_used,
        reason = "Offline byte-owner fixture asserts exact measurements."
    )]
    fn inactive_proposal_binds_actual_bytes_inventory_and_fixed_denied_configuration() {
        let root = tempfile::tempdir().unwrap();
        let bundle = bundle(root.path());
        let output = root.path().join("proposal");
        let proposal = prepare(&bundle, &output).unwrap();
        assert_eq!(proposal.state, "proposed");
        assert_eq!(proposal.envelope.network, NetworkPolicy::Denied);
        assert!(proposal.agent.arguments.is_empty() && proposal.agent.environment.is_empty());
        assert_eq!(
            proposal.agent.tool_integration.as_deref(),
            Some(crate::runtime_configuration::CONTRACT)
        );
        assert_eq!(
            fs::read(output.join("runtime").join(EXECUTABLE)).unwrap(),
            b"measured deterministic fixture bytes"
        );
        let inventory = fs::read(output.join("runtime").join(INVENTORY_PATH)).unwrap();
        let parsed = Inventory::parse(&inventory).unwrap();
        assert_eq!(parsed.runtime_id, proposal.runtime.id);
        assert_eq!(parsed.executable_sha256, proposal.runtime.executable_sha256);
        assert_eq!(
            Digest::of(&inventory).to_string(),
            proposal.inventory_digest
        );
        assert!(output.join("runtime/builtin/skills").is_dir());
        assert!(!output.join("manifest.sig").exists());
        assert!(!output.join("agents.json").exists());
        assert!(prepare(&bundle, &output).is_err());
    }

    #[test]
    #[allow(
        clippy::unwrap_used,
        reason = "Offline hostile byte-owner fixture asserts fail-closed output."
    )]
    fn changed_linked_or_unbound_release_refuses_before_publication() {
        for case in 0..3 {
            let root = tempfile::tempdir().unwrap();
            let bundle = bundle(root.path());
            let executable = bundle.join("bin").join(EXECUTABLE);
            match case {
                0 => {
                    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
                    fs::write(&executable, b"substituted").unwrap();
                }
                1 => {
                    fs::remove_file(&executable).unwrap();
                    std::os::unix::fs::symlink(root.path().join(EXECUTABLE), &executable).unwrap();
                }
                _ => {
                    let mut manifest = release::read_manifest(&bundle).unwrap();
                    manifest.release_id = Digest::of(b"unbound").to_string();
                    fs::set_permissions(
                        bundle.join("manifest.json"),
                        fs::Permissions::from_mode(0o600),
                    )
                    .unwrap();
                    fs::write(
                        bundle.join("manifest.json"),
                        serde_json::to_vec(&manifest).unwrap(),
                    )
                    .unwrap();
                }
            }
            let output = root.path().join("refused");
            assert!(prepare(&bundle, &output).is_err());
            assert!(!output.exists());
        }
    }
}
