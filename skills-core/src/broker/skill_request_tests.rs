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
        expires_at_ms: 5000,
    }
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
