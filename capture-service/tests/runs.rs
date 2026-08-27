use louiselm_capture::{
    BeadsCleanup, GeneratedWorkReservation, ReapAction, ReserveResult, RunAdmission, RunDraft,
    RunSession, RunStore,
};

const TOKEN: &str = "generate-token-1234";

fn admission(id: &str, ceiling: u64) -> RunAdmission {
    RunAdmission {
        id: id.to_owned(),
        generated_work_ceiling: ceiling,
        park_ttl_ms: 3_600_000,
    }
}

fn session(id: &str) -> RunSession {
    RunSession {
        id: id.to_owned(),
        session_id: "codex/session-123".to_owned(),
        agent: "codex".to_owned(),
        acp_session_id: "acp-session-123".to_owned(),
        working_dir: "/tmp/project".to_owned(),
        load_session: true,
    }
}

fn admit(store: &RunStore, id: &str) {
    store.admit(admission(id, 5), TOKEN).expect("admit");
    store.attach(session(id)).expect("attach");
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
    }
}

#[test]
fn cold_park_survives_reopen_and_reaps_each_claim_once() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let run_id = "11111111-1111-4111-8111-111111111111";
    let store = RunStore::new(temporary.path()).expect("store");
    admit(&store, run_id);
    store.park_cold(draft(run_id), 1_000).expect("park");
    let persisted = store.run(run_id).expect("persisted run");
    assert_eq!(persisted.agent.as_deref(), Some("codex"));
    assert_eq!(persisted.acp_session_id.as_deref(), Some("acp-session-123"));
    assert_eq!(persisted.working_dir.as_deref(), Some("/tmp/project"));
    assert_eq!(persisted.load_session, Some(true));
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
            .reap_expired(3_601_000, |action| {
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
        store
            .reap_expired(3_602_000, |_| Ok(()))
            .expect("repeat reap"),
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
            .park_cold(run, 1_000)
            .is_err()
    );
}

#[test]
fn admission_persists_separate_run_ceilings_and_refuses_lowering() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let store = RunStore::new(temporary.path()).expect("store");
    let first = "77777777-7777-4777-8777-777777777777";
    let second = "88888888-8888-4888-8888-888888888888";

    store
        .admit(admission(first, 3), TOKEN)
        .expect("first admission");
    store.attach(session(first)).expect("first attach");
    store
        .admit(admission(second, 8), TOKEN)
        .expect("second admission");
    store.attach(session(second)).expect("second attach");
    drop(store);

    let store = RunStore::new(temporary.path()).expect("reopen store");
    assert_eq!(store.run(first).expect("first").generated_work.ceiling, 3);
    assert_eq!(store.run(second).expect("second").generated_work.ceiling, 8);
    assert!(store.admit(admission(first, 2), TOKEN).is_err());
}

#[test]
fn generated_work_reservation_is_durable_and_exhaustion_parks_atomically() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let store = RunStore::new(temporary.path()).expect("store");
    let run_id = "abababab-abab-4bab-8bab-abababababab";
    store.admit(admission(run_id, 1), TOKEN).expect("admit");
    store.attach(session(run_id)).expect("attach");

    assert_eq!(
        store
            .reserve_generated_work(
                GeneratedWorkReservation {
                    run_id: run_id.to_owned(),
                    token: TOKEN.to_owned(),
                    mutation_id: "cdcdcdcd-cdcd-4dcd-8dcd-cdcdcdcdcdcd".to_owned(),
                    kind: "beads_issue".to_owned(),
                    units: 1,
                },
                1_000
            )
            .expect("reserve"),
        ReserveResult::Reserved
    );
    drop(store);

    let store = RunStore::new(temporary.path()).expect("reopen store");
    let pending = store.run(run_id).expect("pending Run");
    assert_eq!(pending.generated_work.reserved, 1);
    assert_eq!(pending.generated_work.consumed, 0);
    assert_eq!(
        store
            .reserve_generated_work(
                GeneratedWorkReservation {
                    run_id: run_id.to_owned(),
                    token: TOKEN.to_owned(),
                    mutation_id: "efefefef-efef-4fef-8fef-efefefefefef".to_owned(),
                    kind: "beads_issue".to_owned(),
                    units: 1,
                },
                2_000
            )
            .expect("exhaust"),
        ReserveResult::Exhausted
    );
    let parked = store.run(run_id).expect("parked Run");
    assert_eq!(parked.state, "parked");
    assert_eq!(parked.parked_at_ms, Some(2_000));
    assert_eq!(parked.park_expires_at_ms, 3_602_000);
}

#[test]
fn confirmation_consumes_and_definite_failure_releases_a_reservation() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let store = RunStore::new(temporary.path()).expect("store");
    let run_id = "acacacac-acac-4cac-8cac-acacacacacac";
    admit(&store, run_id);
    let first = "bcbcbcbc-bcbc-4cbc-8cbc-bcbcbcbcbcbc";
    let second = "cececece-cece-4ece-8ece-cececececece";
    for mutation_id in [first, second] {
        assert_eq!(
            store
                .reserve_generated_work(
                    GeneratedWorkReservation {
                        run_id: run_id.to_owned(),
                        token: TOKEN.to_owned(),
                        mutation_id: mutation_id.to_owned(),
                        kind: "beads_issue".to_owned(),
                        units: 1,
                    },
                    1_000
                )
                .expect("reserve"),
            ReserveResult::Reserved
        );
    }

    store
        .confirm_generated_work(run_id, TOKEN, first, "louiselm-created")
        .expect("confirm");
    store
        .release_generated_work(run_id, TOKEN, second)
        .expect("release");
    let run = store.run(run_id).expect("Run");
    assert_eq!(run.generated_work.consumed, 1);
    assert_eq!(run.generated_work.reserved, 0);
    assert_eq!(run.generated_issue(first), Some("louiselm-created"));
}

#[test]
fn cold_park_requires_prior_admission() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let store = RunStore::new(temporary.path()).expect("store");

    assert!(
        store
            .park_cold(draft("99999999-9999-4999-8999-999999999999"), 1_000)
            .is_err()
    );
}

#[test]
fn resumable_listing_excludes_disposed_runs() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let store = RunStore::new(temporary.path()).expect("store");
    let expired_id = "22222222-2222-4222-8222-222222222222";
    admit(&store, expired_id);
    store.park_cold(draft(expired_id), 1_000).expect("park");
    admit(&store, "33333333-3333-4333-8333-333333333333");
    store
        .park_cold(draft("33333333-3333-4333-8333-333333333333"), 1_000)
        .expect("park");
    admit(&store, "44444444-4444-4444-8444-444444444444");
    store
        .park_cold(draft("44444444-4444-4444-8444-444444444444"), 1_000)
        .expect("park");
    let active_id = "55555555-5555-4555-8555-555555555555";
    admit(&store, active_id);
    store
        .park_cold(draft(active_id), 3_000)
        .expect("park active");
    store.reap_expired(3_602_000, |_| Ok(())).expect("reap");

    admit(&store, "66666666-6666-4666-8666-666666666666");
    store
        .park_cold(draft("66666666-6666-4666-8666-666666666666"), 0)
        .expect("park expired");

    let summaries = store.list_resumable(3_602_000).expect("list");
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].id, active_id);
}

#[test]
fn failed_cleanup_stays_durable_for_a_later_retry() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let store = RunStore::new(temporary.path()).expect("store");
    let run_id = "22222222-2222-4222-8222-222222222222";
    admit(&store, run_id);
    store.park_cold(draft(run_id), 1_000).expect("park");

    assert!(
        store
            .reap_expired(3_601_000, |_| Err("br unavailable".to_owned()))
            .is_err()
    );
    assert_eq!(store.run(run_id).expect("run").state, "cold_parked");

    assert_eq!(store.reap_expired(3_601_000, |_| Ok(())).expect("retry"), 1);
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
