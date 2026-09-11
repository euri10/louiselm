//! Real capability and supervisor transports with a pinned native fixture.
//! The test executable is the measured Agent; this is not an installed launch proof.
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "Tests assert bounded fixture construction."
)]

use super::*;
use crate::{
    launch_transport::{KernelCredentials, TransportCompletion},
    sandbox::{IdentityPlan, default_system_roots},
};

pub(in crate::launch_supervisor) struct Fixture {
    pub gate: Box<dyn CapabilityGate>,
    pub agent: SeqpacketChannel,
    pub broker: Arc<dyn LaunchBroker>,
    pub broker_channel: SeqpacketChannel,
    pub binding: CapabilityBinding,
    pub process: Arc<KernelProcess>,
    pub tools: super::super::tool_execution::ToolExecutor,
}

pub(in crate::launch_supervisor) fn settle<T: Send + 'static>(
    start: impl FnOnce(TransportCompletion<T>) -> Result<(), TransportError>,
) -> T {
    let (tx, rx) = mpsc::channel();
    start(Box::new(move |result| {
        let _ = tx.send(result);
    }))
    .unwrap();
    rx.recv_timeout(Duration::from_secs(5)).unwrap().unwrap()
}

#[expect(
    clippy::too_many_lines,
    reason = "One native fixture constructs its pinned gate, two actual transports and existing isolated executor; no production setup abstraction is introduced."
)]
pub(in crate::launch_supervisor) fn fixture(root: &Path) -> Fixture {
    let pid = std::process::id();
    let credentials = KernelCredentials {
        pid,
        uid: rustix::process::getuid().as_raw(),
        gid: rustix::process::getgid().as_raw(),
    };
    let process = Arc::new(
        KernelProcess::from_exec_stop(
            credentials,
            rustix::process::pidfd_open(
                rustix::process::Pid::from_raw(i32::try_from(pid).unwrap()).unwrap(),
                rustix::process::PidfdFlags::empty(),
            )
            .unwrap(),
            &fs::File::open(std::env::current_exe().unwrap()).unwrap(),
        )
        .unwrap(),
    );
    let path = root.join("agent.sock");
    let bound = SeqpacketListener::bind_disabled(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let metadata = fs::metadata(&path).unwrap();
    let binding = CapabilityBinding {
        session_id: "session-1".to_owned(),
        run_id: "run-1".to_owned(),
        channel_id: "agent-capability".to_owned(),
        envelope_revision: 1,
        identity_slot: 1,
        assigned_uid: credentials.uid,
        assigned_gid: credentials.gid,
        agent_pid: pid,
    };
    let mut gate = SystemCapabilityGate {
        state: Some(ListenerState::Bound(bound)),
        path: path.clone(),
        device: metadata.dev(),
        inode: metadata.ino(),
        channel: Channel::UnixSocket {
            id: binding.channel_id.clone(),
            host_path: path.clone(),
            guest_path: PathBuf::from(SYSTEM_CAPABILITY_GUEST_PATH),
        },
        binding: None,
        process: None,
        commands: None,
        expected_session_id: binding.session_id.clone(),
        expected_run_id: binding.run_id.clone(),
        expected_envelope_revision: 1,
        expected_identity: Identity {
            slot: 1,
            uid: credentials.uid,
            gid: credentials.gid,
        },
        accepted: Arc::default(),
    };
    gate.bind(
        binding.clone(),
        AgentAuthentication {
            credentials,
            process: Some(Arc::clone(&process)),
            tool_isolation: None,
        },
    )
    .unwrap();
    gate.enable().unwrap();
    let connector = SeqpacketConnector::new().unwrap();
    let agent =
        settle(|complete| connector.connect(&path, CredentialPin::Process(credentials), complete));
    let broker_path = root.join("broker.sock");
    let listener = SeqpacketListener::bind(&broker_path).unwrap();
    let (tx, rx) = mpsc::channel();
    listener
        .accept(
            CredentialPin::Process(credentials),
            Box::new(move |result| tx.send(result).unwrap()),
        )
        .unwrap();
    let supervisor = settle(|complete| {
        connector.connect(&broker_path, CredentialPin::Process(credentials), complete)
    });
    let broker_channel = rx.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
    let broker = Arc::new(SeqpacketLaunchBroker::new(
        connector,
        broker_path,
        CredentialPin::Process(credentials),
        supervisor,
    ));
    for name in ["runtime", "home", "workspace"] {
        fs::create_dir(root.join(name)).unwrap();
    }
    let plan = ConfinementPlan {
        session_id: "fixture".to_owned(),
        runtime_root: root.join("runtime"),
        executable: root.join("runtime/agent"),
        arguments: vec![],
        environment: std::collections::BTreeMap::new(),
        home: root.join("home"),
        workspace: root.join("workspace"),
        system_roots: default_system_roots(),
        network: crate::registry::NetworkPolicy::Denied,
        identity: IdentityPlan::NamespaceOnly,
        channels: vec![],
    };
    let bootstrap = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("louiselm-launch");
    assert!(bootstrap.exists());
    let tools = super::super::tool_execution::ToolExecutor::new(
        BubblewrapBackend::new()
            .with_bootstrap(&bootstrap)
            .without_cgroup(),
        &plan,
    )
    .unwrap();
    Fixture {
        gate: Box::new(gate),
        agent,
        broker,
        broker_channel,
        binding,
        process,
        tools,
    }
}
