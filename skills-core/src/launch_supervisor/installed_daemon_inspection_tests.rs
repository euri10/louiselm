//! Real operator executable and Agent self-status on the installed daemon.

use super::*;
use crate::{
    broker::operator::InspectError,
    launch_protocol::{COMMAND_SCHEMA, SessionStatus},
};

#[path = "installed_operator_lifecycle_tests.rs"]
mod lifecycle;

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
    assert_eq!(output.stdout, [] as [u8; 0]);
    let actual: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(
        actual,
        serde_json::json!({
            "schema": "louiselm.conformance-waiver-error/1", "error": error,
            "next_action": error.next_action(),
        })
    );
}

#[test]
fn privileged_initial_waiver_prepares_approves_then_launches() {
    use crate::{
        broker::waiver::Outcome,
        conformance::{admission::Enforcement, preparation::Preparation},
    };
    if std::env::var_os("LOUISELM_REQUIRE_CONTROL_DAEMON").is_none() {
        return;
    }
    assert!(rustix::process::geteuid().is_root());
    let root = tempfile::Builder::new()
        .prefix("louiselm-waiver-")
        .tempdir_in("/var/lib")
        .unwrap();
    mounts(root.path());
    let _account = BrokerAccount::create();
    let (paths, config, manager) = install_daemon_with(root.path(), Enforcement::Enforced);
    drop(
        crate::conformance::installed::CertificateStore::open(
            &paths.state_root.join("conformance"),
        )
        .unwrap(),
    );
    let mut daemon = process(&manager, BROKER_UID, false);
    ready(&config);
    let output = preparation_command(&config);
    let os = fs::read_to_string("/usr/lib/os-release").unwrap();
    if !os.lines().any(|line| line == "ID=debian")
        || !os.lines().any(|line| line == "VERSION_ID=\"13\"")
    {
        assert!(
            !output.status.success(),
            "unsupported host must refuse preparation"
        );
        assert!(!paths.state_root.join("pre-admission/session.json").exists());
        terminate(&mut daemon);
        eprintln!("unsupported host refused; no positive initial-waiver acceptance claimed");
        return;
    }
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(output.stdout, [] as [u8; 0]);
    assert_eq!(output.stderr, b"louiselm-launch: conformance preparation refused; restore host evidence or inspect launcher policy\n");
    assert!(!paths.state_root.join("pre-admission/session.json").exists());
    incomplete_guard_certificate(&paths);
    let output = preparation_command(&config);
    assert!(output.status.success(), "{output:?}");
    let observation: Preparation = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        observation.condition,
        crate::conformance::admission::Condition::Stale
    );
    assert_preparation_boundaries(&paths, &config, &observation);
    assert!(!Path::new(STATE).join("receipts/sessions/session").exists());
    let proposal = serde_json::json!({"request_id":"review-initial", "condition":"stale", "rationale":"Inspect this disposable host", "expires_at_ms":observation.observed_at_ms + 10_000});
    let plan = waiver_command(
        &config,
        &["plan", "session", "--json"],
        Some(&serde_json::to_vec(&proposal).unwrap()),
    );
    assert!(plan.status.success(), "{plan:?}");
    let outcome: Outcome = serde_json::from_slice(&plan.stdout).unwrap();
    assert!(!outcome.active);
    let plan = outcome.plan.unwrap();
    let applied = waiver_command(&config, &["apply", "session", &plan.digest, "--json"], None);
    assert!(applied.status.success(), "{applied:?}");
    let approved: Outcome = serde_json::from_slice(&applied.stdout).unwrap();
    assert!(approved.active);
    assert!(!Path::new(STATE).join("receipts/sessions/session").exists());
    let session = launch(&paths, &config, root.path(), "session").unwrap();
    assert_eq!(session.receipt().payload.sequence, 1);
    let evidence = assert_waived_report_binding(&config, &approved.receipt.unwrap().digest);
    attention::assert_waiver_expiry_projects_without_reads(&session);
    terminate(&mut daemon);
    let mut restarted = process(&manager, BROKER_UID, false);
    ready(&config);
    let historical = session_command(config.operator_uid, "conformance", "session");
    assert!(historical.status.success(), "{historical:?}");
    let retained: serde_json::Value = serde_json::from_slice(&historical.stdout).unwrap();
    for field in ["report", "admission", "waiver"] {
        assert_eq!(
            retained[field], evidence[field],
            "restart must retain exact {field}"
        );
    }
    session.dispose().unwrap();
    attention::assert_posture_cleared("session");
    terminate(&mut restarted);
}

fn assert_waived_report_binding(
    config: &LauncherConfig,
    receipt_digest: &str,
) -> serde_json::Value {
    let historical = session_command(config.operator_uid, "conformance", "session");
    assert!(historical.status.success(), "{historical:?}");
    let evidence: serde_json::Value = serde_json::from_slice(&historical.stdout).unwrap();
    assert_eq!(evidence["admission"]["status"], "waived");
    let report = evidence["report"].as_str().unwrap();
    let parsed = crate::conformance::Report::parse_canonical(report.as_bytes()).unwrap();
    assert_eq!(
        parsed.result().unwrap(),
        crate::conformance::ReportResult::Incomplete
    );
    assert_eq!(
        evidence["admission"]["report_digest"],
        Digest::of(report.as_bytes()).to_string()
    );
    assert_eq!(evidence["waiver"]["receipt_digest"], receipt_digest);
    assert_signed_admission(&evidence);
    evidence
}

fn assert_signed_admission(evidence: &serde_json::Value) {
    let bytes = fs::read(
        Path::new(STATE).join("receipts/sessions/session/00000000000000000000.receipt.json"),
    )
    .unwrap();
    let receipt = crate::launch_receipt::SignedReceipt::parse_canonical(&bytes).unwrap();
    let crate::launch_receipt::ReceiptOutcome::Launch {
        evidence: launch, ..
    } = receipt.payload.outcome
    else {
        panic!("sequence zero must be the signed admission");
    };
    assert_eq!(
        serde_json::to_value(launch.conformance).unwrap(),
        evidence["admission"]
    );
}

fn preparation_command(config: &LauncherConfig) -> std::process::Output {
    let mut preparation = Command::new(&config.launcher_path)
        .arg("prepare")
        .env_clear()
        .env("SUDO_UID", config.operator_uid.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = preparation.stdin.take().unwrap();
    input
        .write_all(&named_request("session").canonical_bytes())
        .unwrap();
    input.write_all(b"\n").unwrap();
    drop(input);
    preparation.wait_with_output().unwrap()
}

#[test]
fn privileged_certified_admission_retains_report_and_suspends_on_evidence_loss() {
    use crate::conformance::{ReportResult, admission::Enforcement, installed::Certificate};
    if std::env::var_os("LOUISELM_REQUIRE_CONTROL_DAEMON").is_none() {
        return;
    }
    assert!(rustix::process::geteuid().is_root());
    let root = tempfile::Builder::new()
        .prefix("louiselm-certified-")
        .tempdir_in("/var/lib")
        .unwrap();
    mounts(root.path());
    let _account = BrokerAccount::create();
    // One retained Session still leaves the certifier's three required slots.
    let (paths, config, manager) = install_unseeded_daemon(root.path(), Enforcement::Enforced, 4);
    assert!(
        process(&manager, BROKER_UID, true)
            .0
            .wait()
            .unwrap()
            .success()
    );
    let mut daemon = process(&manager, BROKER_UID, false);
    ready(&config);
    let output = certification_command(&config);
    let os = fs::read_to_string("/usr/lib/os-release").unwrap();
    if !os.lines().any(|line| line == "ID=debian")
        || !os.lines().any(|line| line == "VERSION_ID=\"13\"")
    {
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        terminate(&mut daemon);
        eprintln!("unsupported host refused; no positive certified admission claimed");
        return;
    }
    assert!(output.status.success(), "{output:?}");
    let certificate = Certificate::parse_canonical(&output.stdout).unwrap();
    assert_eq!(
        certificate.observations.result().unwrap(),
        ReportResult::Passed
    );
    let session = launch(&paths, &config, root.path(), "session").unwrap();
    let evidence = conformance_value(&config);
    assert_eq!(evidence["admission"]["status"], "certified");
    assert_eq!(
        evidence["report"].as_str().unwrap().as_bytes(),
        certificate.observations.canonical_bytes().unwrap()
    );
    assert_eq!(
        evidence["admission"]["report_digest"],
        certificate.observations.digest().unwrap().to_string()
    );
    assert!(evidence["waiver"].is_null());
    assert_signed_admission(&evidence);
    // Explicit disposable-gate output, never production logging or host authority.
    eprintln!(
        "QA_CERTIFIED_ADMISSION: {}",
        serde_json::json!({
            "authority": "disposable_fixture", "session_id": "session",
            "certificate": std::str::from_utf8(&output.stdout).unwrap(),
            "conformance": evidence,
        })
    );
    terminate(&mut daemon);
    let mut restarted = process(&manager, BROKER_UID, false);
    ready(&config);
    let retained = conformance_value(&config);
    for field in ["report", "admission", "waiver"] {
        assert_eq!(
            retained[field], evidence[field],
            "restart must retain exact {field}"
        );
    }
    // Move only this fixture's protected index; retain it for real recertification.
    let state = paths.state_root.join("conformance/state.json");
    let retained_state = state.with_extension("retained");
    fs::rename(&state, &retained_state).unwrap();
    wait_parked(&config);
    let suspended = conformance_value(&config);
    assert_eq!(suspended["last_check"]["suspended"], true);
    assert_eq!(
        suspended["last_check"]["check"],
        serde_json::to_value(crate::launch_protocol::ConformanceCheck::Invalid {
            failure: crate::launch_protocol::ConformanceFailure::Unavailable,
        })
        .unwrap()
    );
    fs::rename(retained_state, state).unwrap();
    let recertified = certification_command(&config);
    assert!(recertified.status.success(), "{recertified:?}");
    assert_eq!(
        Certificate::parse_canonical(&recertified.stdout)
            .unwrap()
            .observations
            .result()
            .unwrap(),
        ReportResult::Passed
    );
    // A new certificate and either read are evidence, never implicit Resume.
    wait_parked(&config);
    assert_eq!(conformance_value(&config)["report"], evidence["report"]);
    session.dispose().unwrap();
    terminate(&mut restarted);
}

fn certification_command(config: &LauncherConfig) -> std::process::Output {
    Command::new(&config.launcher_path)
        .arg("certify")
        .env_clear()
        .env("SUDO_UID", config.operator_uid.to_string())
        .output()
        .unwrap()
}

fn conformance_value(config: &LauncherConfig) -> serde_json::Value {
    let output = session_command(config.operator_uid, "conformance", "session");
    assert!(output.status.success(), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

fn wait_parked(config: &LauncherConfig) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let output = session_command(config.operator_uid, "inspect", "session");
        if output.status.success() {
            let status = SessionStatus::parse_canonical(&output.stdout).unwrap();
            if status.state == crate::launch_receipt::SessionState::Parked {
                assert_eq!(
                    status.channel_state,
                    crate::launch_protocol::ChannelState::Revoked
                );
                return;
            }
        } else {
            // Daemon readiness precedes the retained Session's reattachment.
            assert_eq!(
                output.status.code(),
                Some(i32::from(InspectError::StatusUnavailable.exit_code())),
                "{output:?}"
            );
            assert!(output.stdout.is_empty(), "{output:?}");
            assert_eq!(
                output.stderr,
                InspectError::StatusUnavailable.canonical_bytes()
            );
        }
        assert!(
            Instant::now() < deadline,
            "conformance must suspend the actual tree: {output:?}"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

fn incomplete_guard_certificate(paths: &LauncherPaths) {
    use crate::conformance::{Cleanup, Outcome, ReportResult, SENDER_GUARD_CHECK, installed};
    assert_ne!(
        fs::read_link("/proc/self/ns/net").unwrap(),
        fs::read_link("/proc/1/ns/net").unwrap()
    );
    // Only the later observation failure is injected. The production guard
    // probe, host measurement, protected retention and cleanup are real.
    installed::INCOMPLETE_AFTER_GUARD.with(|incomplete| incomplete.set(true));
    let parent = rustix::process::getppid()
        .unwrap()
        .as_raw_nonzero()
        .get()
        .cast_unsigned();
    let certificate =
        installed::certify(paths, Instant::now() + Duration::from_mins(3), parent).unwrap();
    assert_eq!(
        certificate.observations.result().unwrap(),
        ReportResult::Incomplete
    );
    assert_eq!(certificate.observations.cleanup, Cleanup::Confirmed);
    let guard = certificate
        .observations
        .checks
        .iter()
        .find(|check| check.name == SENDER_GUARD_CHECK)
        .unwrap();
    assert_eq!(guard.control, Outcome::Allowed);
    assert!(matches!(guard.confined, Outcome::Denied(_)));
}

fn assert_preparation_boundaries(
    paths: &LauncherPaths,
    config: &LauncherConfig,
    observation: &crate::conformance::preparation::Preparation,
) {
    use crate::conformance::preparation::{MAX_AGE_MS, Preparation};
    let now = observation.observed_at_ms;
    assert_eq!(
        Preparation::read(paths, config, "session", now).unwrap(),
        *observation
    );
    assert!(observation.validate(now - 1).is_err());
    assert!(observation.validate(now + MAX_AGE_MS).is_err());
    let mut changed = config.clone();
    changed.conformance = crate::conformance::admission::Enforcement::PreCutover;
    assert!(observation.validate_current(&changed, now).is_err());
    changed = config.clone();
    changed.operator_uid += 1;
    assert!(observation.validate_current(&changed, now).is_err());
    let mut rebooted = observation.clone();
    rebooted.boot_id = "00000000-0000-4000-8000-000000000000".into();
    assert!(rebooted.validate_current(config, now).is_err());
    let file = paths.state_root.join("pre-admission/session.json");
    fs::set_permissions(&file, fs::Permissions::from_mode(0o666)).unwrap();
    assert!(Preparation::read(paths, config, "session", now).is_err());
    fs::set_permissions(&file, fs::Permissions::from_mode(0o444)).unwrap();
    let retained = file.with_extension("retained");
    fs::rename(&file, &retained).unwrap();
    std::os::unix::fs::symlink(&retained, &file).unwrap();
    assert!(Preparation::read(paths, config, "session", now).is_err());
    fs::remove_file(&file).unwrap();
    fs::rename(retained, &file).unwrap();
}

fn waiver_command(
    config: &LauncherConfig,
    args: &[&str],
    input: Option<&[u8]>,
) -> std::process::Output {
    let mut child = Command::new("/usr/bin/setpriv")
        .args([
            "--reuid",
            &config.operator_uid.to_string(),
            "--regid",
            &config.operator_uid.to_string(),
            "--clear-groups",
        ])
        .arg("/usr/local/lib/louiselm/current/bin/louiselm-control")
        .arg("waiver")
        .args(args)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    if let Some(input) = input {
        stdin.write_all(input).unwrap();
    }
    drop(stdin);
    child.wait_with_output().unwrap()
}

pub(super) fn refusal(uid: u32, id: &str, error: InspectError) {
    let output = command(uid, id);
    assert_eq!(
        output.status.code(),
        Some(i32::from(error.exit_code())),
        "{output:?}"
    );
    assert_eq!(output.stdout, [] as [u8; 0]);
    assert_eq!(output.stderr, error.canonical_bytes());
    if error != InspectError::StatusUnavailable {
        let evidence = session_command(uid, "conformance", id);
        assert_eq!(
            evidence.status.code(),
            Some(i32::from(error.exit_code())),
            "{evidence:?}"
        );
        assert_eq!(evidence.stdout, [] as [u8; 0]);
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
    assert_eq!(output.stderr, [] as [u8; 0]);
    let status = SessionStatus::parse_canonical(&output.stdout).unwrap();
    assert_eq!(status.canonical_bytes(), output.stdout);
    assert_eq!(status.session_id, id);
    assert_eq!(status.posture.dimensions.len(), 6);
    assert_ne!(
        status.allowed_actions,
        [] as [crate::launch_protocol::LifecycleAction; 0]
    );
    let evidence_output = session_command(config.operator_uid, "conformance", id);
    assert!(evidence_output.status.success(), "{evidence_output:?}");
    assert_eq!(evidence_output.stderr, [] as [u8; 0]);
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
    assert_eq!(
        self_status.allowed_actions,
        [] as [crate::launch_protocol::LifecycleAction; 0]
    );
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
