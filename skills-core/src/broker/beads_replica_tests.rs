#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "Disposable upstream fixtures assert tracker outcomes."
)]

use super::*;
use crate::Digest;
use std::process::Command;

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
