//! Signed-launch fixtures; observations model a trusted backend, not kernel proof.

use crate::support::{Fixture, SshKey, write_file};
use louiselm_skills::{
    Digest,
    discovery::{
        AuthenticatedInputs, DiscoveryError, INVENTORY_PATH, INVENTORY_SCHEMA, Inventory,
        SOURCE_EVIDENCE_SCHEMA,
    },
    discovery_source::{
        Source, SourceControl, SourceEvidence, SourceKind, SourceObservation, SourceRoot,
    },
    isolation::{
        CONTRACT_VERSION, Dimension, DimensionEvidence, IsolationEvidence, KernelPrerequisites,
    },
    launch::{LaunchRequest, PROTOCOL_VERSION, REQUEST_SCHEMA},
    launch_receipt::{
        Authorization, ChainAnchor, LaunchEvidence, RECEIPT_SCHEMA, ReceiptOutcome, ReceiptPayload,
        SIGNED_RECEIPT_SCHEMA, SessionState, SignedReceipt,
    },
    registry::{AgentRegistration, MeasuredFile, Provider, RuntimePackage},
    session_manifest::{MeasuredInput, SessionInputManifest, SessionInputs},
    sshsig::{self, SkPolicy},
};
use std::collections::BTreeMap;

pub struct DiscoveryFixture {
    pub fixture: Fixture,
    pub runtime: RuntimePackage,
    pub manifest: SessionInputManifest,
    pub isolation: IsolationEvidence,
    pub request: LaunchRequest,
    pub receipt: SignedReceipt,
    pub anchor: ChainAnchor,
    key: SshKey,
}

impl DiscoveryFixture {
    pub fn new() -> Self {
        let fixture = Fixture::new();
        let key = SshKey::generate(&fixture, "launcher");
        let (runtime, inventory) = runtime_fixture(&fixture);
        let manifest = manifest_fixture(&runtime);
        let isolation = isolation_fixture(&inventory, &manifest);
        let request = LaunchRequest {
            schema: REQUEST_SCHEMA.into(),
            protocol_version: PROTOCOL_VERSION,
            request_id: "request-1".into(),
            authorization_id: "authorization-1".into(),
            session_id: "session-1".into(),
            run_id: "run-1".into(),
            agent_id: manifest.agent.id.clone(),
            envelope_id: manifest.envelope.id.clone(),
            envelope_revision: 1,
            skill_generation_id: manifest.skill_generation.generation_digest.clone(),
            session_input_manifest_id: manifest.digest().to_string(),
        };
        let anchor = ChainAnchor {
            session_id: request.session_id.clone(),
            run_id: request.run_id.clone(),
            release_id: digest(b"release"),
            signing_key_id: digest(b"launcher-key"),
        };
        let receipt = signed_record(&request, &manifest, &isolation, &anchor, &key);
        Self {
            fixture,
            runtime,
            manifest,
            isolation,
            request,
            receipt,
            anchor,
            key,
        }
    }

    pub fn sign(&mut self) {
        self.request.session_input_manifest_id = self.manifest.digest().to_string();
        self.request.skill_generation_id = self.manifest.skill_generation.generation_digest.clone();
        self.receipt = signed_record(
            &self.request,
            &self.manifest,
            &self.isolation,
            &self.anchor,
            &self.key,
        );
    }

    pub fn authenticate(&self) -> Result<AuthenticatedInputs, DiscoveryError> {
        AuthenticatedInputs::verify(
            &self.request,
            &self.manifest,
            &self.isolation,
            &self.receipt,
            &self.anchor,
            |_, bytes, signature| {
                sshsig::verify(
                    signature,
                    RECEIPT_SCHEMA,
                    bytes,
                    &self.key.public_key(),
                    SkPolicy::none(),
                )
                .is_ok()
            },
        )
    }

    pub fn control(&mut self, kind: SourceKind, control: SourceControl) {
        self.isolation
            .native_sources
            .as_mut()
            .unwrap()
            .sources
            .iter_mut()
            .find(|s| s.source.kind == kind)
            .unwrap()
            .control = control;
    }
}

fn runtime_fixture(fixture: &Fixture) -> (RuntimePackage, Inventory) {
    let root = fixture.path("runtime");
    write_file(&root.join("bin/agent"), "frozen discovery adapter fixture");
    let executable_sha256 = louiselm_skills::registry::measure_file(&root.join("bin/agent"))
        .unwrap()
        .hex()
        .to_owned();
    let inventory = Inventory {
        schema: INVENTORY_SCHEMA.into(),
        adapter_id: "fixture-adapter".into(),
        version: "1".into(),
        runtime_id: "fixture-runtime".into(),
        executable_sha256: executable_sha256.clone(),
        sources: SourceKind::ALL
            .into_iter()
            .map(|kind| Source {
                id: kind.name().into(),
                kind,
                root: if kind == SourceKind::ProjectInstructions {
                    SourceRoot::Workspace
                } else {
                    SourceRoot::Home
                },
                path: format!("sources/{}", kind.name()),
            })
            .collect(),
    };
    let inventory_bytes = inventory.canonical_bytes();
    std::fs::write(root.join(INVENTORY_PATH), &inventory_bytes).unwrap();
    let runtime = RuntimePackage {
        id: "fixture-runtime".into(),
        root,
        executable: "bin/agent".into(),
        executable_sha256,
        adapters: vec![MeasuredFile {
            path: INVENTORY_PATH.into(),
            sha256: Digest::of(&inventory_bytes).hex().to_owned(),
        }],
        version: "1".into(),
        origin: "test fixture".into(),
        library_baseline: vec![],
        isolation_policy_version: CONTRACT_VERSION.into(),
    };
    (runtime, inventory)
}

fn manifest_fixture(runtime: &RuntimePackage) -> SessionInputManifest {
    SessionInputManifest::build(SessionInputs {
        agent: Some(AgentRegistration {
            id: "fixture-agent".into(),
            provider: Provider::Fixed("fixture-provider".into()),
            runtime_id: runtime.id.clone(),
            arguments: vec![],
            environment: BTreeMap::new(),
            tool_integration: None,
        }),
        runtime: Some(runtime.measure().unwrap()),
        skill_generation_id: Some(digest(b"generation")),
        view_digest: Some(digest(b"view")),
        project_instructions: Some(vec![
            MeasuredInput::from_bytes("AGENTS.md", false, b"initial instructions").unwrap(),
        ]),
        tool_schemas: Some(vec![]),
        plugin_schemas: Some(vec![]),
        policy_digest: Some(digest(b"policy")),
        isolation_receipt: Some("isolation-1".into()),
        envelope_id: Some("envelope-1".into()),
        envelope_revision: Some(1),
        acp_mcp_servers: Some(vec![]),
    })
    .unwrap()
}

fn isolation_fixture(inventory: &Inventory, manifest: &SessionInputManifest) -> IsolationEvidence {
    let sources = inventory
        .sources
        .iter()
        .map(|source| SourceObservation {
            source: source.clone(),
            control: match source.kind {
                SourceKind::ProjectInstructions => SourceControl::FrozenSnapshot {
                    digest: digest(&serde_json::to_vec(&manifest.project_instructions).unwrap()),
                },
                SourceKind::ToolSchemas => SourceControl::FrozenSnapshot {
                    digest: digest(&serde_json::to_vec(&manifest.tool_schemas).unwrap()),
                },
                SourceKind::PluginSchemas => SourceControl::FrozenSnapshot {
                    digest: digest(&serde_json::to_vec(&manifest.plugin_schemas).unwrap()),
                },
                SourceKind::ManagedSkills => SourceControl::FrozenSnapshot {
                    digest: manifest.skill_generation.view_digest.clone(),
                },
                _ => SourceControl::Masked {
                    evidence_id: manifest.isolation_receipt.clone(),
                },
            },
        })
        .collect();
    IsolationEvidence {
        contract_version: CONTRACT_VERSION.into(),
        backend: "fixture-backend".into(),
        backend_version: "1".into(),
        kernel: KernelPrerequisites {
            user_namespaces: true,
            pid_namespaces: true,
            network_namespaces: true,
            cgroup_v2: true,
            details: vec![],
        },
        dimensions: Dimension::ALL
            .into_iter()
            .map(|dimension| DimensionEvidence {
                dimension,
                satisfied: true,
                mechanism: "fixture".into(),
                detail: "trusted fixture observation, not host conformance".into(),
            })
            .collect(),
        native_sources: Some(SourceEvidence {
            schema: SOURCE_EVIDENCE_SCHEMA.into(),
            inventory_digest: digest(&inventory.canonical_bytes()),
            evidence_id: "isolation-1".into(),
            fixed_executable: true,
            self_update_disabled: true,
            workspace_rediscovery_disabled: true,
            sources,
        }),
    }
}

fn digest(bytes: &[u8]) -> String {
    Digest::of(bytes).to_string()
}

fn signed_record(
    request: &LaunchRequest,
    manifest: &SessionInputManifest,
    isolation: &IsolationEvidence,
    anchor: &ChainAnchor,
    key: &SshKey,
) -> SignedReceipt {
    let payload = ReceiptPayload {
        schema: RECEIPT_SCHEMA.into(),
        session_id: anchor.session_id.clone(),
        run_id: anchor.run_id.clone(),
        request_id: request.request_id.clone(),
        envelope_revision: request.envelope_revision,
        sequence: 0,
        previous_receipt_digest: None,
        release_id: anchor.release_id.clone(),
        signing_key_id: anchor.signing_key_id.clone(),
        outcome: ReceiptOutcome::Launch {
            authorization: Authorization {
                authorization_id: request.authorization_id.clone(),
                request_id: request.request_id.clone(),
                request_digest: request.digest().to_string(),
            },
            evidence: Box::new(LaunchEvidence {
                launch_request_digest: request.digest().to_string(),
                runtime_measurement_digest: digest(&serde_json::to_vec(&manifest.runtime).unwrap()),
                skill_generation_id: request.skill_generation_id.clone(),
                session_input_manifest_id: request.session_input_manifest_id.clone(),
                isolation_contract: CONTRACT_VERSION.into(),
                isolation_backend_id: "fixture-backend".into(),
                kernel_identity: "fixture-kernel".into(),
                isolation_evidence_digest: digest(&serde_json::to_vec(isolation).unwrap()),
                broker_loss_grace_ms: 0,
                capability_channel_ids: vec!["acp".into()],
            }),
        },
        resulting_state: SessionState::Starting,
    };
    SignedReceipt {
        schema: SIGNED_RECEIPT_SCHEMA.into(),
        signature: key.sign(RECEIPT_SCHEMA, &payload.canonical_bytes()),
        payload,
    }
}
