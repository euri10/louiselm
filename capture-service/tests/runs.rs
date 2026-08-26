use louiselm_capture::{BeadsCleanup, ReapAction, RunAdmission, RunDraft, RunStore};

fn admission(id: &str, ceiling: u64) -> RunAdmission {
    RunAdmission {
        id: id.to_owned(),
        session_id: "codex/session-123".to_owned(),
        agent: "codex".to_owned(),
        acp_session_id: "acp-session-123".to_owned(),
        working_dir: "/tmp/project".to_owned(),
        load_session: true,
        generated_work_ceiling: ceiling,
    }
}

fn admit(store: &RunStore, id: &str) {
    store.admit(admission(id, 5)).expect("admit");
}

fn draft(id: &str) -> RunDraft {
    RunDraft {
        id: id.to_owned(),
        session_id: "codex/session-123".to_owned(),
        agent: "codex".to_owned(),
        acp_session_id: "acp-session-123".to_owned(),
        working_dir: "/tmp/project".to_owned(),
        load_session: true,
        claimed_issue_ids: vec!["louiselm-qbr.3.3".to_owned()],
        park_expires_at_ms: 2_000,
    }
}

#[test]
fn cold_park_survives_reopen_and_reaps_each_claim_once() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let run_id = "11111111-1111-4111-8111-111111111111";
    let store = RunStore::new(temporary.path()).expect("store");
    admit(&store, run_id);
    store.park_cold(draft(run_id)).expect("park");
    let persisted = store.run(run_id).expect("persisted run");
    assert_eq!(persisted.agent, "codex");
    assert_eq!(persisted.acp_session_id, "acp-session-123");
    assert_eq!(persisted.working_dir, "/tmp/project");
    assert!(persisted.load_session);
    assert_eq!(persisted.generated_work.ceiling, 5);
    assert_eq!(persisted.generated_work.consumed, 0);
    assert_eq!(persisted.generated_work.reserved, 0);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        assert_eq!(
            std::fs::metadata(temporary.path().join(format!("{run_id}.json")))
                .expect("record mode")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    drop(store);

    let store = RunStore::new(temporary.path()).expect("reopen store");
    let mut released = Vec::new();
    assert_eq!(
        store
            .reap_expired(2_000, |action| {
                released.push((action.issue_id.clone(), action.actor.clone()));
                Ok(())
            })
            .expect("reap"),
        1
    );
    assert_eq!(
        released,
        vec![(
            "louiselm-qbr.3.3".to_owned(),
            "reaper/codex/session-123".to_owned()
        )]
    );
    assert_eq!(
        store.reap_expired(3_000, |_| Ok(())).expect("repeat reap"),
        0
    );
    assert_eq!(store.run(run_id).expect("run").state, "disposed");
}

#[test]
fn cold_park_rejects_an_agent_without_load_session() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let mut run = draft("33333333-3333-4333-8333-333333333333");
    run.load_session = false;

    assert!(
        RunStore::new(temporary.path())
            .expect("store")
            .park_cold(run)
            .is_err()
    );
}

#[test]
fn admission_persists_separate_run_ceilings_and_refuses_lowering() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let store = RunStore::new(temporary.path()).expect("store");
    let first = "77777777-7777-4777-8777-777777777777";
    let second = "88888888-8888-4888-8888-888888888888";

    store.admit(admission(first, 3)).expect("first admission");
    store.admit(admission(second, 8)).expect("second admission");
    drop(store);

    let store = RunStore::new(temporary.path()).expect("reopen store");
    assert_eq!(store.run(first).expect("first").generated_work.ceiling, 3);
    assert_eq!(store.run(second).expect("second").generated_work.ceiling, 8);
    assert!(store.admit(admission(first, 2)).is_err());
}

#[test]
fn cold_park_requires_prior_admission() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let store = RunStore::new(temporary.path()).expect("store");

    assert!(
        store
            .park_cold(draft("99999999-9999-4999-8999-999999999999"))
            .is_err()
    );
}

#[test]
fn resumable_listing_excludes_disposed_runs() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let store = RunStore::new(temporary.path()).expect("store");
    let expired_id = "22222222-2222-4222-8222-222222222222";
    admit(&store, expired_id);
    store.park_cold(draft(expired_id)).expect("park");
    admit(&store, "33333333-3333-4333-8333-333333333333");
    store
        .park_cold(draft("33333333-3333-4333-8333-333333333333"))
        .expect("park");
    admit(&store, "44444444-4444-4444-8444-444444444444");
    store
        .park_cold(draft("44444444-4444-4444-8444-444444444444"))
        .expect("park");
    let active_id = "55555555-5555-4555-8555-555555555555";
    admit(&store, active_id);
    let mut active = draft(active_id);
    active.park_expires_at_ms = 3_000;
    store.park_cold(active).expect("park active");
    store.reap_expired(2_000, |_| Ok(())).expect("reap");

    let mut un_reaped = draft("66666666-6666-4666-8666-666666666666");
    un_reaped.park_expires_at_ms = 1_000;
    admit(&store, "66666666-6666-4666-8666-666666666666");
    store.park_cold(un_reaped).expect("park expired");

    let summaries = store.list_resumable(2_000).expect("list");
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].id, active_id);
}

#[test]
fn failed_cleanup_stays_durable_for_a_later_retry() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let store = RunStore::new(temporary.path()).expect("store");
    let run_id = "22222222-2222-4222-8222-222222222222";
    admit(&store, run_id);
    store.park_cold(draft(run_id)).expect("park");

    assert!(
        store
            .reap_expired(2_000, |_| Err("br unavailable".to_owned()))
            .is_err()
    );
    assert_eq!(store.run(run_id).expect("run").state, "cold_parked");

    assert_eq!(store.reap_expired(2_000, |_| Ok(())).expect("retry"), 1);
    assert_eq!(store.run(run_id).expect("run").state, "disposed");
}

#[test]
fn beads_cleanup_is_bound_to_one_workspace_and_can_only_release() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    assert!(BeadsCleanup::new(temporary.path()).is_err());
    std::fs::create_dir(temporary.path().join(".beads")).expect("beads directory");
    let cleanup = BeadsCleanup::new(temporary.path()).expect("cleanup");
    assert_eq!(
        cleanup.arguments(&ReapAction {
            issue_id: "louiselm-qbr.3.3".to_owned(),
            actor: "reaper/codex/session-123".to_owned()
        }),
        [
            "update",
            "louiselm-qbr.3.3",
            "--assignee",
            "",
            "--actor",
            "reaper/codex/session-123",
            "--json"
        ]
    );
}
