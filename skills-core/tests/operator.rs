//! Authenticated read-only operator endpoint.
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "Bounded socket fixtures assert setup and outcomes."
)]

use louiselm_skills::broker::operator::{InspectError, OperatorServer, inspect};
use std::{os::unix::fs::PermissionsExt, thread, time::Duration};

fn private_root() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    root
}

#[test]
fn wrong_uid_is_refused_before_session_lookup() {
    let root = private_root();
    let path = root.path().join("inspect.sock");
    let uid = rustix::process::geteuid().as_raw();
    let server = OperatorServer::bind(&path, uid + 1).unwrap();
    let worker = thread::spawn(move || {
        server
            .serve_once(
                |_, _| panic!("unauthenticated lookup"),
                |_, _| panic!("unauthenticated skill control"),
            )
            .unwrap();
    });
    assert_eq!(
        inspect(&path, uid, "session", Duration::from_secs(2)),
        Err(InspectError::AuthenticationRefused)
    );
    worker.join().unwrap();
}

#[test]
fn unknown_session_has_typed_error_and_client_checks_broker_identity() {
    let root = private_root();
    let path = root.path().join("inspect.sock");
    let uid = rustix::process::geteuid().as_raw();
    let server = OperatorServer::bind(&path, uid).unwrap();
    let worker = thread::spawn(move || {
        server
            .serve_once(
                |id, _| {
                    assert_eq!(id, "session");
                    Err(InspectError::UnknownSession)
                },
                |_, _| panic!("unexpected skill control"),
            )
            .unwrap();
    });
    assert_eq!(
        inspect(&path, uid, "session", Duration::from_secs(2)),
        Err(InspectError::UnknownSession)
    );
    worker.join().unwrap();
    assert_ne!(
        InspectError::UnknownSession.exit_code(),
        InspectError::BrokerUnavailable.exit_code()
    );
    assert_ne!(
        InspectError::AuthenticationRefused.exit_code(),
        InspectError::BrokerUnavailable.exit_code()
    );
    assert!(
        String::from_utf8(InspectError::UnknownSession.canonical_bytes())
            .unwrap()
            .contains("check_session_id")
    );
}

#[test]
fn client_refuses_foreign_broker_without_sending_subject() {
    let root = private_root();
    let path = root.path().join("inspect.sock");
    let uid = rustix::process::geteuid().as_raw();
    let _server = OperatorServer::bind(&path, uid).unwrap();
    assert_eq!(
        inspect(&path, uid + 1, "session", Duration::from_secs(2)),
        Err(InspectError::AuthenticationRefused)
    );
}

#[test]
fn malformed_subject_is_rejected_before_connecting() {
    for id in ["", "../secret", "two words"] {
        assert_eq!(
            inspect(
                std::path::Path::new("/nonexistent"),
                1,
                id,
                Duration::from_secs(1)
            ),
            Err(InspectError::InvalidRequest)
        );
    }
}

#[test]
fn skill_decisions_use_the_same_authenticated_operator_endpoint() {
    use louiselm_skills::{
        broker::operator::skill_request,
        skill_request::{SkillRequestOutcome, SkillRequestStatus},
    };
    let root = private_root();
    let path = root.path().join("operator.sock");
    let uid = rustix::process::geteuid().as_raw();
    let id = "12345678-1234-4234-8234-123456789abc";
    let server = OperatorServer::bind(&path, uid).unwrap();
    let worker = thread::spawn(move || {
        for expected in [
            None,
            Some(SkillRequestOutcome::Rejected),
            Some(SkillRequestOutcome::Cancelled),
        ] {
            server
                .serve_once(
                    |_, _| panic!("not Session inspection"),
                    |operation, outcome| {
                        assert_eq!(operation, id);
                        assert_eq!(outcome, expected);
                        Ok(SkillRequestStatus {
                            request_id: "request".into(),
                            operation_id: operation.into(),
                            outcome: outcome.unwrap_or(SkillRequestOutcome::Pending),
                        })
                    },
                )
                .unwrap();
        }
    });
    for outcome in [
        None,
        Some(SkillRequestOutcome::Rejected),
        Some(SkillRequestOutcome::Cancelled),
    ] {
        assert_eq!(
            skill_request(&path, uid, id, outcome, Duration::from_secs(2))
                .unwrap()
                .outcome,
            outcome.unwrap_or(SkillRequestOutcome::Pending)
        );
    }
    worker.join().unwrap();
    assert_eq!(
        skill_request(
            &path,
            uid,
            id,
            Some(SkillRequestOutcome::Pending),
            Duration::from_secs(1)
        ),
        Err(InspectError::InvalidRequest)
    );
}

fn read_frame(stream: &mut std::os::unix::net::UnixStream) -> Vec<u8> {
    use std::io::Read;
    let mut header = [0; 4];
    stream.read_exact(&mut header).unwrap();
    let mut bytes = vec![0; u32::from_be_bytes(header) as usize];
    stream.read_exact(&mut bytes).unwrap();
    bytes
}

#[test]
fn malformed_frames_never_lookup_and_do_not_stop_the_listener() {
    use std::{io::Write, net::Shutdown, os::unix::net::UnixStream};
    let root = private_root();
    let path = root.path().join("inspect.sock");
    let uid = rustix::process::geteuid().as_raw();
    let server = OperatorServer::bind(&path, uid).unwrap();
    let payloads = [
        br#"{"schema":"louiselm.operator-inspect/1","session_id":"session","uid":0}"#.as_slice(),
        br#"{"schema":"foreign/1","session_id":"session"}"#,
        br#"{"schema":"louiselm.operator-inspect/1","session_id":"../secret"}"#,
        b"{",
    ];
    let count = payloads.len() + 3;
    let worker = thread::spawn(move || {
        for _ in 0..count {
            server
                .serve_once(
                    |_, _| panic!("invalid lookup"),
                    |_, _| panic!("invalid skill control"),
                )
                .unwrap();
        }
        server
            .serve_once(
                |_, _| Err(InspectError::UnknownSession),
                |_, _| panic!("unexpected skill control"),
            )
            .unwrap();
    });
    let frames = payloads
        .into_iter()
        .map(|payload| {
            let mut frame = u32::try_from(payload.len()).unwrap().to_be_bytes().to_vec();
            frame.extend(payload);
            frame
        })
        .chain([
            0_u32.to_be_bytes().to_vec(),
            u32::MAX.to_be_bytes().to_vec(),
            vec![0, 0],
        ]);
    for frame in frames {
        let mut stream = UnixStream::connect(&path).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        assert_eq!(read_frame(&mut stream), b"louiselm.operator/1");
        stream.write_all(&frame).unwrap();
        stream.shutdown(Shutdown::Write).unwrap();
        assert_eq!(
            read_frame(&mut stream),
            InspectError::InvalidRequest.canonical_bytes()
        );
    }
    assert_eq!(
        inspect(&path, uid, "session", Duration::from_secs(2)),
        Err(InspectError::UnknownSession)
    );
    worker.join().unwrap();
}

#[test]
fn listener_preserves_live_and_foreign_paths_and_recovers_stale_socket() {
    use std::os::unix::{fs::symlink, net::UnixListener};
    let root = private_root();
    let path = root.path().join("inspect.sock");
    let uid = rustix::process::geteuid().as_raw();
    std::fs::write(&path, b"not a socket").unwrap();
    assert!(OperatorServer::bind(&path, uid).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"not a socket");
    std::fs::remove_file(&path).unwrap();
    symlink(root.path().join("foreign"), &path).unwrap();
    assert!(OperatorServer::bind(&path, uid).is_err());
    assert!(path.is_symlink());
    std::fs::remove_file(&path).unwrap();
    let active = UnixListener::bind(&path).unwrap();
    assert!(OperatorServer::bind(&path, uid).is_err());
    drop(active);
    let server = OperatorServer::bind(&path, uid).unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o666
    );
    drop(server);
    assert!(!path.exists());
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
    assert!(OperatorServer::bind(&path, uid).is_err());
}

#[test]
fn slow_peer_cannot_extend_the_clients_total_deadline() {
    use std::{io::Write, os::unix::net::UnixListener, time::Instant};
    let root = private_root();
    let path = root.path().join("inspect.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let worker = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut bytes = 19_u32.to_be_bytes().to_vec();
        bytes.extend(b"louiselm.operator/1");
        for byte in bytes {
            if stream.write_all(&[byte]).is_err() {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
    });
    let start = Instant::now();
    assert_eq!(
        inspect(
            &path,
            rustix::process::geteuid().as_raw(),
            "session",
            Duration::from_millis(80)
        ),
        Err(InspectError::BrokerUnavailable)
    );
    assert!(start.elapsed() < Duration::from_secs(2));
    worker.join().unwrap();
}
