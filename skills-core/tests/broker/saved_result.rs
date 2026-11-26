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

fn fixture(root: &Path) -> SavedResult {
    fixture_with_bytes(root, b"exact saved result")
}

#[expect(
    clippy::too_many_lines,
    reason = "One fixture binds exact source, job, producer/verifier evidence and the durable publication marker for store regressions."
)]
fn fixture_with_bytes(root: &Path, result: &[u8]) -> SavedResult {
    let mut producer_launch = request("producer");
    producer_launch.run_id = "11111111-2222-4333-8444-555555555555".into();
    let mut verifier_launch = request("verifier");
    verifier_launch.run_id.clone_from(&producer_launch.run_id);
    let snapshot = root.join("snapshot");
    fs::create_dir_all(snapshot.join("files")).unwrap();
    let bytes = format!("{{\"schema\":\"louiselm.workspace.snapshot/1\",\"base_commit\":\"{}\",\"files\":[],\"selected\":[],\"changes\":[]}}", "a".repeat(40)).into_bytes();
    fs::write(snapshot.join("snapshot.json"), &bytes).unwrap();
    let work = root.join("producer-work");
    fs::create_dir(&work).unwrap();
    fs::write(work.join("result"), result).unwrap();
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
            launch: producer_launch,
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
                launch: verifier_launch,
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
fn refresh_prepares_separate_changed_base_and_rules_without_releasing_pending_work() {
    use louiselm_skills::broker::promotion::RefreshSelection;
    let root = tempfile::tempdir().unwrap();
    let saved = fixture(root.path());
    let service = verification::reopen(root.path(), "refresh.sock");
    let repository = root.path().join("new-repository");
    fs::create_dir(&repository).unwrap();
    let git = |args: &[&str]| {
        let result = std::process::Command::new("git")
            .current_dir(&repository)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args(args)
            .output()
            .unwrap();
        assert!(result.status.success(), "{result:?}");
    };
    git(&["init", "-q"]);
    fs::write(repository.join("new-base"), b"independent change").unwrap();
    git(&["add", "new-base"]);
    git(&[
        "-c",
        "user.name=fixture",
        "-c",
        "user.email=fixture@invalid",
        "-c",
        "commit.gpgsign=false",
        "commit",
        "-qm",
        "new base",
    ]);
    let snapshot = root.path().join("new-snapshot");
    let preview = louiselm_skills::workspace::prepare(&repository, &[], &snapshot).unwrap();
    let plan = root.path().join("new-plan.json");
    let bytes = br#"{"schema":"louiselm.workspace.verification-plan/1","commands":[{"argv":["false"],"cwd":".","timeout_ms":2000}]}"#;
    fs::write(&plan, bytes).unwrap();
    let selection = RefreshSelection {
        result_id: "promotion".into(),
        revision_digest: saved.request.verification_digest.clone(),
        input_id: "refresh-attempt".into(),
        snapshot,
        snapshot_digest: preview.snapshot_digest,
        expected_head: preview.base_commit,
        plan,
        plan_digest: Digest::of(bytes).to_string(),
    };
    let input = service.prepare_saved_refresh(1000, &selection).unwrap();
    assert_eq!(input.producer, saved.verification.producer);
    assert_ne!(input.job.base_digest, saved.request.job.base_digest);
    assert_eq!(input.job.plan_digest, selection.plan_digest);
    assert_eq!(
        service.prepare_saved_refresh(1000, &selection).unwrap(),
        input
    );
    assert_eq!(service.saved_result(1000, "promotion").unwrap(), saved);
    assert_eq!(
        fs::read(repository.join("new-base")).unwrap(),
        b"independent change"
    );
    assert!(!repository.join("result").exists());
    assert!(!repository.join("SHOULD_NOT_EXECUTE").exists());
    assert!(service.prepare_saved_refresh(1001, &selection).is_err());
    service.discard_saved_result(1000, "promotion").unwrap();
    assert!(service.prepare_saved_refresh(1000, &selection).is_err());

    let uncertain_root = tempfile::tempdir().unwrap();
    let uncertain = fixture(uncertain_root.path());
    let uncertain_service = verification::reopen(uncertain_root.path(), "uncertain.sock");
    let journal = uncertain_root
        .path()
        .join("authorizations/promotions/promotion");
    fs::create_dir_all(&journal).unwrap();
    fs::write(
        journal.join("request.json"),
        serde_json::to_vec(&uncertain.request).unwrap(),
    )
    .unwrap();
    fs::write(journal.join("0.grant"), b"0").unwrap();
    assert!(
        uncertain_service
            .prepare_saved_refresh(1000, &selection)
            .is_err()
    );
    assert_eq!(
        uncertain_service
            .saved_result(1000, "promotion")
            .unwrap()
            .request,
        uncertain.request
    );
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
fn saved_review_pages_exact_bytes_after_restart_without_deciding() {
    use louiselm_skills::broker::promotion::{SavedReviewCursor, SavedReviewSide};

    let root = TempDir::new().unwrap();
    let saved = fixture(root.path());
    let service = verification::reopen(root.path(), "review-first.sock");
    drop(service);
    fs::remove_dir_all(root.path().join("producer-work")).unwrap();
    let restarted = verification::reopen(root.path(), "review-reopened.sock");
    let cursor = SavedReviewCursor {
        file: 0,
        side: SavedReviewSide::Before,
        offset: 0,
    };
    let page = restarted
        .saved_review_page(
            1000,
            "promotion",
            &saved.selection.run_id,
            None,
            cursor.clone(),
            5,
        )
        .unwrap();
    assert_eq!(page.run_id, saved.selection.run_id);
    assert_eq!(page.result_id, "promotion");
    assert_eq!(
        page.agent_id,
        saved.verification.producer.request.launch.agent_id
    );
    assert_eq!(page.base_commit, saved.selection.expected_head);
    let first = page.file.as_ref().unwrap();
    assert_eq!(first.path, "result");
    assert_eq!(first.side, SavedReviewSide::After);
    assert_eq!(first.text, "exact");
    assert!(!first.binary);

    let mut text = first.text.clone();
    let mut next = page.next;
    while let Some(cursor) = next {
        let page = restarted
            .saved_review_page(
                1000,
                "promotion",
                &saved.selection.run_id,
                Some(&page.revision),
                cursor,
                5,
            )
            .unwrap();
        text.push_str(&page.file.as_ref().unwrap().text);
        next = page.next;
    }
    assert_eq!(text, "exact saved result");
    assert!(matches!(
        restarted.saved_review_page(
            1001,
            "promotion",
            &saved.selection.run_id,
            None,
            cursor.clone(),
            5
        ),
        Err(BrokerError::ControllerMismatch)
    ));
    assert!(
        restarted
            .saved_review_page(1000, "promotion", "other-run", None, cursor.clone(), 5)
            .is_err()
    );
    assert!(
        restarted
            .saved_review_page(
                1000,
                "promotion",
                &saved.selection.run_id,
                Some("changed"),
                cursor,
                5
            )
            .is_err()
    );
    assert_eq!(restarted.saved_result(1000, "promotion").unwrap(), saved);
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "One saved publication is observed across interruption, completion, acceptance and restart."
)]
fn saved_review_status_preserves_decisions_and_uncertainty_across_restart() {
    use louiselm_skills::broker::promotion::{
        SavedAcceptance, SavedReviewCursor, SavedReviewSide, SavedReviewState,
    };
    use louiselm_skills::workspace::{
        promotion::ApplicationResult,
        provenance::{OutputProvenance, OutputProvenanceCode},
    };

    let root = TempDir::new().unwrap();
    let saved = fixture(root.path());
    let run = &saved.selection.run_id;
    let cursor = SavedReviewCursor {
        file: 0,
        side: SavedReviewSide::Before,
        offset: 0,
    };
    let service = verification::reopen(root.path(), "status-first.sock");
    assert!(
        service
            .saved_review_status(1000, "promotion", run)
            .unwrap()
            .is_none()
    );
    assert!(
        service
            .saved_review_page(1000, "promotion", run, None, cursor.clone(), 4096)
            .unwrap()
            .file
            .is_some()
    );
    let journal = root.path().join("authorizations/promotions/promotion");
    fs::create_dir_all(&journal).unwrap();
    fs::write(
        journal.join("request.json"),
        serde_json::to_vec(&saved.request).unwrap(),
    )
    .unwrap();
    fs::write(journal.join("0.grant"), b"0").unwrap();
    assert_eq!(
        service
            .saved_review_status(1000, "promotion", run)
            .unwrap()
            .unwrap()
            .state,
        SavedReviewState::ApplicationUnknown
    );
    assert!(
        service
            .saved_review_page(1000, "promotion", run, None, cursor.clone(), 4096)
            .is_err()
    );
    assert!(service.discard_saved_result(1000, "promotion").is_err());
    drop(service);
    let service = verification::reopen(root.path(), "status-reopened.sock");
    let interrupted = service.saved_result(1000, "promotion").unwrap();
    assert_eq!(
        service
            .saved_review_status(1000, "promotion", run)
            .unwrap()
            .unwrap()
            .state,
        SavedReviewState::ApplicationUnknown
    );
    assert!(!journal.join("result.json").exists());
    assert_eq!(
        service.saved_result(1000, "promotion").unwrap(),
        interrupted
    );
    assert_eq!(fs::read_dir(&journal).unwrap().count(), 2);
    assert!(
        !root
            .path()
            .join("authorizations/saved-results/promotion/discarded.json")
            .exists()
    );
    fs::write(journal.join("0.done"), b"0").unwrap();
    fs::write(
        journal.join("result.json"),
        serde_json::to_vec(&ApplicationResult {
            complete: true,
            completed_steps: 1,
            output_provenance: OutputProvenance {
                schema: "louiselm.workspace.output-provenance/1".into(),
                code: OutputProvenanceCode::Untainted,
                taint_digest: None,
                clean_review_refs: vec![],
            },
        })
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        service
            .saved_review_status(1000, "promotion", run)
            .unwrap()
            .unwrap()
            .state,
        SavedReviewState::ApplicationCompleted
    );
    assert!(
        service
            .saved_result(1000, "promotion")
            .unwrap()
            .accepted
            .is_none()
    );
    let accepted = SavedAcceptance {
        commit: "c".repeat(40),
        approval_digest: Digest::of(b"exact review").to_string(),
        request_digest: Digest::of(&serde_json::to_vec(&saved.request).unwrap()).to_string(),
    };
    service
        .accept_saved_result(1000, "promotion", &accepted)
        .unwrap();
    let status = service
        .saved_review_status(1000, "promotion", run)
        .unwrap()
        .unwrap();
    assert_eq!(status.state, SavedReviewState::Accepted);
    let wire = serde_json::to_value(&status).unwrap();
    assert!(wire.get("file").is_none());
    assert!(wire.get("verification_digest").is_none());
    assert!(
        service
            .saved_review_page(1000, "promotion", run, None, cursor.clone(), 4096)
            .is_err()
    );
    assert!(matches!(
        service.saved_review_status(1001, "promotion", run),
        Err(BrokerError::ControllerMismatch)
    ));
    assert!(matches!(
        service.saved_review_status(1000, "promotion", "foreign"),
        Err(BrokerError::RequestMismatch)
    ));
    assert!(service.saved_review_status(1000, "missing", run).is_err());
    drop(service);
    let reopened = verification::reopen(root.path(), "status-accepted.sock");
    assert_eq!(
        reopened
            .saved_review_status(1000, "promotion", run)
            .unwrap()
            .unwrap()
            .state,
        SavedReviewState::Accepted
    );

    let discarded_root = TempDir::new().unwrap();
    let discarded_saved = fixture(discarded_root.path());
    let discard = verification::reopen(discarded_root.path(), "status-discard.sock");
    discard.discard_saved_result(1000, "promotion").unwrap();
    assert_eq!(
        discard
            .saved_review_status(1000, "promotion", &discarded_saved.selection.run_id)
            .unwrap()
            .unwrap()
            .state,
        SavedReviewState::Discarded
    );
    assert!(
        discard
            .saved_review_page(
                1000,
                "promotion",
                &discarded_saved.selection.run_id,
                None,
                cursor,
                4096
            )
            .is_err()
    );
}

#[test]
fn saved_review_bounds_utf8_and_reports_binary_or_corrupt_bytes() {
    use louiselm_skills::broker::promotion::{SavedReviewCursor, SavedReviewSide};

    let root = TempDir::new().unwrap();
    let bytes = format!("{}é🙂end", "a".repeat(4095));
    let saved = fixture_with_bytes(root.path(), bytes.as_bytes());
    let service = verification::reopen(root.path(), "review-large.sock");
    let cursor = SavedReviewCursor {
        file: 0,
        side: SavedReviewSide::Before,
        offset: 0,
    };
    let first = service
        .saved_review_page(
            1000,
            "promotion",
            &saved.selection.run_id,
            None,
            cursor.clone(),
            4096,
        )
        .unwrap();
    assert_eq!(first.file.as_ref().unwrap().text.len(), 4095);
    let second = service
        .saved_review_page(
            1000,
            "promotion",
            &saved.selection.run_id,
            Some(&first.revision),
            first.next.unwrap(),
            4096,
        )
        .unwrap();
    assert_eq!(second.file.unwrap().text, "é🙂end");
    assert!(second.next.is_none());
    let retained = root
        .path()
        .join("authorizations/saved-results/promotion/transfer/job/source/result");
    fs::set_permissions(&retained, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(retained, b"corrupted").unwrap();
    assert!(
        service
            .saved_review_page(
                1000,
                "promotion",
                &saved.selection.run_id,
                None,
                cursor.clone(),
                4096
            )
            .is_err()
    );

    let binary_root = TempDir::new().unwrap();
    let binary = fixture_with_bytes(binary_root.path(), b"binary\0content");
    let binary_service = verification::reopen(binary_root.path(), "review-binary.sock");
    let page = binary_service
        .saved_review_page(
            1000,
            "promotion",
            &binary.selection.run_id,
            None,
            cursor,
            4096,
        )
        .unwrap();
    assert!(page.file.as_ref().unwrap().binary);
    assert_eq!(page.file.unwrap().text, "");
    assert!(page.next.is_none());
}

#[test]
fn saved_review_crosses_authenticated_operator_socket() {
    use louiselm_skills::broker::{
        operator::{
            self, InspectError, OperatorServer, VerificationControlRequest,
            VerificationControlResponse,
        },
        promotion::{SavedReviewCursor, SavedReviewSide},
    };

    let root = TempDir::new().unwrap();
    let saved = fixture(root.path());
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let path = root.path().join("review-operator.sock");
    let uid = rustix::process::geteuid().as_raw();
    let server = OperatorServer::bind(&path, uid).unwrap();
    let service = verification::reopen(root.path(), "review-owner.sock");
    let worker = thread::spawn(move || {
        server
            .serve_once(
                |_, _| Err(InspectError::UnknownSession),
                |_| panic!("not launch-input staging"),
                |request, _| {
                    let VerificationControlRequest::SavedReviewPage {
                        result_id,
                        run_id,
                        revision,
                        cursor,
                        limit,
                    } = request
                    else {
                        panic!("unexpected verification request");
                    };
                    let page = service
                        .saved_review_page(
                            1000,
                            result_id,
                            run_id,
                            revision.as_deref(),
                            cursor.clone(),
                            *limit,
                        )
                        .map_err(|_| InspectError::StatusUnavailable)?;
                    Ok(VerificationControlResponse::SavedReviewPage {
                        page: Box::new(page),
                    })
                },
                |_| panic!("not authorization"),
                |_, _| panic!("not dependencies"),
                |_, _| panic!("not inspection"),
                |_| panic!("not conformance"),
                |_, _, _| panic!("not skill"),
                |_, _| panic!("not Beads"),
                |_, _| panic!("not retention"),
                |_, _, _| panic!("not waiver"),
                |_, _, _| panic!("not extension"),
            )
            .unwrap();
    });
    let response = operator::verification(
        &path,
        uid,
        &VerificationControlRequest::SavedReviewPage {
            result_id: "promotion".into(),
            run_id: saved.selection.run_id,
            revision: None,
            cursor: SavedReviewCursor {
                file: 0,
                side: SavedReviewSide::Before,
                offset: 0,
            },
            limit: 4096,
        },
        Duration::from_secs(3),
    )
    .unwrap();
    worker.join().unwrap();
    let VerificationControlResponse::SavedReviewPage { page } = response else {
        panic!("missing review page")
    };
    assert_eq!(page.file.unwrap().text, "exact saved result");
    assert_eq!(page.current_verification, "unavailable");
}

#[test]
fn saved_decision_crosses_authenticated_operator_socket_without_content() {
    use louiselm_skills::broker::{
        operator::{
            self, InspectError, OperatorServer, VerificationControlRequest,
            VerificationControlResponse,
        },
        promotion::{SavedReviewCursor, SavedReviewSide},
    };
    let root = TempDir::new().unwrap();
    let saved = fixture(root.path());
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let path = root.path().join("decision-operator.sock");
    let uid = rustix::process::geteuid().as_raw();
    let server = OperatorServer::bind(&path, uid).unwrap();
    let service = verification::reopen(root.path(), "decision-owner.sock");
    service.discard_saved_result(1000, "promotion").unwrap();
    let worker = thread::spawn(move || {
        server
            .serve_once(
                |_, _| Err(InspectError::UnknownSession),
                |_| panic!("not launch-input staging"),
                |request, _| {
                    let VerificationControlRequest::SavedReviewPage {
                        result_id, run_id, ..
                    } = request
                    else {
                        panic!("unexpected request")
                    };
                    let status = service
                        .saved_review_status(1000, result_id, run_id)
                        .map_err(|_| InspectError::StatusUnavailable)?
                        .ok_or(InspectError::StatusUnavailable)?;
                    Ok(VerificationControlResponse::SavedReviewStatus {
                        status: Box::new(status),
                    })
                },
                |_| panic!("not authorization"),
                |_, _| panic!("not dependencies"),
                |_, _| panic!("not inspection"),
                |_| panic!("not conformance"),
                |_, _, _| panic!("not skill"),
                |_, _| panic!("not Beads"),
                |_, _| panic!("not retention"),
                |_, _, _| panic!("not waiver"),
                |_, _, _| panic!("not extension"),
            )
            .unwrap();
    });
    let response = operator::verification(
        &path,
        uid,
        &VerificationControlRequest::SavedReviewPage {
            result_id: "promotion".into(),
            run_id: saved.selection.run_id,
            revision: None,
            cursor: SavedReviewCursor {
                file: 0,
                side: SavedReviewSide::Before,
                offset: 0,
            },
            limit: 4096,
        },
        Duration::from_secs(3),
    )
    .unwrap();
    worker.join().unwrap();
    let wire = serde_json::to_value(response).unwrap();
    assert_eq!(wire["kind"], "saved_review_status");
    assert_eq!(wire["status"]["state"], "discarded");
    assert!(wire["status"].get("file").is_none());
    assert!(wire["status"].get("verification_digest").is_none());
}

#[test]
fn saved_attention_reconciles_durably_and_only_a_decision_clears_it() {
    use louiselm_skills::broker::attention::{Outbox, ProjectionChange};
    let root = tempfile::tempdir().unwrap();
    let saved = fixture(root.path());
    let service = verification::reopen(root.path(), "pending.sock");
    service.reconcile_saved_promotions().unwrap();
    service.reconcile_saved_promotions().unwrap();
    let outbox_path = root.path().join("authorizations/attention-outbox");
    let outbox = Outbox::open(&outbox_path).unwrap();
    let pending = outbox.next().unwrap().unwrap();
    let wire = pending.wire();
    assert_eq!(wire["change"]["attention"]["kind"], "promotion_pending");
    assert_eq!(
        wire["change"]["attention"]["subject_id"],
        saved.selection.run_id
    );
    assert_eq!(wire["change"]["attention"]["result_id"], "promotion");
    assert!(!wire.to_string().contains("verification"));
    assert!(!wire.to_string().contains("exact saved result"));
    outbox
        .acknowledge(pending.sequence, &pending.digest())
        .unwrap();
    service.reconcile_saved_promotions().unwrap();
    assert!(outbox.next().unwrap().is_none());
    assert!(!service.saved_result(1000, "promotion").unwrap().discarded);
    drop(service);
    let restarted = verification::reopen(root.path(), "pending-reopened.sock");
    restarted.reconcile_saved_promotions().unwrap();
    assert!(
        Outbox::open(&outbox_path)
            .unwrap()
            .next()
            .unwrap()
            .is_none()
    );
    restarted.discard_saved_result(1000, "promotion").unwrap();
    restarted.reconcile_saved_promotions().unwrap();
    let cleared = outbox.next().unwrap().unwrap();
    let ProjectionChange::Upsert(created) = pending.change else {
        panic!("pending creation")
    };
    let ProjectionChange::Clear(resolved) = cleared.change else {
        panic!("exact decision clear")
    };
    assert_eq!(created, resolved);
    assert_eq!(cleared.sequence, pending.sequence + 1);
}

#[test]
fn immutable_refresh_lineage_survives_discard_and_reopen_without_forging_authority() {
    use louiselm_skills::broker::promotion::{RefreshInput, RefreshSelection};
    let root = tempfile::tempdir().unwrap();
    let saved = fixture(root.path());
    let directory = root.path().join("authorizations/saved-results/promotion");
    let original_bytes = fs::read(directory.join("saved.json")).unwrap();
    let input_root = root.path().join("verification-inputs/refresh-storage");
    fs::create_dir_all(&input_root).unwrap();
    let copied = std::process::Command::new("/bin/cp")
        .arg("-R")
        .arg(directory.join("transfer"))
        .arg(input_root.join("transfer"))
        .status()
        .unwrap();
    assert!(copied.success());
    let input = RefreshInput {
        schema: "louiselm.workspace.refresh/1".into(),
        selection: RefreshSelection {
            result_id: "promotion".into(),
            revision_digest: saved.request.verification_digest.clone(),
            input_id: "refresh-storage".into(),
            snapshot: directory.join("transfer/snapshot"),
            snapshot_digest: saved.request.job.snapshot_digest.clone(),
            expected_head: saved.selection.expected_head.clone(),
            plan: saved.selection.plan.clone(),
            plan_digest: saved.request.job.plan_digest.clone(),
        },
        producer: saved.verification.producer.clone(),
        job: saved.request.job.clone(),
        controller_uid: 1000,
    };
    let input_bytes = serde_json::to_vec(&input).unwrap();
    fs::write(input_root.join("refresh.json"), &input_bytes).unwrap();
    let mut revised = saved.clone();
    revised.verification.execution.request.launch.session_id = "fresh-verifier".into();
    revised.verification.execution.request.launch.run_id = "fresh-run".into();
    revised.verification.execution.request.operation = VerificationOperation::Refresh {
        input_id: input.selection.input_id.clone(),
        input_digest: Digest::of(&input_bytes).to_string(),
        job_digest: input.job.job_digest.clone(),
    };
    revised.request.verifier_session_id = "fresh-verifier".into();
    revised.selection.verifier_session_id = "fresh-verifier".into();
    revised.request.verification_digest =
        Digest::of(&serde_json::to_vec(&revised.verification).unwrap()).to_string();
    let revisions = directory.join("revisions");
    fs::create_dir(&revisions).unwrap();
    fs::set_permissions(&revisions, fs::Permissions::from_mode(0o700)).unwrap();
    let published = revisions.join("refresh-storage");
    fs::create_dir(&published).unwrap();
    fs::set_permissions(&published, fs::Permissions::from_mode(0o700)).unwrap();
    let copied = std::process::Command::new("/bin/cp")
        .arg("-R")
        .arg(input_root.join("transfer"))
        .arg(published.join("transfer"))
        .status()
        .unwrap();
    assert!(copied.success());
    let bytes = serde_json::to_vec(&revised).unwrap();
    fs::write(published.join("saved.json"), &bytes).unwrap();
    fs::write(
        published.join("published.json"),
        serde_json::to_vec(&Digest::of(&bytes).to_string()).unwrap(),
    )
    .unwrap();
    fs::write(directory.join("next.json"), serde_json::to_vec(&serde_json::json!({ "input_id": "refresh-storage", "digest": Digest::of(&bytes).to_string() })).unwrap()).unwrap();
    // Synthetic store characterization only: no authenticated verifier/cleanup exists.
    let service = verification::reopen(root.path(), "revision.sock");
    assert_eq!(service.saved_result(1000, "promotion").unwrap(), revised);
    assert!(
        service
            .publish_saved_refresh(1000, &revised.verification)
            .is_err()
    );
    assert!(service.verification_status("fresh-verifier").is_err());
    let discarded = service.discard_saved_result(1000, "promotion").unwrap();
    assert!(discarded.discarded);
    assert_eq!(
        fs::read(directory.join("saved.json")).unwrap(),
        original_bytes
    );
    drop(service);
    let restarted = verification::reopen(root.path(), "revision-reopened.sock");
    assert_eq!(
        restarted.saved_result(1000, "promotion").unwrap(),
        discarded
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
    service.reconcile_saved_promotions().unwrap();
    let outbox = louiselm_skills::broker::attention::Outbox::open(
        &root.path().join("authorizations/attention-outbox"),
    )
    .unwrap();
    let pending = outbox.next().unwrap().unwrap();
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
    service.reconcile_saved_promotions().unwrap();
    assert_eq!(outbox.next().unwrap(), Some(pending));
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
