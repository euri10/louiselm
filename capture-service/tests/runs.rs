use louiselm_capture::{BeadsCleanup, ReapAction, RunDraft, RunStore};

fn draft(id: &str) -> RunDraft {
    RunDraft {
        id: id.to_owned(),
        session_id: "codex/session-123".to_owned(),
        claimed_issue_ids: vec!["louiselm-qbr.3.3".to_owned()],
        park_expires_at_ms: 2_000,
    }
}

#[test]
fn cold_park_survives_reopen_and_reaps_each_claim_once() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let run_id = "11111111-1111-4111-8111-111111111111";
    let store = RunStore::new(temporary.path()).expect("store");
    store.park_cold(draft(run_id)).expect("park");
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
fn failed_cleanup_stays_durable_for_a_later_retry() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let store = RunStore::new(temporary.path()).expect("store");
    let run_id = "22222222-2222-4222-8222-222222222222";
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
