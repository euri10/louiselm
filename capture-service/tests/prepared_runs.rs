//! Durable private prepared-Run registration behavior.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test fixtures abort on setup or assertion failure"
)]

use louiselm_capture::{PreparedRunBinding, PreparedRunStore};

const RUN: &str = "11111111-1111-4111-8111-111111111111";

fn binding() -> PreparedRunBinding {
    PreparedRunBinding {
        run_id: RUN.into(),
        project_id: "01".repeat(16),
        source_id: "02".repeat(16),
        checkout_id: "03".repeat(16),
        agents: vec!["codex".into()],
    }
}

#[test]
fn exact_snapshots_survive_restart_republication_and_unpublication() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("prepared");
    let store = PreparedRunStore::open(&path).unwrap();
    let original = "{\"prompt\":\"private instruction /local/path\"}\n";
    let source = root.path().join("selection.json");
    std::fs::write(&source, original).unwrap();
    let selected = std::fs::read_to_string(&source).unwrap();
    let first = store.publish(None, &binding(), &selected).unwrap();
    std::fs::remove_file(source).unwrap();
    assert_eq!(first.revision, 1);
    assert_eq!(
        store.read(&first.id, first.revision).unwrap().selection,
        original
    );
    let list = serde_json::to_string(&store.list().unwrap()).unwrap();
    assert!(!list.contains("private instruction"));
    assert!(!list.contains("/local/path"));
    drop(store);

    let store = PreparedRunStore::open(&path).unwrap();
    assert_eq!(store.read(&first.id, 1).unwrap().selection, original);
    let revised = "{\"prompt\":\"revised instruction\"}";
    let second = store
        .publish(Some((&first.id, 1)), &binding(), revised)
        .unwrap();
    assert_eq!(second.id, first.id);
    assert_eq!(second.revision, 2);
    assert!(
        store.read(&first.id, 1).is_err(),
        "old previews must refresh"
    );
    assert!(
        store
            .publish(Some((&first.id, 1)), &binding(), original)
            .is_err()
    );
    assert_eq!(store.read(&first.id, 2).unwrap().selection, revised);
    assert!(
        store.unpublish(&first.id, 1).is_err(),
        "stale removal must refuse"
    );
    store.unpublish(&first.id, 2).unwrap();
    assert_eq!(store.list().unwrap(), Vec::new());
    assert!(store.read(&first.id, 2).is_err());
    drop(store);

    let store = PreparedRunStore::open(&path).unwrap();
    let history = store.inspect(&first.id).unwrap();
    assert!(!history.published);
    assert_eq!(history.revisions.len(), 2);
    assert_eq!(history.revisions[0].selection, original);
    assert_eq!(history.revisions[1].selection, revised);
}

#[test]
fn invalid_or_conflicting_registration_is_refused_without_mutation() {
    let root = tempfile::tempdir().unwrap();
    let store = PreparedRunStore::open(root.path().join("prepared")).unwrap();
    let first = store.publish(None, &binding(), "{}").unwrap();
    assert!(store.publish(None, &binding(), "{}").is_err());
    let mut changed = binding();
    changed.run_id = "22222222-2222-4222-8222-222222222222".into();
    assert!(store.publish(Some((&first.id, 1)), &changed, "{}").is_err());
    changed = binding();
    changed.project_id = "04".repeat(16);
    assert!(store.publish(Some((&first.id, 1)), &changed, "{}").is_err());
    changed = binding();
    changed.agents.clear();
    assert!(store.publish(None, &changed, "{}").is_err());
    assert!(store.publish(None, &binding(), "not json").is_err());
    assert!(
        store
            .publish(None, &binding(), &"x".repeat(65_537))
            .is_err()
    );
    assert_eq!(store.list().unwrap(), vec![first]);
}

#[test]
fn concurrent_republication_has_only_one_winner() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("prepared");
    let store = PreparedRunStore::open(&path).unwrap();
    let first = store.publish(None, &binding(), "{}").unwrap();
    let other = PreparedRunStore::open(&path).unwrap();
    let id = first.id.clone();
    let left =
        std::thread::spawn(move || store.publish(Some((&id, 1)), &binding(), "{\"choice\":1}"));
    let id = first.id.clone();
    let right =
        std::thread::spawn(move || other.publish(Some((&id, 1)), &binding(), "{\"choice\":2}"));
    let results = [left.join().unwrap(), right.join().unwrap()];
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    let store = PreparedRunStore::open(&path).unwrap();
    assert_eq!(store.inspect(&first.id).unwrap().revisions.len(), 2);
}

#[test]
fn missing_or_damaged_existing_store_fails_closed() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("prepared");
    let store = PreparedRunStore::open(&path).unwrap();
    store.publish(None, &binding(), "{}").unwrap();
    drop(store);
    std::fs::write(path.join("registrations.json"), b"{}").unwrap();
    assert!(PreparedRunStore::open(&path).is_err());
    std::fs::remove_file(path.join("registrations.json")).unwrap();
    assert!(PreparedRunStore::open(&path).is_err());
}
