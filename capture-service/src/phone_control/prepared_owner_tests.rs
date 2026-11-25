//! Explicit owner publication of an exact private selection.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test fixtures abort on setup or assertion failure"
)]

use std::{
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::Path,
};

use super::{
    prepared_owner::{Publication, publish_with},
    source_checkout_tests::{materialize, setup},
};
use crate::{PreparedRunStore, RunAdmission, RunStore};

const RUN: &str = "11111111-1111-4111-8111-111111111111";

pub(super) fn selection(root: &Path, head: &str, prompt: &str) -> String {
    let digest = format!("sha256:{}", "01".repeat(32));
    serde_json::json!({
        "schema":"louiselm.operator.bead-run/2",
        "envelope":{"schema":"louiselm.broker.run-envelope/2","run_id":RUN,
            "envelope_id":"env","envelope_revision":1,"controller_uid":std::fs::metadata(root).unwrap().uid(),
            "expires_at_ms":0,"verification_plan_digest":digest,"max_sessions":6,
            "bead_scope":{"project_digest":digest,"role":"coordinator","issue_ids":["a","b","c"],
                "effects":[{"kind":"comment_add"}],"max_mutations":3,"expires_at_ms":0},
            "provider_requests":{"disclosure_profile":digest,"provider":"openai",
                "upstream":"https://api.openai.com/v1/responses","addresses":["127.0.0.1"],
                "models":["gpt-5.6-luna"],"max_effort":"low","max_run_requests":3,"expires_at_ms":0}},
        "beads":[{"id":"a","prompt":prompt},{"id":"b","prompt":"second"},{"id":"c","prompt":"third"}],
        "manifest":{"agent":{"id":"codex"},"envelope":{"id":"env","revision":1},
            "skills":{"mode":"off","view_digest":digest},"cache_base_digest":digest,"isolation":{"network":"denied"}},
        "cache":"/private/cache","plan":"/private/plan","snapshot_parent":"/private/snapshots",
        "input_group":1500,
        "worktree":{"path":root.join("checkout"),"journal_parent":"/private/journal","head":head}
    }).to_string()
}

fn private_file(path: &Path, content: &str) {
    std::fs::write(path, content).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

#[test]
fn publication_snapshots_the_reviewed_file_without_starting_a_run() {
    let (root, sources, source_id) = setup();
    let checkout = sources
        .prepare_checkout_with(RUN, &source_id, &root.path().join("checkout"), materialize)
        .unwrap();
    let prepared = PreparedRunStore::open(root.path().join("prepared")).unwrap();
    let runs = RunStore::new(root.path().join("runs")).unwrap();
    let file = root.path().join("selection.json");
    let bytes = selection(root.path(), &checkout.head, "private instructions");
    private_file(&file, &bytes);
    let request = Publication {
        run_id: RUN,
        source_id: &source_id,
        checkout_id: &checkout.checkout_id,
        selection: &file,
        previous: None,
    };
    let summary = publish_with(&sources, &prepared, &runs, &request, |_| Ok(())).unwrap();
    assert_eq!(summary.run_id, RUN);
    assert!(runs.run(RUN).is_err());
    assert!(
        !serde_json::to_string(&prepared.list().unwrap())
            .unwrap()
            .contains("private instructions")
    );
    std::fs::remove_file(&file).unwrap();
    drop(prepared);
    let restored = PreparedRunStore::open(root.path().join("prepared")).unwrap();
    let saved = restored.read(&summary.id, summary.revision).unwrap();
    assert_eq!(saved.selection, bytes);
    assert_eq!(saved.binding.source_id, source_id);
    assert_eq!(saved.binding.checkout_id, checkout.checkout_id);
    assert_eq!(saved.binding.agents, vec!["codex"]);
}

#[test]
fn stale_republication_and_unpublish_preserve_private_history() {
    let (root, sources, source_id) = setup();
    let checkout = sources
        .prepare_checkout_with(RUN, &source_id, &root.path().join("checkout"), materialize)
        .unwrap();
    let prepared = PreparedRunStore::open(root.path().join("prepared")).unwrap();
    let runs = RunStore::new(root.path().join("runs")).unwrap();
    let file = root.path().join("selection.json");
    private_file(&file, &selection(root.path(), &checkout.head, "first"));
    let mut request = Publication {
        run_id: RUN,
        source_id: &source_id,
        checkout_id: &checkout.checkout_id,
        selection: &file,
        previous: None,
    };
    let first = publish_with(&sources, &prepared, &runs, &request, |_| Ok(())).unwrap();
    private_file(&file, &selection(root.path(), &checkout.head, "second"));
    request.previous = Some((&first.id, first.revision));
    let second = publish_with(&sources, &prepared, &runs, &request, |_| Ok(())).unwrap();
    assert_eq!(second.revision, first.revision + 1);
    assert!(publish_with(&sources, &prepared, &runs, &request, |_| Ok(())).is_err());
    prepared.unpublish(&second.id, second.revision).unwrap();
    assert_eq!(prepared.list().unwrap(), Vec::new());
    assert_eq!(prepared.inspect(&second.id).unwrap().revisions.len(), 2);
}

#[test]
fn existing_run_and_mismatched_or_nonprivate_selection_are_refused() {
    let (root, sources, source_id) = setup();
    let checkout = sources
        .prepare_checkout_with(RUN, &source_id, &root.path().join("checkout"), materialize)
        .unwrap();
    let prepared = PreparedRunStore::open(root.path().join("prepared")).unwrap();
    let runs = RunStore::new(root.path().join("runs")).unwrap();
    let file = root.path().join("selection.json");
    private_file(
        &file,
        &selection(root.path(), &"04".repeat(20), "wrong head"),
    );
    let request = Publication {
        run_id: RUN,
        source_id: &source_id,
        checkout_id: &checkout.checkout_id,
        selection: &file,
        previous: None,
    };
    assert!(publish_with(&sources, &prepared, &runs, &request, |_| Ok(())).is_err());
    private_file(&file, &selection(root.path(), &checkout.head, "private"));
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(publish_with(&sources, &prepared, &runs, &request, |_| Ok(())).is_err());
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    runs.admit(
        RunAdmission {
            id: RUN.into(),
            generated_work_ceiling: 1,
            park_ttl_ms: 1000,
        },
        "generate-token-1234",
    )
    .unwrap();
    assert!(publish_with(&sources, &prepared, &runs, &request, |_| Ok(())).is_err());
    assert_eq!(prepared.list().unwrap(), Vec::new());
}

#[test]
fn duplicate_identity_fields_and_symlinked_selection_are_refused() {
    let (root, sources, source_id) = setup();
    let checkout = sources
        .prepare_checkout_with(RUN, &source_id, &root.path().join("checkout"), materialize)
        .unwrap();
    let prepared = PreparedRunStore::open(root.path().join("prepared")).unwrap();
    let runs = RunStore::new(root.path().join("runs")).unwrap();
    let file = root.path().join("selection.json");
    let original = selection(root.path(), &checkout.head, "private");
    let duplicate = original.replace(
        &format!("\"run_id\":\"{RUN}\""),
        &format!("\"run_id\":\"{RUN}\",\"run_id\":\"{RUN}\""),
    );
    assert_ne!(duplicate, original);
    private_file(&file, &duplicate);
    let request = Publication {
        run_id: RUN,
        source_id: &source_id,
        checkout_id: &checkout.checkout_id,
        selection: &file,
        previous: None,
    };
    assert!(publish_with(&sources, &prepared, &runs, &request, |_| Ok(())).is_err());
    private_file(&file, &original);
    let link = root.path().join("linked-selection.json");
    std::os::unix::fs::symlink(&file, &link).unwrap();
    let linked = Publication {
        selection: &link,
        ..request
    };
    assert!(publish_with(&sources, &prepared, &runs, &linked, |_| Ok(())).is_err());
    assert_eq!(prepared.list().unwrap(), Vec::new());
}

#[test]
fn publication_rejects_incomplete_active_or_contradictory_drafts() {
    let (root, sources, source_id) = setup();
    let checkout = sources
        .prepare_checkout_with(RUN, &source_id, &root.path().join("checkout"), materialize)
        .unwrap();
    let prepared = PreparedRunStore::open(root.path().join("prepared")).unwrap();
    let runs = RunStore::new(root.path().join("runs")).unwrap();
    let file = root.path().join("selection.json");
    let request = Publication {
        run_id: RUN,
        source_id: &source_id,
        checkout_id: &checkout.checkout_id,
        selection: &file,
        previous: None,
    };
    let valid: serde_json::Value =
        serde_json::from_str(&selection(root.path(), &checkout.head, "private")).unwrap();
    let mut incomplete = valid.clone();
    incomplete["envelope"]
        .as_object_mut()
        .unwrap()
        .remove("bead_scope");
    let mut active = valid.clone();
    active["envelope"]["expires_at_ms"] = 99.into();
    let mut scope = valid.clone();
    scope["envelope"]["bead_scope"]["issue_ids"] = serde_json::json!(["a", "b", "other"]);
    let mut limits = valid.clone();
    limits["execution"] = serde_json::json!({"kind":"duration","duration_ms":0});
    let mut null_execution = valid.clone();
    null_execution["execution"] = serde_json::Value::Null;
    let mut skills = valid.clone();
    skills["manifest"]["skills"] = serde_json::json!({"mode":"off","view_digest":"bad"});
    let mut commands = valid.clone();
    commands["envelope"]["commands"] = serde_json::json!({"expires_at_ms":0});
    for value in [
        incomplete,
        active,
        scope,
        limits,
        null_execution,
        skills,
        commands,
    ] {
        private_file(&file, &value.to_string());
        assert!(
            publish_with(&sources, &prepared, &runs, &request, |_| Ok(())).is_err(),
            "invalid draft published"
        );
    }
    assert_eq!(prepared.list().unwrap(), Vec::new());
}
