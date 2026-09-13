//! Production broker/supervisor restoration and the measured synthetic load contract.
use super::*;
use crate::{
    broker::{
        cold_resume::{ColdLoadOutcome, WithheldCommand},
        lifecycle::LifecycleCaller,
    },
    launch_protocol::{LIFECYCLE_REQUEST_SCHEMA, LifecycleAction, LifecycleRequest},
    launch_receipt::SessionState,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ColdCase {
    Loaded,
    FailedLoad,
    UnavailableBalance,
}

fn target() -> LaunchRequest {
    LaunchRequest {
        request_id: "cold-request".into(),
        authorization_id: "cold-authorization".into(),
        session_id: "cold-target".into(),
        ..request()
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "One broker worker preserves allocation, restore, load observation and finalization ordering across the installed handshake."
)]
pub(super) fn broker_reconstruct(broker: &InstalledBroker, uid: u32, root: &Path) {
    let caller = LifecycleCaller::Operator { uid };
    let missing_balance = root.join("missing-balance").exists();
    if missing_balance {
        fs::write(
            root.join("state/audit/decisions.jsonl"),
            b"unreadable prior accounting\n",
        )
        .unwrap();
    }
    let allocated = broker
        .authorize_cold_resume("session", &target(), &caller)
        .unwrap();
    assert_eq!(
        broker
            .authorize_cold_resume("session", &target(), &caller)
            .unwrap(),
        allocated
    );
    if missing_balance {
        assert!(allocated.commands.is_none());
        assert_eq!(
            allocated.withheld_command,
            Some(WithheldCommand::AccountingUnavailable)
        );
    } else {
        assert!(matches!(
            allocated.commands.as_ref().unwrap().uses,
            None | Some(1)
        ));
    }
    println!("COLD_AUTHORIZED");
    let mut session = broker.serve_launch().unwrap();
    assert!(broker.admit_recovery("cold-target").is_err());
    // Base ACP work is available, but effects cannot precede successful load.
    assert!(!broker.step(&mut session).unwrap());
    let park = LifecycleRequest {
        schema: LIFECYCLE_REQUEST_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        request_id: "cold-park".into(),
        session_id: "cold-target".into(),
        run_id: "run".into(),
        authorization_id: "cold-park".into(),
        action: LifecycleAction::Park,
        expected_state: SessionState::Running,
        expected_receipt_sequence: Some(1),
        envelope_revision: 1,
    };
    broker
        .request_lifecycle(&mut session, &caller, &park)
        .unwrap();
    let restored = broker.restore_cold_resume(&mut session, &caller).unwrap();
    assert_eq!(
        broker.restore_cold_resume(&mut session, &caller).unwrap(),
        restored
    );
    let resume = LifecycleRequest {
        request_id: "cold-resume".into(),
        authorization_id: "cold-resume".into(),
        action: LifecycleAction::Resume,
        expected_state: SessionState::Parked,
        expected_receipt_sequence: Some(2),
        ..park
    };
    broker
        .request_lifecycle(&mut session, &caller, &resume)
        .unwrap();
    println!("COLD_LOAD_READY");
    let mut result = String::new();
    std::io::stdin().read_line(&mut result).unwrap();
    let outcome = if result == "loaded\n" {
        ColdLoadOutcome::Loaded {
            acp_session_id: "fixture-acp".into(),
        }
    } else {
        assert_eq!(result, "failed\n");
        ColdLoadOutcome::Failed
    };
    broker
        .finish_cold_resume(&mut session, &caller, &outcome)
        .unwrap();
    if outcome == ColdLoadOutcome::Failed {
        assert!(!session.channel().is_closed());
        assert!(
            broker
                .finish_cold_resume(
                    &mut session,
                    &caller,
                    &ColdLoadOutcome::Loaded {
                        acp_session_id: "fixture-acp".into()
                    }
                )
                .is_err()
        );
        while !broker.step(&mut session).unwrap() {}
        session.close();
        println!("COLD_FAILED");
        return;
    }
    broker
        .finish_cold_resume(&mut session, &caller, &outcome)
        .unwrap();
    broker.admit_recovery("cold-target").unwrap();
    println!("COLD_LOADED");
    while !broker.step(&mut session).unwrap() {}
    println!("COLD_TERMINAL");
}

#[expect(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "One installed fixture owns the broker, supervisor, controller relay and cleanup through successful and failed reconstruction."
)]
pub(super) fn controller_reconstruct(
    paths: &LauncherPaths,
    config: &LauncherConfig,
    registry: &Path,
    sessions: &Path,
    broker: &mut Child,
    lines: &mpsc::Receiver<String>,
    case: ColdCase,
    uncapped: bool,
) {
    let fail_load = case == ColdCase::FailedLoad;
    marker(lines, "COLD_AUTHORIZED");
    let mut platform =
        SystemLaunchPlatform::new(paths.clone(), config.clone(), Duration::from_secs(5)).unwrap();
    platform.registry_root = registry.to_owned();
    let supervisor = LaunchSupervisor::new(
        connect_control_broker(config, Duration::from_secs(5)).unwrap(),
        Arc::new(InstalledLaunchSigner::open(paths, Duration::from_secs(5)).unwrap()),
        Arc::new(platform),
        Arc::new(Registry::open_trusted(registry).unwrap()),
        sessions.to_owned(),
        Duration::from_secs(5),
    );
    let (tx, rx) = mpsc::channel();
    let now = u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    supervisor
        .launch(
            target(),
            config.operator_uid,
            now,
            Box::new(move |r| {
                tx.send(r).unwrap();
            }),
        )
        .unwrap();
    let session = rx.recv_timeout(Duration::from_secs(20)).unwrap().unwrap();
    let (mut input, relay_input) = std::os::unix::net::UnixStream::pair().unwrap();
    let (relay_output, output) = std::os::unix::net::UnixStream::pair().unwrap();
    output
        .set_read_timeout(Some(Duration::from_secs(15)))
        .unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let owner = thread::spawn(move || {
        let result = session.relay_stdio(
            RelayStdio::new(
                BufReader::new(fs::File::from(OwnedFd::from(relay_input))),
                fs::File::from(OwnedFd::from(relay_output)),
            )
            .unwrap(),
        );
        done_tx.send(result).unwrap();
    });
    let mut output = BufReader::new(output);
    let command = ToolExecutionRequest {
        schema: TOOL_EXECUTION_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        request_id: "before-load".into(),
        session_id: "cold-target".into(),
        run_id: "run".into(),
        envelope_revision: 1,
        sequence: 1,
        command: COMMAND.into(),
        timeout_ms: 5000,
    };
    let refused = exchange(&mut input, &mut output, &command.canonical_bytes());
    assert!(matches!(
        refused.operation,
        CommandOperation::Result {
            outcome: CommandOutcome::NotStarted { .. }
        }
    ));
    marker(lines, "COLD_LOAD_READY");
    input.write_all(b"\x1f").unwrap();
    let mut reply = String::new();
    output.read_line(&mut reply).unwrap();
    assert_eq!(reply, "\x1f\n");
    input
        .write_all(if fail_load {
            b"\x1dload wrong-acp\n"
        } else {
            b"\x1dload fixture-acp\n"
        })
        .unwrap();
    reply.clear();
    output.read_line(&mut reply).unwrap();
    if fail_load {
        assert_ne!(reply, format!("\x1d{}\n", b"positive-control\n".len()));
        broker
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"failed\n")
            .unwrap();
        marker(lines, "COLD_FAILED");
    } else {
        // The measured fixture counts each ordinary echoed byte.
        assert_eq!(reply, format!("\x1d{}\n", b"positive-control\n".len()));
        broker
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"loaded\n")
            .unwrap();
        marker(lines, "COLD_LOADED");
        for sequence in 1..=2 {
            let next = ToolExecutionRequest {
                request_id: format!("restored-{sequence}"),
                sequence,
                ..command.clone()
            };
            let response = exchange(&mut input, &mut output, &next.canonical_bytes());
            assert_eq!(
                matches!(
                    response.operation,
                    CommandOperation::Result {
                        outcome: CommandOutcome::Completed { .. }
                    }
                ),
                case != ColdCase::UnavailableBalance && (uncapped || sequence == 1),
                "remaining allocation cannot reset at finalize/retry"
            );
        }
    }
    drop(input);
    if !fail_load {
        marker(lines, "COLD_TERMINAL");
    }
    done_rx
        .recv_timeout(Duration::from_secs(15))
        .unwrap()
        .unwrap();
    owner.join().unwrap();
    crate::launcher_install::acquire_identity(paths, 1)
        .unwrap()
        .release()
        .unwrap();
}
