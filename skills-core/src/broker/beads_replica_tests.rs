#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "Disposable upstream fixtures assert tracker outcomes."
)]

use super::*;
use crate::Digest;
use std::{os::unix::fs::PermissionsExt, process::Command};

fn native(
    program: &Path,
    workspace: &Path,
    beads: Option<&Path>,
    args: &[&str],
) -> serde_json::Value {
    let mut command = Command::new(program);
    command.args(args).env_clear().current_dir(workspace);
    if let Some(beads) = beads {
        command.env("BEADS_DIR", beads);
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "upstream fixture failed: {output:?}"
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn replica_build_explicitly_imports_the_exported_history() {
    let fixture = tempfile::tempdir().unwrap();
    let source = fixture.path().join("source");
    fs::create_dir_all(source.join(".beads")).unwrap();
    let program = fixture.path().join("br");
    fs::write(
        &program,
        br#"#!/bin/sh
set -eu
test "$1" = sync
case "$2" in
  --flush-only)
    test "$FSQLITE_TRUSTED_UNIX_GID" = 1500
    test "$FSQLITE_TRUSTED_UNIX_DATABASE" = "$PWD/.beads/beads.db"
    printf issues > "$BEADS_JSONL"
    printf history > "${BEADS_JSONL%/*}/issues.history.jsonl"
    ;;
  --import-only)
    test -z "${FSQLITE_TRUSTED_UNIX_DATABASE+x}"
    test -z "${FSQLITE_TRUSTED_UNIX_GID+x}"
    test -f "$BEADS_DIR/issues.history.jsonl"
    printf database > "$BEADS_DIR/beads.db"
    ;;
  *) exit 1 ;;
esac
"#,
    )
    .unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
    let tracker = TrackerConfig {
        program_digest: Digest::of(&fs::read(&program).unwrap()),
        program,
        workspace_root: source,
        scratch: fixture.path().into(),
        trusted_group: Some(1500),
    };
    let files = export(&tracker, "fixture/replica").unwrap();
    assert_eq!(files[".beads/issues.history.jsonl"], b"history");
    assert_eq!(files[".beads/beads.db"], b"database");
}

#[test]
#[ignore = "requires explicit upstream br and a disposable tracker snapshot"]
fn real_br_replica_preserves_representative_tracker_history() {
    let program = PathBuf::from(std::env::var_os("LOUISELM_TEST_BR").expect("upstream br"));
    let snapshot = PathBuf::from(
        std::env::var_os("LOUISELM_TEST_TRACKER_SNAPSHOT").expect("disposable tracker snapshot"),
    );
    let issue = std::env::var("LOUISELM_TEST_TRACKER_ISSUE").expect("snapshot issue");
    let beads = snapshot.join(".beads");
    if let Ok(wal) = fs::metadata(beads.join("beads.db-wal")) {
        assert_eq!(wal.len(), 0, "snapshot database must be checkpointed");
    }
    let fixture = tempfile::tempdir().unwrap();
    let source = fixture.path().join("source");
    fs::create_dir_all(source.join(".beads")).unwrap();
    for name in [
        "beads.db",
        "config.yaml",
        "issues.jsonl",
        "issues.history.jsonl",
        "metadata.json",
        "policy.yaml",
    ] {
        fs::copy(beads.join(name), source.join(".beads").join(name)).unwrap();
    }
    let tracker = TrackerConfig {
        program_digest: Digest::of(&fs::read(&program).unwrap()),
        program: program.clone(),
        workspace_root: source.clone(),
        scratch: fixture.path().into(),
        trusted_group: None,
    };
    let replica = fixture.path().join("replica");
    fs::create_dir(&replica).unwrap();
    let files = export(&tracker, "fixture/replica").unwrap();
    assert!(files.contains_key(".beads/issues.history.jsonl"));
    beads_replica::publish(&replica, "first", &files).unwrap();
    let current = replica.join("current/.beads");
    let mut before = native(&program, &source, None, &["show", &issue, "--json"]);
    let mut after = native(
        &program,
        fixture.path(),
        Some(&current),
        &["show", &issue, "--json"],
    );
    // This local database path is deliberately not part of portable JSONL.
    for view in [&mut before, &mut after] {
        view[0].as_object_mut().unwrap().remove("source_repo_path");
    }
    assert!(before == after, "native issue views differ");
    let events = |database: &Path| {
        let output = Command::new("/usr/bin/sqlite3")
            .arg(format!("file:{}?mode=ro", database.display()))
            .arg("select count(*) from events")
            .output()
            .unwrap();
        assert!(output.status.success(), "event count unavailable");
        String::from_utf8(output.stdout).unwrap()
    };
    let source_events = events(&source.join(".beads/beads.db"));
    assert_ne!(source_events.trim(), "0");
    assert_eq!(source_events, events(&current.join("beads.db")));
}

#[test]
#[ignore = "requires explicit upstream br (and optionally bvr), no network"]
#[expect(
    clippy::too_many_lines,
    reason = "One disposable real-upstream lifecycle proves read parity, mutation refresh and canonical isolation."
)]
fn real_br_replica_preserves_native_reads_after_refresh() {
    let program = PathBuf::from(std::env::var_os("LOUISELM_TEST_BR").expect("upstream br"));
    let fixture = tempfile::tempdir().unwrap();
    let source = fixture.path().join("source");
    fs::create_dir(&source).unwrap();
    native(
        &program,
        &source,
        None,
        &[
            "init",
            "--prefix",
            "fixture",
            "--actor",
            "fixture/setup",
            "--json",
        ],
    );
    let issue = native(
        &program,
        &source,
        None,
        &[
            "create",
            "Replica target",
            "--actor",
            "fixture/setup",
            "--json",
        ],
    );
    let id = issue["id"].as_str().unwrap();
    let tracker = TrackerConfig {
        program_digest: Digest::of(&fs::read(&program).unwrap()),
        program: program.clone(),
        workspace_root: source.clone(),
        scratch: fixture.path().into(),
        trusted_group: None,
    };
    let replica = fixture.path().join("replica");
    fs::create_dir(&replica).unwrap();
    beads_replica::publish(
        &replica,
        "first",
        &export(&tracker, "fixture/replica").unwrap(),
    )
    .unwrap();
    let current = replica.join("current/.beads");
    let first = native(
        &program,
        fixture.path(),
        Some(&current),
        &["show", id, "--json"],
    );
    assert_eq!(first[0]["title"], "Replica target");
    native(
        &program,
        &source,
        None,
        &[
            "comments",
            "add",
            id,
            "--message",
            "canonical comment",
            "--actor",
            "fixture/replica",
            "--json",
        ],
    );
    let before = native(
        &program,
        fixture.path(),
        Some(&current),
        &["comments", "list", id, "--json"],
    );
    assert_eq!(before.as_array().unwrap().len(), 0);
    beads_replica::publish(
        &replica,
        "second",
        &export(&tracker, "fixture/replica").unwrap(),
    )
    .unwrap();
    let after = native(
        &program,
        fixture.path(),
        Some(&current),
        &["comments", "list", id, "--json"],
    );
    assert_eq!(after.as_array().unwrap().len(), 1);
    assert_eq!(after[0]["text"], "canonical comment");
    assert_eq!(
        native(
            &program,
            fixture.path(),
            Some(&current),
            &["ready", "--json"]
        )[0]["id"],
        id
    );
    if let Some(viewer) = std::env::var_os("LOUISELM_TEST_BVR") {
        let next = native(
            Path::new(&viewer),
            fixture.path(),
            Some(&current),
            &["--robot-next"],
        );
        assert_eq!(next["id"], id);
    }
    // Local edits are disposable and have no route back to canonical state.
    native(
        &program,
        fixture.path(),
        Some(&current),
        &[
            "update",
            id,
            "--title",
            "local only",
            "--actor",
            "fixture/session",
            "--json",
        ],
    );
    assert_eq!(
        native(&program, &source, None, &["show", id, "--json"])[0]["title"],
        "Replica target"
    );
}
