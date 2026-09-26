//! Missing evidence remains unknown across restart and durable quarantine wins.

use super::*;
use louiselm_skills::{
    broker::{lifecycle::LifecycleStore, verification::VerificationStatus},
    launch_protocol::{VERIFICATION_SCHEMA, VerificationOperation, VerificationRequest},
    launch_receipt::ReceiptHead,
};

pub(super) fn reopen(root: &Path, socket: &str) -> BrokerService {
    BrokerService::bind(
        &root.join(socket),
        AuthorizationStore::open(&root.join("authorizations"), pool(4)).unwrap(),
        ReceiptStore::open(&root.join("receipts"), trusted_release()).unwrap(),
        AuditLog::open(&root.join("audit")).unwrap(),
        local_pin(),
    )
    .unwrap()
}

#[test]
fn spent_without_actual_evidence_is_unknown_after_restart_and_quarantine() {
    for tainted in ["producer", "verifier"] {
        let root = TempDir::new().unwrap();
        let launch = request("verifier");
        consumed_authorization(root.path(), &launch);
        consumed_authorization(root.path(), &request("producer"));
        let service = reopen(root.path(), "broker.sock");
        assert_eq!(
            service.verification_status("verifier").unwrap(),
            VerificationStatus::NotRequested
        );
        let directory = root.path().join("authorizations/verification");
        fs::create_dir(&directory).unwrap();
        let intent = VerificationRequest {
            schema: VERIFICATION_SCHEMA.into(),
            protocol_version: PROTOCOL_VERSION,
            request_id: "verify".into(),
            launch,
            head: ReceiptHead {
                sequence: 1,
                digest: Digest::of(b"head").to_string(),
            },
            expires_at_ms: 30000,
            operation: VerificationOperation::Run {
                producer_session_id: "producer".into(),
                export_request_id: "export".into(),
                export_digest: Digest::of(b"export").to_string(),
                job_digest: Digest::of(b"job").to_string(),
            },
        };
        fs::write(
            directory.join("intent-verifier.json"),
            intent.canonical_bytes(),
        )
        .unwrap();
        assert_eq!(
            service.verification_status("verifier").unwrap(),
            VerificationStatus::Unknown
        );
        drop(service);
        let restarted = reopen(root.path(), "restarted.sock");
        assert_eq!(
            restarted.verification_status("verifier").unwrap(),
            VerificationStatus::Unknown
        );
        LifecycleStore::open(&root.path().join("authorizations/lifecycle"))
            .unwrap()
            .quarantine(tainted)
            .unwrap();
        assert!(matches!(
            restarted.verification_status("verifier").unwrap(),
            VerificationStatus::Quarantined { .. }
        ));
    }
}

#[test]
fn foreign_or_corrupt_durable_verification_intent_is_not_success() {
    let root = TempDir::new().unwrap();
    consumed_authorization(root.path(), &request("verifier"));
    let service = reopen(root.path(), "broker.sock");
    let directory = root.path().join("authorizations/verification");
    fs::create_dir(&directory).unwrap();
    fs::write(directory.join("intent-verifier.json"), b"{}").unwrap();
    assert!(service.verification_status("verifier").is_err());
}
