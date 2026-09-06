//! Behavioral coverage for diff.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Test fixtures abort on setup failure and assert failures directly."
)]

//! Update diffs: what changed between the package under review and the one it
//! would replace.

mod support;

use louiselm_skills::diff::{Change, LineKind, PackageDiff};
use support::{Fixture, write_file};

#[test]
fn a_diff_names_every_kind_of_change_and_shows_the_lines() {
    let fixture = Fixture::new();
    let base = fixture.candidate("base");
    write_file(&base.join("SKILL.md"), "line one\nline two\nline three\n");
    write_file(&base.join("gone.md"), "removed\n");

    let target = fixture.candidate("target");
    write_file(
        &target.join("SKILL.md"),
        "line one\nline two changed\nline three\n",
    );
    write_file(&target.join("added.md"), "new\n");

    let (base_package, _) = fixture.capture(&base).expect("base capture succeeds");
    let (target_package, _) = fixture.capture(&target).expect("target capture succeeds");
    let diff = PackageDiff::between(&base_package, &target_package).expect("diff runs");

    assert_eq!(diff.base_digest, base_package.digest.to_string());
    assert_eq!(diff.target_digest, target_package.digest.to_string());
    assert_eq!(
        diff.entries
            .iter()
            .map(|entry| (entry.path.as_str(), entry.change))
            .collect::<Vec<_>>(),
        vec![
            ("SKILL.md", Change::ContentChanged),
            ("added.md", Change::Added),
            ("gone.md", Change::Removed),
        ],
    );

    let changed = &diff.entries[0];
    let removed = changed
        .lines
        .iter()
        .find(|line| line.kind == LineKind::Removed)
        .expect("the old line is shown");
    let added = changed
        .lines
        .iter()
        .find(|line| line.kind == LineKind::Added)
        .expect("the new line is shown");
    assert_eq!(removed.text, "line two");
    assert_eq!(added.text, "line two changed");
}

#[test]
fn an_identical_package_diffs_to_nothing() {
    let fixture = Fixture::new();
    let candidate = fixture.candidate("candidate");
    write_file(&candidate.join("SKILL.md"), "body\n");

    let (package, _) = fixture.capture(&candidate).expect("capture succeeds");
    let diff = PackageDiff::between(&package, &package).expect("diff runs");

    assert!(diff.entries.is_empty());
    assert!(!diff.has_changes());
}

#[test]
fn a_mode_change_alone_is_still_a_change() {
    use std::{fs, os::unix::fs::PermissionsExt};

    let fixture = Fixture::new();
    let base = fixture.candidate("base");
    write_file(&base.join("SKILL.md"), "body\n");
    write_file(&base.join("run.sh"), "#!/bin/sh\n");

    let target = fixture.candidate("target");
    write_file(&target.join("SKILL.md"), "body\n");
    write_file(&target.join("run.sh"), "#!/bin/sh\n");
    fs::set_permissions(target.join("run.sh"), fs::Permissions::from_mode(0o755))
        .expect("mode is settable");

    let (base_package, _) = fixture.capture(&base).expect("base capture succeeds");
    let (target_package, _) = fixture.capture(&target).expect("target capture succeeds");
    let diff = PackageDiff::between(&base_package, &target_package).expect("diff runs");

    assert_eq!(
        diff.entries
            .iter()
            .map(|entry| (entry.path.as_str(), entry.change))
            .collect::<Vec<_>>(),
        vec![("run.sh", Change::ModeChanged)],
    );
    assert!(diff.entries[0].became_executable);
}

#[test]
fn binary_content_is_reported_as_changed_without_pretending_to_show_lines() {
    let fixture = Fixture::new();
    let base = fixture.candidate("base");
    write_file(&base.join("SKILL.md"), "body\n");
    std::fs::write(base.join("blob.bin"), [0x00, 0x01, 0x02]).expect("file is writable");

    let target = fixture.candidate("target");
    write_file(&target.join("SKILL.md"), "body\n");
    std::fs::write(target.join("blob.bin"), [0x00, 0x01, 0x03]).expect("file is writable");

    let (base_package, _) = fixture.capture(&base).expect("base capture succeeds");
    let (target_package, _) = fixture.capture(&target).expect("target capture succeeds");
    let diff = PackageDiff::between(&base_package, &target_package).expect("diff runs");

    let entry = &diff.entries[0];
    assert_eq!(entry.change, Change::ContentChanged);
    assert!(entry.lines.is_empty());
    assert_eq!(entry.note.as_deref(), Some("binary content"));
}

#[test]
fn diff_text_is_escaped_like_every_other_reviewer_facing_surface() {
    let fixture = Fixture::new();
    let base = fixture.candidate("base");
    write_file(&base.join("SKILL.md"), "plain\n");
    let target = fixture.candidate("target");
    write_file(&target.join("SKILL.md"), "hostile \u{1b}[2J banner\n");

    let (base_package, _) = fixture.capture(&base).expect("base capture succeeds");
    let (target_package, _) = fixture.capture(&target).expect("target capture succeeds");
    let diff = PackageDiff::between(&base_package, &target_package).expect("diff runs");

    for entry in &diff.entries {
        for line in &entry.lines {
            assert!(
                !line.text.chars().any(char::is_control),
                "raw control character survived into a diff: {:?}",
                line.text,
            );
        }
    }
}
