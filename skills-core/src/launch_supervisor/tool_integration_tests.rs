//! Measured production Agent/tool composition inside the disposable acceptance VM.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Test fixtures assert setup and observable production outcomes."
)]

use super::*;
use crate::{
    launch::{PROTOCOL_VERSION, REQUEST_SCHEMA},
    launch_protocol::{TOOL_EXECUTION_SCHEMA, ToolExecutionRequest},
    launcher_install::IdentityPool,
    registry::{NetworkPolicy, Registry},
    release::{
        Component, MANIFEST_SCHEMA, PolicyIdentity, ReleaseManifest, SourceIdentity,
        ToolchainIdentity,
    },
    sandbox::IdentityPlan,
};
use std::collections::BTreeMap;

fn write_json(path: &Path, value: &impl serde::Serialize) {
    fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
}

fn write_registry(path: &Path, entries: &impl serde::Serialize) {
    write_json(
        path,
        &serde_json::json!({"schema": crate::registry::REGISTRY_SCHEMA, "entries": entries}),
    );
}

pub(super) fn manifest(executable: &Path) -> ReleaseManifest {
    let mut manifest = ReleaseManifest {
        schema: MANIFEST_SCHEMA.to_owned(),
        release_id: String::new(),
        version: "test".to_owned(),
        built_at_ms: 1,
        source: SourceIdentity {
            commit: "fixture".to_owned(),
            clean: true,
            describe: "fixture".to_owned(),
            dependencies_digest: Digest::of(b"fixture").to_string(),
        },
        toolchain: ToolchainIdentity {
            rustc: "fixture".to_owned(),
            cargo: "fixture".to_owned(),
            target: "fixture".to_owned(),
        },
        policy: PolicyIdentity {
            version: "fixture".to_owned(),
            digest: Digest::of(b"fixture").to_string(),
        },
        schemas: vec![],
        components: vec![Component {
            name: "louiselm-tool-test-agent".to_owned(),
            path: "bin/louiselm-tool-test-agent".to_owned(),
            sha256: Digest::of(&fs::read(executable).unwrap()).hex().to_owned(),
            size: fs::metadata(executable).unwrap().len(),
            executable: true,
        }],
    };
    manifest.release_id = manifest.digest().to_string();
    manifest
}

#[test]
fn measured_integration_rejects_missing_evidence_and_runtime_overrides() {
    let root = tempfile::tempdir().unwrap();
    let executable = root.path().join("agent");
    fs::write(&executable, b"exact fixture bytes").unwrap();
    let plan = ConfinementPlan {
        session_id: "session".to_owned(),
        runtime_root: root.path().to_path_buf(),
        executable: executable.clone(),
        arguments: vec![],
        environment: BTreeMap::new(),
        home: root.path().join("home"),
        workspace: root.path().join("workspace"),
        system_roots: vec![],
        identity: IdentityPlan::NamespaceOnly,
        network: NetworkPolicy::Denied,
        channels: vec![],
    };
    let manifest = manifest(&executable);
    let release = &manifest.release_id;
    assert!(
        super::super::ToolIsolationEvidence::measure(&plan, root.path(), release, "backend")
            .is_err()
    );
    write_json(&root.path().join("manifest.json"), &manifest);
    assert!(
        super::super::ToolIsolationEvidence::measure(&plan, root.path(), release, "backend")
            .is_ok()
    );
    let mut invalid = plan.clone();
    invalid
        .environment
        .insert("LD_PRELOAD".to_owned(), "workspace/plugin.so".to_owned());
    assert!(
        super::super::ToolIsolationEvidence::measure(&invalid, root.path(), release, "backend")
            .is_err()
    );
    invalid = plan.clone();
    invalid.arguments.push("workspace/plugin".to_owned());
    assert!(
        super::super::ToolIsolationEvidence::measure(&invalid, root.path(), release, "backend")
            .is_err()
    );
    fs::write(&executable, b"other runtime bytes").unwrap();
    assert!(
        super::super::ToolIsolationEvidence::measure(&plan, root.path(), release, "backend")
            .is_err()
    );
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "One privileged composition owns startup, admission, execution, cancellation and cleanup evidence."
)]
fn privileged_measured_agent_owns_isolated_tool_lifecycle() {
    if std::env::var_os("LOUISELM_REQUIRE_TOOL_ISOLATION").is_none() {
        eprintln!("skipping: set LOUISELM_REQUIRE_TOOL_ISOLATION=1 only in the disposable VM");
        return;
    }
    assert!(
        rustix::process::geteuid().is_root(),
        "privileged proof cannot skip without root"
    );
    assert!(
        fs::read_to_string("/proc/self/uid_map")
            .unwrap()
            .split_whitespace()
            .eq(["0", "0", "4294967295"])
    );
    let root = tempfile::Builder::new()
        .prefix("louiselm-tools-")
        .tempdir_in("/var/lib")
        .unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o755)).unwrap();
    let binaries = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let bootstrap = binaries.join("louiselm-launch");
    let fixture_binary = binaries.join("louiselm-tool-test-agent");
    let runtime = root.path().join("runtime");
    fs::create_dir(&runtime).unwrap();
    let executable = runtime.join("agent");
    fs::copy(&fixture_binary, &executable).unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
    let release = manifest(&executable);
    let prefix = root.path().join("release");
    let release_root = prefix.join("releases").join(&release.release_id);
    fs::create_dir_all(&release_root).unwrap();
    write_json(&release_root.join("manifest.json"), &release);
    let sessions = root.path().join("sessions");
    fs::create_dir(&sessions).unwrap();
    fs::set_permissions(&sessions, fs::Permissions::from_mode(0o711)).unwrap();
    let registry_root = root.path().join("registry");
    fs::create_dir(&registry_root).unwrap();
    let agent_json = serde_json::json!({"id":"agent", "provider":"fixture", "runtime_id":"runtime", "arguments":[], "environment":{}, "tool_integration":super::super::tool_integration::CONTRACT});
    write_registry(
        &registry_root.join("agents.json"),
        &vec![agent_json.clone()],
    );
    write_registry(
        &registry_root.join("runtimes.json"),
        &serde_json::json!([{
            "id":"runtime", "root":runtime, "executable":"agent", "executable_sha256":release.components[0].sha256,
            "adapters":[], "version":"fixture", "origin":"fixture"
        }]),
    );
    write_registry(
        &registry_root.join("envelopes.json"),
        &serde_json::json!([{"id":"empty", "network":"denied", "description":"zero tool authority"}]),
    );
    Registry::open_trusted(&registry_root).unwrap();
    let cgroup_root = Path::new("/sys/fs/cgroup").join(root.path().file_name().unwrap());
    fs::create_dir(&cgroup_root).unwrap();
    fs::set_permissions(&cgroup_root, fs::Permissions::from_mode(0o700)).unwrap();
    let bwrap = PathBuf::from("/usr/bin/bwrap");
    let bwrap_digest = Digest::of(&fs::read(&bwrap).unwrap()).to_string();
    let config = LauncherConfig {
        schema: "fixture".to_owned(),
        operator: "fixture".to_owned(),
        operator_uid: 1000,
        broker_uid: 1001,
        broker_gid: 1001,
        broker_socket_path: root.path().join("broker.sock"),
        release_id: release.release_id.clone(),
        launcher_digest: Digest::of(b"fixture").to_string(),
        launcher_path: bootstrap.clone(),
        ssh_keygen_path: PathBuf::from("/usr/bin/ssh-keygen"),
        ssh_keygen_digest: Digest::of(b"fixture").to_string(),
        getent_path: PathBuf::from("/usr/bin/getent"),
        getent_digest: Digest::of(b"fixture").to_string(),
        bwrap_path: bwrap.clone(),
        bwrap_digest: bwrap_digest.clone(),
        pool: IdentityPool {
            uid_start: 4_008_000,
            gid_start: 4_008_000,
            slots: 1,
        },
    };
    let platform = SystemLaunchPlatform {
        registry_root: registry_root.clone(),
        paths: LauncherPaths {
            release_prefix: prefix,
            ..LauncherPaths::system()
        },
        config,
        backend: BubblewrapBackend::for_launcher(
            &bwrap,
            &bootstrap,
            &cgroup_root,
            "fixture".to_owned(),
        ),
        timeout: Duration::from_secs(5),
    };
    let request = LaunchRequest {
        schema: REQUEST_SCHEMA.to_owned(),
        protocol_version: PROTOCOL_VERSION,
        request_id: "launch".to_owned(),
        authorization_id: "auth".to_owned(),
        session_id: "session".to_owned(),
        run_id: "run".to_owned(),
        agent_id: "agent".to_owned(),
        envelope_id: "empty".to_owned(),
        envelope_revision: 1,
        skill_generation_id: Digest::of(b"generation").to_string(),
        session_input_manifest_id: Digest::of(b"input").to_string(),
    };
    let plan = crate::launch::resolve(
        &request,
        &Registry::open_trusted(&registry_root).unwrap(),
        &sessions,
        IdentityPlan::HostIdentity {
            uid: 4_008_000,
            gid: 4_008_000,
        },
    )
    .unwrap()
    .plan;
    let workspace = plan.workspace.clone();
    let prepared = platform.prepare(plan).unwrap();
    let mut running = prepared.start().unwrap();
    let authentication = running.authentication().unwrap();
    assert_eq!(authentication.credentials.uid, 4_008_000);
    let verify = |authentication: &AgentAuthentication| {
        let (tx, rx) = mpsc::channel();
        platform
            .verify_tool_isolation(
                &request,
                authentication,
                Box::new(move |result| {
                    tx.send(result).unwrap();
                }),
            )
            .unwrap();
        rx.recv_timeout(Duration::from_secs(5)).unwrap()
    };
    assert_eq!(
        verify(&authentication),
        Ok(Digest::of(
            &authentication
                .tool_isolation
                .as_ref()
                .unwrap()
                .canonical_bytes()
        ))
    );
    let mut missing = authentication.clone();
    missing.tool_isolation = None;
    assert_eq!(
        verify(&missing),
        Err(SupervisorError::ToolIsolationUnproven)
    );
    let evidence: serde_json::Value = serde_json::from_slice(
        &authentication
            .tool_isolation
            .as_ref()
            .unwrap()
            .canonical_bytes(),
    )
    .unwrap();
    assert_eq!(evidence["executable_digest"], release.components[0].sha256);
    for choice in [None, Some("unsupported-agent")] {
        let mut invalid = agent_json.clone();
        if let Some(choice) = choice {
            invalid["tool_integration"] = choice.into();
        } else {
            invalid.as_object_mut().unwrap().remove("tool_integration");
        }
        write_registry(&registry_root.join("agents.json"), &vec![invalid]);
        let (tx, rx) = mpsc::channel();
        platform
            .check_integration(&request, Box::new(move |result| tx.send(result).unwrap()))
            .unwrap();
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            Err(SupervisorError::ToolIsolationUnproven)
        );
        assert_eq!(
            verify(&authentication),
            Err(SupervisorError::ToolIsolationUnproven)
        );
    }
    write_registry(&registry_root.join("agents.json"), &vec![agent_json]);
    let credentials = authentication.credentials;
    let principal = crate::launch_protocol::CommandPrincipal {
        channel_id: "agent-capability".to_owned(),
        pid: credentials.pid,
        uid: credentials.uid,
        gid: credentials.gid,
    };
    let enforcement = crate::launch_supervisor::command::CommandEnforcer::new(
        CapabilityBinding {
            session_id: "session".to_owned(),
            run_id: "run".to_owned(),
            channel_id: principal.channel_id.clone(),
            envelope_revision: 1,
            identity_slot: 0,
            assigned_uid: credentials.uid,
            assigned_gid: credentials.gid,
            agent_pid: credentials.pid,
        },
        Arc::clone(authentication.process.as_ref().unwrap()),
    )
    .unwrap();
    let command = |sequence, command: &str| {
        let request = ToolExecutionRequest {
            schema: TOOL_EXECUTION_SCHEMA.to_owned(),
            protocol_version: 1,
            request_id: format!("tool-{sequence}"),
            session_id: "session".to_owned(),
            run_id: "run".to_owned(),
            envelope_revision: 1,
            sequence,
            command: command.to_owned(),
            timeout_ms: 5_000,
        };
        let decision = crate::launch_protocol::CommandMessage {
            schema: crate::launch_protocol::COMMAND_SCHEMA.to_owned(),
            protocol_version: 1,
            request_id: request.request_id.clone(),
            session_id: request.session_id.clone(),
            run_id: request.run_id.clone(),
            envelope_revision: 1,
            operation: crate::launch_protocol::CommandOperation::Authorize {
                principal: principal.clone(),
                principal_sequence: sequence,
                dispatch_sequence: sequence,
                command_digest: Digest::of(command.as_bytes()).to_string(),
                timeout_ms: 5000,
                valid_for_ms: 30000,
            },
        };
        enforcement
            .admit(&request, &principal, &decision, Instant::now())
            .unwrap()
    };
    let (tx, rx) = mpsc::channel();
    let hostile = format!(
        "test ! -e /proc/{}/mem && ! kill -0 {} 2>/dev/null && test ! -e ../home/authority && printf confined > result; printf output",
        authentication.credentials.pid, authentication.credentials.pid,
    );
    fs::write(sessions.join("session/home/authority"), b"private").unwrap();
    running
        .execute_tool(
            command(1, &hostile),
            Box::new(move |result| {
                tx.send(result).unwrap();
            }),
        )
        .unwrap();
    let output = rx.recv_timeout(Duration::from_secs(10)).unwrap().unwrap();
    assert_eq!((output.exit_code, output.stdout.as_str()), (0, "output"));
    assert_eq!(
        fs::read_to_string(workspace.join("result")).unwrap(),
        "confined"
    );
    let (tx, rx) = mpsc::channel();
    running
        .execute_tool(
            command(2, "touch ready; while test ! -e resume; do sleep 0.01; done; touch resumed; (sleep 1; touch escaped) & sleep 30"),
            Box::new(move |result| {
                tx.send(result).unwrap();
            }),
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !workspace.join("ready").exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(5));
    }
    assert!(workspace.join("ready").exists());
    running.park().unwrap();
    fs::write(workspace.join("resume"), b"resume").unwrap();
    thread::sleep(Duration::from_millis(100));
    assert!(
        !workspace.join("resumed").exists(),
        "Session Park must freeze the nested tool tree"
    );
    running.resume().unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !workspace.join("resumed").exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(5));
    }
    assert!(workspace.join("resumed").exists());
    let queued_after_disposal = command(3, "touch after-disposal");
    running.dispose().unwrap();
    assert!(rx.recv_timeout(Duration::from_secs(3)).unwrap().is_err());
    assert!(
        running
            .execute_tool(queued_after_disposal, Box::new(|_| {}))
            .is_err()
    );
    thread::sleep(Duration::from_millis(1_100));
    assert!(!workspace.join("escaped").exists());
    assert!(!workspace.join("after-disposal").exists());
    assert!(!authentication.process.unwrap().valid().unwrap());
    assert_eq!(
        fs::read_dir(&cgroup_root)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().unwrap().is_dir())
            .count(),
        0
    );
    fs::remove_dir(cgroup_root).unwrap();
}
