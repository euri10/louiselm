//! Assigned-UID measured Agent and helper, only in the disposable launcher VM.
#![allow(clippy::unwrap_used, reason = "Tests assert privileged fixture setup.")]

use super::*;
use crate::{
    release::Component,
    sandbox::{IdentityPlan, default_system_roots},
};

pub(in crate::launch_supervisor) struct MeasuredFixture {
    pub process: Box<dyn RunningAgent>,
    pub gate: Box<dyn CapabilityGate>,
    pub binding: CapabilityBinding,
    pub input: std::process::ChildStdin,
    pub output: std::process::ChildStdout,
    pub workspace: PathBuf,
    pub cgroup: PathBuf,
}

#[expect(
    clippy::too_many_lines,
    reason = "One disposable fixture binds measured release bytes, real assigned-UID startup and the exact capability gate."
)]
pub(in crate::launch_supervisor) fn fixture(root: &Path) -> MeasuredFixture {
    assert!(rustix::process::geteuid().is_root());
    fs::set_permissions(root, fs::Permissions::from_mode(0o711)).unwrap();
    let base = root.join("measured");
    fs::create_dir(&base).unwrap();
    fs::set_permissions(&base, fs::Permissions::from_mode(0o711)).unwrap();
    let binaries = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let runtime = base.join("runtime");
    fs::create_dir(&runtime).unwrap();
    let executable = runtime.join("agent");
    fs::copy(binaries.join("louiselm-tool-test-agent"), &executable).unwrap();
    let release_root = base.join("release");
    fs::create_dir_all(release_root.join("bin")).unwrap();
    let helper = release_root.join("bin/louiselm-tool-test-helper");
    fs::copy(binaries.join("louiselm-tool-test-helper"), &helper).unwrap();
    let mut manifest = super::tool_integration_tests::manifest(&executable);
    manifest.components.push(Component {
        name: "louiselm-tool-test-helper".into(),
        path: "bin/louiselm-tool-test-helper".into(),
        sha256: Digest::of(&fs::read(&helper).unwrap()).hex().into(),
        size: fs::metadata(&helper).unwrap().len(),
        executable: true,
    });
    manifest.release_id = manifest.digest().to_string();
    fs::write(
        release_root.join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    let uid = 4_009_000;
    let path = base.join("agent.sock");
    let bound = SeqpacketListener::bind_disabled(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    chown(&path, Some(uid), Some(uid)).unwrap();
    let metadata = fs::metadata(&path).unwrap();
    let channel = Channel::UnixSocket {
        id: "agent-capability".into(),
        host_path: path.clone(),
        guest_path: PathBuf::from(SYSTEM_CAPABILITY_GUEST_PATH),
    };
    let plan = ConfinementPlan {
        session_id: "session-1".into(),
        runtime_root: runtime,
        executable,
        arguments: vec![],
        environment: std::collections::BTreeMap::new(),
        home: base.join("home"),
        workspace: base.join("workspace"),
        cache: None,
        beads_replica: None,
        system_roots: default_system_roots(),
        network: crate::registry::NetworkPolicy::Denied,
        identity: IdentityPlan::HostIdentity { uid, gid: uid },
        channels: vec![channel.clone()],
    };
    let cgroup = Path::new("/sys/fs/cgroup").join(format!(
        "louiselm-grant-{}",
        root.file_name().unwrap().to_string_lossy()
    ));
    fs::create_dir(&cgroup).unwrap();
    fs::set_permissions(&cgroup, fs::Permissions::from_mode(0o700)).unwrap();
    let backend = BubblewrapBackend::for_launcher(
        Path::new("/usr/bin/bwrap"),
        &binaries.join("louiselm-launch"),
        &cgroup,
        "fixture".into(),
    );
    let evidence = super::super::ToolIsolationEvidence::measure(
        &plan,
        &release_root,
        &manifest.release_id,
        "fixture",
    )
    .unwrap();
    let mut tools = super::super::tool_execution::ToolExecutor::new(
        backend.within_session(&plan.session_id).unwrap(),
        &plan,
    )
    .unwrap();
    tools.measured_helper = Some(
        super::super::tool_helper::MeasuredHelper::measure(&release_root, &manifest.release_id)
            .unwrap(),
    );
    let mut session = backend.prepare(&plan).unwrap().start().unwrap();
    let pin = session.agent_identity().unwrap();
    let input = session.take_stdin().unwrap();
    let output = session.take_stdout().unwrap();
    let stderr = session.take_stderr().unwrap();
    thread::spawn(move || {
        use std::io::Read;
        let mut diagnostics = String::new();
        stderr.take(1024).read_to_string(&mut diagnostics).unwrap();
        if !diagnostics.is_empty() {
            eprintln!("{diagnostics}");
        }
    });
    let binding = CapabilityBinding {
        session_id: "session-1".into(),
        run_id: "run-1".into(),
        channel_id: "agent-capability".into(),
        envelope_revision: 1,
        identity_slot: 1,
        assigned_uid: uid,
        assigned_gid: uid,
        agent_pid: pin.credentials().pid,
    };
    let mut gate = SystemCapabilityGate {
        state: Some(ListenerState::Bound(bound)),
        path,
        device: metadata.dev(),
        inode: metadata.ino(),
        channel,
        binding: None,
        process: None,
        commands: None,
        expected_session_id: binding.session_id.clone(),
        expected_run_id: binding.run_id.clone(),
        expected_envelope_revision: 1,
        expected_identity: Identity {
            slot: 1,
            uid,
            gid: uid,
        },
        accepted: Arc::default(),
    };
    gate.bind(
        binding.clone(),
        AgentAuthentication {
            credentials: pin.credentials(),
            process: Some(pin),
            tool_isolation: Some(evidence.clone()),
        },
    )
    .unwrap();
    gate.enable().unwrap();
    let mut process = SystemRunningAgent::new(session);
    process.tools = Some(tools);
    process.tool_isolation = Some(evidence);
    MeasuredFixture {
        process: Box::new(process),
        gate: Box::new(gate),
        binding,
        input,
        output,
        workspace: plan.workspace,
        cgroup,
    }
}
