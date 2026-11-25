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
    assert_eq!(
        store.check_current_root(&root_id).unwrap(),
        std::fs::canonicalize(&parent).unwrap()
    );
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
        store.check_current_root(&root_id),
        Err(PhoneProjectError::Invalid("approved root identity changed"))
    ));
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

#[test]
fn local_root_listing_is_bounded_and_cursor_advances() {
    let base = tempfile::tempdir().unwrap();
    let parent = base.path().join("approved");
    std::fs::create_dir(&parent).unwrap();
    let store = PhoneProjectStore::open(base.path().join("owner-state")).unwrap();
    for _ in 0..5 {
        store.register_root(&parent).unwrap();
    }
    let first = store.roots_after("").unwrap();
    assert_eq!(first.roots.len(), 4);
    let next = first.next_after.unwrap();
    assert_eq!(next, first.roots.last().unwrap().id);
    let second = store.roots_after(&next).unwrap();
    assert_eq!(second.roots.len(), 1);
    assert!(second.roots[0].id > next);
    assert_eq!(second.next_after, None);
    assert!(matches!(
        store.roots_after("invalid"),
        Err(PhoneProjectError::Invalid("invalid root cursor"))
    ));
}

#[test]
fn local_project_listing_survives_restart_and_paginates_without_rebinding() {
    let base = tempfile::tempdir().unwrap();
    let parent = base.path().join("approved");
    std::fs::create_dir(&parent).unwrap();
    let state = base.path().join("owner-state");
    let store = PhoneProjectStore::open(&state).unwrap();
    let root = store.register_root(&parent).unwrap();
    let mut ids = Vec::new();
    for number in 0..5 {
        let project = parent.join(format!("project-{number}"));
        std::fs::create_dir(&project).unwrap();
        ids.push(
            store
                .register_project(&project, std::slice::from_ref(&root))
                .unwrap(),
        );
    }
    ids.sort();
    drop(store);
    let store = PhoneProjectStore::open(&state).unwrap();
    let first = store.projects_after("").unwrap();
    assert_eq!(first.projects.len(), 4);
    assert_eq!(first.projects[0].id, ids[0]);
    assert_eq!(first.projects[0].root_ids, vec![root]);
    assert!(first.projects[0].path.starts_with(parent.to_str().unwrap()));
    let cursor = first.next_after.unwrap();
    assert_eq!(cursor, ids[3]);
    let second = store.projects_after(&cursor).unwrap();
    assert_eq!(second.projects.len(), 1);
    assert_eq!(second.projects[0].id, ids[4]);
    assert_eq!(second.next_after, None);
    assert!(matches!(
        store.projects_after("invalid"),
        Err(PhoneProjectError::Invalid("invalid project cursor"))
    ));
}

#[test]
fn a_new_parent_can_be_explicitly_linked_to_the_same_live_project() {
    let base = tempfile::tempdir().unwrap();
    let parent = base.path().join("approved");
    let project = parent.join("project");
    let foreign = base.path().join("foreign");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir(&foreign).unwrap();
    let state = base.path().join("owner-state");
    let store = PhoneProjectStore::open(&state).unwrap();
    let original = store.register_root(&project).unwrap();
    let project_id = store.register_project(&project, &[original]).unwrap();
    let added = store.register_root(&parent).unwrap();
    let unrelated = store.register_root(&foreign).unwrap();
    let record = binding(&project_id);
    let agents = vec!["codex".into(), "other".into()];
    assert!(
        !store
            .matches_scope(&record, std::slice::from_ref(&added), &agents)
            .unwrap()
    );
    assert!(matches!(
        store.link_project_roots(&project_id, std::slice::from_ref(&unrelated)),
        Err(PhoneProjectError::Invalid(
            "project is outside approved root"
        ))
    ));
    store
        .link_project_roots(&project_id, std::slice::from_ref(&added))
        .unwrap();
    store
        .link_project_roots(&project_id, std::slice::from_ref(&added))
        .unwrap();
    drop(store);
    std::fs::remove_dir(&project).unwrap();
    let store = PhoneProjectStore::open(&state).unwrap();
    assert!(store.matches_scope(&record, &[added], &agents).unwrap());
    assert!(store.link_project_roots(&project_id, &[unrelated]).is_err());
}

#[test]
fn linking_a_replaced_project_or_parent_never_rebinds_its_identity() {
    let base = tempfile::tempdir().unwrap();
    let parent = base.path().join("approved");
    let project = parent.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let store = PhoneProjectStore::open(base.path().join("owner-state")).unwrap();
    let root = store.register_root(&parent).unwrap();
    let project_id = store
        .register_project(&project, std::slice::from_ref(&root))
        .unwrap();
    std::fs::rename(&project, parent.join("old-project")).unwrap();
    std::fs::create_dir(&project).unwrap();
    assert!(matches!(
        store.link_project_roots(&project_id, std::slice::from_ref(&root)),
        Err(PhoneProjectError::Invalid("project identity changed"))
    ));
    let replacement = store
        .register_project(&project, std::slice::from_ref(&root))
        .unwrap();
    std::fs::rename(&parent, base.path().join("old-parent")).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    assert!(store.link_project_roots(&replacement, &[root]).is_err());
}
