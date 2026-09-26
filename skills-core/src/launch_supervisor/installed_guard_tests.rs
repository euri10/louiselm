//! Disposable-VM composition of the installed Brokered launch handoff.

use super::*;
use crate::conformance::{
    ReportResult,
    admission::Enforcement,
    installed::{certify, measure},
};
use crate::{
    broker::{BrokerSession, lifecycle::LifecycleCaller},
    launch_protocol::{LIFECYCLE_REQUEST_SCHEMA, LifecycleAction, LifecycleRequest},
    launch_receipt::SessionState,
};

pub(super) fn approval(now_ms: u64) -> crate::provider_request::ApprovedProviderRequests {
    crate::provider_request::ApprovedProviderRequests {
        disclosure_profile: crate::provider_request::disclosure::profile_digest(),
        provider: "openai".into(),
        upstream: "https://api.openai.com/v1/responses".into(),
        addresses: vec!["192.0.2.1".parse().unwrap()],
        max_run_requests: 1,
        models: vec!["fixture-model".into()],
        max_effort: crate::provider_request::ReasoningEffort::Low,
        expires_at_ms: now_ms + 120_000,
    }
}

pub(super) fn park_and_dispose(broker: &InstalledBroker, session: &mut BrokerSession, uid: u32) {
    let caller = LifecycleCaller::Operator { uid };
    let request = |request_id: &str, action, state, sequence| LifecycleRequest {
        schema: LIFECYCLE_REQUEST_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        request_id: request_id.into(),
        session_id: "session".into(),
        run_id: "run".into(),
        authorization_id: format!("operator-{request_id}"),
        action,
        expected_state: state,
        expected_receipt_sequence: Some(sequence),
        envelope_revision: 1,
    };
    broker
        .request_lifecycle(
            session,
            &caller,
            &request(
                "guard-park",
                LifecycleAction::Park,
                SessionState::Running,
                1,
            ),
        )
        .unwrap();
    while broker.inspect("session").unwrap().unwrap().state != SessionState::Parked {
        assert!(!broker.step(session).unwrap());
    }
    println!("BROKER_PARKED");
    assert!(
        broker
            .request_lifecycle(
                session,
                &caller,
                &request(
                    "guard-stale-resume",
                    LifecycleAction::Resume,
                    SessionState::Parked,
                    2,
                ),
            )
            .is_err()
    );
    broker
        .request_lifecycle(
            session,
            &caller,
            &request(
                "guard-dispose",
                LifecycleAction::Disposal,
                SessionState::Parked,
                2,
            ),
        )
        .unwrap();
    assert_eq!(
        broker.inspect("session").unwrap().unwrap().state,
        SessionState::Terminal
    );
    println!("BROKER_TERMINAL");
}

#[test]
fn privileged_installed_brokered_guard_start_and_disposal() {
    installed_guard_case(false, false, false);
}

#[test]
fn privileged_installed_brokered_guard_lost_close_ack_poison() {
    installed_guard_case(true, false, false);
}

#[test]
fn privileged_installed_brokered_guard_park_closes_before_receipt() {
    installed_guard_case(false, true, false);
}

#[test]
fn privileged_installed_brokered_guard_broker_outage_poison() {
    installed_guard_case(false, false, true);
}

#[expect(
    clippy::too_many_lines,
    reason = "One disposable fixture owns the certificate, non-root broker, launch and terminal containment."
)]
fn installed_guard_case(hold_close_ack: bool, park: bool, broker_outage: bool) {
    if std::env::var_os("LOUISELM_REQUIRE_BROKER_GUARD").is_none() {
        eprintln!("requires disposable root and a current installed guard certificate");
        return;
    }
    assert!(rustix::process::geteuid().is_root());
    let _account = BrokerAccount::create();
    let root = tempfile::Builder::new()
        .prefix("louiselm-broker-guard-")
        .tempdir_in("/var/lib")
        .unwrap();
    let (paths, mut config, registry_root) = install_fixture_with_slots(root.path(), 3);
    fs::write(root.path().join("brokered"), b"").unwrap();
    if hold_close_ack {
        fs::write(root.path().join("guard-close-no-ack"), b"").unwrap();
    }
    if park {
        fs::write(root.path().join("guard-park"), b"").unwrap();
    }
    registry_record(
        &registry_root.join("envelopes.json"),
        &serde_json::json!([{
            "id":"envelope","network":"brokered","description":"guarded fixture"
        }]),
    );
    config.conformance = Enforcement::Enforced;
    write_json(&paths.state_root.join("config.json"), &config);
    write_json(&paths.state_root.join("public-config.json"), &config);
    let parent = rustix::process::getppid()
        .unwrap()
        .as_raw_nonzero()
        .get()
        .cast_unsigned();
    measure(&paths, &config, Instant::now() + Duration::from_mins(3)).unwrap();
    let certificate = certify(&paths, Instant::now() + Duration::from_mins(3), parent).unwrap();
    assert_eq!(
        certificate.observations.result().unwrap(),
        ReportResult::Passed,
        "{:?}",
        certificate.observations
    );

    let (mut broker_child, lines) = broker_process(root.path(), None);
    marker(&lines, "BROKER_READY");
    let mut platform =
        SystemLaunchPlatform::new(paths.clone(), config.clone(), Duration::from_secs(5)).unwrap();
    platform.registry_root = registry_root.clone();
    platform.guarded_start_allowed = true;
    let sessions = root.path().join("sessions");
    fs::create_dir(&sessions).unwrap();
    fs::set_permissions(&sessions, fs::Permissions::from_mode(0o711)).unwrap();
    let supervisor = LaunchSupervisor::new(
        connect_control_broker(&config, Duration::from_secs(5)).unwrap(),
        Arc::new(InstalledLaunchSigner::open(&paths, Duration::from_secs(5)).unwrap()),
        Arc::new(platform),
        Arc::new(Registry::open_trusted(&registry_root).unwrap()),
        sessions,
        Duration::from_secs(5),
    );
    let (sent, result) = mpsc::channel();
    let now_ms = u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    supervisor
        .launch(
            request(),
            config.operator_uid,
            now_ms,
            Box::new(move |outcome| sent.send(outcome).unwrap()),
        )
        .unwrap();
    let session = result
        .recv_timeout(Duration::from_secs(30))
        .unwrap()
        .unwrap();
    marker(&lines, "BROKER_GUARD_ACKED");
    let agent_pid: u32 = marker(&lines, "BROKER_RUNNING ")
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    if hold_close_ack {
        marker(&lines, "BROKER_GUARD_HELD");
    }
    if broker_outage {
        broker_child.0.kill().unwrap();
        assert!(!broker_child.0.wait().unwrap().success());
    }
    let (_input, controller_input) = std::os::unix::net::UnixStream::pair().unwrap();
    let (controller_output, _output) = std::os::unix::net::UnixStream::pair().unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let relay = RelayStdio::new(
            BufReader::new(fs::File::from(OwnedFd::from(controller_input))),
            fs::File::from(OwnedFd::from(controller_output)),
        )
        .unwrap();
        let _ = done.send(session.relay_stdio(relay));
    });
    if !park {
        rustix::process::kill_process(
            rustix::process::Pid::from_raw(i32::try_from(agent_pid).unwrap()).unwrap(),
            rustix::process::Signal::TERM,
        )
        .unwrap();
    }
    let outcome = finished.recv_timeout(Duration::from_secs(20)).unwrap();
    if hold_close_ack || broker_outage {
        assert_eq!(outcome.err(), Some(SupervisorError::CleanupUnproven));
        if hold_close_ack {
            marker(&lines, "BROKER_NO_TERMINAL");
        }
    } else {
        outcome.unwrap();
        if park {
            marker(&lines, "BROKER_PARKED");
        }
        marker(&lines, "BROKER_TERMINAL");
    }
    if !broker_outage {
        assert!(broker_child.0.wait().unwrap().success());
    }
    if hold_close_ack || broker_outage {
        assert!(matches!(
            crate::launcher_install::acquire_identity(&paths, 0),
            Err(crate::launcher_install::LauncherError::Poisoned { slot: 0 })
        ));
    } else {
        crate::launcher_install::acquire_identity(&paths, 0)
            .unwrap()
            .release()
            .unwrap();
    }
}
