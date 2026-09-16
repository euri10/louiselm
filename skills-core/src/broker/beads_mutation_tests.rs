#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "Fixtures assert storage and tracker outcomes."
)]
use super::*;
use crate::broker::tracker_runner::TrackerOutput;
use std::cell::Cell;

struct FakeRunner {
    exit_code: Option<i32>,
    calls: Cell<u32>,
}

impl FakeRunner {
    fn new(exit_code: i32) -> Self {
        Self {
            exit_code: Some(exit_code),
            calls: Cell::new(0),
        }
    }

    fn calls(&self) -> u32 {
        self.calls.get()
    }
}

impl TrackerRunner for FakeRunner {
    fn run(&self, _invocation: &TrackerInvocation) -> Result<TrackerOutput, BrokerError> {
        self.calls.set(self.calls.get() + 1);
        Ok(TrackerOutput {
            exit_code: self.exit_code,
        })
    }
}

fn binding() -> Binding {
    Binding {
        session_id: "session-1".to_owned(),
        run_id: "run-1".to_owned(),
        agent_id: "claude".to_owned(),
        envelope_revision: 1,
        controller_uid: 1000,
    }
}

fn request(id: &str) -> BeadsMutationRequest {
    BeadsMutationRequest {
        request_id: id.to_owned(),
        kind: BeadsMutationKind::CommentAdd {
            issue_id: "louiselm-qbr.5.1.5".to_owned(),
            text: "hello".to_owned(),
        },
    }
}

fn tracker(root: &std::path::Path) -> TrackerConfig {
    TrackerConfig {
        program: std::path::PathBuf::from("/usr/bin/true"),
        program_digest: Digest::of(b"test-runner"),
        workspace_root: root.to_path_buf(),
        scratch: root.to_path_buf(),
    }
}

#[test]
fn first_request_invokes_the_runner_once_and_records_success() {
    let root = tempfile::tempdir().unwrap();
    let store = BeadsMutations::open(root.path()).unwrap();
    let runner = FakeRunner::new(0);
    let status = store
        .accept(
            &binding(),
            &request("req-1"),
            &permission(),
            1,
            &runner,
            &tracker(root.path()),
        )
        .unwrap();
    assert_eq!(status.outcome, BeadsMutationOutcome::Completed);
    assert_eq!(runner.calls(), 1);
}

#[test]
fn identical_replay_returns_the_same_receipt_without_invoking_again() {
    let root = tempfile::tempdir().unwrap();
    let store = BeadsMutations::open(root.path()).unwrap();
    let runner = FakeRunner::new(0);
    let first = store
        .accept(
            &binding(),
            &request("req-1"),
            &permission(),
            1,
            &runner,
            &tracker(root.path()),
        )
        .unwrap();
    let second = store
        .accept(
            &binding(),
            &request("req-1"),
            &permission(),
            2,
            &runner,
            &tracker(root.path()),
        )
        .unwrap();
    assert_eq!(first.operation_id, second.operation_id);
    assert_eq!(second.outcome, BeadsMutationOutcome::Completed);
    assert_eq!(runner.calls(), 1);
}

#[test]
fn a_durable_intent_with_no_outcome_never_reinvokes_the_tracker() {
    let root = tempfile::tempdir().unwrap();
    let store = BeadsMutations::open(root.path()).unwrap();

    // Both a crash before execution and a crash after br commits leave this
    // state. Absence of an outcome cannot prove the comment was not posted.
    let record = Record {
        binding: binding(),
        request_id: "req-1".into(),
        request_digest: Digest::of(&serde_json::to_vec(&request("req-1")).unwrap()).to_string(),
        project_digest: tracker(root.path()).project_digest(),
        operation_id: "01234567-89ab-4cde-8f01-23456789abcd".to_owned(),
        created_at_ms: 1,
    };
    write_new_record(&store.request_path(&binding().session_id, "req-1"), &record).unwrap();

    let runner = FakeRunner::new(0);
    let status = store
        .accept(
            &binding(),
            &request("req-1"),
            &permission(),
            2,
            &runner,
            &tracker(root.path()),
        )
        .unwrap();
    assert_eq!(status.operation_id, record.operation_id);
    assert_eq!(status.outcome, BeadsMutationOutcome::Unknown);
    assert_eq!(runner.calls(), 0);
}

#[test]
fn durable_receipts_do_not_retain_the_comment_body() {
    let root = tempfile::tempdir().unwrap();
    let store = BeadsMutations::open(root.path()).unwrap();
    let runner = FakeRunner::new(0);
    let request = request("req-private");
    store
        .accept(
            &binding(),
            &request,
            &permission(),
            1,
            &runner,
            &tracker(root.path()),
        )
        .unwrap();
    let bytes =
        fs::read_to_string(store.request_path(&binding().session_id, &request.request_id)).unwrap();
    assert!(
        !bytes.contains("hello"),
        "the broker record must retain only a digest of content"
    );
}

#[test]
fn nonzero_exit_records_failed_not_completed() {
    let root = tempfile::tempdir().unwrap();
    let store = BeadsMutations::open(root.path()).unwrap();
    let runner = FakeRunner::new(7);
    let status = store
        .accept(
            &binding(),
            &request("req-1"),
            &permission(),
            1,
            &runner,
            &tracker(root.path()),
        )
        .unwrap();
    assert_eq!(
        status.outcome,
        BeadsMutationOutcome::Failed { exit_code: Some(7) }
    );
}

#[test]
fn a_conflicting_replay_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let store = BeadsMutations::open(root.path()).unwrap();
    let runner = FakeRunner::new(0);
    store
        .accept(
            &binding(),
            &request("req-1"),
            &permission(),
            1,
            &runner,
            &tracker(root.path()),
        )
        .unwrap();
    let mut conflicting = request("req-1");
    conflicting.kind = BeadsMutationKind::CommentAdd {
        issue_id: "louiselm-qbr.5.1.5".to_owned(),
        text: "different text".to_owned(),
    };
    let error = store
        .accept(
            &binding(),
            &conflicting,
            &permission(),
            2,
            &runner,
            &tracker(root.path()),
        )
        .expect_err("changed content under the same request id must be refused");
    assert!(matches!(error, BrokerError::RequestMismatch));
}

#[test]
fn durability_survives_a_restart() {
    let root = tempfile::tempdir().unwrap();
    {
        let store = BeadsMutations::open(root.path()).unwrap();
        let runner = FakeRunner::new(0);
        store
            .accept(
                &binding(),
                &request("req-1"),
                &permission(),
                1,
                &runner,
                &tracker(root.path()),
            )
            .unwrap();
    }
    let store = BeadsMutations::open(root.path()).unwrap();
    let runner = FakeRunner::new(0);
    let status = store
        .accept(
            &binding(),
            &request("req-1"),
            &permission(),
            2,
            &runner,
            &tracker(root.path()),
        )
        .unwrap();
    assert_eq!(status.outcome, BeadsMutationOutcome::Completed);
    assert_eq!(
        runner.calls(),
        0,
        "a durably recorded outcome must not re-invoke br"
    );
}

fn permission() -> ApprovedBeadsComments {
    ApprovedBeadsComments {
        issue_ids: vec!["louiselm-qbr.5.1.5".into()],
        max_comments: 2,
        expires_at_ms: 100,
    }
}

#[test]
fn a_runner_error_leaves_an_unknown_nonrepeatable_attempt() {
    struct Uncertain(Cell<u32>);
    impl TrackerRunner for Uncertain {
        fn run(&self, _: &TrackerInvocation) -> Result<TrackerOutput, BrokerError> {
            self.0.set(self.0.get() + 1);
            Err(BrokerError::TrackerInvocation(std::io::Error::other(
                "lost exit status",
            )))
        }
    }
    let root = tempfile::tempdir().unwrap();
    let store = BeadsMutations::open(root.path()).unwrap();
    let runner = Uncertain(Cell::new(0));
    assert!(
        store
            .accept(
                &binding(),
                &request("uncertain"),
                &permission(),
                1,
                &runner,
                &tracker(root.path())
            )
            .is_err()
    );
    drop(store);
    let store = BeadsMutations::open(root.path()).unwrap();
    let status = store
        .accept(
            &binding(),
            &request("uncertain"),
            &permission(),
            2,
            &runner,
            &tracker(root.path()),
        )
        .unwrap();
    assert_eq!(status.outcome, BeadsMutationOutcome::Unknown);
    assert_eq!(runner.0.get(), 1);
}

#[test]
fn comment_budget_survives_restart_and_identical_replays_do_not_spend_it() {
    let root = tempfile::tempdir().unwrap();
    let store = BeadsMutations::open(root.path()).unwrap();
    let runner = FakeRunner::new(0);
    let mut permission = permission();
    permission.max_comments = 1;
    let first = store
        .accept(
            &binding(),
            &request("one"),
            &permission,
            1,
            &runner,
            &tracker(root.path()),
        )
        .unwrap();
    drop(store);
    let store = BeadsMutations::open(root.path()).unwrap();
    assert_eq!(
        store
            .accept(
                &binding(),
                &request("one"),
                &permission,
                2,
                &runner,
                &tracker(root.path())
            )
            .unwrap(),
        first
    );
    assert!(
        store
            .accept(
                &binding(),
                &request("two"),
                &permission,
                2,
                &runner,
                &tracker(root.path())
            )
            .is_err()
    );
    let mut other = binding();
    other.session_id = "second-session".into();
    assert!(
        store
            .accept(
                &other,
                &request("one"),
                &permission,
                2,
                &runner,
                &tracker(root.path())
            )
            .is_ok()
    );
    assert_eq!(runner.calls(), 2);
}

#[test]
fn replay_cannot_move_to_another_actor_project_or_revision() {
    let root = tempfile::tempdir().unwrap();
    let store = BeadsMutations::open(root.path()).unwrap();
    let runner = FakeRunner::new(0);
    store
        .accept(
            &binding(),
            &request("one"),
            &permission(),
            1,
            &runner,
            &tracker(root.path()),
        )
        .unwrap();
    for field in ["agent", "revision", "project"] {
        let mut binding = binding();
        let mut tracker = tracker(root.path());
        match field {
            "agent" => binding.agent_id = "forged".into(),
            "revision" => binding.envelope_revision += 1,
            _ => tracker.workspace_root = root.path().join("other"),
        }
        assert!(
            store
                .accept(
                    &binding,
                    &request("one"),
                    &permission(),
                    2,
                    &runner,
                    &tracker
                )
                .is_err()
        );
    }
    assert_eq!(runner.calls(), 1);
}

#[test]
fn simultaneous_sessions_share_the_store_without_sharing_retry_identities() {
    struct Concurrent(std::sync::atomic::AtomicU32);
    impl TrackerRunner for Concurrent {
        fn run(&self, _: &TrackerInvocation) -> Result<TrackerOutput, BrokerError> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(TrackerOutput { exit_code: Some(0) })
        }
    }
    let root = tempfile::tempdir().unwrap();
    let store = BeadsMutations::open(root.path()).unwrap();
    let runner = Concurrent(std::sync::atomic::AtomicU32::new(0));
    let tracker = tracker(root.path());
    let barrier = std::sync::Barrier::new(4);
    std::thread::scope(|scope| {
        for session in ["one", "one", "two", "two"] {
            let store = &store;
            let runner = &runner;
            let tracker = &tracker;
            let barrier = &barrier;
            scope.spawn(move || {
                let mut binding = binding();
                binding.session_id = session.into();
                barrier.wait();
                let mut permission = permission();
                permission.expires_at_ms = 60_000;
                assert_eq!(
                    store
                        .accept(
                            &binding,
                            &request("same-key"),
                            &permission,
                            1,
                            runner,
                            tracker
                        )
                        .unwrap()
                        .outcome,
                    BeadsMutationOutcome::Completed
                );
            });
        }
    });
    assert_eq!(runner.0.load(std::sync::atomic::Ordering::SeqCst), 2);
}
