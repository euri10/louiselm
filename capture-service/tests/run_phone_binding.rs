//! Retained owner bindings for phone-readable Runs.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test fixtures abort on setup or assertion failure"
)]

use louiselm_capture::{RunAdmission, RunProjectBinding, RunSession, RunStore};

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
        store
            .admit_phone_bound(admission(), TOKEN, binding())
            .is_err()
    );
    assert!(store.add_phone_contributor(RUN, "codex").is_err());
    assert_eq!(store.run(RUN).unwrap().phone_binding, None);
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
            assert!(competing_lock.try_lock_exclusive().is_err());
            binding.project_id.clone()
        })
        .unwrap();
    assert_eq!(project, binding().project_id);
    competing_lock.try_lock_exclusive().unwrap();
    FileExt::unlock(&competing_lock).unwrap();
    store.add_phone_contributor(RUN, "other").unwrap();
    assert!(store.with_phone_binding(RUN, 1, |_, _| ()).is_err());
    assert_eq!(
        store
            .with_phone_binding(RUN, 0, |binding, _| binding.agents.len())
            .unwrap(),
        2
    );
}
