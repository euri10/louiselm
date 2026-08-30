use louiselm_capture::{
    BeadsCleanup, GeneratedWorkReservation, ReapAction, ReserveResult, ResumeResult, RunAdmission,
    RunDraft, RunSession, RunStore,
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
            .expect("reap")
            .disposed,
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
            .expect("repeat reap")
            .disposed,
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
fn typed_generator_reservation_consumes_outputs_one_unit_at_a_time() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let store = RunStore::new(temporary.path()).expect("store");
    let run_id = "abababab-abab-4bab-8bab-abababababab";
    let mutation_id = "cdcdcdcd-cdcd-4dcd-8dcd-cdcdcdcdcdcd";
    admit(&store, run_id);

    assert_eq!(
        store
            .reserve_generated_work(
                GeneratedWorkReservation {
                    run_id: run_id.to_owned(),
                    token: TOKEN.to_owned(),
                    mutation_id: mutation_id.to_owned(),
                    kind: "skill_generator".to_owned(),
                    units: 3,
                },
                1_000,
            )
            .expect("reserve"),
        ReserveResult::Reserved
    );
    store
        .confirm_generated_work(run_id, TOKEN, mutation_id, "issue-1")
        .expect("first output");
    let after_first = store.run(run_id).expect("first output state");
    assert_eq!(after_first.generated_work.consumed, 1);
    assert_eq!(after_first.generated_work.reserved, 2);
    store
        .confirm_generated_work(run_id, TOKEN, mutation_id, "issue-2")
        .expect("second output");
    let after_second = store.run(run_id).expect("second output state");
    assert_eq!(after_second.generated_work.consumed, 2);
    assert_eq!(after_second.generated_work.reserved, 1);
    store
        .release_generated_work(run_id, TOKEN, mutation_id)
        .expect("release unused");
    let completed = store.run(run_id).expect("completed generator");
    assert_eq!(completed.generated_work.consumed, 2);
    assert_eq!(completed.generated_work.reserved, 0);
}

#[test]
fn a_generator_reservation_larger_than_remaining_capacity_cannot_start() {
    // AC3: a Generator may not start unless its *full* declared maximum fits.
    // Both halves run against the same occupancy — three of five units consumed,
    // two remaining — so the only difference between them is the requested size.
    let reserve_after_three_consumed = |units: u64| {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let store = RunStore::new(temporary.path()).expect("store");
        let run_id = "abababab-abab-4bab-8bab-abababababab";
        let occupying = "cdcdcdcd-cdcd-4dcd-8dcd-cdcdcdcdcdcd";
        admit(&store, run_id);
        store
            .reserve_generated_work(
                GeneratedWorkReservation {
                    run_id: run_id.to_owned(),
                    token: TOKEN.to_owned(),
                    mutation_id: occupying.to_owned(),
                    kind: "skill_generator".to_owned(),
                    units: 3,
                },
                1_000,
            )
            .expect("occupy three units");
        for issue in ["issue-1", "issue-2", "issue-3"] {
            store
                .confirm_generated_work(run_id, TOKEN, occupying, issue)
                .expect("consume occupying unit");
        }
        let occupied = store.run(run_id).expect("occupied Run");
        assert_eq!(occupied.generated_work.consumed, 3);
        assert_eq!(occupied.generated_work.reserved, 0);
        assert_eq!(occupied.generated_work.ceiling, 5);

        let result = store
            .reserve_generated_work(
                GeneratedWorkReservation {
                    run_id: run_id.to_owned(),
                    token: TOKEN.to_owned(),
                    mutation_id: "efefefef-efef-4fef-8fef-efefefefefef".to_owned(),
                    kind: "skill_generator".to_owned(),
                    units,
                },
                2_000,
            )
            .expect("reserve");
        let pending = store
            .pending_generation_ids(run_id, TOKEN)
            .expect("pending identities");
        (result, store.run(run_id).expect("resulting Run"), pending)
    };

    // Exactly the remaining capacity fits and starts.
    let (fitting, active, fitting_pending) = reserve_after_three_consumed(2);
    assert_eq!(fitting, ReserveResult::Reserved);
    assert_eq!(active.state, "active");
    assert_eq!(active.generated_work.reserved, 2);
    assert_eq!(fitting_pending.len(), 1);

    // One unit more than remains is refused whole: no partial reservation is
    // taken, and the Run Parks for an operator decision instead.
    let (oversized, parked, oversized_pending) = reserve_after_three_consumed(3);
    assert_eq!(oversized, ReserveResult::Exhausted);
    assert_eq!(parked.state, "parked");
    assert_eq!(parked.generated_work.consumed, 3);
    assert_eq!(parked.generated_work.reserved, 0);
    assert!(oversized_pending.is_empty());
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
    assert_eq!(summaries[0].claims, vec!["louiselm-qbr.3.3"]);
}

#[test]
fn failed_cleanup_stays_durable_for_a_later_retry() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let store = RunStore::new(temporary.path()).expect("store");
    let run_id = "22222222-2222-4222-8222-222222222222";
    admit(&store, run_id);
    store.park_cold(draft(run_id), 1_000).expect("park");

    let summary = store
        .reap_expired(3_601_000, |_| Err("br unavailable".to_owned()))
        .expect("reap pass itself succeeds even when a release fails");
    assert_eq!(summary.disposed, 0);
    assert_eq!(
        summary.failed,
        vec![(run_id.to_owned(), "br unavailable".to_owned())]
    );
    assert_eq!(store.run(run_id).expect("run").state, "cold_parked");

    let retry = store.reap_expired(3_601_000, |_| Ok(())).expect("retry");
    assert_eq!(retry.disposed, 1);
    assert!(retry.failed.is_empty());
    assert_eq!(store.run(run_id).expect("run").state, "disposed");
}

#[test]
fn a_failing_release_does_not_block_reaping_other_runs() {
    // Regression for louiselm-hvot: reap_expired used to propagate the first
    // failing release out of the whole pass, so one permanently-broken claim
    // (or an unresolvable `br`) silently blocked every other expired Run in
    // the store, forever.
    let temporary = tempfile::tempdir().expect("temporary directory");
    let store = RunStore::new(temporary.path()).expect("store");
    let failing_id = "44444444-4444-4444-8444-444444444444";
    let healthy_id = "55555555-5555-4555-8555-555555555555";
    let mut failing_draft = draft(failing_id);
    failing_draft.claimed_issue_ids = vec!["louiselm-failing".to_owned()];
    let mut healthy_draft = draft(healthy_id);
    healthy_draft.claimed_issue_ids = vec!["louiselm-healthy".to_owned()];
    admit(&store, failing_id);
    store.park_cold(failing_draft, 1_000).expect("park failing");
    admit(&store, healthy_id);
    store.park_cold(healthy_draft, 1_000).expect("park healthy");

    let summary = store
        .reap_expired(3_601_000, |action| {
            if action.issue_id == "louiselm-failing" {
                return Err("br unavailable".to_owned());
            }
            Ok(())
        })
        .expect("reap pass");

    assert_eq!(summary.disposed, 1);
    assert_eq!(
        summary.failed,
        vec![(failing_id.to_owned(), "br unavailable".to_owned())]
    );
    assert_eq!(store.run(failing_id).expect("run").state, "cold_parked");
    assert_eq!(store.run(healthy_id).expect("run").state, "disposed");
}

#[test]
fn beads_cleanup_is_bound_to_one_workspace_and_can_only_release() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    assert!(BeadsCleanup::new(temporary.path(), "br").is_err());
    std::fs::create_dir(temporary.path().join(".beads")).expect("beads directory");
    let cleanup = BeadsCleanup::new(temporary.path(), "br").expect("cleanup");
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

#[test]
fn release_uses_the_configured_executable_not_a_bare_path_lookup() {
    // Regression for louiselm-hvot: BeadsCleanup used to run `Command::new("br")`,
    // a bare name resolved against the caller's PATH. A long-running daemon's PATH
    // is not guaranteed to contain `br` at all, and that failure was silently
    // discarded by every caller. Prove the configured executable is what actually
    // runs, not a hardcoded "br" string.
    let temporary = tempfile::tempdir().expect("temporary directory");
    std::fs::create_dir(temporary.path().join(".beads")).expect("beads directory");
    let script = temporary.path().join("fake-br");
    std::fs::write(
        &script,
        "#!/bin/sh\necho fake-br saw: \"$@\" 1>&2\nexit 1\n",
    )
    .expect("write fake br");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    }
    let cleanup = BeadsCleanup::new(temporary.path(), &script).expect("cleanup");
    let error = cleanup
        .release(&ReapAction {
            issue_id: "louiselm-x".to_owned(),
            actor: "reaper/codex/s".to_owned(),
        })
        .expect_err("fake br exits nonzero");
    assert!(
        error.contains("fake-br saw:"),
        "expected the configured executable to run, got: {error}"
    );
}

#[test]
fn operator_raise_and_cold_resume_are_revisioned_and_idempotent() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let store = RunStore::new(temporary.path()).expect("store");
    let run_id = "12121212-1212-4212-8212-121212121212";
    admit(&store, run_id);
    store.park_cold(draft(run_id), 1_000).expect("cold Park");
    let parked = store.run(run_id).expect("Parked");
    assert!(
        store
            .raise_generated_work_ceiling(run_id, parked.revision - 1, 8)
            .is_err()
    );
    let raised = store
        .raise_generated_work_ceiling(run_id, parked.revision, 8)
        .expect("raise");
    assert_eq!(raised.generated_work_ceiling, 8);
    assert_eq!(raised.revision, parked.revision + 1);
    assert_eq!(raised.state, parked.state);
    let after_raise = store.run(run_id).expect("raised Run");
    assert_eq!(after_raise.session_id, parked.session_id);
    assert_eq!(after_raise.park_ttl_ms, parked.park_ttl_ms);
    assert_eq!(after_raise.park_expires_at_ms, parked.park_expires_at_ms);

    let operation = "34343434-3434-4434-8434-343434343434";
    assert_eq!(
        store
            .begin_resume(run_id, raised.revision, operation, 2_000, 300_000)
            .expect("begin"),
        ResumeResult::Resuming
    );
    let resuming = store.run(run_id).expect("resuming");
    assert_eq!(resuming.state, "resuming");
    assert_eq!(
        store
            .begin_resume(run_id, raised.revision, operation, 99_000, 300_000)
            .expect("idempotent begin"),
        ResumeResult::Resuming
    );
    assert_eq!(
        store.run(run_id).expect("same revision").revision,
        resuming.revision
    );
    assert!(
        store
            .reserve_generated_work(
                GeneratedWorkReservation {
                    run_id: run_id.to_owned(),
                    token: TOKEN.to_owned(),
                    mutation_id: "56565656-5656-4656-8656-565656565656".to_owned(),
                    kind: "beads_issue".to_owned(),
                    units: 1,
                },
                3_000,
            )
            .is_err()
    );
    assert_eq!(
        store
            .finalize_resume(run_id, resuming.revision, operation, false)
            .expect("failed load"),
        ResumeResult::ColdParked
    );
    let failed = store.run(run_id).expect("cold Parked again");
    assert_eq!(failed.park_expires_at_ms, 302_000);
    assert_eq!(
        store
            .finalize_resume(run_id, resuming.revision, operation, false)
            .expect("idempotent failure"),
        ResumeResult::ColdParked
    );
    assert_eq!(
        store.run(run_id).expect("same failed revision").revision,
        failed.revision
    );

    let retry = "78787878-7878-4878-8878-787878787878";
    assert_eq!(
        store
            .begin_resume(run_id, failed.revision, retry, 4_000, 300_000)
            .expect("retry"),
        ResumeResult::Resuming
    );
    let retrying = store.run(run_id).expect("retrying");
    assert_eq!(
        store
            .finalize_resume(run_id, retrying.revision, retry, true)
            .expect("complete"),
        ResumeResult::Active
    );
    assert_eq!(store.run(run_id).expect("active").state, "active");
}

#[test]
fn resume_lease_expiry_disposes_and_warm_resume_activates_directly() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let store = RunStore::new(temporary.path()).expect("store");
    let cold_id = "23232323-2323-4232-8232-232323232323";
    admit(&store, cold_id);
    store.park_cold(draft(cold_id), 1_000).expect("cold Park");
    let cold = store.run(cold_id).expect("cold");
    store
        .begin_resume(
            cold_id,
            cold.revision,
            "45454545-4545-4454-8454-454545454545",
            2_000,
            300_000,
        )
        .expect("begin");
    let mut released = Vec::new();
    assert_eq!(
        store
            .reap_expired(302_000, |action| {
                released.push(action.issue_id.clone());
                Ok(())
            })
            .expect("reap resume")
            .disposed,
        1
    );
    assert_eq!(released, vec!["louiselm-qbr.3.3"]);
    assert_eq!(store.run(cold_id).expect("disposed").state, "disposed");

    let warm_id = "67676767-6767-4767-8767-676767676767";
    let mut warm_admission = admission(warm_id, 1);
    warm_admission.park_ttl_ms = 300_000;
    store.admit(warm_admission, TOKEN).expect("admit warm");
    store.attach(session(warm_id)).expect("attach warm");
    let first_mutation = "89898989-8989-4989-8989-898989898989";
    assert_eq!(
        store
            .reserve_generated_work(
                GeneratedWorkReservation {
                    run_id: warm_id.to_owned(),
                    token: TOKEN.to_owned(),
                    mutation_id: first_mutation.to_owned(),
                    kind: "beads_issue".to_owned(),
                    units: 1,
                },
                500,
            )
            .expect("reserve first"),
        ReserveResult::Reserved
    );
    store
        .confirm_generated_work(warm_id, TOKEN, first_mutation, "first")
        .expect("confirm first");
    assert_eq!(
        store
            .reserve_generated_work(
                GeneratedWorkReservation {
                    run_id: warm_id.to_owned(),
                    token: TOKEN.to_owned(),
                    mutation_id: "91919191-9191-4191-8191-919191919191".to_owned(),
                    kind: "beads_issue".to_owned(),
                    units: 1,
                },
                1_000,
            )
            .expect("exhaust warm"),
        ReserveResult::Exhausted
    );
    let warm = store.run(warm_id).expect("warm Park");
    assert_eq!(
        store
            .begin_resume(
                warm_id,
                warm.revision,
                "90909090-9090-4090-8090-909090909090",
                2_000,
                300_000,
            )
            .expect("warm resume"),
        ResumeResult::Active
    );
    assert_eq!(store.run(warm_id).expect("warm active").state, "active");
}
