//! Root-owned listener with a distinct broker sender; no lifecycle authority is minted.
use super::*;
use crate::launch_protocol::{
    BROKER_RECONNECT_SCHEMA, RESPONSE_SCHEMA, STATUS_REQUEST_SCHEMA, StatusRequest,
};
use crate::launch_transport::{LauncherPacket, SeqpacketListener};
use rustix::net::{
    AddressFamily, SocketAddrUnix, SocketFlags, SocketType, bind, listen, socket_with,
};

const WORKER_NAME: &str =
    "launch_supervisor::system::installed_tests::socket_activation::socket_worker";

fn finish<T: Send + 'static>(queue: impl FnOnce(Box<dyn FnOnce(T) + Send>)) -> T {
    let (tx, rx) = mpsc::channel();
    queue(Box::new(move |value| tx.send(value).unwrap()));
    rx.recv_timeout(Duration::from_secs(10)).unwrap()
}

fn status_request() -> StatusRequest {
    StatusRequest {
        schema: STATUS_REQUEST_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        request_id: "socket-status".into(),
        session_id: "session".into(),
        run_id: "run".into(),
    }
}

#[test]
fn socket_worker() {
    let Ok(mode) = std::env::var("LOUISELM_SOCKET_WORKER") else {
        return;
    };
    let descriptor = rustix::io::fcntl_dupfd_cloexec(std::io::stdin(), 3).unwrap();
    let listener = SeqpacketListener::adopt(descriptor).unwrap();
    let channel = finish(|done| {
        listener
            .accept(CredentialPin::Identity { uid: 0, gid: 0 }, done)
            .unwrap();
    })
    .unwrap();
    let bytes = if mode == "reconnect" {
        let packet = finish(|done| channel.receive(done).unwrap()).unwrap();
        let LauncherPacket::Request(ProtocolMessage::BrokerReconnect(reconnect)) = packet.packet
        else {
            panic!("expected reconnect");
        };
        ProtocolResponse {
            schema: RESPONSE_SCHEMA.into(),
            protocol_version: PROTOCOL_VERSION,
            request_id: reconnect.request_id.clone(),
            result: ResponseResult::BrokerReconnect { reconnect },
        }
        .canonical_bytes()
    } else {
        status_request().canonical_bytes()
    };
    finish(|done| channel.send(bytes, done).unwrap()).unwrap();
    channel.close();
    listener.close();
}

fn worker(listener: &OwnedFd, uid: u32, mode: &str) -> BrokerChild {
    BrokerChild(
        Command::new("/usr/bin/setpriv")
            .args([
                "--reuid",
                &uid.to_string(),
                "--regid",
                &uid.to_string(),
                "--clear-groups",
            ])
            .arg(std::env::current_exe().unwrap())
            .args([WORKER_NAME, "--exact", "--nocapture"])
            .env_clear()
            .env("LOUISELM_SOCKET_WORKER", mode)
            .stdin(Stdio::from(listener.try_clone().unwrap()))
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    )
}

#[test]
fn privileged_manager_listener_authenticates_only_broker_senders_across_restart() {
    if std::env::var_os("LOUISELM_REQUIRE_BROKER_LAUNCH").is_none() {
        eprintln!("skipping: requires disposable launcher VM");
        return;
    }
    assert!(rustix::process::geteuid().is_root());
    let _account = BrokerAccount::create();
    let root = tempfile::Builder::new()
        .prefix("louiselm-socket-")
        .tempdir_in("/var/lib")
        .unwrap();
    let (_, config, _) = install_fixture(root.path());
    let manager = socket_with(
        AddressFamily::UNIX,
        SocketType::SEQPACKET,
        SocketFlags::CLOEXEC,
        None,
    )
    .unwrap();
    // The manager must retain credentials even for packets queued before
    // service startup. Adoption also asserts SO_PASSCRED on its owned handle.
    rustix::net::sockopt::set_socket_passcred(&manager, true).unwrap();
    bind(
        &manager,
        &SocketAddrUnix::new(&config.broker_socket_path).unwrap(),
    )
    .unwrap();
    listen(&manager, 8).unwrap();

    let mut first = worker(&manager, BROKER_UID, "status");
    let broker = connect_control_broker(&config, Duration::from_secs(10))
        .expect("manager peer and dedicated broker sender are distinct");
    let request = finish(|done| broker.receive_session_request(done).unwrap()).unwrap();
    assert!(matches!(request, ProtocolMessage::Status(status) if status == status_request()));
    assert!(first.0.wait().unwrap().success());

    // A fresh process adopts the very same manager-held socket; the existing
    // supervisor uses its production reconnect path, not a new adapter.
    let mut restarted = worker(&manager, BROKER_UID, "reconnect");
    let checkpoint = BrokerReconnect {
        schema: BROKER_RECONNECT_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        request_id: "reconnect".into(),
        session_id: "session".into(),
        run_id: "run".into(),
        envelope_revision: 1,
        sequence: 1,
        receipt_digest: Digest::of(b"checkpoint").to_string(),
    };
    assert_eq!(
        finish(|done| broker.reconnect_session(checkpoint.clone(), done).unwrap()).unwrap(),
        checkpoint
    );
    assert!(restarted.0.wait().unwrap().success());

    // Even the listener's root owner cannot supply a broker packet. A foreign
    // unprivileged sender holding the fd is refused identically.
    for uid in [0, 65534] {
        let mut hostile = worker(&manager, uid, "status");
        let connection = connect_control_broker(&config, Duration::from_secs(10)).unwrap();
        assert!(matches!(
            finish(|done| connection.receive_session_request(done).unwrap()),
            Err(SupervisorError::BrokerUnavailable)
        ));
        assert!(hostile.0.wait().unwrap().success());
    }
}
