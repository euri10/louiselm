//! Authenticated broker consumption, restart, quarantine and operator pin path.
use super::*;
use louiselm_skills::broker::{
    lifecycle::LifecycleStore,
    operator::{self, InspectError, OperatorServer},
};

#[test]
fn retention_operator_path_survives_restart_and_reports_quarantine_without_payloads() {
    let root = TempDir::new().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let launch = request("retained");
    let authorization = consumed_authorization(root.path(), &launch);
    let uid = authorization.controller_uid;
    let service = verification::reopen(root.path(), "broker.sock");
    let original = service.workspace_retention(uid, "retained", None).unwrap();
    assert_eq!(original.record.launch, launch);
    assert!(!original.record.pinned);
    assert!(
        service
            .workspace_retention(uid + 1, "retained", Some(true))
            .is_err()
    );
    service
        .workspace_retention(uid, "retained", Some(true))
        .unwrap();
    drop(service);
    LifecycleStore::open(&root.path().join("authorizations/lifecycle"))
        .unwrap()
        .quarantine("retained")
        .unwrap();
    let service = verification::reopen(root.path(), "restarted.sock");
    let reopened = service.workspace_retention(uid, "retained", None).unwrap();
    assert!(reopened.record.pinned && reopened.quarantined);
    assert_eq!(reopened.record.expires_at_ms, original.record.expires_at_ms);
    let socket = root.path().join("operator.sock");
    let server_uid = getuid().as_raw();
    let endpoint = OperatorServer::bind(&socket, server_uid).unwrap();
    let worker = thread::spawn(move || {
        endpoint
            .serve_once(
                |_, _| Err(louiselm_skills::broker::operator::InspectError::UnknownSession),
                |_, _| panic!("no live Session lookup"),
                |_| panic!("no conformance query"),
                |_, _| panic!("no Skill decision"),
                |_, _| panic!("no Beads decision"),
                |id, pin| {
                    service
                        .workspace_retention(uid, id, pin)
                        .map_err(|_| InspectError::StatusUnavailable)
                },
                |_, _, _| panic!("not waiver control"),
                |_, _, _| Err(louiselm_skills::broker::provider_extension::ExtensionError::Unknown),
            )
            .unwrap();
    });
    let result = operator::workspace_retention(
        &socket,
        server_uid,
        "retained",
        Some(false),
        Duration::from_secs(2),
    )
    .unwrap();
    worker.join().unwrap();
    assert!(!result.record.pinned);
    assert!(result.quarantined);
    assert_eq!(result.record.expires_at_ms, original.record.expires_at_ms);
}
