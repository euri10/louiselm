//! Retained owner bindings for phone-readable Runs.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test fixtures abort on setup or assertion failure"
)]

use louiselm_capture::{RunAdmission, RunProjectBinding, RunSession, RunStore, RunStoreError};

const RUN: &str = "11111111-1111-4111-8111-111111111111";
const TOKEN: &str = "generate-token-1234";

fn admission() -> RunAdmission {
    RunAdmission {
        id: RUN.into(),
        generated_work_ceiling: 5,
        park_ttl_ms: 3_600_000,
    }
}

fn binding() -> RunProjectBinding {
    RunProjectBinding {
        run_id: RUN.into(),
        project_id: "01".repeat(16),
        source_id: "02".repeat(16),
        checkout_id: "03".repeat(16),
        agents: vec!["codex".into()],
    }
}

#[test]
fn binding_precedes_attachment_and_retains_all_contributors() {
    let root = tempfile::tempdir().unwrap();
    let store = RunStore::new(root.path()).unwrap();
    store
        .admit_phone_bound(admission(), TOKEN, binding())
        .unwrap();
    assert_eq!(store.run(RUN).unwrap().phone_binding, Some(binding()));
    store
        .attach(RunSession {
            id: RUN.into(),
            session_id: "claude/acp".into(),
            agent: "claude".into(),
            acp_session_id: "acp".into(),
            working_dir: "/temporary/worker".into(),
            load_session: true,
        })
        .unwrap();
    let attached_revision = store.run(RUN).unwrap().revision;
    assert_eq!(
        store.run(RUN).unwrap().phone_binding.unwrap().agents,
        vec!["codex", "claude"]
    );
    store.add_phone_contributor(RUN, "other").unwrap();
    let changed = store.run(RUN).unwrap();
    assert!(changed.revision > attached_revision);
    assert_eq!(
        changed.phone_binding.unwrap().agents,
        vec!["codex", "claude", "other"]
    );
    store.add_phone_contributor(RUN, "other").unwrap();
    assert_eq!(store.run(RUN).unwrap().revision, changed.revision);
    drop(store);
    let store = RunStore::new(root.path()).unwrap();
    let retained = store.run(RUN).unwrap().phone_binding.unwrap();
    assert_eq!(retained.project_id, binding().project_id);
    assert_eq!(retained.agents, vec!["codex", "claude", "other"]);
}

#[test]
fn old_unbound_runs_never_gain_a_binding_by_retroactive_admission() {
    let root = tempfile::tempdir().unwrap();
    let store = RunStore::new(root.path()).unwrap();
    store.admit(admission(), TOKEN).unwrap();
    assert!(
        matches!(store.admit_phone_bound(admission(), TOKEN, binding()), Err(RunStoreError::Invalid(error)) if error == "Run UUID conflicts with existing record")
    );
    assert!(
        matches!(store.add_phone_contributor(RUN, "codex"), Err(RunStoreError::Invalid(error)) if error == "Run has no owner project binding")
    );
    assert_eq!(store.run(RUN).unwrap().phone_binding, None);
}

#[test]
fn differently_cased_old_run_cannot_be_reused_for_phone_admission() {
    let root = tempfile::tempdir().unwrap();
    let store = RunStore::new(root.path()).unwrap();
    let lower = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
    let upper = lower.to_ascii_uppercase();
    let mut old = admission();
    old.id = upper.clone();
    store.admit(old.clone(), TOKEN).unwrap();
    store.admit(old, TOKEN).unwrap();
    let mut new = admission();
    new.id = lower.into();
    let mut proposed = binding();
    proposed.run_id = lower.into();
    assert!(matches!(
        store.admit_phone_bound(new, TOKEN, proposed),
        Err(RunStoreError::Invalid(error)) if error == "Run UUID conflicts with existing record"
    ));
    assert_eq!(store.run(&upper).unwrap().phone_binding, None);
    assert!(!root.path().join(format!("{lower}.json")).exists());
}

#[test]
fn ambiguous_retained_uuid_spellings_are_never_phone_readable() {
    let root = tempfile::tempdir().unwrap();
    let separate = tempfile::tempdir().unwrap();
    let lower = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
    let upper = lower.to_ascii_uppercase();
    let store = RunStore::new(root.path()).unwrap();
    let mut old = admission();
    old.id = upper.clone();
    store.admit(old, TOKEN).unwrap();
    let other = RunStore::new(separate.path()).unwrap();
    let mut new = admission();
    new.id = lower.into();
    let mut proposed = binding();
    proposed.run_id = lower.into();
    other.admit_phone_bound(new, TOKEN, proposed).unwrap();
    std::fs::copy(
        separate.path().join(format!("{lower}.json")),
        root.path().join(format!("{lower}.json")),
    )
    .unwrap();
    assert!(matches!(
        store.with_phone_binding(lower, 0, |_, _| ()),
        Err(RunStoreError::Invalid(error)) if error == "Run UUID conflicts with existing record"
    ));
    assert!(store.add_phone_contributor(lower, "other").is_err());
    assert!(
        store
            .attach(RunSession {
                id: lower.into(),
                session_id: "codex/acp".into(),
                agent: "codex".into(),
                acp_session_id: "acp".into(),
                working_dir: "/temporary/worker".into(),
                load_session: false,
            })
            .is_err()
    );
    assert_eq!(store.run(&upper).unwrap().phone_binding, None);
}

#[test]
fn competing_case_alias_admissions_cannot_both_win() {
    let root = tempfile::tempdir().unwrap();
    let store = RunStore::new(root.path()).unwrap();
    let lower = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
    let upper = lower.to_ascii_uppercase();
    let mut local = admission();
    local.id = upper.clone();
    let mut phone = admission();
    phone.id = lower.into();
    let mut proposed = binding();
    proposed.run_id = lower.into();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let local_store = store.clone();
    let local_barrier = barrier.clone();
    let local = std::thread::spawn(move || {
        local_barrier.wait();
        local_store.admit(local, TOKEN)
    });
    let phone = std::thread::spawn(move || {
        barrier.wait();
        store.admit_phone_bound(phone, TOKEN, proposed)
    });
    assert_ne!(local.join().unwrap().is_ok(), phone.join().unwrap().is_ok());
    assert_ne!(
        root.path().join(format!("{upper}.json")).exists(),
        root.path().join(format!("{lower}.json")).exists()
    );
}

#[test]
fn unpublished_draft_checks_unused_uuid_under_the_run_lock() {
    use fs2::FileExt;

    let root = tempfile::tempdir().unwrap();
    let store = RunStore::new(root.path()).unwrap();
    let competing = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.path().join(".lock"))
        .unwrap();
    let value = store
        .with_unused_run(RUN, || {
            assert_eq!(
                competing.try_lock_exclusive().unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock
            );
            42
        })
        .unwrap();
    assert_eq!(value, 42);
    competing.try_lock_exclusive().unwrap();
    FileExt::unlock(&competing).unwrap();
    store.admit(admission(), TOKEN).unwrap();
    assert!(store.with_unused_run(RUN, || ()).is_err());
    assert!(
        store
            .with_unused_run("AAAAAAAA-BBBB-4CCC-8DDD-EEEEEEEEEEEE", || ())
            .is_err()
    );
}

#[test]
fn unused_uuid_guard_refuses_a_case_equivalent_legacy_record() {
    let root = tempfile::tempdir().unwrap();
    let store = RunStore::new(root.path()).unwrap();
    let lower = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
    let mut old = admission();
    old.id = lower.to_ascii_uppercase();
    store.admit(old, TOKEN).unwrap();
    assert!(store.with_unused_run(lower, || ()).is_err());
}

#[test]
fn contributor_updates_are_serialized() {
    let root = tempfile::tempdir().unwrap();
    let store = RunStore::new(root.path()).unwrap();
    store
        .admit_phone_bound(admission(), TOKEN, binding())
        .unwrap();
    let other = store.clone();
    let left = std::thread::spawn(move || store.add_phone_contributor(RUN, "agent-a"));
    let right = std::thread::spawn(move || other.add_phone_contributor(RUN, "agent-b"));
    left.join().unwrap().unwrap();
    right.join().unwrap().unwrap();
    let store = RunStore::new(root.path()).unwrap();
    let agents = store.run(RUN).unwrap().phone_binding.unwrap().agents;
    assert_eq!(agents.len(), 3);
    assert!(agents.contains(&"codex".to_owned()));
    assert!(agents.contains(&"agent-a".to_owned()));
    assert!(agents.contains(&"agent-b".to_owned()));
}

#[test]
fn protected_projection_checks_current_revision_while_contributors_are_locked() {
    use fs2::FileExt;
    use std::fs::OpenOptions;

    let root = tempfile::tempdir().unwrap();
    let store = RunStore::new(root.path()).unwrap();
    store
        .admit_phone_bound(admission(), TOKEN, binding())
        .unwrap();
    let competing_lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.path().join(".lock"))
        .unwrap();
    let project = store
        .with_phone_binding(RUN, 1, |binding, view| {
            assert_eq!(view.revision, 1);
            assert_eq!(
                competing_lock.try_lock_exclusive().unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock
            );
            binding.project_id.clone()
        })
        .unwrap();
    assert_eq!(project, binding().project_id);
    competing_lock.try_lock_exclusive().unwrap();
    FileExt::unlock(&competing_lock).unwrap();
    store.add_phone_contributor(RUN, "other").unwrap();
    assert!(
        matches!(store.with_phone_binding(RUN, 1, |_, _| ()), Err(RunStoreError::Invalid(error)) if error == "stale Run revision: expected 1, found 2")
    );
    assert_eq!(
        store
            .with_phone_binding(RUN, 0, |binding, _| binding.agents.len())
            .unwrap(),
        2
    );
}

#[test]
fn durable_views_distinguish_unknown_and_advertised_load_session() {
    let root = tempfile::tempdir().unwrap();
    let store = RunStore::new(root.path()).unwrap();
    store
        .admit_phone_bound(admission(), TOKEN, binding())
        .unwrap();
    let admitted = serde_json::to_value(store.view(RUN).unwrap()).unwrap();
    assert_eq!(admitted.get("load_session"), Some(&serde_json::Value::Null));
    store
        .attach(RunSession {
            id: RUN.into(),
            session_id: "codex/acp".into(),
            agent: "codex".into(),
            acp_session_id: "acp".into(),
            working_dir: "/temporary/private-worker".into(),
            load_session: false,
        })
        .unwrap();
    let current = serde_json::to_value(store.view(RUN).unwrap()).unwrap();
    let snapshot = serde_json::to_value(store.snapshot().unwrap()).unwrap();
    assert_eq!(current["load_session"], false);
    assert_eq!(snapshot[0], current);
    assert!(!current.to_string().contains("/temporary/private-worker"));
    drop(store);
    let restored = RunStore::new(root.path()).unwrap();
    assert_eq!(
        serde_json::to_value(restored.view(RUN).unwrap()).unwrap()["load_session"],
        false
    );
    let second = "22222222-2222-4222-8222-222222222222";
    let mut other_admission = admission();
    let mut other_binding = binding();
    other_admission.id = second.into();
    other_binding.run_id = second.into();
    restored
        .admit_phone_bound(other_admission, TOKEN, other_binding)
        .unwrap();
    restored
        .attach(RunSession {
            id: second.into(),
            session_id: "codex/other".into(),
            agent: "codex".into(),
            acp_session_id: "other".into(),
            working_dir: "/temporary/private-worker".into(),
            load_session: true,
        })
        .unwrap();
    assert_eq!(
        serde_json::to_value(restored.view(second).unwrap()).unwrap()["load_session"],
        true
    );
}

#[test]
fn syntactically_valid_contributor_corruption_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let store = RunStore::new(root.path()).unwrap();
    store
        .admit_phone_bound(admission(), TOKEN, binding())
        .unwrap();
    store.add_phone_contributor(RUN, "other").unwrap();
    let file = root.path().join(format!("{RUN}.json"));
    let mut state: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    state["phone_binding"]["agents"] = serde_json::json!(["codex"]);
    std::fs::write(file, serde_json::to_vec(&state).unwrap()).unwrap();
    assert!(
        matches!(store.run(RUN), Err(RunStoreError::Invalid(error)) if error == "Run project binding integrity failed")
    );
}
