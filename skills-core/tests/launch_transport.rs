#![cfg(target_os = "linux")]

use std::{
    io::{Read, Write},
    os::{
        fd::{AsRawFd, BorrowedFd, OwnedFd},
        unix::process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread,
    time::Duration,
};

use louiselm_skills::{
    launch_protocol::{
        self, ErrorCode, PROTOCOL_VERSION, ProtocolError, ProtocolMessage, ProtocolResponse,
        RESPONSE_SCHEMA, ResponseResult, STATUS_REQUEST_SCHEMA, StatusRequest,
    },
    launch_transport::{
        AuthenticatedPacket, CredentialPin, KernelCredentials, LauncherPacket, MAX_PACKET_BYTES,
        SeqpacketChannel, SeqpacketConnector, SeqpacketListener, TransportError,
    },
};
use rustix::{
    io::{FdFlags, fcntl_getfd, fcntl_setfd},
    net::{
        AddressFamily, SendFlags, SocketAddrUnix, SocketFlags, SocketType, bind, connect, listen,
        send, socket_with,
    },
    process::{getgid, getpid, getuid},
};
use tempfile::TempDir;

const CALLBACK_TIMEOUT: Duration = Duration::from_secs(2);
const HELPER_FD: &str = "LOUISELM_TEST_SEQPACKET_FD";
const HELPER_PACKET: &str = "LOUISELM_TEST_SEQPACKET_PACKET";

fn current_credentials() -> KernelCredentials {
    KernelCredentials {
        pid: getpid().as_raw_pid() as u32,
        uid: getuid().as_raw(),
        gid: getgid().as_raw(),
    }
}

fn process_pin() -> CredentialPin {
    CredentialPin::Process(current_credentials())
}

fn identity_pin() -> CredentialPin {
    let credentials = current_credentials();
    CredentialPin::Identity {
        uid: credentials.uid,
        gid: credentials.gid,
    }
}

fn status(request_id: &str) -> StatusRequest {
    StatusRequest {
        schema: STATUS_REQUEST_SCHEMA.to_owned(),
        protocol_version: PROTOCOL_VERSION,
        request_id: request_id.to_owned(),
        session_id: "session-1".to_owned(),
        run_id: "run-1".to_owned(),
    }
}

fn status_bytes(request_id: &str) -> Vec<u8> {
    status(request_id).canonical_bytes()
}

fn response(request_id: &str) -> ProtocolResponse {
    ProtocolResponse {
        schema: RESPONSE_SCHEMA.to_owned(),
        protocol_version: PROTOCOL_VERSION,
        request_id: request_id.to_owned(),
        result: ResponseResult::Error {
            error: ProtocolError::new(ErrorCode::BrokerUnavailable, None, None),
        },
    }
}

fn path_in(directory: &TempDir) -> PathBuf {
    directory.path().join("launcher.sock")
}

fn accepted(
    listener: &SeqpacketListener,
    pin: CredentialPin,
) -> Receiver<Result<SeqpacketChannel, TransportError>> {
    let (complete, result) = mpsc::sync_channel(1);
    listener
        .accept(
            pin,
            Box::new(move |accepted| {
                let _ = complete.send(accepted);
            }),
        )
        .expect("accept is queued");
    result
}

fn connected(
    connector: &SeqpacketConnector,
    path: &Path,
    pin: CredentialPin,
) -> Receiver<Result<SeqpacketChannel, TransportError>> {
    let (complete, result) = mpsc::sync_channel(1);
    connector
        .connect(
            path,
            pin,
            Box::new(move |connected| {
                let _ = complete.send(connected);
            }),
        )
        .expect("connect is queued");
    result
}

fn wait<T>(result: Receiver<Result<T, TransportError>>) -> Result<T, TransportError> {
    result
        .recv_timeout(CALLBACK_TIMEOUT)
        .expect("transport callback completes")
}

fn send_packet(channel: &SeqpacketChannel, bytes: Vec<u8>) -> Result<(), TransportError> {
    let (complete, result) = mpsc::sync_channel(1);
    channel.send(
        bytes,
        Box::new(move |sent| {
            let _ = complete.send(sent);
        }),
    )?;
    wait(result)
}

fn receive_packet(channel: &SeqpacketChannel) -> Result<AuthenticatedPacket, TransportError> {
    let (complete, result) = mpsc::sync_channel(1);
    channel.receive(Box::new(move |received| {
        let _ = complete.send(received);
    }))?;
    wait(result)
}

struct ConnectedPair {
    _directory: TempDir,
    _listener: SeqpacketListener,
    client: SeqpacketChannel,
    server: SeqpacketChannel,
}

fn connected_pair(pin: CredentialPin) -> ConnectedPair {
    let directory = TempDir::new().expect("temporary rendezvous directory");
    let path = path_in(&directory);
    let listener = SeqpacketListener::bind(&path).expect("listener binds");
    let accept_result = accepted(&listener, pin);
    let connector = SeqpacketConnector::new().expect("connector starts");
    let connect_result = connected(&connector, &path, pin);
    let client = wait(connect_result).expect("client connects");
    let server = wait(accept_result).expect("server accepts");
    ConnectedPair {
        _directory: directory,
        _listener: listener,
        client,
        server,
    }
}

struct RawConnection {
    _directory: TempDir,
    _listener: SeqpacketListener,
    peer: OwnedFd,
    server: SeqpacketChannel,
}

fn raw_connection(pin: CredentialPin) -> RawConnection {
    let directory = TempDir::new().expect("temporary rendezvous directory");
    let path = path_in(&directory);
    let listener = SeqpacketListener::bind(&path).expect("listener binds");
    let accept_result = accepted(&listener, pin);
    let peer = socket_with(
        AddressFamily::UNIX,
        SocketType::SEQPACKET,
        SocketFlags::CLOEXEC,
        None,
    )
    .expect("raw seqpacket socket opens");
    let address = SocketAddrUnix::new(&path).expect("socket address is valid");
    connect(&peer, &address).expect("raw peer connects");
    let server = wait(accept_result).expect("server accepts raw peer");
    RawConnection {
        _directory: directory,
        _listener: listener,
        peer,
        server,
    }
}

fn raw_send(peer: &OwnedFd, bytes: &[u8]) {
    assert_eq!(
        send(peer, bytes, SendFlags::NOSIGNAL).expect("raw packet sends"),
        bytes.len(),
    );
}

fn assert_request(packet: AuthenticatedPacket, expected_bytes: &[u8], request_id: &str) {
    assert_eq!(packet.bytes, expected_bytes);
    assert!(matches!(
        packet.packet,
        LauncherPacket::Request(ProtocolMessage::Status(StatusRequest {
            request_id: found,
            ..
        })) if found == request_id
    ));
}

fn protocol_code(error: TransportError) -> ErrorCode {
    match error {
        TransportError::Protocol(error) => error.code,
        other => panic!("expected a protocol error, got {other:?}"),
    }
}

#[test]
fn seqpacket_preserves_one_message_per_packet_in_both_protocol_directions() {
    let pair = connected_pair(process_pin());
    let first = status_bytes("status-1");
    let second_response = response("status-2");
    let second = second_response.canonical_bytes();

    send_packet(&pair.client, first.clone()).expect("first packet sends");
    send_packet(&pair.client, second.clone()).expect("second packet sends");

    assert_request(
        receive_packet(&pair.server).expect("first packet arrives"),
        &first,
        "status-1",
    );
    let received = receive_packet(&pair.server).expect("second packet arrives");
    assert_eq!(received.bytes, second);
    assert_eq!(
        received.packet,
        LauncherPacket::Response(Box::new(second_response)),
    );

    let hostile = raw_connection(process_pin());
    let mut concatenated = status_bytes("status-3");
    concatenated.extend_from_slice(&status_bytes("status-4"));
    raw_send(&hostile.peer, &concatenated);
    assert_eq!(
        protocol_code(receive_packet(&hostile.server).expect_err("trailing JSON is rejected")),
        ErrorCode::MalformedMessage,
    );
    assert!(hostile.server.is_closed());
}

#[test]
fn packet_limit_is_inclusive_and_truncation_is_fatal() {
    assert_eq!(
        MAX_PACKET_BYTES,
        launch_protocol::MAX_PROTOCOL_MESSAGE_BYTES
    );
    let mut maximum = status_bytes("maximum");
    maximum.resize(MAX_PACKET_BYTES, b' ');
    assert!(launch_protocol::decode_message(&maximum).is_ok());

    let pair = connected_pair(process_pin());
    send_packet(&pair.client, maximum.clone()).expect("maximum-size packet sends");
    assert_request(
        receive_packet(&pair.server).expect("maximum-size packet arrives intact"),
        &maximum,
        "maximum",
    );

    let sender_rejection = connected_pair(process_pin());
    let error = send_packet(&sender_rejection.client, vec![b' '; MAX_PACKET_BYTES + 1])
        .expect_err("sender rejects an oversized packet");
    assert!(matches!(error, TransportError::PacketTooLarge));
    assert!(sender_rejection.client.is_closed());

    let receiver_rejection = raw_connection(process_pin());
    raw_send(&receiver_rejection.peer, &vec![b' '; MAX_PACKET_BYTES + 1]);
    let error = receive_packet(&receiver_rejection.server)
        .expect_err("receiver rejects a truncated packet");
    assert!(matches!(error, TransportError::TruncatedPacket));
    assert!(receiver_rejection.server.is_closed());
}

#[test]
fn invalid_packets_are_typed_and_close_the_affected_channel() {
    let canonical = String::from_utf8(status_bytes("hostile")).expect("status is UTF-8");
    let cases = [
        (b"".to_vec(), None),
        (b"{".to_vec(), Some(ErrorCode::MalformedMessage)),
        (
            format!("{canonical}{{}}").into_bytes(),
            Some(ErrorCode::MalformedMessage),
        ),
        (
            canonical
                .replace(STATUS_REQUEST_SCHEMA, "louiselm.launch.unknown/1")
                .into_bytes(),
            Some(ErrorCode::UnsupportedSchema),
        ),
        (
            canonical
                .replace(r#""protocol_version":1"#, r#""protocol_version":2"#)
                .into_bytes(),
            Some(ErrorCode::UnsupportedVersion),
        ),
    ];

    for (bytes, expected_code) in cases {
        let connection = raw_connection(process_pin());
        raw_send(&connection.peer, &bytes);
        let error = receive_packet(&connection.server).expect_err("invalid packet is rejected");
        match expected_code {
            Some(code) => assert_eq!(protocol_code(error), code),
            None => assert!(matches!(error, TransportError::EmptyPacket)),
        }
        assert!(connection.server.is_closed());
        assert!(matches!(
            connection
                .server
                .receive(Box::new(|_| panic!("closed channel completed"))),
            Err(TransportError::Closed)
        ));
    }
}

#[test]
fn credentials_come_from_the_kernel_and_claimed_json_identity_is_rejected() {
    let credentials = current_credentials();
    let directory = TempDir::new().expect("temporary rendezvous directory");
    let path = path_in(&directory);
    let listener = SeqpacketListener::bind(&path).expect("listener binds");
    let accept_result = accepted(&listener, CredentialPin::Process(credentials));
    let peer = socket_with(
        AddressFamily::UNIX,
        SocketType::SEQPACKET,
        SocketFlags::CLOEXEC,
        None,
    )
    .expect("raw seqpacket socket opens");
    let address = SocketAddrUnix::new(&path).expect("socket address is valid");
    connect(&peer, &address).expect("raw peer connects");
    let bytes = status_bytes("kernel-credentials");
    raw_send(&peer, &bytes);
    let server = wait(accept_result).expect("server accepts the already-sent first packet");
    assert_eq!(server.peer_credentials(), credentials);
    let packet = receive_packet(&server).expect("packet is authenticated");
    assert_eq!(packet.peer_credentials, credentials);
    assert_eq!(packet.message_credentials, credentials);
    assert_request(packet, &bytes, "kernel-credentials");

    let hostile = raw_connection(process_pin());
    let claimed = String::from_utf8(status_bytes("claimed-credentials"))
        .expect("status is UTF-8")
        .replace(
            r#""request_id":"claimed-credentials""#,
            r#""request_id":"claimed-credentials","host_pid":1,"host_uid":0,"host_gid":0"#,
        );
    raw_send(&hostile.peer, claimed.as_bytes());
    assert_eq!(
        protocol_code(
            receive_packet(&hostile.server).expect_err("claimed credentials are rejected")
        ),
        ErrorCode::MalformedMessage,
    );
    assert!(hostile.server.is_closed());
}

#[test]
fn broker_identity_and_per_message_process_pins_fail_closed() {
    let credentials = current_credentials();
    let wrong_uid = if credentials.uid == 0 { 1 } else { 0 };
    let directory = TempDir::new().expect("temporary rendezvous directory");
    let path = path_in(&directory);
    let listener = SeqpacketListener::bind(&path).expect("listener binds");
    let accept_result = accepted(
        &listener,
        CredentialPin::Identity {
            uid: wrong_uid,
            gid: credentials.gid,
        },
    );
    let connector = SeqpacketConnector::new().expect("connector starts");
    let connect_result = connected(&connector, &path, process_pin());
    let client = wait(connect_result).expect("client reaches listener");
    let error = wait(accept_result).expect_err("wrong broker identity is rejected");
    assert!(matches!(
        error,
        TransportError::PeerCredentialsMismatch { .. }
    ));
    assert!(matches!(
        receive_packet(&client).expect_err("rejected connection is closed"),
        TransportError::Disconnected
    ));

    let inherited = raw_connection(CredentialPin::Process(credentials));
    let mut child = spawn_inherited_sender(&inherited.peer, &status_bytes("child-sender"));
    let child_pid = child.id();
    child
        .stdin
        .take()
        .expect("helper stdin is piped")
        .write_all(b"go")
        .expect("helper is released");
    let error =
        receive_packet(&inherited.server).expect_err("a child cannot use its parent's process pin");
    match error {
        TransportError::MessageCredentialsMismatch { expected, actual } => {
            assert_eq!(expected, CredentialPin::Process(credentials));
            assert_eq!(actual.pid, child_pid);
        }
        other => panic!("expected message credential mismatch, got {other:?}"),
    }
    assert!(inherited.server.is_closed());
    assert!(child.wait().expect("helper exits").success());
}

#[test]
fn peer_and_per_message_credentials_are_independent_kernel_observations() {
    let parent = current_credentials();
    let connection = raw_connection(identity_pin());
    let bytes = status_bytes("delegated-sender");
    let mut child = spawn_inherited_sender(&connection.peer, &bytes);
    let child_pid = child.id();
    child
        .stdin
        .take()
        .expect("helper stdin is piped")
        .write_all(b"go")
        .expect("helper is released");

    let packet = receive_packet(&connection.server).expect("identity pin accepts child sender");
    assert_eq!(packet.peer_credentials, parent);
    assert_eq!(packet.message_credentials.pid, child_pid);
    assert_eq!(packet.message_credentials.uid, parent.uid);
    assert_eq!(packet.message_credentials.gid, parent.gid);
    assert_ne!(
        packet.peer_credentials.pid, packet.message_credentials.pid,
        "SO_PEERCRED must not be substituted for SCM_CREDENTIALS",
    );
    assert_request(packet, &bytes, "delegated-sender");
    assert!(child.wait().expect("helper exits").success());
}

#[test]
fn disconnect_closes_one_channel_and_listener_accepts_a_reconnection() {
    let directory = TempDir::new().expect("temporary rendezvous directory");
    let path = path_in(&directory);
    let listener = SeqpacketListener::bind(&path).expect("listener binds");
    let connector = SeqpacketConnector::new().expect("connector starts");

    let first_accept = accepted(&listener, process_pin());
    let first_connect = connected(&connector, &path, process_pin());
    let first_client = wait(first_connect).expect("first client connects");
    let first_server = wait(first_accept).expect("first client is accepted");
    let first = status_bytes("first-connection");
    send_packet(&first_client, first.clone()).expect("first client sends");
    assert_request(
        receive_packet(&first_server).expect("first server receives"),
        &first,
        "first-connection",
    );
    let _ = first_client.close();
    assert!(first_client.is_closed());
    assert!(matches!(
        receive_packet(&first_server).expect_err("peer close is reported"),
        TransportError::Disconnected
    ));
    assert!(first_server.is_closed());

    let second_accept = accepted(&listener, process_pin());
    let second_connect = connected(&connector, &path, process_pin());
    let second_client = wait(second_connect).expect("second client connects");
    let second_server = wait(second_accept).expect("second client is accepted");
    let second = status_bytes("second-connection");
    send_packet(&second_client, second.clone()).expect("second client sends");
    assert_request(
        receive_packet(&second_server).expect("second server receives"),
        &second,
        "second-connection",
    );
}

#[test]
fn a_full_listener_backlog_returns_without_stranding_the_connector_worker() {
    let directory = TempDir::new().expect("temporary rendezvous directory");
    let path = path_in(&directory);
    let listener = socket_with(
        AddressFamily::UNIX,
        SocketType::SEQPACKET,
        SocketFlags::CLOEXEC,
        None,
    )
    .expect("raw listener opens");
    let address = SocketAddrUnix::new(&path).expect("socket address is valid");
    bind(&listener, &address).expect("raw listener binds");
    listen(&listener, 1).expect("raw listener listens with a small backlog");

    let connector = SeqpacketConnector::new().expect("connector starts");
    let mut clients = Vec::new();
    let mut busy = false;
    for _ in 0..8 {
        match wait(connected(&connector, &path, process_pin())) {
            Ok(client) => clients.push(client),
            Err(TransportError::ConnectBusy) => {
                busy = true;
                break;
            }
            Err(other) => panic!("unexpected connect result: {other:?}"),
        }
    }
    assert!(busy, "the deliberately full listener backlog is reported");

    let pending = connected(&connector, &path, process_pin());
    drop(connector);
    assert!(matches!(
        wait(pending),
        Err(TransportError::Closed | TransportError::ConnectBusy)
    ));
    drop(clients);
}

#[test]
fn receive_and_immediate_send_failures_complete_off_the_calling_thread() {
    let RawConnection {
        _directory,
        _listener,
        peer,
        server,
    } = raw_connection(process_pin());
    let (registered, registration) = mpsc::sync_channel(1);
    let (completed, completion) = mpsc::sync_channel(1);
    let registration_thread = thread::spawn(move || {
        let caller = thread::current().id();
        let queued = server.receive(Box::new(move |result| {
            let _ = completed.send((thread::current().id(), result));
        }));
        let _ = registered.send((caller, queued, server));
    });

    let (caller_thread, queued, server) = registration
        .recv_timeout(CALLBACK_TIMEOUT)
        .expect("receive returns while no packet is available");
    queued.expect("receive is queued");
    assert!(matches!(
        completion.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    let bytes = status_bytes("async-receive");
    raw_send(&peer, &bytes);
    let (callback_thread, result) = completion
        .recv_timeout(CALLBACK_TIMEOUT)
        .expect("receive callback completes");
    assert_ne!(callback_thread, caller_thread);
    assert_request(result.expect("receive succeeds"), &bytes, "async-receive");
    registration_thread
        .join()
        .expect("registration thread exits");
    assert!(!server.is_closed());

    let pair = connected_pair(process_pin());
    let caller_thread = thread::current().id();
    let (completed, completion) = mpsc::sync_channel(1);
    pair.client
        .send(
            vec![b' '; MAX_PACKET_BYTES + 1],
            Box::new(move |result| {
                let _ = completed.send((thread::current().id(), result));
            }),
        )
        .expect("validation is queued");
    let (callback_thread, result) = completion
        .recv_timeout(CALLBACK_TIMEOUT)
        .expect("send callback completes");
    assert_ne!(callback_thread, caller_thread);
    assert!(matches!(
        result.expect_err("oversized send fails"),
        TransportError::PacketTooLarge
    ));
}

#[test]
fn receive_queue_is_bounded_and_close_completes_every_admitted_callback_once() {
    let pair = connected_pair(process_pin());
    let (completed, completions) = mpsc::channel();
    let mut admitted = 0;

    for _ in 0..64 {
        let completed = completed.clone();
        match pair.server.receive(Box::new(move |result| {
            let _ = completed.send(result);
        })) {
            Ok(()) => admitted += 1,
            Err(TransportError::QueueFull) => break,
            Err(other) => panic!("unexpected receive admission error: {other:?}"),
        }
    }

    assert!(admitted > 0);
    assert!(admitted < 64, "the receive queue must have a fixed bound");
    assert!(pair.server.close());
    drop(completed);
    for _ in 0..admitted {
        assert!(matches!(
            completions
                .recv_timeout(CALLBACK_TIMEOUT)
                .expect("every admitted receive completes"),
            Err(TransportError::Closed)
        ));
    }
    assert!(matches!(
        completions.recv_timeout(Duration::from_millis(20)),
        Err(mpsc::RecvTimeoutError::Disconnected)
    ));
}

#[test]
fn dropping_a_channel_never_waits_for_a_user_callback() {
    let ConnectedPair {
        _directory,
        _listener,
        client,
        server,
    } = connected_pair(process_pin());
    let (entered, callback_entered) = mpsc::sync_channel(1);
    let (release, callback_release) = mpsc::sync_channel(1);
    let (exited, callback_exited) = mpsc::sync_channel(1);
    server
        .receive(Box::new(move |result| {
            assert!(result.is_ok());
            let _ = entered.send(());
            let _ = callback_release.recv();
            let _ = exited.send(());
        }))
        .expect("receive is queued");
    send_packet(&client, status_bytes("blocked-callback")).expect("packet sends");
    callback_entered
        .recv_timeout(CALLBACK_TIMEOUT)
        .expect("callback starts");

    let (dropped, drop_finished) = mpsc::sync_channel(1);
    let drop_thread = thread::spawn(move || {
        drop(server);
        let _ = dropped.send(());
    });
    drop_finished
        .recv_timeout(CALLBACK_TIMEOUT)
        .expect("channel drop does not join the blocked worker");
    release.send(()).expect("callback is released");
    callback_exited
        .recv_timeout(CALLBACK_TIMEOUT)
        .expect("callback exits");
    drop_thread.join().expect("drop thread exits");
}

fn spawn_inherited_sender(peer: &OwnedFd, bytes: &[u8]) -> Child {
    let raw_fd = peer.as_raw_fd();
    let mut command = Command::new(std::env::current_exe().expect("test executable is known"));
    command
        .args(["--exact", "inherited_sender_helper", "--nocapture"])
        .env(HELPER_FD, raw_fd.to_string())
        .env(
            HELPER_PACKET,
            String::from_utf8(bytes.to_vec()).expect("test packet is UTF-8"),
        )
        .stdin(Stdio::piped());
    // SAFETY: the fd stays open through spawn, and these fcntl operations are
    // async-signal-safe and allocate nothing between fork and exec.
    unsafe {
        command.pre_exec(move || {
            let fd = BorrowedFd::borrow_raw(raw_fd);
            let mut flags = fcntl_getfd(fd).map_err(std::io::Error::from)?;
            flags.remove(FdFlags::CLOEXEC);
            fcntl_setfd(fd, flags).map_err(std::io::Error::from)
        });
    }
    command.spawn().expect("sender helper starts")
}

#[test]
fn inherited_sender_helper() {
    let Some(raw_fd) = std::env::var_os(HELPER_FD) else {
        return;
    };
    let raw_fd = raw_fd
        .into_string()
        .expect("helper fd is UTF-8")
        .parse()
        .expect("helper fd is numeric");
    let bytes = std::env::var(HELPER_PACKET)
        .expect("helper packet is present")
        .into_bytes();
    let mut release = [0_u8; 2];
    std::io::stdin()
        .read_exact(&mut release)
        .expect("parent releases helper");
    assert_eq!(&release, b"go");

    // SAFETY: the parent deliberately inherited this live socket descriptor
    // into only this helper, and the borrow ends before the helper exits.
    let peer = unsafe { BorrowedFd::borrow_raw(raw_fd) };
    assert_eq!(
        send(peer, &bytes, SendFlags::NOSIGNAL).expect("helper packet sends"),
        bytes.len(),
    );
}
