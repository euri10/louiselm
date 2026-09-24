//! Behavioral coverage for capture.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Test fixtures abort on setup failure and assert failures directly."
)]

//! Capture behaviour: what becomes a package, and what is refused outright.
//!
//! Every refusal here is a case where the tree cannot be described
//! unambiguously by a canonical manifest. The assertion that matters in each
//! is not only the error but the store staying empty: a refused capture must
//! publish nothing.

mod support;

use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    process::Command,
};

use louiselm_skills::{
    CaptureError, Policy, PublishOutcome, Store, canonical::PathError, manifest::ManifestError,
    store::StoreError,
};
use support::{Fixture, write_file};

#[test]
fn identical_trees_in_different_places_produce_one_digest() {
    let fixture = Fixture::new();
    let first = fixture.candidate("first");
    write_file(&first.join("SKILL.md"), "---\nname: demo\n---\nbody\n");
    write_file(&first.join("references/notes.md"), "notes\n");

    let second = fixture.candidate("second/nested/deeper");
    write_file(&second.join("references/notes.md"), "notes\n");
    write_file(&second.join("SKILL.md"), "---\nname: demo\n---\nbody\n");
    fs::set_permissions(second.join("SKILL.md"), fs::Permissions::from_mode(0o600))
        .expect("mode is settable");

    let (left, outcome) = fixture.capture(&first).expect("first capture succeeds");
    let (right, second_outcome) = fixture.capture(&second).expect("second capture succeeds");

    assert_eq!(left.digest, right.digest);
    assert_eq!(outcome, PublishOutcome::Created);
    assert_eq!(second_outcome, PublishOutcome::Existing);
    assert_eq!(fixture.store().list().expect("store lists").len(), 1);
}

#[test]
fn empty_directories_leave_no_trace_in_the_manifest() {
    let fixture = Fixture::new();
    let flat = fixture.candidate("flat");
    write_file(&flat.join("SKILL.md"), "body\n");

    let padded = fixture.candidate("padded");
    write_file(&padded.join("SKILL.md"), "body\n");
    fs::create_dir_all(padded.join("empty/deeper")).expect("directories are creatable");

    let (left, _) = fixture.capture(&flat).expect("flat capture succeeds");
    let (right, _) = fixture.capture(&padded).expect("padded capture succeeds");

    assert_eq!(left.digest, right.digest);
    assert_eq!(left.manifest.entries.len(), 1);
}

#[test]
fn a_symlink_becomes_the_bytes_it_pointed_at() {
    let fixture = Fixture::new();
    let plain = fixture.candidate("plain");
    write_file(&plain.join("SKILL.md"), "body\n");
    write_file(&plain.join("shared.md"), "shared\n");

    let linked = fixture.candidate("linked");
    write_file(&linked.join("SKILL.md"), "body\n");
    write_file(&linked.join("real/shared.md"), "shared\n");
    symlink("real/shared.md", linked.join("shared.md")).expect("symlink is creatable");
    fs::remove_file(linked.join("real/shared.md")).expect("target is removable");
    fs::create_dir_all(linked.join("real")).ok();
    write_file(&linked.join("real/shared.md"), "shared\n");

    let (packaged, _) = fixture.capture(&linked).expect("linked capture succeeds");

    assert_eq!(
        packaged
            .manifest
            .entries
            .iter()
            .map(|entry| entry.path.as_str())
            .collect::<Vec<_>>(),
        vec!["SKILL.md", "real/shared.md", "shared.md"],
    );
    let materialized = packaged.file_path("shared.md");
    assert!(
        !fs::symlink_metadata(&materialized)
            .expect("packaged file exists")
            .file_type()
            .is_symlink(),
        "a packaged symlink must be materialized as a regular file",
    );

    write_file(&linked.join("real/shared.md"), "tampered\n");
    assert_eq!(
        fs::read_to_string(&materialized).expect("packaged bytes are readable"),
        "shared\n",
        "mutating the link target must not reach a published package",
    );

    let lineage = fixture
        .store()
        .lineage(&packaged.digest)
        .expect("lineage is readable")
        .expect("lineage was recorded");
    let link = lineage.captures[0]
        .links
        .iter()
        .find(|link| link.path == "shared.md")
        .expect("the link origin is recorded");
    assert_eq!(link.declared_target, "real/shared.md");
    assert!(!link.escapes_root);
}

#[test]
fn a_link_out_of_the_candidate_is_captured_and_marked_in_lineage() {
    let fixture = Fixture::new();
    let outside = fixture.path("outside");
    write_file(&outside.join("vendor.md"), "vendor\n");

    let candidate = fixture.candidate("candidate");
    write_file(&candidate.join("SKILL.md"), "body\n");
    symlink(outside.join("vendor.md"), candidate.join("vendor.md")).expect("symlink is creatable");

    let (packaged, _) = fixture.capture(&candidate).expect("capture succeeds");
    let lineage = fixture
        .store()
        .lineage(&packaged.digest)
        .expect("lineage is readable")
        .expect("lineage was recorded");

    assert_eq!(lineage.escaping_links().len(), 1);
    assert_eq!(lineage.escaping_links()[0].path, "vendor.md");
    assert!(
        packaged.manifest.entry("vendor.md").is_some(),
        "content reached by an escaping link is still packaged",
    );
}

#[test]
fn a_directory_cycle_is_refused_and_publishes_nothing() {
    let fixture = Fixture::new();
    let candidate = fixture.candidate("candidate");
    write_file(&candidate.join("SKILL.md"), "body\n");
    fs::create_dir_all(candidate.join("inner")).expect("directory is creatable");
    symlink(&candidate, candidate.join("inner/loop")).expect("symlink is creatable");

    let error = fixture.capture(&candidate).expect_err("a cycle is refused");

    assert!(
        matches!(error, StoreError::Capture(CaptureError::Cycle { .. })),
        "unexpected error: {error}",
    );
    fixture.assert_store_is_empty();
}

#[test]
fn a_hardlinked_file_is_refused_because_its_bytes_have_a_second_writer() {
    let fixture = Fixture::new();
    let candidate = fixture.candidate("candidate");
    write_file(&candidate.join("SKILL.md"), "body\n");
    fs::hard_link(candidate.join("SKILL.md"), candidate.join("alias.md"))
        .expect("hard link is creatable");

    let error = fixture
        .capture(&candidate)
        .expect_err("a hard link is refused");

    assert!(
        matches!(
            error,
            StoreError::Capture(CaptureError::HardlinkAmbiguity { links: 2, .. }),
        ),
        "unexpected error: {error}",
    );
    fixture.assert_store_is_empty();
}

#[test]
fn a_special_file_is_refused() {
    let fixture = Fixture::new();
    let candidate = fixture.candidate("candidate");
    write_file(&candidate.join("SKILL.md"), "body\n");
    let fifo = candidate.join("pipe");
    let status = Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .expect("mkfifo runs");
    assert!(status.success(), "mkfifo failed");

    let error = fixture.capture(&candidate).expect_err("a fifo is refused");

    assert!(
        matches!(
            error,
            StoreError::Capture(CaptureError::SpecialFile { ref kind, .. }) if kind == "fifo",
        ),
        "unexpected error: {error}",
    );
    fixture.assert_store_is_empty();
}

#[test]
fn paths_that_a_filesystem_would_merge_are_refused() {
    let fixture = Fixture::new();
    let candidate = fixture.candidate("candidate");
    write_file(&candidate.join("SKILL.md"), "body\n");
    write_file(&candidate.join("skill.md"), "different\n");

    let error = fixture
        .capture(&candidate)
        .expect_err("colliding paths are refused");

    assert!(
        matches!(
            error,
            StoreError::Capture(CaptureError::Manifest(ManifestError::CollidingPaths { .. })),
        ),
        "unexpected error: {error}",
    );
    fixture.assert_store_is_empty();
}

#[test]
fn unicode_collisions_publish_nothing_when_non_ascii_paths_are_allowed() {
    let policy =
        support::policy_with(&[("\"allow_non_ascii\": false", "\"allow_non_ascii\": true")]);
    for (first, second) in [("café.md", "cafe\u{301}.md"), ("ﬁle.md", "file.md")] {
        let fixture = Fixture::new();
        let candidate = fixture.candidate("candidate");
        write_file(&candidate.join("SKILL.md"), "body\n");
        write_file(&candidate.join(first), "first\n");
        write_file(&candidate.join(second), "second\n");
        let error = fixture
            .capture_with(&candidate, &policy)
            .expect_err("Unicode collision refused");
        assert!(
            matches!(
                error,
                StoreError::Capture(CaptureError::Manifest(ManifestError::CollidingPaths { .. }))
            ),
            "unexpected error: {error}"
        );
        fixture.assert_store_is_empty();
    }
}

#[test]
fn a_non_ascii_path_is_refused_under_the_default_policy() {
    let fixture = Fixture::new();
    let candidate = fixture.candidate("candidate");
    write_file(&candidate.join("SKILL.md"), "body\n");
    write_file(&candidate.join("réadme.md"), "body\n");

    let error = fixture
        .capture(&candidate)
        .expect_err("non-ASCII is refused");

    assert!(
        matches!(
            error,
            StoreError::Capture(CaptureError::Path {
                source: PathError::NonAscii(_),
                ..
            }),
        ),
        "unexpected error: {error}",
    );
    fixture.assert_store_is_empty();
}

#[test]
fn policy_limits_are_enforced_before_anything_is_published() {
    let fixture = Fixture::new();
    let candidate = fixture.candidate("candidate");
    write_file(&candidate.join("SKILL.md"), &"x".repeat(64));

    let tight = support::policy_with(&[("\"max_file_bytes\": 5242880", "\"max_file_bytes\": 16")]);
    let error = fixture
        .capture_with(&candidate, &tight)
        .expect_err("an oversized file is refused");

    assert!(
        matches!(error, StoreError::Capture(CaptureError::LimitExceeded(_))),
        "unexpected error: {error}",
    );
    fixture.assert_store_is_empty();
}

#[test]
fn modes_collapse_to_one_executable_bit_and_packaged_files_are_read_only() {
    let fixture = Fixture::new();
    let candidate = fixture.candidate("candidate");
    write_file(&candidate.join("SKILL.md"), "body\n");
    write_file(&candidate.join("scripts/run.sh"), "#!/bin/sh\necho hi\n");
    fs::set_permissions(
        candidate.join("scripts/run.sh"),
        fs::Permissions::from_mode(0o4711),
    )
    .expect("mode is settable");

    let (packaged, _) = fixture.capture(&candidate).expect("capture succeeds");

    let script = packaged
        .manifest
        .entry("scripts/run.sh")
        .expect("the script is packaged");
    assert!(script.executable);
    assert!(
        !packaged
            .manifest
            .entry("SKILL.md")
            .expect("skill is packaged")
            .executable
    );

    let stored_mode = fs::metadata(packaged.file_path("scripts/run.sh"))
        .expect("packaged file exists")
        .permissions()
        .mode()
        & 0o7777;
    assert_eq!(stored_mode, 0o555, "setuid and write bits must not survive");
    let stored_mode = fs::metadata(packaged.file_path("SKILL.md"))
        .expect("packaged file exists")
        .permissions()
        .mode()
        & 0o7777;
    assert_eq!(stored_mode, 0o444);
}

#[test]
fn an_unreadable_candidate_root_is_refused() {
    let fixture = Fixture::new();
    let missing = fixture.path("nowhere");

    let error = Store::open(&fixture.store_root())
        .expect("store opens")
        .capture(&missing, &Policy::embedded(), 0)
        .expect_err("a missing root is refused");

    assert!(
        matches!(error, StoreError::Capture(CaptureError::Root { .. })),
        "unexpected error: {error}",
    );
}
