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
    prepared_owner::{PreparedOwner, Publication, publish_with},
    prepared_selection::project_detail,
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
    let mut credential_url = valid.clone();
    credential_url["envelope"]["provider_requests"]["upstream"] =
        "https://secret@api.openai.com/v1/responses".into();
    let mut multicast = valid.clone();
    multicast["envelope"]["provider_requests"]["addresses"] = serde_json::json!(["224.0.0.1"]);
    for value in [
        incomplete,
        active,
        scope,
        limits,
        null_execution,
        skills,
        commands,
        credential_url,
        multicast,
    ] {
        private_file(&file, &value.to_string());
        assert!(
            publish_with(&sources, &prepared, &runs, &request, |_| Ok(())).is_err(),
            "invalid draft published"
        );
    }
    for upstream in [
        "http://api.openai.com/v1/responses",
        "https://api.openai.com/v1/responses?token=dummy",
        "https://api.openai.com/v1/responses#fragment",
        "https://api.openai.com:443/v1/responses",
        "https://api.openai.com/other",
    ] {
        let mut invalid = valid.clone();
        invalid["envelope"]["provider_requests"]["upstream"] = upstream.into();
        private_file(&file, &invalid.to_string());
        assert!(publish_with(&sources, &prepared, &runs, &request, |_| Ok(())).is_err());
    }
    for addresses in [
        serde_json::json!([]),
        serde_json::json!(["0.0.0.0"]),
        serde_json::json!(["127.0.0.1", "127.0.0.1"]),
    ] {
        let mut invalid = valid.clone();
        invalid["envelope"]["provider_requests"]["addresses"] = addresses;
        private_file(&file, &invalid.to_string());
        assert!(publish_with(&sources, &prepared, &runs, &request, |_| Ok(())).is_err());
    }
    assert_eq!(prepared.list().unwrap(), Vec::new());
}

#[test]
fn prepared_detail_is_exact_bounded_and_omits_local_input_paths() {
    let (root, sources, source_id) = setup();
    let checkout = sources
        .prepare_checkout_with(RUN, &source_id, &root.path().join("checkout"), materialize)
        .unwrap();
    let prepared = PreparedRunStore::open(root.path().join("prepared")).unwrap();
    let runs = RunStore::new(root.path().join("runs")).unwrap();
    let file = root.path().join("selection.json");
    private_file(
        &file,
        &selection(root.path(), &checkout.head, "inspect /intentional/path"),
    );
    let request = Publication {
        run_id: RUN,
        source_id: &source_id,
        checkout_id: &checkout.checkout_id,
        selection: &file,
        previous: None,
    };
    let summary = publish_with(&sources, &prepared, &runs, &request, |_| Ok(())).unwrap();
    let saved = prepared.read(&summary.id, summary.revision).unwrap();
    let detail = project_detail(&saved, std::fs::metadata(root.path()).unwrap().uid()).unwrap();
    assert_eq!(detail["state"], "prepared");
    assert_eq!(detail["beads"][0]["prompt"], "inspect /intentional/path");
    assert_eq!(
        detail["execution"],
        serde_json::json!({"kind":"duration","duration_ms":1_800_000})
    );
    assert_eq!(detail["max_sessions"], 6);
    assert_eq!(detail["bead_scope"]["max_mutations"], 3);
    assert_eq!(detail["provider_requests"]["max_run_requests"], 3);
    assert_eq!(
        detail["provider_requests"]["models"],
        serde_json::json!(["gpt-5.6-luna"])
    );
    assert!(!detail.to_string().contains("/private/cache"));
    assert!(!detail.to_string().contains("/private/journal"));
    assert!(!detail.to_string().contains(root.path().to_str().unwrap()));
    assert!(runs.run(RUN).is_err());
    let mut formerly_published = saved.clone();
    let mut invalid: serde_json::Value =
        serde_json::from_str(&formerly_published.selection).unwrap();
    invalid["envelope"]["provider_requests"]["upstream"] =
        "https://secret@api.openai.com/v1/responses".into();
    formerly_published.selection = invalid.to_string();
    assert!(
        project_detail(
            &formerly_published,
            std::fs::metadata(root.path()).unwrap().uid()
        )
        .is_err()
    );
    let mut wrong = saved;
    wrong.binding.agents = vec!["other".into()];
    assert!(project_detail(&wrong, std::fs::metadata(root.path()).unwrap().uid()).is_err());
}

#[test]
fn protected_detail_holds_unused_run_and_exact_publication_locks() {
    use fs2::FileExt;
    let (root, sources, source_id) = setup();
    let checkout = sources
        .prepare_checkout_with(RUN, &source_id, &root.path().join("checkout"), materialize)
        .unwrap();
    std::fs::rename(root.path().join("sources"), root.path().join("run-sources")).unwrap();
    let prepared = PreparedRunStore::open(root.path().join("prepared-runs")).unwrap();
    let runs = RunStore::new(root.path().join("runs")).unwrap();
    let source_store =
        super::source::PhoneSourceStore::open(root.path().join("run-sources")).unwrap();
    let file = root.path().join("selection.json");
    private_file(&file, &selection(root.path(), &checkout.head, "private"));
    let request = Publication {
        run_id: RUN,
        source_id: &source_id,
        checkout_id: &checkout.checkout_id,
        selection: &file,
        previous: None,
    };
    let summary = publish_with(&source_store, &prepared, &runs, &request, |_| Ok(())).unwrap();
    let mut owner = PreparedOwner::new(root.path().to_path_buf(), runs.clone());
    owner.set_test_integration(|| Ok(()));
    let candidates = owner.reader().candidates().unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].0, summary);
    assert_eq!(
        candidates[0].1,
        prepared
            .read(&summary.id, summary.revision)
            .unwrap()
            .binding
    );
    let open_lock = |path: &Path| {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap()
    };
    let run_lock = open_lock(&root.path().join("runs/.lock"));
    let prepared_lock = open_lock(&root.path().join("prepared-runs/.lock"));
    owner
        .with_published_by(
            &summary.id,
            summary.revision,
            || Ok(()),
            |saved, uid| {
                assert_eq!(
                    run_lock.try_lock_exclusive().unwrap_err().kind(),
                    std::io::ErrorKind::WouldBlock
                );
                assert_eq!(
                    prepared_lock.try_lock_exclusive().unwrap_err().kind(),
                    std::io::ErrorKind::WouldBlock
                );
                assert_eq!(project_detail(saved, uid).unwrap()["state"], "prepared");
            },
        )
        .unwrap();
    assert!(
        owner
            .with_published_by(&summary.id, summary.revision + 1, || Ok(()), |_, _| ())
            .is_err()
    );
    runs.admit(
        RunAdmission {
            id: RUN.into(),
            generated_work_ceiling: 1,
            park_ttl_ms: 1000,
        },
        "generate-token-1234",
    )
    .unwrap();
    assert!(
        owner
            .with_published_by(&summary.id, summary.revision, || Ok(()), |_, _| ())
            .is_err()
    );
    prepared.unpublish(&summary.id, summary.revision).unwrap();
    assert_eq!(owner.reader().candidates().unwrap(), Vec::new());
    assert!(
        owner
            .with_published_by(&summary.id, summary.revision, || Ok(()), |_, _| ())
            .is_err()
    );
}
