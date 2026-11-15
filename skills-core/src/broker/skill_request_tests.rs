//! Durable replay and cancellation contracts, independent of delivery availability.
#![allow(clippy::unwrap_used, reason = "Fixtures assert storage outcomes.")]
use super::*;
use crate::skill_request::{
    ApprovedSkillRequests, SkillRequest, SkillRequestOutcome, SkillSubject,
};

fn binding() -> Binding {
    Binding {
        session_id: "session-1".into(),
        run_id: "12345678-1234-4234-8234-123456789abc".into(),
        envelope_revision: 1,
        controller_uid: 1000,
    }
}
fn request() -> SkillRequest {
    SkillRequest {
        request_id: "request-1".into(),
        subject: SkillSubject::Session,
        packages: vec![crate::Digest::of(b"package").to_string()],
        agents: vec!["codex".into()],
    }
}
fn permission() -> ApprovedSkillRequests {
    ApprovedSkillRequests {
        agents: vec!["codex".into()],
        allow_run: true,
        allow_supply: false,
        expires_at_ms: 5000,
    }
}

fn supply_request() -> SkillRequest {
    let mut value = serde_json::to_value(request()).unwrap();
    value["subject"] = serde_json::json!("supply");
    let mut packages = vec![
        crate::Digest::of(b"package").to_string(),
        crate::Digest::of(b"second-package").to_string(),
    ];
    packages.sort();
    value["packages"] = serde_json::json!(packages);
    serde_json::from_value(value).unwrap()
}

fn supply_permission() -> ApprovedSkillRequests {
    serde_json::from_value(serde_json::json!({
        "agents": ["codex"], "allow_run": false,
        "allow_supply": true, "expires_at_ms": 5000,
    }))
    .unwrap()
}

#[test]
fn supply_permission_is_explicit_and_independent_of_run_permission() {
    let request = supply_request();
    let missing: ApprovedSkillRequests = serde_json::from_value(serde_json::json!({
        "agents": ["codex"], "allow_run": true, "expires_at_ms": 5000,
    }))
    .unwrap();
    assert!(!missing.permits(&request, 1000));
    assert!(supply_permission().permits(&request, 1000));
    assert!(!supply_permission().permits(&request, 5000));
    let mut run = request.clone();
    run.subject = SkillSubject::Run;
    assert!(!supply_permission().permits(&run, 1000));
    let mut foreign_agent = request.clone();
    foreign_agent.agents = vec!["other".into()];
    assert!(!supply_permission().permits(&foreign_agent, 1000));

    let root = tempfile::tempdir().unwrap();
    let store = SkillRequests::open(&root.path().join("requests")).unwrap();
    let outbox = Outbox::open(&root.path().join("outbox")).unwrap();
    for permission in [None, Some(&missing)] {
        assert!(matches!(
            store.accept(&binding(), &request, permission, 1000, &outbox),
            Err(BrokerError::InvalidGrant)
        ));
    }
    assert!(store.records().unwrap().is_empty());
    assert!(outbox.next().unwrap().is_none());
}

#[test]
fn supply_batch_outlives_origin_and_restarts_until_explicit_outcome() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("requests");
    let store = SkillRequests::open(&path).unwrap();
    let outbox = Outbox::open(&root.path().join("outbox")).unwrap();
    let request = supply_request();
    let accepted = store
        .accept(
            &binding(),
            &request,
            Some(&supply_permission()),
            1000,
            &outbox,
        )
        .unwrap();
    let pending = outbox.next().unwrap().unwrap();
    let wire = pending.wire();
    assert_eq!(wire["change"]["attention"]["subject_kind"], "supply");
    assert_eq!(wire["change"]["attention"]["subject_id"], "1000");
    assert_eq!(
        wire["change"]["attention"]["kind"],
        "skill_approval_pending"
    );
    assert!(wire["change"]["attention"].get("packages").is_none());
    outbox
        .acknowledge(pending.sequence, &pending.digest())
        .unwrap();
    store
        .end_subject(&AttentionSubject::Session(binding().session_id), &outbox)
        .unwrap();
    store
        .observe_run(
            &RunLifecycle {
                run_id: binding().run_id,
                revision: 1,
                state: RunState::Disposed,
            },
            &outbox,
        )
        .unwrap();
    drop(store);
    let store = SkillRequests::open(&path).unwrap();
    store.reconcile(&outbox).unwrap();
    assert_eq!(
        store.inspect(1000, &accepted.operation_id).unwrap(),
        accepted
    );
    assert!(outbox.next().unwrap().is_none());
    assert_eq!(
        store
            .accept(
                &binding(),
                &request,
                Some(&supply_permission()),
                1001,
                &outbox
            )
            .unwrap(),
        accepted
    );
    let mut late = request.clone();
    late.request_id = "late-supply".into();
    assert!(matches!(
        store.accept(&binding(), &late, Some(&supply_permission()), 1001, &outbox),
        Err(BrokerError::InvalidGrant)
    ));
    assert!(matches!(
        store.inspect(2000, &accepted.operation_id),
        Err(BrokerError::ControllerMismatch)
    ));
    assert!(matches!(
        store.finish(
            2000,
            &accepted.operation_id,
            SkillRequestOutcome::Rejected,
            &outbox
        ),
        Err(BrokerError::ControllerMismatch)
    ));
    let rejected = store
        .finish(
            1000,
            &accepted.operation_id,
            SkillRequestOutcome::Rejected,
            &outbox,
        )
        .unwrap();
    assert_eq!(rejected.outcome, SkillRequestOutcome::Rejected);
    let cleared = outbox.next().unwrap().unwrap();
    assert_eq!(
        cleared.wire()["change"]["key"],
        serde_json::json!({
            "subject_kind": "supply", "subject_id": "1000", "kind": "skill_approval_pending",
            "source_operation_id": accepted.operation_id,
        })
    );
    assert!(matches!(cleared.change, ProjectionChange::Clear(_)));
}

#[test]
fn subject_end_is_ordered_with_one_or_many_requested_packages() {
    for subject in [SkillSubject::Session, SkillSubject::Run] {
        for batch in [false, true] {
            for signing_first in [false, true] {
                let root = tempfile::tempdir().unwrap();
                let path = root.path().join("requests");
                let store = SkillRequests::open(&path).unwrap();
                let outbox = Outbox::open(&root.path().join("outbox")).unwrap();
                let mut item = request();
                item.subject = subject;
                if batch {
                    item.packages
                        .push(crate::Digest::of(b"another").to_string());
                    item.packages.sort();
                }
                if subject == SkillSubject::Run {
                    store
                        .observe_run(
                            &RunLifecycle {
                                run_id: binding().run_id,
                                revision: 1,
                                state: RunState::Active,
                            },
                            &outbox,
                        )
                        .unwrap();
                }
                let pending = store
                    .accept(&binding(), &item, Some(&permission()), 1000, &outbox)
                    .unwrap();
                if signing_first {
                    store
                        .reserve_signing(1000, &pending.operation_id, &[], None)
                        .unwrap();
                }
                if subject == SkillSubject::Session {
                    store
                        .end_subject(&AttentionSubject::Session(binding().session_id), &outbox)
                        .unwrap();
                } else {
                    store
                        .observe_run(
                            &RunLifecycle {
                                run_id: binding().run_id,
                                revision: 2,
                                state: RunState::Disposed,
                            },
                            &outbox,
                        )
                        .unwrap();
                }
                drop(store);
                let store = SkillRequests::open(&path).unwrap();
                store.reconcile(&outbox).unwrap();
                assert_eq!(
                    store.status(&pending.operation_id).unwrap().outcome,
                    if signing_first {
                        SkillRequestOutcome::Pending
                    } else {
                        SkillRequestOutcome::Cancelled
                    }
                );
                assert!(matches!(
                    store.reserve_signing(1000, &pending.operation_id, &[], None),
                    Err(BrokerError::RequestMismatch)
                ));
            }
        }
    }
}

#[test]
fn uncertain_reservation_bytes_never_authorize_signing_or_cancellation() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("requests");
    let store = SkillRequests::open(&path).unwrap();
    let outbox = Outbox::open(&root.path().join("outbox")).unwrap();
    let pending = store
        .accept(&binding(), &request(), Some(&permission()), 1000, &outbox)
        .unwrap();
    // Corrupt evidence never authorizes a retry or terminal decision.
    fs::write(
        path.join("signing")
            .join(format!("{}.json", pending.operation_id)),
        b"\"",
    )
    .unwrap();
    drop(store);
    let store = SkillRequests::open(&path).unwrap();
    assert!(matches!(
        store.reserve_signing(1000, &pending.operation_id, &[], None),
        Err(BrokerError::Storage(_))
    ));
    assert!(matches!(
        store.finish(
            1000,
            &pending.operation_id,
            SkillRequestOutcome::Cancelled,
            &outbox
        ),
        Err(BrokerError::Storage(_))
    ));
    assert_eq!(
        store.status(&pending.operation_id).unwrap().outcome,
        SkillRequestOutcome::Pending
    );
}

#[test]
fn an_unpublished_reservation_cannot_block_a_terminal_decision() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("requests");
    let store = SkillRequests::open(&path).unwrap();
    let outbox = Outbox::open(&root.path().join("outbox")).unwrap();
    let pending = store
        .accept(&binding(), &request(), Some(&permission()), 1000, &outbox)
        .unwrap();
    // A crash before publication may leave its private staging file behind.
    fs::write(path.join(".orphaned-reservation-fixture"), b"{").unwrap();
    drop(store);
    let store = SkillRequests::open(&path).unwrap();
    assert_eq!(
        store
            .finish(
                1000,
                &pending.operation_id,
                SkillRequestOutcome::Cancelled,
                &outbox
            )
            .unwrap()
            .outcome,
        SkillRequestOutcome::Cancelled
    );
    assert!(matches!(
        store.reserve_signing(1000, &pending.operation_id, &[], None),
        Err(BrokerError::RequestMismatch)
    ));
}

#[test]
fn a_source_decision_and_a_batch_signing_reservation_are_atomic() {
    for source_first in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("requests");
        let store = SkillRequests::open(&path).unwrap();
        let outbox = Outbox::open(&root.path().join("outbox")).unwrap();
        let source = store
            .accept(
                &binding(),
                &supply_request(),
                Some(&supply_permission()),
                1000,
                &outbox,
            )
            .unwrap();
        let mut batch = supply_request();
        batch.request_id = "batch".into();
        let batch = store
            .accept(
                &binding(),
                &batch,
                Some(&supply_permission()),
                1000,
                &outbox,
            )
            .unwrap();
        let sources = vec![source.operation_id.clone()];
        if source_first {
            assert_eq!(
                store
                    .finish(
                        1000,
                        &source.operation_id,
                        SkillRequestOutcome::Cancelled,
                        &outbox
                    )
                    .unwrap()
                    .outcome,
                SkillRequestOutcome::Cancelled
            );
            assert!(matches!(
                store.reserve_signing(1000, &batch.operation_id, &sources, None),
                Err(BrokerError::RequestMismatch)
            ));
        } else {
            store
                .reserve_signing(1000, &batch.operation_id, &sources, None)
                .unwrap();
            assert_eq!(
                store
                    .finish(
                        1000,
                        &source.operation_id,
                        SkillRequestOutcome::Cancelled,
                        &outbox
                    )
                    .unwrap()
                    .outcome,
                SkillRequestOutcome::Pending
            );
            drop(store);
            let store = SkillRequests::open(&path).unwrap();
            store.reconcile(&outbox).unwrap();
            assert_eq!(
                store.status(&source.operation_id).unwrap().outcome,
                SkillRequestOutcome::Pending
            );
            assert!(matches!(
                store.reserve_signing(1000, &batch.operation_id, &sources, None),
                Err(BrokerError::RequestMismatch)
            ));
        }
    }
}

#[test]
fn batch_reservation_refuses_foreign_duplicate_and_non_supply_sources() {
    let root = tempfile::tempdir().unwrap();
    let store = SkillRequests::open(&root.path().join("requests")).unwrap();
    let outbox = Outbox::open(&root.path().join("outbox")).unwrap();
    let source = store
        .accept(
            &binding(),
            &supply_request(),
            Some(&supply_permission()),
            1000,
            &outbox,
        )
        .unwrap();
    let mut target_request = supply_request();
    target_request.request_id = "target".into();
    let target = store
        .accept(
            &binding(),
            &target_request,
            Some(&supply_permission()),
            1000,
            &outbox,
        )
        .unwrap();
    let mut foreign_binding = binding();
    foreign_binding.session_id = "foreign".into();
    foreign_binding.controller_uid = 2000;
    let foreign = store
        .accept(
            &foreign_binding,
            &supply_request(),
            Some(&supply_permission()),
            1000,
            &outbox,
        )
        .unwrap();
    let mut session_request = request();
    session_request.request_id = "session-source".into();
    let session = store
        .accept(
            &binding(),
            &session_request,
            Some(&permission()),
            1000,
            &outbox,
        )
        .unwrap();
    for sources in [
        vec![target.operation_id.clone()],
        vec![source.operation_id.clone(), source.operation_id.clone()],
        vec!["invalid".into()],
        vec![foreign.operation_id],
        vec![session.operation_id],
    ] {
        assert!(matches!(
            store.reserve_signing(1000, &target.operation_id, &sources, None),
            Err(BrokerError::RequestMismatch)
        ));
        assert_eq!(
            store.status(&target.operation_id).unwrap().outcome,
            SkillRequestOutcome::Pending
        );
    }
    store
        .reserve_signing(1000, &target.operation_id, &[source.operation_id], None)
        .unwrap();
}

#[test]
fn concurrent_supply_retries_create_one_request_and_one_projection() {
    let root = tempfile::tempdir().unwrap();
    let store = SkillRequests::open(&root.path().join("requests")).unwrap();
    let outbox = Outbox::open(&root.path().join("outbox")).unwrap();
    let barrier = std::sync::Barrier::new(4);
    let results = std::thread::scope(|scope| {
        (0..4)
            .map(|_| {
                scope.spawn(|| {
                    barrier.wait();
                    store
                        .accept(
                            &binding(),
                            &supply_request(),
                            Some(&supply_permission()),
                            1000,
                            &outbox,
                        )
                        .unwrap()
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert!(results.iter().all(|result| *result == results[0]));
    assert_eq!(store.records().unwrap().len(), 1);
    let pending = outbox.next().unwrap().unwrap();
    outbox
        .acknowledge(pending.sequence, &pending.digest())
        .unwrap();
    assert!(outbox.next().unwrap().is_none());
}

#[test]
fn supply_projection_requires_canonical_operator_and_exact_outcome() {
    use crate::broker::attention::{AttentionCondition, AttentionReason};
    let root = tempfile::tempdir().unwrap();
    let outbox = Outbox::open(&root.path().join("outbox")).unwrap();
    let valid = AttentionCondition {
        linked_run_id: None,
        subject: AttentionSubject::Supply("1000".into()),
        operation_id: "12345678-1234-4234-8234-123456789abc".into(),
        created_at_ms: 1000,
        reason: AttentionReason::SkillApprovalPending,
    };
    for id in ["", "0", "01000", "+1000", "-1", "4294967296", "operator"] {
        let mut invalid = valid.clone();
        invalid.subject = AttentionSubject::Supply(id.into());
        assert!(matches!(
            outbox.enqueue("invalid", ProjectionChange::Upsert(invalid)),
            Err(BrokerError::InvalidGrant)
        ));
    }
    let mut linked = valid.clone();
    linked.linked_run_id = Some(binding().run_id);
    let mut wrong_kind = valid.clone();
    wrong_kind.reason = AttentionReason::PermissionRequired;
    for invalid in [
        ProjectionChange::Upsert(linked),
        ProjectionChange::Upsert(wrong_kind),
        ProjectionChange::ClearSubject(valid.subject.clone()),
    ] {
        assert!(matches!(
            outbox.enqueue("invalid", invalid),
            Err(BrokerError::InvalidGrant)
        ));
    }
    assert!(outbox.next().unwrap().is_none());
    outbox
        .enqueue("pending", ProjectionChange::Upsert(valid.clone()))
        .unwrap();
    outbox
        .enqueue("settled", ProjectionChange::Clear(valid))
        .unwrap();
}

#[test]
fn retry_restart_and_terminal_outcomes_preserve_exact_operation() {
    let root = tempfile::tempdir().unwrap();
    let store = SkillRequests::open(&root.path().join("requests")).unwrap();
    let outbox = Outbox::open(&root.path().join("outbox")).unwrap();
    let accepted = store
        .accept(&binding(), &request(), Some(&permission()), 1000, &outbox)
        .unwrap();
    assert_eq!(accepted.outcome, SkillRequestOutcome::Pending);
    assert_eq!(
        store
            .accept(&binding(), &request(), Some(&permission()), 1001, &outbox)
            .unwrap(),
        accepted
    );
    let mut conflict = request();
    conflict.packages = vec![crate::Digest::of(b"changed").to_string()];
    assert!(
        store
            .accept(&binding(), &conflict, Some(&permission()), 1001, &outbox)
            .is_err()
    );
    drop(store);
    let store = SkillRequests::open(&root.path().join("requests")).unwrap();
    let cancelled = store
        .finish(
            1000,
            &accepted.operation_id,
            SkillRequestOutcome::Cancelled,
            &outbox,
        )
        .unwrap();
    assert_eq!(cancelled.outcome, SkillRequestOutcome::Cancelled);
    assert_eq!(
        store
            .accept(&binding(), &request(), Some(&permission()), 1002, &outbox)
            .unwrap(),
        cancelled
    );
    assert!(
        store
            .finish(
                2000,
                &accepted.operation_id,
                SkillRequestOutcome::Rejected,
                &outbox
            )
            .is_err()
    );
    assert!(
        store
            .finish(
                1000,
                &accepted.operation_id,
                SkillRequestOutcome::Rejected,
                &outbox
            )
            .is_err()
    );
    let mut again = request();
    again.request_id = "request-2".into();
    assert_ne!(
        store
            .accept(&binding(), &again, Some(&permission()), 1003, &outbox)
            .unwrap()
            .operation_id,
        accepted.operation_id
    );
}

#[test]
fn permission_scope_expiry_and_subject_end_never_expand_authority() {
    let root = tempfile::tempdir().unwrap();
    let store = SkillRequests::open(&root.path().join("requests")).unwrap();
    let outbox = Outbox::open(&root.path().join("outbox")).unwrap();
    assert!(
        store
            .accept(&binding(), &request(), None, 1000, &outbox)
            .is_err()
    );
    assert!(
        store
            .accept(&binding(), &request(), Some(&permission()), 5000, &outbox)
            .is_err()
    );
    let mut wrong = request();
    wrong.agents = vec!["other".into()];
    assert!(
        store
            .accept(&binding(), &wrong, Some(&permission()), 1000, &outbox)
            .is_err()
    );
    let pending = store
        .accept(&binding(), &request(), Some(&permission()), 1000, &outbox)
        .unwrap();
    let mut other = binding();
    other.session_id = "session-2".into();
    let other_request = store
        .accept(&other, &request(), Some(&permission()), 1000, &outbox)
        .unwrap();
    store
        .end_subject(&AttentionSubject::Session("session-1".into()), &outbox)
        .unwrap();
    assert_eq!(
        store.status(&pending.operation_id).unwrap().outcome,
        SkillRequestOutcome::Cancelled
    );
    assert_eq!(
        store.status(&other_request.operation_id).unwrap().outcome,
        SkillRequestOutcome::Pending
    );
    let mut late = request();
    late.request_id = "late".into();
    assert!(
        store
            .accept(&binding(), &late, Some(&permission()), 1001, &outbox)
            .is_err()
    );
}

#[test]
fn run_observations_are_monotonic_and_park_retains_pending_requests() {
    let root = tempfile::tempdir().unwrap();
    let store = SkillRequests::open(&root.path().join("requests")).unwrap();
    let outbox = Outbox::open(&root.path().join("outbox")).unwrap();
    let mut request = request();
    request.subject = SkillSubject::Run;
    assert!(
        store
            .accept(&binding(), &request, Some(&permission()), 1000, &outbox)
            .is_err()
    );
    let mut observation = RunLifecycle {
        run_id: binding().run_id,
        revision: 1,
        state: RunState::Parked,
    };
    store.observe_run(&observation, &outbox).unwrap();
    let pending = store
        .accept(&binding(), &request, Some(&permission()), 1000, &outbox)
        .unwrap();
    observation.revision = 2;
    observation.state = RunState::Disposed;
    store.observe_run(&observation, &outbox).unwrap();
    assert_eq!(
        store.status(&pending.operation_id).unwrap().outcome,
        SkillRequestOutcome::Cancelled
    );
    observation.revision = 1;
    observation.state = RunState::Active;
    assert!(store.observe_run(&observation, &outbox).is_err());
    observation.revision = 3;
    assert!(store.observe_run(&observation, &outbox).is_err());
}

#[test]
fn recovery_completes_projection_and_terminal_fact_crash_windows() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("requests");
    let store = SkillRequests::open(&path).unwrap();
    let outbox = Outbox::open(&root.path().join("outbox")).unwrap();
    let mut run_request = request();
    run_request.subject = SkillSubject::Run;
    let record = Record {
        binding: binding(),
        request: run_request.clone(),
        operation_id: operation_uuid().unwrap(),
        created_at_ms: 1000,
    };
    // Crash after exact request persistence but before its outbox enqueue.
    write_new_record(
        &store.request_path(&record.binding.session_id, &record.request.request_id),
        &record,
    )
    .unwrap();
    assert!(outbox.next().unwrap().is_none());
    // Crash after atomic authoritative terminal observation, before cancellation.
    write_new_record(
        &store.run_path(&record.binding.run_id),
        &RunLifecycle {
            run_id: record.binding.run_id.clone(),
            revision: 2,
            state: RunState::Disposed,
        },
    )
    .unwrap();
    drop(store);
    let store = SkillRequests::open(&path).unwrap();
    store.reconcile(&outbox).unwrap();
    assert_eq!(
        store.status(&record.operation_id).unwrap().outcome,
        SkillRequestOutcome::Cancelled
    );
    let pending = outbox.next().unwrap().unwrap();
    assert!(matches!(pending.change, ProjectionChange::Upsert(_)));
    outbox
        .acknowledge(pending.sequence, &pending.digest())
        .unwrap();
    let cleared = outbox.next().unwrap().unwrap();
    assert!(matches!(cleared.change, ProjectionChange::Clear(_)));
    outbox
        .acknowledge(cleared.sequence, &cleared.digest())
        .unwrap();
    store.reconcile(&outbox).unwrap();
    assert!(outbox.next().unwrap().is_none());
    assert_eq!(
        store
            .accept(&binding(), &run_request, Some(&permission()), 2000, &outbox)
            .unwrap()
            .outcome,
        SkillRequestOutcome::Cancelled
    );
    run_request.request_id = "new-late-request".into();
    assert!(
        store
            .accept(&binding(), &run_request, Some(&permission()), 2000, &outbox)
            .is_err()
    );
}
