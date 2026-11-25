//! Private identities and retained approved-root relationships for Run reads.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test fixtures abort on setup or assertion failure"
)]

use louiselm_capture::{PhoneProjectError, PhoneProjectStore, RunProjectBinding};

fn binding(project_id: &str) -> RunProjectBinding {
    RunProjectBinding {
        run_id: "11111111-1111-4111-8111-111111111111".into(),
        project_id: project_id.into(),
        source_id: "02".repeat(16),
        checkout_id: "03".repeat(16),
        agents: vec!["codex".into(), "other".into()],
    }
}

#[test]
fn retained_parent_scope_survives_project_deletion_and_requires_every_agent() {
    let base = tempfile::tempdir().unwrap();
    let parent = base.path().join("approved");
    let project = parent.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let store = PhoneProjectStore::open(base.path().join("owner-state")).unwrap();
    let root_id = store.register_root(&parent).unwrap();
    let project_id = store
        .register_project(&project, std::slice::from_ref(&root_id))
        .unwrap();
    let record = binding(&project_id);
    assert!(
        store
            .matches_scope(
                &record,
                std::slice::from_ref(&root_id),
                &["codex".into(), "other".into()]
            )
            .unwrap()
    );
    assert!(
        !store
            .matches_scope(&record, std::slice::from_ref(&root_id), &["codex".into()])
            .unwrap()
    );
    assert!(
        !store
            .matches_scope(&record, &[], &["codex".into(), "other".into()])
            .unwrap()
    );
    std::fs::remove_dir(project).unwrap();
    drop(store);
    let store = PhoneProjectStore::open(base.path().join("owner-state")).unwrap();
    assert!(
        store
            .matches_scope(&record, &[root_id], &["codex".into(), "other".into()])
            .unwrap()
    );
}

#[test]
fn replacement_never_reuses_an_existing_project_identity() {
    let base = tempfile::tempdir().unwrap();
    let parent = base.path().join("approved");
    let project = parent.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let store = PhoneProjectStore::open(base.path().join("owner-state")).unwrap();
    let root_id = store.register_root(&parent).unwrap();
    let first = store
        .register_project(&project, std::slice::from_ref(&root_id))
        .unwrap();
    std::fs::rename(&project, parent.join("moved")).unwrap();
    std::fs::create_dir(&project).unwrap();
    assert!(matches!(
        store.check_current_project(&first),
        Err(PhoneProjectError::Invalid("project identity changed"))
    ));
    let second = store.register_project(&project, &[root_id]).unwrap();
    assert_ne!(first, second);
}

#[test]
fn a_foreign_or_replaced_parent_cannot_gain_a_relationship() {
    let base = tempfile::tempdir().unwrap();
    let parent = base.path().join("approved");
    let foreign = base.path().join("foreign");
    let project = parent.join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir(&foreign).unwrap();
    let store = PhoneProjectStore::open(base.path().join("owner-state")).unwrap();
    let root_id = store.register_root(&parent).unwrap();
    let foreign_id = store.register_root(&foreign).unwrap();
    assert!(matches!(
        store.register_project(&project, &[foreign_id]),
        Err(PhoneProjectError::Invalid(
            "project is outside approved root"
        ))
    ));
    std::fs::rename(&parent, base.path().join("old-root")).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    assert!(matches!(
        store.register_project(&project, &[root_id]),
        Err(PhoneProjectError::Invalid("approved root identity changed"))
    ));
}

#[test]
fn syntactically_valid_project_identity_corruption_is_refused() {
    let base = tempfile::tempdir().unwrap();
    let parent = base.path().join("approved");
    std::fs::create_dir(&parent).unwrap();
    let path = base.path().join("owner-state");
    let store = PhoneProjectStore::open(&path).unwrap();
    store.register_root(&parent).unwrap();
    drop(store);
    let file = path.join("projects.json");
    let mut state: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    let inode = state["roots"][0]["directory"]["inode"].as_u64().unwrap();
    state["roots"][0]["directory"]["inode"] = (inode + 1).into();
    std::fs::write(file, serde_json::to_vec(&state).unwrap()).unwrap();
    assert!(matches!(
        PhoneProjectStore::open(&path),
        Err(PhoneProjectError::Invalid("store integrity check failed"))
    ));
}
