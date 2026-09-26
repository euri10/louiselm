//! Deterministic fixture driver; invoked only by the disposable-VM gate.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Test fixtures abort on setup failure and assert exact refusals."
)]

use super::handoff::HandoffStage;
use super::*;
use crate::launch_protocol::{PROTOCOL_VERSION, ProtocolResponse, RESPONSE_SCHEMA, ResponseResult};
use crate::launch_transport::{CredentialPin, KernelCredentials, SeqpacketConnector};
use serde_json::{Value, json};
use std::{
    io::{BufRead, IoSlice, Write},
    os::unix::{fs::PermissionsExt, net::UnixListener, process::CommandExt},
    path::Path,
    sync::mpsc,
};

fn emit(value: &Value) {
    println!("GUARD_FIXTURE {value}");
    std::io::stdout().flush().unwrap();
}

fn read(lines: &mut impl Iterator<Item = std::io::Result<String>>) -> Value {
    serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap()
}

pub(super) fn number(value: &Value, key: &str) -> u32 {
    value[key].as_u64().unwrap().try_into().unwrap()
}

#[test]
fn invalid_scope_is_refused_before_any_platform_effect() {
    for (session, run, revision, deadline) in [
        ("", "run", 1, u64::MAX),
        ("session", "run", 0, u64::MAX),
        ("session", "run", 1, 1),
    ] {
        assert_eq!(
            validate_scope(&GuardScope {
                session_id: session.into(),
                run_id: run.into(),
                revision,
                deadline_ns: deadline
            }),
            Err(GuardError::Authority)
        );
    }
}

#[test]
fn enrollment_response_round_trips_and_rejects_malformed_scope() {
    let mut response = ProtocolResponse {
        schema: RESPONSE_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        request_id: "enrolled".into(),
        result: ResponseResult::SenderGuardEnrolled {
            enrollment: GuardEnrollment {
                scope: GuardScope {
                    session_id: "s".into(),
                    run_id: "r".into(),
                    revision: 1,
                    deadline_ns: 100,
                },
                guard_id: 1,
                runtime_pid: 2,
                broker_pid: 3,
                address: "127.0.0.1:12345".parse().unwrap(),
                listener_cookie: 1,
                network_id: 1,
            },
        },
    };
    response.validate().unwrap();
    assert_eq!(
        serde_json::from_slice::<ProtocolResponse>(&response.canonical_bytes()).unwrap(),
        response
    );
    if let ResponseResult::SenderGuardEnrolled { enrollment } = &mut response.result {
        enrollment.scope.revision = 0;
    }
    assert_eq!(
        response.validate().unwrap_err().code,
        crate::launch_protocol::ErrorCode::InvalidRequest
    );
}

#[test]
#[ignore = "requires scripts/test-sender-guard-loader.py in disposable KVM"]
fn production_loader_worker() {
    assert_eq!(
        std::process::Command::new("systemd-detect-virt")
            .arg("--vm")
            .output()
            .unwrap()
            .stdout,
        b"kvm\n"
    );
    assert!(rustix::process::geteuid().is_root());
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    let setup = read(&mut lines);
    let connector = SeqpacketConnector::new().unwrap();
    let (sent, ready) = mpsc::channel();
    connector
        .connect(
            Path::new(setup["broker"].as_str().unwrap()),
            CredentialPin::Identity {
                uid: 4_020_010,
                gid: 4_020_010,
            },
            Box::new(move |result| sent.send(result).unwrap()),
        )
        .unwrap();
    let broker = ready.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
    let current = super::launch_fixture::launch(&broker, &setup);
    let now = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    let scope = GuardScope {
        session_id: setup["session"].as_str().unwrap().into(),
        run_id: current
            .as_ref()
            .map_or_else(|| "fixture-run".into(), |status| status.run_id.clone()),
        revision: current
            .as_ref()
            .map_or(1, |status| status.envelope_revision),
        deadline_ns: u64::try_from(now.tv_sec).unwrap() * 1_000_000_000
            + u64::try_from(now.tv_nsec).unwrap()
            + setup["deadline_ms"].as_u64().unwrap_or(300_000) * 1_000_000,
    };
    let mut guard = SenderGuard::load(scope.clone(), broker).unwrap();
    let pid = number(&setup, "runtime");
    let network = File::open(format!("/proc/{pid}/ns/net")).unwrap();
    let endpoint = guard
        .bind_in_namespace(&network, "127.0.0.1:0".parse().unwrap())
        .unwrap();
    // The driver owns and has waited for this exact stopped child. Construct
    // the same measured lifetime pin used at the production exec stop.
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
    assert!(status.lines().any(|line| line.starts_with("State:\tT")));
    let pin = pidfd_open(
        Pid::from_raw(i32::try_from(pid).unwrap()).unwrap(),
        PidfdFlags::empty(),
    )
    .unwrap();
    let executable = File::open(setup["executable"].as_str().unwrap()).unwrap();
    let runtime = KernelProcess::from_exec_stop(
        KernelCredentials {
            pid,
            uid: number(&setup, "uid"),
            gid: number(&setup, "uid"),
        },
        pin,
        &executable,
    )
    .unwrap();
    assert_eq!(guard.activate(&scope), Err(GuardError::Enrollment));
    guard.enroll_at_exec_stop(&runtime).unwrap();
    assert!(
        runtime.valid().unwrap(),
        "enrolled runtime must retain its kernel identity proof"
    );
    assert_eq!(guard.activate(&scope), Err(GuardError::Enrollment));
    assert_eq!(
        guard.enroll_at_exec_stop(&runtime),
        Err(GuardError::Enrollment)
    );
    let ids: Vec<_> = guard
        .maps
        .values()
        .map(|map| map.info().unwrap().info.id)
        .collect();
    if setup["invalid_handoff"].as_bool().unwrap_or(false) {
        guard.endpoint.as_mut().unwrap().rule.listener += 1;
        assert!(matches!(
            guard.announce_enrollment("guard-enrolled", Duration::from_secs(5)),
            Err(GuardError::Lost)
        ));
        assert!(!guard.announced);
        emit(&json!({"refused":true,"maps":ids}));
        return;
    }
    guard
        .announce_enrollment("guard-enrolled", Duration::from_secs(5))
        .unwrap();
    emit(&json!({"ready":true,"port":endpoint.socket.local_addr().unwrap().port(),"maps":ids}));
    serve_actions(&mut guard, &runtime, scope, lines, current.as_ref());
}

#[expect(
    clippy::too_many_lines,
    reason = "One fixture opcode dispatcher keeps the externally driven lifecycle sequence visible."
)]
fn serve_actions(
    guard: &mut SenderGuard,
    runtime: &KernelProcess,
    mut scope: GuardScope,
    mut lines: impl Iterator<Item = std::io::Result<String>>,
    current: Option<&crate::launch_protocol::SupervisorStatus>,
) {
    while let Some(line) = lines.next() {
        let action: Value = serde_json::from_str(&line.unwrap()).unwrap();
        match action["op"].as_str().unwrap() {
            "status" => {
                super::launch_fixture::status(&guard.broker, current.unwrap());
                emit(&json!({"status":true}));
            }
            "revoke" => {
                guard.revoke().unwrap();
                assert_eq!(guard.activate(&scope), Err(GuardError::Enrollment));
                emit(&json!({"revoked":true}));
            }
            "activate" => {
                guard.activate(&scope).unwrap();
                emit(&json!({"activated":true}));
            }
            "connect" => {
                handoff_socket(guard, &scope, &action);
            }
            "transfer" => {
                emit(&json!({"handoff":true}));
                let result = guard.handoff_upstream(
                    &scope,
                    action["address"].as_str().unwrap().parse().unwrap(),
                    "guard-upstream",
                    Duration::from_secs(5),
                );
                if action["refused"].as_bool().unwrap_or(false) {
                    assert!(result.is_err());
                    assert!(
                        guard
                            .map("policy")
                            .unwrap()
                            .lookup(
                                &guard.endpoint.as_ref().unwrap().rule.port.to_ne_bytes(),
                                MapFlags::ANY,
                            )
                            .unwrap()
                            .is_none(),
                        "failed handoff retained active endpoint policy"
                    );
                    assert_eq!(guard.dispose(), Err(GuardError::Cleanup));
                    emit(&json!({"refused":true}));
                    continue;
                }
                let cookie = result.unwrap();
                emit(&json!({"connected":true,"cookie":cookie}));
            }
            "transfer-race" => {
                let revoker = guard.revoker().unwrap();
                let destination = action["address"].as_str().unwrap().parse().unwrap();
                let stage = match action["stage"].as_str().unwrap() {
                    "connect" => HandoffStage::Connected,
                    "queue" => HandoffStage::Queued,
                    "ack" => HandoffStage::AckWait,
                    other => panic!("unknown handoff stage {other}"),
                };
                std::thread::scope(|threads| {
                    let (entered, ready) = mpsc::sync_channel(1);
                    let (resume, released) = mpsc::sync_channel(1);
                    let transfer_scope = scope.clone();
                    let guard_ref = &mut *guard;
                    let transfer = threads.spawn(move || {
                        guard_ref.handoff_upstream_with(
                            &transfer_scope,
                            destination,
                            "guard-upstream",
                            Duration::from_secs(5),
                            move |at| {
                                if at == stage {
                                    entered.send(()).unwrap();
                                    released.recv().unwrap();
                                }
                            },
                        )
                    });
                    ready.recv_timeout(Duration::from_secs(5)).unwrap();
                    emit(&json!({"held":true}));
                    let command = read(&mut lines);
                    assert_eq!(command["op"], "revoke");
                    revoker.revoke().unwrap();
                    emit(&json!({"revoked":true}));
                    resume.send(()).unwrap();
                    assert!(transfer.join().unwrap().is_err());
                    emit(&json!({"refused":true}));
                });
            }
            "protected" => {
                assert_eq!(
                    std::fs::remove_file("/sys/fs/bpf/endpoint_send")
                        .unwrap_err()
                        .raw_os_error(),
                    Some(30)
                );
                for name in ["tasks", "owners", "lost", "ports"] {
                    let map = guard.map(name).unwrap();
                    let key = vec![0; map.key_size().try_into().unwrap()];
                    let value = vec![0; map.value_size().try_into().unwrap()];
                    assert_eq!(
                        map.update(&key, &value, MapFlags::ANY).unwrap_err().kind(),
                        libbpf_rs::ErrorKind::PermissionDenied
                    );
                }
                emit(&json!({"protected":true}));
            }
            "stale" => {
                let mut stale = scope.clone();
                stale.session_id = "other".into();
                assert_eq!(guard.activate(&stale), Err(GuardError::Authority));
                stale = scope.clone();
                stale.run_id = "other".into();
                assert_eq!(guard.activate(&stale), Err(GuardError::Authority));
                stale = scope.clone();
                stale.revision += 1;
                assert_eq!(guard.activate(&stale), Err(GuardError::Authority));
                emit(&json!({"stale_refused":true}));
            }
            "revise" => {
                let stale = guard.revoker().unwrap();
                scope.revision += 1;
                // Deadline authorization belongs to broker policy. The loader
                // binds the replacement scope, not an invented policy ceiling.
                scope.deadline_ns += 1_000_000_000;
                guard.revise(scope.clone()).unwrap();
                assert_eq!(guard.activate(&scope), Err(GuardError::Enrollment));
                guard
                    .announce_enrollment("guard-revised", Duration::from_secs(5))
                    .unwrap();
                assert_eq!(stale.revoke(), Err(GuardError::Authority));
                emit(&json!({"revised":true}));
            }
            "lost" => {
                let deadline = std::time::Instant::now() + Duration::from_secs(3);
                while guard.live().is_ok() && std::time::Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(1));
                }
                assert_eq!(guard.activate(&scope), Err(GuardError::Lost));
                assert!(!runtime.valid().unwrap());
                assert_eq!(
                    guard.enroll_at_exec_stop(runtime),
                    Err(GuardError::Enrollment)
                );
                emit(&json!({"lost":true}));
            }
            "retire" => {
                let cookie = action["cookie"].as_u64().unwrap();
                guard.retire_upstream(&scope, cookie).unwrap();
                assert_eq!(
                    guard.retire_upstream(&scope, cookie),
                    Err(GuardError::Socket)
                );
                emit(&json!({"retired":true}));
            }
            "exec" => {
                let error = std::process::Command::new("/bin/sleep").arg("60").exec();
                panic!("exec failed: {error}");
            }
            "close" => {
                guard.dispose().unwrap();
                guard.dispose().unwrap();
                assert!(matches!(
                    guard.activate(&scope),
                    Err(GuardError::Enrollment | GuardError::Lost)
                ));
                break;
            }
            other => panic!("unknown test operation {other}"),
        }
    }
}

fn handoff_socket(guard: &mut SenderGuard, scope: &GuardScope, action: &Value) {
    let socket = guard
        .connect_upstream(
            scope,
            action["address"].as_str().unwrap().parse().unwrap(),
            Duration::from_secs(3),
        )
        .unwrap();
    let path = action["handoff"].as_str().unwrap();
    let listener = UnixListener::bind(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o666)).unwrap();
    emit(&json!({"handoff":true}));
    let (channel, _) = listener.accept().unwrap();
    let peer = rustix::net::sockopt::socket_peercred(&channel).unwrap();
    assert_eq!(
        KernelCredentials::from(peer),
        guard.broker.peer_credentials()
    );
    let fds = socket.descriptors();
    let mut space = [std::mem::MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(3))];
    let mut ancillary = rustix::net::SendAncillaryBuffer::new(&mut space);
    assert!(ancillary.push(rustix::net::SendAncillaryMessage::ScmRights(&fds)));
    rustix::net::sendmsg(
        &channel,
        &[IoSlice::new(b"G")],
        &mut ancillary,
        rustix::net::SendFlags::NOSIGNAL,
    )
    .unwrap();
    emit(&json!({"connected":true,"cookie":socket.cookie().unwrap()}));
}
