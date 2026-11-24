//! Saved bytes outlive disposable Session storage and do not confer application authority.

use super::*;
use louiselm_skills::{
    broker::{
        promotion::{SavedResult, SavedSelection},
        verification::VerificationRecord,
    },
    launch_protocol::{
        VERIFICATION_SCHEMA, VerificationExecution, VerificationExport, VerificationOperation,
        VerificationRequest, VerificationStep,
    },
    launch_receipt::ReceiptHead,
    workspace::{bundle, verification as jobs},
};

#[expect(
    clippy::too_many_lines,
    reason = "One fixture binds exact source, job, producer/verifier evidence and the durable publication marker for store regressions."
)]
fn fixture(root: &Path) -> SavedResult {
    let snapshot = root.join("snapshot");
    fs::create_dir_all(snapshot.join("files")).unwrap();
    let bytes = format!("{{\"schema\":\"louiselm.workspace.snapshot/1\",\"base_commit\":\"{}\",\"files\":[],\"selected\":[],\"changes\":[]}}", "a".repeat(40)).into_bytes();
    fs::write(snapshot.join("snapshot.json"), &bytes).unwrap();
    let work = root.join("producer-work");
    fs::create_dir(&work).unwrap();
    fs::write(work.join("result"), b"exact saved result").unwrap();
    let bundle_path = root.join("bundle");
    let exported = bundle::export(&snapshot, &Digest::of(&bytes), &work, &bundle_path).unwrap();
    let plan = root.join("plan.json");
    let plan_bytes = br#"{"schema":"louiselm.workspace.verification-plan/1","commands":[{"argv":["true"],"cwd":".","timeout_ms":1000}]}"#;
    fs::write(&plan, plan_bytes).unwrap();
    let job_path = root.join("job");
    let job = jobs::prepare(
        &snapshot,
        &Digest::of(&bytes),
        &bundle_path,
        &Digest::parse(&exported.bundle_digest).unwrap(),
        &plan,
        &Digest::of(plan_bytes),
        &job_path,
    )
    .unwrap();
    let head = ReceiptHead {
        sequence: 2,
        digest: Digest::of(b"receipt").to_string(),
    };
    let producer = VerificationExport {
        request: VerificationRequest {
            schema: VERIFICATION_SCHEMA.into(),
            protocol_version: PROTOCOL_VERSION,
            request_id: "export".into(),
            launch: request("producer"),
            head: head.clone(),
            expires_at_ms: 1,
            operation: VerificationOperation::Export {
                input_id: "inputs".into(),
                input_digest: Digest::of(b"inputs").to_string(),
            },
        },
        job: job.clone(),
        integration_digest: Digest::of(b"tools").to_string(),
    };
    let verification = VerificationRecord {
        producer: producer.clone(),
        execution: VerificationExecution {
            request: VerificationRequest {
                schema: VERIFICATION_SCHEMA.into(),
                protocol_version: PROTOCOL_VERSION,
                request_id: "verify".into(),
                launch: request("verifier"),
                head: ReceiptHead {
                    sequence: 1,
                    digest: head.digest.clone(),
                },
                expires_at_ms: 1,
                operation: VerificationOperation::Run {
                    producer_session_id: "producer".into(),
                    export_request_id: "export".into(),
                    export_digest: producer.digest().unwrap().to_string(),
                    job_digest: job.job_digest.clone(),
                },
            },
            job: job.clone(),
            integration_digest: Digest::of(b"tools").to_string(),
            verifier_inputs_digest: Digest::of(b"qualified-verifier-inputs").to_string(),
            steps: vec![VerificationStep::Completed {
                exit_code: 0,
                timed_out: false,
            }],
            cleanup_proven: true,
            interrupted: false,
        },
        terminal_head: head,
    };
    let mut selection = promotion::selection();
    selection.job = job;
    selection.verification_digest =
        Digest::of(&serde_json::to_vec(&verification).unwrap()).to_string();
    // The fixture stands for an already authenticated publication, not a fabricated verifier execution.
    let saved = SavedResult {
        schema: "louiselm.workspace.saved-result/1".into(),
        selection: SavedSelection {
            schema: "louiselm.run-promotion-selection/1".into(),
            run_id: verification.producer.request.launch.run_id.clone(),
            bead_id: "bead".into(),
            producer_session_id: selection.producer_session_id.clone(),
            verifier_session_id: selection.verifier_session_id.clone(),
            request_id: selection.request_id.clone(),
            checkout: root.join("checkout"),
            journal_parent: root.join("journal"),
            expected_head: "a".repeat(40),
            plan: plan.clone(),
        },
        request: selection,
        verification,
        changes: louiselm_skills::workspace::promotion::ChangePreview {
            added: vec!["result".into()],
            modified: vec![],
            deleted: vec![],
        },
        discarded: false,
        accepted: None,
        application: louiselm_skills::broker::promotion::PromotionStatus::NotRequested,
    };
    let directory = root.join("authorizations/saved-results/promotion");
    fs::create_dir_all(directory.join("transfer")).unwrap();
    fs::set_permissions(
        directory.parent().unwrap(),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    fs::rename(snapshot, directory.join("transfer/snapshot")).unwrap();
    fs::rename(job_path, directory.join("transfer/job")).unwrap();
    fs::write(
        directory.join("saved.json"),
        serde_json::to_vec(&saved).unwrap(),
    )
    .unwrap();
    fs::write(
        directory.join("published.json"),
        serde_json::to_vec(&Digest::of(&serde_json::to_vec(&saved).unwrap()).to_string()).unwrap(),
    )
    .unwrap();
    saved
}

#[test]
fn saved_result_survives_reopen_and_producer_removal_until_explicit_discard() {
    let root = TempDir::new().unwrap();
    let saved = fixture(root.path());
    let service = verification::reopen(root.path(), "first.sock");
    assert_eq!(service.saved_result(1000, "promotion").unwrap(), saved);
    assert!(matches!(
        service.saved_result(1001, "promotion"),
        Err(BrokerError::ControllerMismatch)
    ));
    fs::remove_dir_all(root.path().join("producer-work")).unwrap();
    drop(service);
    let restarted = verification::reopen(root.path(), "reopened.sock");
    assert_eq!(restarted.saved_result(1000, "promotion").unwrap(), saved);
    assert_eq!(
        restarted.promotion_status("promotion").unwrap(),
        louiselm_skills::broker::promotion::PromotionStatus::NotRequested
    );
    let discarded = restarted.discard_saved_result(1000, "promotion").unwrap();
    assert!(discarded.discarded);
    assert_eq!(
        restarted.discard_saved_result(1000, "promotion").unwrap(),
        discarded
    );
    assert!(
        root.path()
            .join("authorizations/saved-results/promotion/transfer/job/source/result")
            .exists()
    );
}

#[test]
fn accepted_decision_requires_complete_application_and_is_durable_and_idempotent() {
    use louiselm_skills::broker::promotion::SavedAcceptance;
    use louiselm_skills::workspace::promotion::ApplicationResult;
    use louiselm_skills::workspace::provenance::OutputProvenance;
    let root = tempfile::tempdir().unwrap();
    let saved = fixture(root.path());
    let service = verification::reopen(root.path(), "accept.sock");
    let accepted = SavedAcceptance {
        commit: "c".repeat(40),
        approval_digest: Digest::of(b"exact review").to_string(),
        request_digest: Digest::of(&serde_json::to_vec(&saved.request).unwrap()).to_string(),
    };
    assert!(
        service
            .accept_saved_result(1000, "promotion", &accepted)
            .is_err()
    );
    assert!(
        service
            .saved_result(1000, "promotion")
            .unwrap()
            .accepted
            .is_none()
    );
    let journal = root.path().join("authorizations/promotions/promotion");
    fs::create_dir_all(&journal).unwrap();
    fs::write(
        journal.join("request.json"),
        serde_json::to_vec(&saved.request).unwrap(),
    )
    .unwrap();
    fs::write(journal.join("0.grant"), b"0").unwrap();
    assert!(
        service
            .accept_saved_result(1000, "promotion", &accepted)
            .is_err()
    );
    fs::write(journal.join("0.done"), b"0").unwrap();
    fs::write(
        journal.join("result.json"),
        serde_json::to_vec(&ApplicationResult {
            complete: true,
            completed_steps: 1,
            output_provenance: OutputProvenance {
                schema: "louiselm.workspace.output-provenance/1".into(),
                code: louiselm_skills::workspace::provenance::OutputProvenanceCode::Untainted,
                taint_digest: None,
                clean_review_refs: vec![],
            },
        })
        .unwrap(),
    )
    .unwrap();
    assert!(
        service
            .accept_saved_result(1001, "promotion", &accepted)
            .is_err()
    );
    let first = service
        .accept_saved_result(1000, "promotion", &accepted)
        .unwrap();
    assert_eq!(first.accepted.as_ref(), Some(&accepted));
    assert_eq!(
        service
            .accept_saved_result(1000, "promotion", &accepted)
            .unwrap(),
        first
    );
    let mut conflict = accepted.clone();
    conflict.commit = "d".repeat(40);
    assert!(
        service
            .accept_saved_result(1000, "promotion", &conflict)
            .is_err()
    );
    assert!(service.discard_saved_result(1000, "promotion").is_err());
    drop(service);
    let reopened = verification::reopen(root.path(), "accepted-reopen.sock");
    assert_eq!(
        reopened.saved_result(1000, "promotion").unwrap().accepted,
        Some(accepted)
    );
}

#[test]
fn tampered_payload_and_replaced_saved_directory_are_refused() {
    let root = TempDir::new().unwrap();
    fixture(root.path());
    let service = verification::reopen(root.path(), "broker.sock");
    let directory = root.path().join("authorizations/saved-results/promotion");
    let path = directory.join("transfer/job/source/result");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(path, b"tampered").unwrap();
    assert!(matches!(
        service.saved_result(1000, "promotion"),
        Err(BrokerError::Workspace(_))
    ));
    let replaced = root.path().join("replaced");
    fs::rename(&directory, &replaced).unwrap();
    std::os::unix::fs::symlink(&replaced, &directory).unwrap();
    assert!(matches!(
        service.saved_result(1000, "promotion"),
        Err(BrokerError::Workspace(_))
    ));
}

#[test]
fn concurrent_discard_is_idempotent_and_incomplete_publication_is_not_saved() {
    let root = TempDir::new().unwrap();
    fixture(root.path());
    let service = Arc::new(verification::reopen(root.path(), "broker.sock"));
    let threads: Vec<_> = (0..2)
        .map(|_| {
            let service = Arc::clone(&service);
            thread::spawn(move || service.discard_saved_result(1000, "promotion").unwrap())
        })
        .collect();
    for result in threads {
        assert!(result.join().unwrap().discarded);
    }
    let directory = root.path().join("authorizations/saved-results/promotion");
    fs::remove_file(directory.join("saved.json")).unwrap();
    assert!(matches!(
        service.saved_result(1000, "promotion"),
        Err(BrokerError::InvalidGrant)
    ));
}

#[test]
fn publication_without_durable_confirmation_is_not_complete() {
    let root = TempDir::new().unwrap();
    fixture(root.path());
    fs::remove_file(
        root.path()
            .join("authorizations/saved-results/promotion/published.json"),
    )
    .unwrap();
    let service = verification::reopen(root.path(), "broker.sock");
    assert!(matches!(
        service.saved_result(1000, "promotion"),
        Err(BrokerError::InvalidGrant)
    ));
}

#[test]
fn foreign_operator_cannot_enter_saved_application_or_read_cached_outcomes() {
    use std::{io::Write as _, os::unix::net::UnixStream};
    let root = TempDir::new().unwrap();
    let service = verification::reopen(root.path(), "broker.sock");
    let mut request = promotion::selection();
    request.destination.uid = rustix::process::geteuid().as_raw() + 1;
    let directory = root.path().join("authorizations/promotions/promotion");
    fs::create_dir_all(&directory).unwrap();
    fs::write(
        directory.join("request.json"),
        serde_json::to_vec(&request).unwrap(),
    )
    .unwrap();
    let (mut operator, broker) = UnixStream::pair().unwrap();
    let worker =
        thread::spawn(move || service.serve_saved_promotion("promotion", broker, |_, _, _| false));
    let bytes = serde_json::to_vec(&request).unwrap();
    operator
        .write_all(&u32::try_from(bytes.len()).unwrap().to_be_bytes())
        .unwrap();
    operator.write_all(&bytes).unwrap();
    assert!(matches!(
        worker.join().unwrap(),
        Err(BrokerError::ControllerMismatch)
    ));
}

#[test]
#[ignore = "Two-phase fixture; execute only across a fresh disposable launcher VM shutdown/start."]
fn saved_result_survives_disposable_vm_reboot() {
    use std::os::unix::fs::DirBuilderExt;

    let phase =
        std::env::var("LOUISELM_SAVED_RESULT_REBOOT_PHASE").expect("explicit VM gate phase");
    let root = Path::new("/var/tmp/louiselm-256gs-saved-reboot");
    if phase == "prepare" {
        fs::DirBuilder::new().mode(0o700).create(root).unwrap();
        let saved = fixture(root);
        let service = verification::reopen(root, "before.sock");
        assert_eq!(service.saved_result(1000, "promotion").unwrap(), saved);
        fs::write(
            root.join("expected.json"),
            serde_json::to_vec(&saved).unwrap(),
        )
        .unwrap();
        fs::remove_dir_all(root.join("producer-work")).unwrap();
        println!("SAVED_RESULT_BEFORE_REBOOT");
    } else {
        assert_eq!(phase, "inspect");
        let expected: SavedResult =
            serde_json::from_slice(&fs::read(root.join("expected.json")).unwrap()).unwrap();
        let service = verification::reopen(root, "after.sock");
        assert_eq!(service.saved_result(1000, "promotion").unwrap(), expected);
        assert!(!root.join("producer-work").exists());
        assert!(
            service
                .discard_saved_result(1000, "promotion")
                .unwrap()
                .discarded
        );
        println!("SAVED_RESULT_AFTER_REBOOT");
    }
}
