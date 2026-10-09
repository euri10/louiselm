//! Actual installed operator CLI, owning daemon worker and supervisor lifecycle.

use super::*;
use crate::{
    launch_protocol::{
        ErrorCode, LIFECYCLE_REQUEST_SCHEMA, LifecycleAction, LifecycleRequest, ProtocolError,
    },
    launch_receipt::{SessionState, SignedReceipt},
};

fn request(status: &SessionStatus, action: LifecycleAction, id: &str) -> LifecycleRequest {
    LifecycleRequest {
        schema: LIFECYCLE_REQUEST_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        request_id: id.into(),
        session_id: status.session_id.clone(),
        run_id: status.run_id.clone(),
        authorization_id: format!("operator-{id}"),
        action,
        expected_state: status.state,
        expected_receipt_sequence: status.broker_head.as_ref().map(|head| head.sequence),
        envelope_revision: status.envelope_revision,
    }
}

fn command(uid: u32, request: &LifecycleRequest) -> std::process::Output {
    let mut child = Command::new("/usr/bin/setpriv")
        .args([
            "--reuid",
            &uid.to_string(),
            "--regid",
            &uid.to_string(),
            "--clear-groups",
        ])
        .arg("/usr/local/lib/louiselm/current/bin/louiselm-control")
        .args(["lifecycle", "--json"])
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&request.canonical_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn completed(config: &LauncherConfig, request: &LifecycleRequest) -> SignedReceipt {
    let output = command(config.operator_uid, request);
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    serde_json::from_slice::<Result<SignedReceipt, ProtocolError>>(&output.stdout)
        .unwrap()
        .unwrap()
}

#[test]
fn privileged_activated_daemon_operator_lifecycle() {
    if std::env::var_os("LOUISELM_REQUIRE_CONTROL_DAEMON").is_none() {
        eprintln!("skipping: requires disposable VM and private mount namespace");
        return;
    }
    assert!(rustix::process::geteuid().is_root());
    let root = tempfile::Builder::new()
        .prefix("louiselm-operator-lifecycle-")
        .tempdir_in("/var/lib")
        .unwrap();
    mounts(root.path());
    let _account = BrokerAccount::create();
    let (paths, config, manager) = install_daemon(root.path());
    let mut daemon = process(&manager, BROKER_UID, false);
    ready(&config);
    let session = launch(&paths, &config, root.path(), "session").unwrap();
    let running = super::status(&config, "session");
    let park = request(&running, LifecycleAction::Park, "operator-park");
    for uid in [0, AGENT_UID, BROKER_UID] {
        let refused = command(uid, &park);
        assert_eq!(
            refused.status.code(),
            Some(i32::from(InspectError::AuthenticationRefused.exit_code()))
        );
        assert_eq!(
            refused.stderr,
            InspectError::AuthenticationRefused.canonical_bytes()
        );
        assert_eq!(refused.stdout, [] as [u8; 0]);
    }
    assert_eq!(
        super::status(&config, "session").broker_head,
        running.broker_head
    );
    let parked = completed(&config, &park);
    assert_eq!(parked.payload.resulting_state, SessionState::Parked);
    assert_eq!(
        super::status(&config, "session").state,
        SessionState::Parked
    );
    assert_eq!(completed(&config, &park), parked);
    let mut stale = request(&running, LifecycleAction::Resume, "stale-resume");
    stale.expected_state = SessionState::Parked;
    let refused = command(config.operator_uid, &stale);
    assert_eq!(refused.status.code(), Some(7), "{refused:?}");
    let error = serde_json::from_slice::<Result<SignedReceipt, ProtocolError>>(&refused.stdout)
        .unwrap()
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::ReceiptSequenceMismatch);
    terminate(&mut daemon);
    let mut restarted = process(&manager, BROKER_UID, false);
    ready(&config);
    let current = super::status(&config, "session");
    assert_eq!(current.state, SessionState::Parked);
    assert_eq!(completed(&config, &park), parked);
    let resume = request(&current, LifecycleAction::Resume, "operator-resume");
    let resumed = completed(&config, &resume);
    assert_eq!(resumed.payload.resulting_state, SessionState::Running);
    assert_eq!(
        super::status(&config, "session").state,
        SessionState::Running
    );
    assert_eq!(completed(&config, &resume), resumed);
    let disposal = request(
        &super::status(&config, "session"),
        LifecycleAction::Disposal,
        "operator-dispose",
    );
    let disposed = completed(&config, &disposal);
    assert_eq!(disposed.payload.resulting_state, SessionState::Terminal);
    session.dispose().unwrap();
    terminate(&mut restarted);
    let mut restarted = process(&manager, BROKER_UID, false);
    ready(&config);
    assert_eq!(completed(&config, &disposal), disposed);
    let mut conflict = disposal;
    conflict.authorization_id = "changed-authorization".into();
    let refused = command(config.operator_uid, &conflict);
    assert_eq!(refused.status.code(), Some(7));
    let error = serde_json::from_slice::<Result<SignedReceipt, ProtocolError>>(&refused.stdout)
        .unwrap()
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::RequestIdConflict);
    terminate(&mut restarted);
}
