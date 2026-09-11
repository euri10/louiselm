//! Exact-process launch authentication and evidence-versus-acknowledgement gates.

use super::*;
use rustix::net::{
    AddressFamily, SendFlags, SocketAddrUnix, SocketFlags, SocketType, connect, send, socket_with,
};
use std::process::{Command, Stdio};

fn fixture(pin: CredentialPin) -> (TempDir, LaunchRequest, BrokerService) {
    let root = TempDir::new().unwrap();
    let request = request("session-1");
    let authorizations =
        AuthorizationStore::open(&root.path().join("authorizations"), pool(4)).unwrap();
    authorizations.authorize(&grant(&request), 1000).unwrap();
    let service = BrokerService::bind(
        &root.path().join("control.sock"),
        authorizations,
        ReceiptStore::open(&root.path().join("receipts"), trusted_release()).unwrap(),
        AuditLog::open(&root.path().join("audit")).unwrap(),
        pin,
    )
    .unwrap();
    (root, request, service)
}

#[test]
fn a_foreign_first_connector_cannot_consume_pending_authority() {
    let pin = CredentialPin::Identity {
        uid: getuid().as_raw() ^ 1,
        gid: getgid().as_raw(),
    };
    let (root, request, service) = fixture(pin);
    let peer = socket_with(
        AddressFamily::UNIX,
        SocketType::SEQPACKET,
        SocketFlags::CLOEXEC,
        None,
    )
    .unwrap();
    connect(
        &peer,
        &SocketAddrUnix::new(root.path().join("control.sock")).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        service.serve_launch(2000, verify_fixture_signature),
        Err(BrokerError::Transport(_))
    ));
    assert!(
        service
            .authorizations()
            .consumed_for_session(&request.session_id)
            .unwrap()
            .is_none()
    );
    assert!(
        service
            .authorizations()
            .consume(&request, CONTROLLER_UID, 2000)
            .is_ok()
    );
}

#[test]
fn an_inherited_supervisor_socket_cannot_consume_pending_authority() {
    let (root, request, service) = fixture(local_pin());
    let peer = socket_with(
        AddressFamily::UNIX,
        SocketType::SEQPACKET,
        SocketFlags::CLOEXEC,
        None,
    )
    .unwrap();
    connect(
        &peer,
        &SocketAddrUnix::new(root.path().join("control.sock")).unwrap(),
    )
    .unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "launch_gates::inherited_launch_sender",
            "--nocapture",
        ])
        .env(
            "LOUISELM_TEST_INHERITED_LAUNCH",
            String::from_utf8(request.canonical_bytes()).unwrap(),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(peer))
        .spawn()
        .unwrap();
    assert!(matches!(
        service.serve_launch(2000, verify_fixture_signature),
        Err(BrokerError::InvalidGrant)
    ));
    assert!(child.wait().unwrap().success());
    assert!(
        service
            .authorizations()
            .consumed_for_session(&request.session_id)
            .unwrap()
            .is_none()
    );
    assert!(
        service
            .authorizations()
            .consume(&request, CONTROLLER_UID, 2000)
            .is_ok()
    );
}

#[test]
fn inherited_launch_sender() {
    let Ok(packet) = std::env::var("LOUISELM_TEST_INHERITED_LAUNCH") else {
        return;
    };
    assert_eq!(
        send(std::io::stderr(), packet.as_bytes(), SendFlags::NOSIGNAL).unwrap(),
        packet.len()
    );
}

#[test]
fn durable_running_without_final_audit_never_claims_an_acknowledged_channel() {
    if getuid().is_root() {
        eprintln!("skipping: audit permission injection requires an unprivileged worker");
        return;
    }
    let (root, request, service) = fixture(local_pin());
    let socket = root.path().join("control.sock");
    let request_for_peer = request.clone();
    let supervisor = thread::spawn(move || {
        let (authorization, channel) = supervisor_authorization(&socket, &request_for_peer, 2000);
        let launch = launch_receipt(&authorization);
        settle(|complete| channel.send(launch.canonical_bytes(), complete));
        assert_eq!(expect_acknowledgement(&channel).sequence, 0);
        let start = start_receipt(&authorization, &launch);
        settle(|complete| channel.send(start.canonical_bytes(), complete));
        expect_disconnect(&channel);
    });
    let failure = service.serve_launch(2000, |key, payload, signature| {
        if ReceiptPayload::parse_canonical(payload).unwrap().sequence == 1 {
            fs::set_permissions(
                root.path().join("audit/decisions.jsonl"),
                fs::Permissions::from_mode(0o400),
            )
            .unwrap();
        }
        verify_fixture_signature(key, payload, signature)
    });
    assert!(matches!(failure, Err(BrokerError::Storage(_))));
    supervisor.join().unwrap();
    let inspection = service.inspect(&request.session_id).unwrap().unwrap();
    assert_eq!(inspection.state, SessionState::Running);
    assert_eq!(inspection.broker_head.unwrap().sequence, 1);
    assert!(inspection.start_evidence.is_some());
    assert_eq!(
        inspection.launch,
        louiselm_skills::broker::LaunchObservation::DurableOnly
    );
}
