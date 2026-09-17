//! Reconciliation preserves uncertainty, attribution, spent budget and exact original bytes.
use super::*;
use crate::{
    beads_mutation::{BeadsControlDecision, BeadsInspectionDetail, BeadsReconciliation},
    broker::attention::{Outbox, ProjectionChange},
};

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "One ordered restart/replay test must retain the original receipt and spent attempt."
)]
fn operator_attestation_never_replays_unknown_writes_or_refunds_the_attempt() {
    struct LostResult(Cell<u32>);
    impl TrackerRunner for LostResult {
        fn run(&self, _: &TrackerInvocation) -> Result<TrackerOutput, BrokerError> {
            self.0.set(self.0.get() + 1);
            Err(BrokerError::TrackerInvocation(std::io::Error::other(
                "lost result after mutation",
            )))
        }
    }
    for conclusion in [
        BeadsReconciliation::Applied,
        BeadsReconciliation::NotApplied,
    ] {
        let root = tempfile::tempdir().unwrap();
        let store = BeadsMutations::open(root.path()).unwrap();
        let outbox = Outbox::open(&root.path().join("outbox")).unwrap();
        let runner = LostResult(Cell::new(0));
        let mut approved = permission(root.path());
        approved.max_mutations = 1;
        let query = request("uncertain");
        assert!(
            store
                .accept(
                    &binding(),
                    &query,
                    &approved,
                    1,
                    &runner,
                    &tracker(root.path())
                )
                .is_err()
        );
        let original = store
            .accept(
                &binding(),
                &query,
                &approved,
                2,
                &runner,
                &tracker(root.path()),
            )
            .unwrap();
        assert_eq!(original.outcome, BeadsMutationOutcome::Unknown);
        let bytes = fs::read(store.request_path(&binding().session_id, &query.request_id)).unwrap();
        let decision = BeadsControlDecision::Reconcile {
            outcome: conclusion,
            evidence_digest: Digest::of(b"independently retained evidence").to_string(),
        };
        assert!(
            store
                .control(1001, &original.operation_id, Some(&decision), 3, &outbox)
                .is_err()
        );
        assert_eq!(
            fs::read_dir(root.path().join("decisions")).unwrap().count(),
            0
        );
        let resolved = store
            .control(1000, &original.operation_id, Some(&decision), 3, &outbox)
            .unwrap();
        let BeadsInspectionDetail::Mutation {
            ref status,
            ref resolution,
            ..
        } = resolved.detail
        else {
            unreachable!()
        };
        assert_eq!(*status, original);
        let resolution = resolution.as_ref().unwrap();
        assert_eq!(resolution.outcome, conclusion);
        assert_eq!(resolution.operator_uid, 1000);
        assert_eq!(resolution.decided_at_ms, 3);
        assert_eq!(
            fs::read(store.request_path(&binding().session_id, &query.request_id)).unwrap(),
            bytes
        );
        assert!(!store.outcome_path(&original.operation_id).exists());
        drop(store);
        let store = BeadsMutations::open(root.path()).unwrap();
        assert_eq!(
            store
                .control(1000, &original.operation_id, None, 4, &outbox)
                .unwrap(),
            resolved
        );
        assert_eq!(
            store
                .control(1000, &original.operation_id, Some(&decision), 5, &outbox)
                .unwrap(),
            resolved
        );
        let conflicting = BeadsControlDecision::Reconcile {
            outcome: conclusion,
            evidence_digest: Digest::of(b"different evidence").to_string(),
        };
        assert!(
            store
                .control(1000, &original.operation_id, Some(&conflicting), 6, &outbox)
                .is_err()
        );
        assert_eq!(
            store
                .accept(
                    &binding(),
                    &query,
                    &approved,
                    7,
                    &runner,
                    &tracker(root.path())
                )
                .unwrap(),
            original
        );
        assert!(
            store
                .accept(
                    &binding(),
                    &request("new-attempt"),
                    &approved,
                    8,
                    &runner,
                    &tracker(root.path())
                )
                .is_err()
        );
        assert_eq!(runner.0.get(), 1);
    }
}

#[test]
fn completed_outcomes_cannot_be_reconciled_and_failed_outcomes_do_not_prove_no_write() {
    for exit in [0, 7] {
        let root = tempfile::tempdir().unwrap();
        let store = BeadsMutations::open(root.path()).unwrap();
        let outbox = Outbox::open(&root.path().join("outbox")).unwrap();
        let status = store
            .accept(
                &binding(),
                &request("one"),
                &permission(root.path()),
                1,
                &FakeRunner::new(exit),
                &tracker(root.path()),
            )
            .unwrap();
        let decision = BeadsControlDecision::Reconcile {
            outcome: BeadsReconciliation::Applied,
            evidence_digest: Digest::of(b"canonical observation").to_string(),
        };
        let result = store.control(1000, &status.operation_id, Some(&decision), 2, &outbox);
        assert_eq!(result.is_ok(), exit != 0);
        if let Ok(inspected) = result {
            let BeadsInspectionDetail::Mutation {
                status: original, ..
            } = inspected.detail
            else {
                unreachable!()
            };
            assert_eq!(original, status);
        }
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "One ordered lifecycle proves deduplication survives dismissal, restart and outbox acknowledgement."
)]
fn escalation_deduplicates_scope_across_request_ids_restart_and_dismissal() {
    let root = tempfile::tempdir().unwrap();
    let store = BeadsMutations::open(root.path()).unwrap();
    let outbox = Outbox::open(&root.path().join("outbox")).unwrap();
    let mut query = request("one");
    query.required = true;
    let project = tracker(root.path()).project_digest();
    let first = store
        .escalate(&binding(), &query, project.clone(), 1, &outbox)
        .unwrap();
    query.request_id = "different-retry".into();
    query.kind = BeadsMutationKind::CommentAdd {
        issue_id: "louiselm-qbr.5.1.5".into(),
        text: "PRIVATE-PAYLOAD".into(),
    };
    assert_eq!(
        store
            .escalate(&binding(), &query, project.clone(), 2, &outbox)
            .unwrap(),
        first
    );
    assert_eq!(
        fs::read_dir(root.path().join("escalations"))
            .unwrap()
            .count(),
        1
    );
    let path = fs::read_dir(root.path().join("escalations"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert!(
        !fs::read_to_string(path)
            .unwrap()
            .contains("PRIVATE-PAYLOAD")
    );
    assert!(
        store
            .control(
                1001,
                &first.operation_id,
                Some(&BeadsControlDecision::Dismiss),
                3,
                &outbox
            )
            .is_err()
    );
    let dismissed = store
        .control(
            1000,
            &first.operation_id,
            Some(&BeadsControlDecision::Dismiss),
            3,
            &outbox,
        )
        .unwrap();
    assert!(matches!(
        dismissed.detail,
        BeadsInspectionDetail::Escalation {
            dismissed: true,
            ..
        }
    ));
    drop(store);
    let store = BeadsMutations::open(root.path()).unwrap();
    assert_eq!(
        store
            .escalate(&binding(), &query, project, 4, &outbox)
            .unwrap(),
        first
    );
    assert_eq!(
        store
            .control(
                1000,
                &first.operation_id,
                Some(&BeadsControlDecision::Dismiss),
                5,
                &outbox
            )
            .unwrap(),
        dismissed
    );
    assert_eq!(
        fs::read_dir(root.path().join("requests")).unwrap().count(),
        0
    );
    assert_eq!(
        fs::read_dir(root.path().join("outbox/entries"))
            .unwrap()
            .count(),
        2
    );
    let raised = outbox.next().unwrap().unwrap();
    assert!(matches!(raised.change, ProjectionChange::Upsert(_)));
    assert_eq!(
        raised.wire()["change"]["attention"]["kind"],
        "permission_required"
    );
    outbox
        .acknowledge(raised.sequence, &raised.digest())
        .unwrap();
    let cleared = outbox.next().unwrap().unwrap();
    assert!(matches!(cleared.change, ProjectionChange::Clear(_)));
    outbox
        .acknowledge(cleared.sequence, &cleared.digest())
        .unwrap();
    assert!(outbox.next().unwrap().is_none());
}

#[test]
fn failed_escalation_projection_retries_the_original_durable_identity() {
    let root = tempfile::tempdir().unwrap();
    let store = BeadsMutations::open(root.path()).unwrap();
    let outbox = Outbox::open(&root.path().join("outbox")).unwrap();
    let entries = root.path().join("outbox/entries");
    fs::rename(&entries, root.path().join("outbox/saved")).unwrap();
    fs::write(&entries, b"unavailable").unwrap();
    let mut query = request("one");
    query.required = true;
    assert!(
        store
            .escalate(
                &binding(),
                &query,
                tracker(root.path()).project_digest(),
                1,
                &outbox
            )
            .is_err()
    );
    let path = fs::read_dir(root.path().join("escalations"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let before = fs::read(&path).unwrap();
    fs::remove_file(&entries).unwrap();
    fs::rename(root.path().join("outbox/saved"), &entries).unwrap();
    let result = store
        .escalate(
            &binding(),
            &query,
            tracker(root.path()).project_digest(),
            2,
            &outbox,
        )
        .unwrap();
    assert_eq!(fs::read(path).unwrap(), before);
    assert_eq!(
        outbox.next().unwrap().unwrap().wire()["change"]["attention"]["source_operation_id"],
        result.operation_id
    );
}
