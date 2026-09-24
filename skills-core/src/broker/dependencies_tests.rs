#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "Durable dependency-policy fixtures assert exact state transitions."
)]
use super::*;
use crate::dependency_fetch::{ApprovedDependencies, Source};

fn starting() -> StartingLockfile {
    StartingLockfile::cargo(b"version = 4\n").unwrap()
}

fn auth(attendance: Attendance) -> PendingAuthorization {
    PendingAuthorization {
        dependencies: Some(ApprovedDependencies {
            input_manifest_digest: Digest::of(b"inputs").to_string(),
            lockfile_path: "Cargo.lock".into(),
            lockfile_digest: starting().digest().into(),
            registries: vec![],
            preapproved: vec![],
            max_fetches: 1,
            max_bytes: 1024,
            expires_at_ms: 60_000,
        }),
        conformance: crate::launch_protocol::ConformanceAuthorization {
            attendance,
            ..Default::default()
        },
        require_cold_recovery: false,
        authorization_id: "auth".into(),
        request_id: "launch".into(),
        request_digest: Digest::of(b"launch").to_string(),
        controller_uid: 1001,
        session_id: "session".into(),
        run_id: "run".into(),
        agent_id: "codex".into(),
        envelope_revision: 1,
        identity: crate::launcher_install::Identity {
            slot: 1,
            uid: 2001,
            gid: 2001,
        },
        expires_at_ms: 60_000,
        broker_loss_grace_ms: 5000,
        commands: None,
        skill_requests: None,
        beads_mutations: None,
        provider_requests: None,
    }
}

fn request(id: &str) -> DependencyRequest {
    DependencyRequest {
        request_id: id.into(),
        candidate: Candidate {
            name: id.into(),
            version: "1.0.0".into(),
            source: Source::Registry {
                registry: "test".into(),
            },
            integrity: Some(Digest::of(b"bytes").to_string()),
        },
        max_bytes: 64,
    }
}

#[test]
fn dependency_batches_are_atomic_durable_and_intents_never_retry_after_restart() {
    let root = tempfile::tempdir().unwrap();
    let auth = auth(Attendance::Interactive);
    let request = request("new");
    let store = Dependencies::open(root.path()).unwrap();
    assert!(matches!(
        store.admit(&auth, starting(), &request, 1).unwrap(),
        Admission::Status(DependencyStatus::Pending { .. })
    ));
    let id = request.candidate.id().unwrap();
    assert!(
        store
            .control(&auth, 1002, Some(std::slice::from_ref(&id)), 2)
            .is_err()
    );
    assert!(
        store
            .control(
                &auth,
                1001,
                Some(&[id.clone(), Digest::of(b"unknown").to_string()]),
                2
            )
            .is_err()
    );
    drop(store);
    let store = Dependencies::open(root.path()).unwrap();
    assert_eq!(
        store.control(&auth, 1001, None, 3).unwrap().pending.len(),
        1
    );
    let view = store
        .control(&auth, 1001, Some(std::slice::from_ref(&id)), 3)
        .unwrap();
    assert_eq!(view.approved, [id]);
    assert!(view.pending.is_empty());
    assert_eq!(
        store
            .control(&auth, 1001, Some(&view.approved), 3)
            .unwrap()
            .approved,
        view.approved,
        "lost approval replies can be retried exactly"
    );
    drop(store);
    let store = Dependencies::open(root.path()).unwrap();
    let Admission::Fetch { permit, record } = store.admit(&auth, starting(), &request, 4).unwrap()
    else {
        panic!("approved exact candidate")
    };
    assert_eq!(permit.candidate(), &request.candidate);
    // A lost process may have already disclosed the name or written bytes.
    drop(store);
    let store = Dependencies::open(root.path()).unwrap();
    assert!(matches!(
        store.admit(&auth, starting(), &request, 5).unwrap(),
        Admission::Status(DependencyStatus::Unknown)
    ));
    store
        .finish(&auth, &record, &DependencyStatus::Unknown)
        .unwrap();
    let mut next = request.clone();
    next.request_id = "second-attempt".into();
    assert!(matches!(
        store.admit(&auth, starting(), &next, 6).unwrap(),
        Admission::Status(DependencyStatus::Denied)
    ));
    next.request_id = request.request_id.clone();
    next.candidate.version = "2.0.0".into();
    assert!(store.admit(&auth, starting(), &next, 6).is_err());
}

#[test]
fn dependency_unattended_and_unset_scope_never_prompt_or_gain_new_authority() {
    let root = tempfile::tempdir().unwrap();
    let store = Dependencies::open(root.path()).unwrap();
    let mut auth = auth(Attendance::Unattended);
    let request = request("secret-from-prompt");
    assert!(matches!(
        store.admit(&auth, starting(), &request, 1).unwrap(),
        Admission::Status(DependencyStatus::Denied)
    ));
    assert!(
        store
            .control(&auth, 1001, None, 2)
            .unwrap()
            .pending
            .is_empty()
    );
    assert!(
        store
            .control(&auth, 1001, Some(&[request.candidate.id().unwrap()]), 2)
            .is_err()
    );
    auth.dependencies = None;
    assert!(store.admit(&auth, starting(), &request, 3).is_err());
}

#[test]
fn dependency_changed_starting_lockfile_is_refused_before_a_candidate_is_recorded() {
    let root = tempfile::tempdir().unwrap();
    let store = Dependencies::open(root.path()).unwrap();
    let auth = auth(Attendance::Interactive);
    let changed = StartingLockfile::cargo(b"version = 4\n# changed after start\n").unwrap();
    assert!(store.admit(&auth, changed, &request("new"), 1).is_err());
    assert!(
        store
            .control(&auth, 1001, None, 2)
            .unwrap()
            .pending
            .is_empty()
    );
}

#[test]
fn dependency_replay_never_recertifies_mutable_cache_bytes_or_refetches() {
    let root = tempfile::tempdir().unwrap();
    let store = Dependencies::open(root.path()).unwrap();
    let mut auth = auth(Attendance::Unattended);
    let request = request("locked");
    auth.dependencies
        .as_mut()
        .unwrap()
        .preapproved
        .push(request.candidate.clone());
    let Admission::Fetch { record, .. } = store.admit(&auth, starting(), &request, 1).unwrap()
    else {
        panic!("preapproved")
    };
    let digest = Digest::of(b"bytes");
    store
        .finish(
            &auth,
            &record,
            &DependencyStatus::Complete {
                artifact: crate::dependency_fetch::Artifact {
                    name: format!("artifact-{}", digest.hex()),
                    digest: digest.to_string(),
                    size: 5,
                    integrity_verified: true,
                },
            },
        )
        .unwrap();
    assert!(matches!(
        store.admit(&auth, starting(), &request, 2).unwrap(),
        Admission::Status(DependencyStatus::Unknown)
    ));
}

#[test]
fn dependency_review_is_deduplicated_and_bounded_in_pages_of_32() {
    let root = tempfile::tempdir().unwrap();
    let store = Dependencies::open(root.path()).unwrap();
    let auth = auth(Attendance::Interactive);
    for index in 0..35 {
        store
            .admit(&auth, starting(), &request(&format!("new-{index}")), 1)
            .unwrap();
    }
    let mut duplicate = request("new-0");
    duplicate.request_id = "same-candidate".into();
    store.admit(&auth, starting(), &duplicate, 2).unwrap();
    let view = store.control(&auth, 1001, None, 3).unwrap();
    assert_eq!(view.pending.len(), 32);
    assert!(view.has_more);
    let ids: Vec<_> = view
        .pending
        .into_iter()
        .map(|entry| entry.candidate_id)
        .collect();
    let view = store.control(&auth, 1001, Some(&ids), 4).unwrap();
    assert_eq!(view.pending.len(), 3);
    assert!(!view.has_more);
}
