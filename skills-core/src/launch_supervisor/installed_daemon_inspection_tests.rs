//! Real operator executable and Agent self-status on the installed daemon.

use super::*;
use crate::{
    broker::operator::InspectError,
    launch_protocol::{COMMAND_SCHEMA, SessionStatus},
};

pub(super) fn provision() {
    let directory = Path::new("/run/louiselm-operator");
    fs::create_dir(directory).unwrap();
    chown(directory, Some(BROKER_UID), Some(BROKER_UID)).unwrap();
    fs::set_permissions(directory, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn privileged_activated_daemon_refuses_foreign_inspection() {
    if std::env::var_os("LOUISELM_REQUIRE_CONTROL_DAEMON").is_none() {
        eprintln!("skipping: requires disposable VM and private mount namespace");
        return;
    }
    assert!(rustix::process::geteuid().is_root());
    let started = Instant::now();
    let root = tempfile::Builder::new()
        .prefix("louiselm-daemon-inspection-")
        .tempdir_in("/var/lib")
        .unwrap();
    mounts(root.path());
    let _account = BrokerAccount::create();
    let (_, config, manager) = install_daemon(root.path());
    let mut daemon = process(&manager, BROKER_UID, false);
    ready(&config);
    refusal(config.operator_uid, "unknown", InspectError::UnknownSession);
    for uid in [0, AGENT_UID, BROKER_UID] {
        refusal(uid, "session", InspectError::AuthenticationRefused);
        refusal(uid, "unknown", InspectError::AuthenticationRefused);
        waiver_refusal(
            uid,
            "unknown",
            crate::broker::waiver::WaiverError::WrongOperator,
        );
    }
    waiver_refusal(
        config.operator_uid,
        "unknown",
        crate::broker::waiver::WaiverError::Unknown,
    );
    terminate(&mut daemon);
    eprintln!("daemon: inspection refusals at {:?}", started.elapsed());
}

fn command(uid: u32, id: &str) -> std::process::Output {
    session_command(uid, "inspect", id)
}

fn session_command(uid: u32, verb: &str, id: &str) -> std::process::Output {
    control_command(uid, "session", verb, id)
}

fn control_command(uid: u32, group: &str, verb: &str, id: &str) -> std::process::Output {
    Command::new("/usr/bin/setpriv")
        .args([
            "--reuid",
            &uid.to_string(),
            "--regid",
            &uid.to_string(),
            "--clear-groups",
        ])
        .arg("/usr/local/lib/louiselm/current/bin/louiselm-control")
        .args([group, verb, id, "--json"])
        .env_clear()
        .output()
        .unwrap()
}

fn waiver_refusal(uid: u32, id: &str, error: crate::broker::waiver::WaiverError) {
    let output = control_command(uid, "waiver", "inspect", id);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(output.stdout.is_empty());
    let actual: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(
        actual,
        serde_json::json!({
            "schema": "louiselm.conformance-waiver-error/1", "error": error,
            "next_action": error.next_action(),
        })
    );
}

pub(super) fn refusal(uid: u32, id: &str, error: InspectError) {
    let output = command(uid, id);
    assert_eq!(
        output.status.code(),
        Some(i32::from(error.exit_code())),
        "{output:?}"
    );
    assert!(output.stdout.is_empty());
    assert_eq!(output.stderr, error.canonical_bytes());
    if error != InspectError::StatusUnavailable {
        let evidence = session_command(uid, "conformance", id);
        assert_eq!(
            evidence.status.code(),
            Some(i32::from(error.exit_code())),
            "{evidence:?}"
        );
        assert!(evidence.stdout.is_empty());
        assert_eq!(evidence.stderr, error.canonical_bytes());
    }
}

pub(super) fn status(config: &LauncherConfig, id: &str) -> SessionStatus {
    let deadline = Instant::now() + Duration::from_secs(10);
    let output = loop {
        let output = command(config.operator_uid, id);
        if output.status.success() {
            break output;
        }
        assert!(
            Instant::now() < deadline,
            "Session must become queryable: {output:?}"
        );
        assert_eq!(
            output.status.code(),
            Some(i32::from(InspectError::StatusUnavailable.exit_code()))
        );
        thread::sleep(Duration::from_millis(20));
    };
    assert!(output.stderr.is_empty());
    let status = SessionStatus::parse_canonical(&output.stdout).unwrap();
    assert_eq!(status.canonical_bytes(), output.stdout);
    assert_eq!(status.session_id, id);
    assert_eq!(status.posture.dimensions.len(), 6);
    assert!(!status.allowed_actions.is_empty());
    let evidence_output = session_command(config.operator_uid, "conformance", id);
    assert!(evidence_output.status.success(), "{evidence_output:?}");
    assert!(evidence_output.stderr.is_empty());
    let evidence = crate::broker::conformance_inspection::ConformanceInspection::parse_canonical(
        &evidence_output.stdout,
    )
    .unwrap();
    assert_eq!(evidence.session_id, id);
    assert_eq!(evidence.admission, status.conformance_admission);
    assert!(evidence.report.is_none());
    assert!(evidence.waiver.is_none());
    assert!(evidence.last_check.is_none());
    waiver_refusal(
        config.operator_uid,
        id,
        crate::broker::waiver::WaiverError::Unattended,
    );
    let text = String::from_utf8(output.stdout).unwrap();
    for forbidden in [
        "assigned_uid",
        "assigned_gid",
        "\"pid\"",
        "\"path\"",
        "environment",
        "prompt",
        "occupancy",
        "\"slot\"",
        "/var/",
        "/run/",
    ] {
        assert!(!text.contains(forbidden), "operator-only field {forbidden}");
    }
    status
}

pub(super) fn agent(session: LaunchedSession, config: &LauncherConfig) {
    let id = session.receipt().payload.session_id.clone();
    let (mut input, controller_input) = std::os::unix::net::UnixStream::pair().unwrap();
    let (controller_output, output) = std::os::unix::net::UnixStream::pair().unwrap();
    output
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let worker = thread::spawn(move || {
        session.relay_stdio(
            RelayStdio::new(
                BufReader::new(fs::File::from(OwnedFd::from(controller_input))),
                fs::File::from(OwnedFd::from(controller_output)),
            )
            .unwrap(),
        )
    });
    let mut output = BufReader::new(output);
    let mut query = CommandMessage {
        schema: COMMAND_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        request_id: "self-status".into(),
        session_id: id.clone(),
        run_id: "run".into(),
        envelope_revision: 1,
        operation: CommandOperation::StatusRequest {},
    };
    let reply = exchange(&mut input, &mut output, &query.canonical_bytes());
    let CommandOperation::StatusResult {
        status: self_status,
    } = reply.operation
    else {
        panic!("expected Agent status, got {reply:?}");
    };
    assert!(self_status.allowed_actions.is_empty());
    let mut operator = status(config, &id);
    operator.allowed_actions.clear();
    assert_eq!(operator.canonical_bytes(), self_status.canonical_bytes());
    for foreign in ["session", "unknown-session"] {
        query.session_id = foreign.into();
        let reply = exchange(&mut input, &mut output, &query.canonical_bytes());
        assert_eq!(reply.session_id, id);
        assert!(matches!(
            reply.operation,
            CommandOperation::StatusRefused {
                error: crate::launch_protocol::ErrorCode::SubjectMismatch
            }
        ));
    }
    drop(input);
    worker.join().unwrap().unwrap();
}
