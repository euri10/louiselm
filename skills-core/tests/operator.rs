//! Authenticated read-only operator endpoint.
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "Bounded socket fixtures assert setup and outcomes."
)]

use louiselm_skills::broker::operator::{InspectError, OperatorServer, inspect};
use std::{os::unix::fs::PermissionsExt, thread, time::Duration};

#[path = "operator/dependencies.rs"]
mod dependencies;
#[path = "operator/provider_extension.rs"]
mod provider_extension;
#[path = "operator/waiver.rs"]
mod waiver;

fn private_root() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    root
}

#[test]
fn beads_client_requires_the_exact_requested_decision_in_its_reply() {
    use louiselm_skills::{
        Digest,
        beads_mutation::{
            BeadsControlDecision, BeadsInspection, BeadsInspectionDetail, BeadsMutationOutcome,
            BeadsMutationStatus, BeadsReconciliation, BeadsResolution,
        },
        broker::operator::beads_mutation,
    };
    for scenario in ["missing", "wrong-evidence", "wrong-conclusion", "confirmed"] {
        let root = private_root();
        let path = root.path().join("inspect.sock");
        let uid = rustix::process::geteuid().as_raw();
        let server = OperatorServer::bind(&path, uid).unwrap();
        let operation = "12345678-1234-4234-8234-123456789abc";
        let decision = BeadsControlDecision::Reconcile {
            outcome: BeadsReconciliation::NotApplied,
            evidence_digest: Digest::of(b"requested evidence").to_string(),
        };
        let worker =
            thread::spawn(move || {
                server
                .serve_once(
                    |_, _| Err(louiselm_skills::broker::operator::InspectError::UnknownSession),
                    |_, _| panic!("not Session lookup"),
                    |_| panic!("not conformance lookup"),
                    |_, _| panic!("not skill control"),
                    |id, received| {
                        assert!(received.is_some());
                        Ok(BeadsInspection {
                            operation_id: id.into(),
                            session_id: "session".into(),
                            run_id: "run".into(),
                            envelope_revision: 1,
                            detail: BeadsInspectionDetail::Mutation {
                                project_digest: Digest::of(b"project").to_string(),
                                request_digest: Digest::of(b"request").to_string(),
                                status: BeadsMutationStatus {
                                    request_id: "request".into(),
                                    operation_id: id.into(),
                                    outcome: BeadsMutationOutcome::Unknown,
                                },
                                resolution: (scenario != "missing").then(|| BeadsResolution {
                                    outcome: if scenario == "wrong-conclusion" {
                                        BeadsReconciliation::Applied
                                    } else {
                                        BeadsReconciliation::NotApplied
                                    },
                                    evidence_digest: Digest::of(if scenario == "wrong-evidence" {
                                        b"unrelated"
                                    } else {
                                        b"requested evidence"
                                    })
                                    .to_string(),
                                    operator_uid: uid,
                                    decided_at_ms: 1,
                                }),
                            },
                        })
                    },
                    |_, _| panic!("not retention control"),
                    |_, _, _| panic!("not waiver control"),
|_, _, _| Err(louiselm_skills::broker::provider_extension::ExtensionError::Unknown),
)
                .unwrap();
            });
        let result = beads_mutation(
            &path,
            uid,
            operation,
            Some(&decision),
            Duration::from_secs(2),
        );
        worker.join().unwrap();
        assert_eq!(
            result.is_ok(),
            scenario == "confirmed",
            "{scenario}: {result:?}"
        );
    }
}

#[test]
fn beads_operator_request_reaches_authenticated_control() {
    use std::io::{Read, Write};
    let root = private_root();
    let path = root.path().join("inspect.sock");
    let uid = rustix::process::geteuid().as_raw();
    let server = OperatorServer::bind(&path, uid).unwrap();
    let worker = thread::spawn(move || {
        server
            .serve_once(
                |_, _| Err(louiselm_skills::broker::operator::InspectError::UnknownSession),
                |_, _| panic!("not Session lookup"),
                |_| panic!("not conformance lookup"),
                |_, _| panic!("not skill control"),
                |id, decision| {
                    use louiselm_skills::beads_mutation::{
                        BeadsInspection, BeadsInspectionDetail, BeadsMutationOutcome,
                        BeadsMutationStatus,
                    };
                    assert!(decision.is_none());
                    Ok(BeadsInspection {
                        operation_id: id.into(),
                        session_id: "session".into(),
                        run_id: "run".into(),
                        envelope_revision: 1,
                        detail: BeadsInspectionDetail::Mutation {
                            project_digest: louiselm_skills::Digest::of(b"project").to_string(),
                            request_digest: louiselm_skills::Digest::of(b"request").to_string(),
                            status: BeadsMutationStatus {
                                request_id: "request".into(),
                                operation_id: id.into(),
                                outcome: BeadsMutationOutcome::Unknown,
                            },
                            resolution: None,
                        },
                    })
                },
                |_, _| panic!("unexpected retention control"),
                |_, _, _| panic!("not waiver control"),
                |_, _, _| Err(louiselm_skills::broker::provider_extension::ExtensionError::Unknown),
            )
            .unwrap();
    });
    let mut client = std::os::unix::net::UnixStream::connect(path).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut size = [0; 4];
    client.read_exact(&mut size).unwrap();
    let mut ready = vec![0; u32::from_be_bytes(size) as usize];
    client.read_exact(&mut ready).unwrap();
    let request = br#"{"schema":"louiselm.operator-beads/1","operation_id":"12345678-1234-4234-8234-123456789abc","decision":null}"#;
    client
        .write_all(&u32::try_from(request.len()).unwrap().to_be_bytes())
        .unwrap();
    client.write_all(request).unwrap();
    client.read_exact(&mut size).unwrap();
    let mut reply = vec![0; u32::from_be_bytes(size) as usize];
    client.read_exact(&mut reply).unwrap();
    let reply: serde_json::Value = serde_json::from_slice(&reply).unwrap();
    worker.join().unwrap();
    assert_eq!(
        reply["operation_id"], "12345678-1234-4234-8234-123456789abc",
        "{reply}"
    );
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
                |_, _| Err(louiselm_skills::broker::operator::InspectError::UnknownSession),
                |_, _| panic!("unauthenticated lookup"),
                |_| panic!("unauthenticated conformance lookup"),
                |_, _| panic!("unauthenticated skill control"),
                |_, _| panic!("unauthenticated Beads control"),
                |_, _| panic!("unexpected retention control"),
                |_, _, _| panic!("not waiver control"),
                |_, _, _| Err(louiselm_skills::broker::provider_extension::ExtensionError::Unknown),
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
                |_, _| Err(louiselm_skills::broker::operator::InspectError::UnknownSession),
                |id, _| {
                    assert_eq!(id, "session");
                    Err(InspectError::UnknownSession)
                },
                |_| panic!("unexpected conformance lookup"),
                |_, _| panic!("unexpected skill control"),
                |_, _| panic!("unexpected Beads control"),
                |_, _| panic!("unexpected retention control"),
                |_, _, _| panic!("not waiver control"),
                |_, _, _| Err(louiselm_skills::broker::provider_extension::ExtensionError::Unknown),
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
    let worker =
        thread::spawn(move || {
            for expected in [
                None,
                Some(SkillRequestOutcome::Rejected),
                Some(SkillRequestOutcome::Cancelled),
            ] {
                server
                .serve_once(
                    |_, _| Err(louiselm_skills::broker::operator::InspectError::UnknownSession),
                    |_, _| panic!("not Session inspection"),
                    |_| panic!("not conformance inspection"),
                    |operation, outcome| {
                        assert_eq!(operation, id);
                        assert_eq!(outcome, expected);
                        Ok(SkillRequestStatus {
                            request_id: "request".into(),
                            operation_id: operation.into(),
                            outcome: outcome.unwrap_or(SkillRequestOutcome::Pending),
                            packages: vec![louiselm_skills::Digest::of(b"skill").to_string()],
                            agents: vec!["codex".into()],
                            admission: None,
                        })
                    },
                    |_, _| panic!("unexpected Beads control"),
                    |_, _| panic!("unexpected retention control"),
                    |_, _, _| panic!("not waiver control"),
|_, _, _| Err(louiselm_skills::broker::provider_extension::ExtensionError::Unknown),
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
fn conformance_inspection_distinguishes_an_unknown_session() {
    use std::{io::Write, os::unix::net::UnixStream};
    let root = private_root();
    let path = root.path().join("inspect.sock");
    let uid = rustix::process::geteuid().as_raw();
    let server = OperatorServer::bind(&path, uid).unwrap();
    let worker = thread::spawn(move || {
        server
            .serve_once(
                |_, _| Err(louiselm_skills::broker::operator::InspectError::UnknownSession),
                |_, _| Err(InspectError::UnknownSession),
                |_| Err(InspectError::UnknownSession),
                |_, _| panic!("not a skill decision"),
                |_, _| panic!("not a Beads decision"),
                |_, _| panic!("not a retention decision"),
                |_, _, _| panic!("not waiver control"),
                |_, _, _| Err(louiselm_skills::broker::provider_extension::ExtensionError::Unknown),
            )
            .unwrap();
    });
    let mut stream = UnixStream::connect(&path).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    assert_eq!(read_frame(&mut stream), b"louiselm.operator/1");
    let request = br#"{"schema":"louiselm.operator-conformance/1","session_id":"unknown"}"#;
    stream
        .write_all(&u32::try_from(request.len()).unwrap().to_be_bytes())
        .unwrap();
    stream.write_all(request).unwrap();
    let reply = read_frame(&mut stream);
    worker.join().unwrap();
    assert_eq!(reply, InspectError::UnknownSession.canonical_bytes());
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
                    |_, _| Err(louiselm_skills::broker::operator::InspectError::UnknownSession),
                    |_, _| panic!("invalid lookup"),
                    |_| panic!("invalid conformance lookup"),
                    |_, _| panic!("invalid skill control"),
                    |_, _| panic!("invalid Beads control"),
                    |_, _| panic!("unexpected retention control"),
                    |_, _, _| panic!("not waiver control"),
                    |_, _, _| {
                        Err(louiselm_skills::broker::provider_extension::ExtensionError::Unknown)
                    },
                )
                .unwrap();
        }
        server
            .serve_once(
                |_, _| Err(louiselm_skills::broker::operator::InspectError::UnknownSession),
                |_, _| Err(InspectError::UnknownSession),
                |_| panic!("unexpected conformance lookup"),
                |_, _| panic!("unexpected skill control"),
                |_, _| panic!("unexpected Beads control"),
                |_, _| panic!("unexpected retention control"),
                |_, _, _| panic!("not waiver control"),
                |_, _, _| Err(louiselm_skills::broker::provider_extension::ExtensionError::Unknown),
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
