//! Warm handoff acknowledgments share the retained Session's single reader.
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "Deterministic socket fixture setup and exact protocol assertions."
)]
use super::*;
use crate::launch_transport::{KernelCredentials, SeqpacketListener, TransportCompletion};
use std::os::fd::AsFd;

fn settle<T: Send + 'static>(
    start: impl FnOnce(TransportCompletion<T>) -> Result<(), TransportError>,
) -> T {
    let (send, receive) = mpsc::channel();
    start(Box::new(move |result| {
        let _ = send.send(result);
    }))
    .unwrap();
    receive
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap()
}

struct Fixture {
    root: tempfile::TempDir,
    _listener: SeqpacketListener,
    broker: SeqpacketLaunchBroker,
    peer: SeqpacketChannel,
}

fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("broker.sock");
    let listener = SeqpacketListener::bind(&path).unwrap();
    let credentials = KernelCredentials {
        pid: std::process::id(),
        uid: rustix::process::geteuid().as_raw(),
        gid: rustix::process::getegid().as_raw(),
    };
    let pin = CredentialPin::Process(credentials);
    let (send, receive) = mpsc::channel();
    listener
        .accept(
            pin.clone(),
            Box::new(move |result| {
                let _ = send.send(result);
            }),
        )
        .unwrap();
    let connector = SeqpacketConnector::new().unwrap();
    let channel = settle(|complete| connector.connect(&path, pin.clone(), complete));
    let peer = receive
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    Fixture {
        root,
        _listener: listener,
        broker: SeqpacketLaunchBroker::new(connector, path, pin, channel),
        peer,
    }
}

fn enrollment() -> GuardEnrollment {
    GuardEnrollment {
        scope: crate::launch_protocol::GuardScope {
            session_id: "session".into(),
            run_id: "run".into(),
            envelope_revision: 7,
            revision: 2,
            deadline_ns: u64::MAX,
        },
        guard_id: 1,
        runtime_pid: 2,
        broker_pid: 3,
        address: SocketAddr::from(([127, 0, 0, 1], 9000)),
        listener_cookie: 4,
        network_id: 5,
    }
}

fn reply(peer: &SeqpacketChannel, request: &str, enrollment: GuardEnrollment) {
    let response = ProtocolResponse {
        schema: RESPONSE_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        request_id: request.into(),
        result: ResponseResult::SenderGuardAccepted { enrollment },
    };
    settle(|complete| peer.send(response.canonical_bytes(), complete));
}

#[test]
fn warm_handoff_uses_single_reader_and_retired_ack_is_inert() {
    let fixture = fixture();
    let (send, receive) = mpsc::channel();
    fixture
        .broker
        .receive_session_request(Box::new(move |result| {
            let _ = send.send(result);
        }))
        .unwrap();
    let (send, prepared) = mpsc::channel();
    let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let pin = File::open(fixture.root.path()).unwrap();
    let network = File::open("/proc/thread-self/ns/net").unwrap();
    let enrollment = enrollment();
    fixture
        .broker
        .send_guarded_listener(
            "resume",
            enrollment.clone(),
            [socket.as_fd(), pin.as_fd(), network.as_fd()],
            Box::new(move |result| {
                let _ = send.send(result);
            }),
        )
        .unwrap();
    let transferred = settle(|complete| fixture.peer.receive(complete));
    assert!(transferred.descriptors.is_some());
    let mut retired = enrollment.clone();
    retired.scope.revision = 1;
    reply(&fixture.peer, "old-resume", retired);
    reply(&fixture.peer, "resume", enrollment);
    assert_eq!(
        prepared.recv_timeout(Duration::from_secs(2)).unwrap(),
        Ok(())
    );
    assert!(
        receive.try_recv().is_err(),
        "handoff must not consume the Session request completion"
    );
    let request = crate::launch_protocol::StatusRequest {
        schema: crate::launch_protocol::STATUS_REQUEST_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        request_id: "status".into(),
        session_id: "session".into(),
        run_id: "run".into(),
    };
    settle(|complete| fixture.peer.send(request.canonical_bytes(), complete));
    assert_eq!(
        receive.recv_timeout(Duration::from_secs(2)).unwrap(),
        Ok(ProtocolMessage::Status(request))
    );
    fixture.broker.close();
}

#[test]
fn inexact_handoff_ack_and_close_complete_preparation_as_failure() {
    for close in [false, true] {
        let fixture = fixture();
        fixture
            .broker
            .receive_session_request(Box::new(|_| {}))
            .unwrap();
        let (send, prepared) = mpsc::channel();
        let descriptor = File::open(fixture.root.path()).unwrap();
        fixture
            .broker
            .send_guarded_listener(
                "resume",
                enrollment(),
                [descriptor.as_fd(); 3],
                Box::new(move |result| {
                    let _ = send.send(result);
                }),
            )
            .unwrap();
        let _transferred = settle(|complete| fixture.peer.receive(complete));
        if close {
            fixture.broker.close();
        } else {
            let mut changed = enrollment();
            changed.scope.deadline_ns -= 1;
            reply(&fixture.peer, "resume", changed);
        }
        assert!(
            prepared
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .is_err()
        );
        fixture.broker.close();
        assert!(
            prepared.try_recv().is_err(),
            "completion must be exactly once"
        );
    }
}

#[test]
fn acknowledged_closure_cancels_preparation_and_allows_a_fresh_revision() {
    let fixture = fixture();
    fixture
        .broker
        .receive_session_request(Box::new(|_| {}))
        .unwrap();
    let (send, prepared) = mpsc::channel();
    let descriptor = File::open(fixture.root.path()).unwrap();
    let old = enrollment();
    fixture
        .broker
        .send_guarded_listener(
            "old",
            old.clone(),
            [descriptor.as_fd(); 3],
            Box::new(move |result| {
                let _ = send.send(result);
            }),
        )
        .unwrap();
    let _handoff = settle(|complete| fixture.peer.receive(complete));
    thread::scope(|workers| {
        let broker = &fixture.broker;
        let closing_scope = old.clone();
        let close =
            workers.spawn(move || broker.close_sender_guard(closing_scope, Duration::from_secs(2)));
        let _closing = settle(|complete| fixture.peer.receive(complete));
        let response = ProtocolResponse {
            schema: RESPONSE_SCHEMA.into(),
            protocol_version: PROTOCOL_VERSION,
            request_id: "guard-close".into(),
            result: ResponseResult::SenderGuardClosed {
                enrollment: old.clone(),
            },
        };
        settle(|complete| fixture.peer.send(response.canonical_bytes(), complete));
        assert_eq!(close.join().unwrap(), Ok(()));
    });
    let cancelled = prepared.recv_timeout(Duration::from_millis(50));
    let (send, fresh) = mpsc::channel();
    let mut scope = old.clone();
    scope.scope.revision += 1;
    let accepted = fixture.broker.send_guarded_listener(
        "fresh",
        scope.clone(),
        [descriptor.as_fd(); 3],
        Box::new(move |result| {
            let _ = send.send(result);
        }),
    );
    if accepted.is_ok() {
        let _handoff = settle(|complete| fixture.peer.receive(complete));
        reply(&fixture.peer, "old", old);
        reply(&fixture.peer, "fresh", scope);
        assert_eq!(fresh.recv_timeout(Duration::from_secs(2)).unwrap(), Ok(()));
    }
    fixture.broker.close();
    assert!(
        matches!(cancelled, Ok(Err(_))),
        "closure must retire the held preparation callback"
    );
    assert_eq!(accepted, Ok(()));
}
