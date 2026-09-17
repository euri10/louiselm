//! Source snapshots freeze selected bytes before private workspace creation.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "Tests abort on disposable fixture failures."
)]

use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt, symlink},
    path::Path,
    process::{Command, Output},
};

use serde_json::Value;
use tempfile::TempDir;

#[path = "workspace_cli/verification.rs"]
mod verification;

#[path = "workspace_cli/launch_inputs.rs"]
mod launch_inputs;

fn git(root: &Path, args: &[&str]) -> Output {
    let output = Command::new("/usr/bin/git")
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
        ])
        .args(args)
        .current_dir(root)
        .env_clear()
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn fixture() -> TempDir {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "--quiet", "--template="]);
    fs::write(repo.join("tracked.txt"), "committed\n").unwrap();
    fs::write(repo.join("deleted.txt"), "remove me\n").unwrap();
    fs::write(repo.join(".gitignore"), ".env\ncache/\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "--quiet", "-m", "base"]);
    temp
}

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_louiselm-skills"))
        .arg("workspace")
        .args(args)
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "core.hooksPath")
        .env("GIT_CONFIG_VALUE_0", "/untrusted-hooks")
        .output()
        .unwrap()
}

fn successful(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn prepare(temp: &TempDir, name: &str, includes: &[&str]) -> Value {
    let repo = temp.path().join("repo");
    let snapshot = temp.path().join(name);
    let mut args = vec![
        "prepare",
        "--repository",
        repo.to_str().unwrap(),
        "--output",
        snapshot.to_str().unwrap(),
        "--robot-json",
    ];
    for path in includes {
        args.extend(["--include", path]);
    }
    successful(&run(&args))
}

fn materialize(temp: &TempDir, digest: &str, output: &str) -> Output {
    run(&[
        "materialize",
        "--snapshot",
        temp.path().join("snapshot").to_str().unwrap(),
        "--digest",
        digest,
        "--output",
        temp.path().join(output).to_str().unwrap(),
        "--robot-json",
    ])
}

fn export_bundle(temp: &TempDir, digest: &str, output: &str) -> Output {
    run(&[
        "export",
        "--snapshot",
        temp.path().join("snapshot").to_str().unwrap(),
        "--digest",
        digest,
        "--workspace",
        temp.path().join("work").to_str().unwrap(),
        "--output",
        temp.path().join(output).to_str().unwrap(),
        "--robot-json",
    ])
}

fn apply_bundle(temp: &TempDir, snapshot: &str, bundle: &str, output: &str) -> Output {
    run(&[
        "apply",
        "--snapshot",
        temp.path().join("snapshot").to_str().unwrap(),
        "--digest",
        snapshot,
        "--bundle",
        temp.path().join("bundle").to_str().unwrap(),
        "--bundle-digest",
        bundle,
        "--output",
        temp.path().join(output).to_str().unwrap(),
        "--robot-json",
    ])
}

#[test]
fn bundles_reconstruct_exact_bytes_without_trusting_git_or_the_live_checkout() {
    let temp = fixture();
    let snapshot = prepare(&temp, "snapshot", &[]);
    let digest = snapshot["snapshot_digest"].as_str().unwrap();
    successful(&materialize(&temp, digest, "work"));
    let work = temp.path().join("work");
    fs::write(work.join("tracked.txt"), b"changed\0\xff").unwrap();
    fs::remove_file(work.join("deleted.txt")).unwrap();
    fs::create_dir(work.join("added")).unwrap();
    fs::write(work.join("added/run"), b"#!/bin/sh\nexit 99\n").unwrap();
    fs::set_permissions(work.join("added/run"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(work.join(".git/config"), b"invalid hostile Git config").unwrap();
    fs::write(work.join(".git/index"), b"invented changes").unwrap();
    fs::write(
        temp.path().join("repo/tracked.txt"),
        b"unrelated checkout mutation",
    )
    .unwrap();
    let receipt = successful(&export_bundle(&temp, digest, "bundle"));
    let again = successful(&export_bundle(&temp, digest, "again"));
    assert_eq!(receipt, again);
    assert_eq!(receipt["schema"], "louiselm.workspace.bundle-preview/1");
    assert_eq!(receipt["base_digest"], snapshot["base_digest"]);
    assert_eq!(receipt["added"], serde_json::json!(["added/run"]));
    assert_eq!(receipt["modified"], serde_json::json!(["tracked.txt"]));
    assert_eq!(receipt["deleted"], serde_json::json!(["deleted.txt"]));
    assert!(!temp.path().join("bundle/files/.gitignore").exists());
    let applied = successful(&apply_bundle(
        &temp,
        digest,
        receipt["bundle_digest"].as_str().unwrap(),
        "integration",
    ));
    assert_eq!(applied, receipt);
    let integration = temp.path().join("integration");
    assert_eq!(
        fs::read(integration.join("tracked.txt")).unwrap(),
        b"changed\0\xff"
    );
    assert_eq!(
        fs::read(integration.join(".gitignore")).unwrap(),
        b".env\ncache/\n"
    );
    assert_eq!(
        fs::metadata(integration.join("added/run")).unwrap().mode() & 0o777,
        0o700
    );
    assert!(!integration.join("deleted.txt").exists());
    assert!(!integration.join(".git").exists());
}

#[test]
fn bundle_digest_substitution_and_unsafe_workspace_inputs_never_publish() {
    let temp = fixture();
    let snapshot = prepare(&temp, "snapshot", &[]);
    let digest = snapshot["snapshot_digest"].as_str().unwrap();
    successful(&materialize(&temp, digest, "work"));
    let receipt = successful(&export_bundle(&temp, digest, "bundle"));
    let wrong = format!("sha256:{}", "0".repeat(64));
    assert!(
        !apply_bundle(&temp, digest, &wrong, "wrong-bundle")
            .status
            .success()
    );
    assert!(
        !apply_bundle(
            &temp,
            &wrong,
            receipt["bundle_digest"].as_str().unwrap(),
            "wrong-base"
        )
        .status
        .success()
    );
    assert!(!temp.path().join("wrong-bundle").exists());
    assert!(!temp.path().join("wrong-base").exists());
    symlink("/etc/passwd", temp.path().join("work/link")).unwrap();
    assert!(!export_bundle(&temp, digest, "linked").status.success());
    assert!(!temp.path().join("linked").exists());
}

#[test]
fn bundle_export_rejects_special_alias_colliding_and_unbounded_trees() {
    for case in 0..10 {
        let temp = fixture();
        let snapshot = prepare(&temp, "snapshot", &[]);
        let digest = snapshot["snapshot_digest"].as_str().unwrap();
        successful(&materialize(&temp, digest, "work"));
        let work = temp.path().join("work");
        match case {
            0 => fs::hard_link(work.join("tracked.txt"), work.join("alias")).unwrap(),
            1 => rustix::fs::mkfifoat(rustix::fs::CWD, work.join("fifo"), rustix::fs::Mode::RUSR)
                .unwrap(),
            2 => {
                let _socket = std::os::unix::net::UnixListener::bind(work.join("socket")).unwrap();
            }
            3 => fs::write(work.join("TRACKED.txt"), b"collision").unwrap(),
            4 => fs::create_dir_all(work.join("nested/.git")).unwrap(),
            5 => fs::write(work.join("ambiguous\\name"), b"invalid").unwrap(),
            6 => fs::write(work.join("non-ascii-é"), b"invalid").unwrap(),
            7 => fs::File::create(work.join("large"))
                .unwrap()
                .set_len(louiselm_skills::workspace::MAX_FILE_BYTES as u64 + 1)
                .unwrap(),
            8 => fs::create_dir_all(work.join("d/".repeat(65))).unwrap(),
            _ => {
                fs::create_dir(work.join("empty")).unwrap();
                fs::create_dir(work.join("EMPTY")).unwrap();
            }
        }
        let result = export_bundle(&temp, digest, "refused");
        assert!(!result.status.success(), "accepted unsafe case {case}");
        assert!(!temp.path().join("refused").exists());
    }
}

#[test]
fn bundles_cover_mode_only_changes_file_directory_replacement_and_empty_results() {
    let temp = fixture();
    let snapshot = prepare(&temp, "snapshot", &[]);
    let digest = snapshot["snapshot_digest"].as_str().unwrap();
    successful(&materialize(&temp, digest, "work"));
    let work = temp.path().join("work");
    // A hostile root Git link is not followed, inventoried, or consulted.
    fs::remove_dir_all(work.join(".git")).unwrap();
    symlink("/missing/hostile-git", work.join(".git")).unwrap();
    fs::set_permissions(work.join("tracked.txt"), fs::Permissions::from_mode(0o700)).unwrap();
    fs::remove_file(work.join("deleted.txt")).unwrap();
    fs::create_dir(work.join("deleted.txt")).unwrap();
    fs::write(work.join("deleted.txt/nested"), b"file became directory").unwrap();
    let receipt = successful(&export_bundle(&temp, digest, "bundle"));
    successful(&apply_bundle(
        &temp,
        digest,
        receipt["bundle_digest"].as_str().unwrap(),
        "integration",
    ));
    assert_eq!(
        fs::metadata(temp.path().join("integration/tracked.txt"))
            .unwrap()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        fs::read(temp.path().join("integration/deleted.txt/nested")).unwrap(),
        b"file became directory"
    );
    assert_eq!(receipt["modified"], serde_json::json!(["tracked.txt"]));
    // Empty directories do not enter the normalized regular-file contract.
    fs::remove_file(work.join("tracked.txt")).unwrap();
    fs::remove_file(work.join(".gitignore")).unwrap();
    fs::remove_file(work.join("deleted.txt/nested")).unwrap();
    let empty = successful(&export_bundle(&temp, digest, "empty-bundle"));
    assert_eq!(empty["file_count"], 0);
    let applied = run(&[
        "apply",
        "--snapshot",
        temp.path().join("snapshot").to_str().unwrap(),
        "--digest",
        digest,
        "--bundle",
        temp.path().join("empty-bundle").to_str().unwrap(),
        "--bundle-digest",
        empty["bundle_digest"].as_str().unwrap(),
        "--output",
        temp.path().join("empty-tree").to_str().unwrap(),
        "--robot-json",
    ]);
    successful(&applied);
    assert_eq!(
        fs::read_dir(temp.path().join("empty-tree"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn forged_bundle_records_and_payloads_are_refused_without_partial_output() {
    let temp = fixture();
    let snapshot = prepare(&temp, "snapshot", &[]);
    let digest = snapshot["snapshot_digest"].as_str().unwrap();
    successful(&materialize(&temp, digest, "work"));
    fs::write(temp.path().join("work/tracked.txt"), b"changed payload").unwrap();
    let receipt = successful(&export_bundle(&temp, digest, "bundle"));
    let bundle_digest = receipt["bundle_digest"].as_str().unwrap();
    let record = temp.path().join("bundle/bundle.json");
    let original = fs::read_to_string(&record).unwrap();
    let parsed: Value = serde_json::from_str(&original).unwrap();
    fs::set_permissions(&record, fs::Permissions::from_mode(0o600)).unwrap();
    let forged_records = [
        original.replace(".gitignore", "../outside"),
        original.replace(".gitignore", ".git/config"),
        original.replace(".gitignore", "TRACKED.txt"),
        original.replacen(
            &format!("\"size\":{}", parsed["files"][0]["size"]),
            "\"size\":18446744073709551615",
            1,
        ),
        original.replacen("\"executable\":false", "\"executable\":true", 1),
        original.replacen("\"files\":[{", "\"files\":[{\"unknown\":true,", 1),
        original.replacen("\"schema\":", "\"unknown\":true,\"schema\":", 1),
        original.replace(
            snapshot["base_digest"].as_str().unwrap(),
            &format!("sha256:{}", "0".repeat(64)),
        ),
        format!("{original}\n"),
    ];
    for (index, forged) in forged_records.iter().enumerate() {
        assert_ne!(forged, &original, "ineffective forged fixture {index}");
        fs::write(&record, forged).unwrap();
        let expected = louiselm_skills::Digest::of(forged.as_bytes()).to_string();
        let destination = format!("forged-bundle-{index}");
        assert!(
            !apply_bundle(&temp, digest, &expected, &destination)
                .status
                .success(),
            "accepted forged case {index}"
        );
        assert!(!temp.path().join(destination).exists());
    }
    fs::write(&record, &original).unwrap();
    let payload = temp.path().join("bundle/files/tracked.txt");
    fs::set_permissions(&payload, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(&payload, b"tampered").unwrap();
    assert!(
        !apply_bundle(&temp, digest, bundle_digest, "bad-payload")
            .status
            .success()
    );
    assert!(!temp.path().join("bad-payload").exists());
    fs::remove_file(&payload).unwrap();
    symlink("/etc/passwd", &payload).unwrap();
    assert!(
        !apply_bundle(&temp, digest, bundle_digest, "linked-payload")
            .status
            .success()
    );
    assert!(!temp.path().join("linked-payload").exists());
    fs::remove_file(&payload).unwrap();
    fs::write(&payload, b"changed payload").unwrap();
    fs::create_dir(temp.path().join("existing-tree")).unwrap();
    fs::write(temp.path().join("existing-tree/keep"), b"untouched").unwrap();
    assert!(
        !apply_bundle(&temp, digest, bundle_digest, "existing-tree")
            .status
            .success()
    );
    assert_eq!(
        fs::read(temp.path().join("existing-tree/keep")).unwrap(),
        b"untouched"
    );
    assert!(
        !export_bundle(&temp, digest, "work/nested-output")
            .status
            .success()
    );
    assert!(
        !apply_bundle(&temp, digest, bundle_digest, "bundle/nested-output")
            .status
            .success()
    );
    assert!(!fs::read_dir(temp.path()).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".workspace-")
    }));
}

#[test]
fn selected_bytes_survive_checkout_mutation_and_get_independent_git_metadata() {
    let temp = fixture();
    let repo = temp.path().join("repo");
    fs::write(repo.join("tracked.txt"), "selected dirty\n").unwrap();
    fs::remove_file(repo.join("deleted.txt")).unwrap();
    fs::write(repo.join("new.sh"), "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(repo.join("new.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(repo.join("unselected.txt"), "not included\n").unwrap();
    fs::write(repo.join(".env"), "PRIVATE_CONTENT_MARKER\n").unwrap();
    let preview = prepare(&temp, "snapshot", &["tracked.txt", "deleted.txt", "new.sh"]);
    assert_eq!(preview["schema"], "louiselm.workspace.preview/1");
    let changes = preview["changes"].as_array().unwrap();
    for (path, included, kind) in [
        ("tracked.txt", true, "modified"),
        ("deleted.txt", true, "deleted"),
        ("new.sh", true, "untracked"),
        ("unselected.txt", false, "untracked"),
        (".env", false, "ignored"),
    ] {
        assert!(
            changes
                .iter()
                .any(|c| c["path"] == path && c["included"] == included && c["kind"] == kind),
            "{preview}"
        );
    }
    assert!(!preview.to_string().contains("PRIVATE_CONTENT_MARKER"));
    let again = prepare(&temp, "again", &["new.sh", "deleted.txt", "tracked.txt"]);
    assert_eq!(preview["snapshot_digest"], again["snapshot_digest"]);
    fs::write(repo.join("tracked.txt"), "later unrelated change\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "--quiet", "-m", "later"]);
    let receipt = successful(&materialize(
        &temp,
        preview["snapshot_digest"].as_str().unwrap(),
        "work",
    ));
    assert_eq!(receipt["base_digest"], preview["base_digest"]);
    let work = temp.path().join("work");
    assert_eq!(
        fs::read_to_string(work.join("tracked.txt")).unwrap(),
        "selected dirty\n"
    );
    assert!(!work.join("deleted.txt").exists());
    assert!(!work.join("unselected.txt").exists());
    assert!(!work.join(".env").exists());
    assert_eq!(
        fs::metadata(work.join("new.sh")).unwrap().mode() & 0o777,
        0o700
    );
    assert!(work.join(".git").is_dir());
    assert!(!work.join(".git/objects/info/alternates").exists());
    assert!(git(&work, &["status", "--porcelain"]).stdout.is_empty());
    assert_ne!(
        fs::metadata(work.join("tracked.txt")).unwrap().ino(),
        fs::metadata(temp.path().join("snapshot/files/tracked.txt"))
            .unwrap()
            .ino()
    );
    fs::write(work.join("tracked.txt"), "private edit\n").unwrap();
    assert_eq!(
        fs::read_to_string(repo.join("tracked.txt")).unwrap(),
        "later unrelated change\n"
    );
    assert_eq!(
        fs::read_to_string(temp.path().join("snapshot/files/tracked.txt")).unwrap(),
        "selected dirty\n"
    );
}

#[test]
fn default_snapshot_uses_head_even_when_the_index_and_worktree_are_dirty() {
    let temp = fixture();
    let repo = temp.path().join("repo");
    fs::write(repo.join("tracked.txt"), "staged\n").unwrap();
    fs::write(repo.join("staged.txt"), "new in index\n").unwrap();
    git(&repo, &["add", "."]);
    fs::write(repo.join("tracked.txt"), "unstaged\n").unwrap();
    let preview = prepare(&temp, "snapshot", &[]);
    successful(&materialize(
        &temp,
        preview["snapshot_digest"].as_str().unwrap(),
        "work",
    ));
    assert_eq!(
        fs::read_to_string(temp.path().join("work/tracked.txt")).unwrap(),
        "committed\n"
    );
    assert!(!temp.path().join("work/staged.txt").exists());
    assert!(
        preview["changes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["path"] == "staged.txt" && c["included"] == false)
    );
}

#[test]
fn tampering_wrong_digests_and_existing_destinations_never_publish() {
    let temp = fixture();
    let preview = prepare(&temp, "snapshot", &[]);
    let digest = preview["snapshot_digest"].as_str().unwrap();
    assert!(
        !materialize(&temp, &format!("sha256:{}", "0".repeat(64)), "wrong")
            .status
            .success()
    );
    assert!(!temp.path().join("wrong").exists());
    fs::create_dir(temp.path().join("existing")).unwrap();
    fs::write(temp.path().join("existing/keep"), "keep").unwrap();
    assert!(!materialize(&temp, digest, "existing").status.success());
    assert_eq!(
        fs::read_to_string(temp.path().join("existing/keep")).unwrap(),
        "keep"
    );
    let file = temp.path().join("snapshot/files/tracked.txt");
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(file, "substituted\n").unwrap();
    assert!(!materialize(&temp, digest, "tampered").status.success());
    assert!(!temp.path().join("tampered").exists());
}

#[test]
fn links_special_files_and_git_metadata_are_not_source_inputs() {
    let temp = fixture();
    let repo = temp.path().join("repo");
    symlink("/etc", repo.join("escape")).unwrap();
    rustix::fs::mkfifoat(rustix::fs::CWD, repo.join("fifo"), rustix::fs::Mode::RUSR).unwrap();
    for (index, path) in [
        "../escape",
        ".git/config",
        "escape/passwd",
        "fifo",
        "missing",
    ]
    .iter()
    .enumerate()
    {
        let destination = temp.path().join(format!("refused-{index}"));
        let output = run(&[
            "prepare",
            "--repository",
            repo.to_str().unwrap(),
            "--output",
            destination.to_str().unwrap(),
            "--include",
            path,
            "--robot-json",
        ]);
        assert!(!output.status.success(), "accepted {path}");
        assert!(!destination.exists());
    }
}

#[test]
fn source_hooks_filters_and_fsmonitor_cannot_execute_and_binary_bytes_stay_raw() {
    let temp = fixture();
    let repo = temp.path().join("repo");
    fs::write(repo.join(".gitattributes"), "*.txt filter=probe\n").unwrap();
    fs::write(repo.join("binary.dat"), [0, 255, 13, 10, 0]).unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "--quiet", "-m", "raw source"]);
    let hook = temp.path().join("probe");
    fs::write(
        &hook,
        format!(
            "#!/bin/sh\n/usr/bin/touch '{}'\nexit 1\n",
            temp.path().join("EXECUTED").display()
        ),
    )
    .unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o700)).unwrap();
    let hooks = temp.path().join("hooks");
    fs::create_dir(&hooks).unwrap();
    for name in ["pre-commit", "reference-transaction", "post-checkout"] {
        fs::copy(&hook, hooks.join(name)).unwrap();
    }
    for key in [
        "filter.probe.clean",
        "filter.probe.smudge",
        "core.fsmonitor",
    ] {
        git(&repo, &["config", key, hook.to_str().unwrap()]);
    }
    git(
        &repo,
        &["config", "core.hooksPath", hooks.to_str().unwrap()],
    );
    git(&repo, &["config", "filter.probe.required", "true"]);
    let original_config = fs::read(repo.join(".git/config")).unwrap();
    let original_index = fs::read(repo.join(".git/index")).unwrap();
    let preview = prepare(&temp, "snapshot", &[]);
    successful(&materialize(
        &temp,
        preview["snapshot_digest"].as_str().unwrap(),
        "work",
    ));
    assert!(!temp.path().join("EXECUTED").exists());
    assert_eq!(fs::read(repo.join(".git/config")).unwrap(), original_config);
    assert_eq!(fs::read(repo.join(".git/index")).unwrap(), original_index);
    assert_eq!(
        fs::read(temp.path().join("work/binary.dat")).unwrap(),
        [0, 255, 13, 10, 0]
    );
    assert!(
        !fs::read_to_string(temp.path().join("work/.git/config"))
            .unwrap()
            .contains("probe")
    );
    assert!(
        git(&temp.path().join("work"), &["remote"])
            .stdout
            .is_empty()
    );
}

#[test]
fn ignored_files_require_exact_selection_and_unselected_links_keep_committed_bytes() {
    let temp = fixture();
    let repo = temp.path().join("repo");
    fs::write(repo.join(".env"), "explicit selection\n").unwrap();
    fs::create_dir(repo.join("cache")).unwrap();
    fs::write(repo.join("cache/selected"), "selected cache file\n").unwrap();
    fs::write(repo.join("cache/secret"), "never included\n").unwrap();
    fs::remove_file(repo.join("tracked.txt")).unwrap();
    symlink("/etc/passwd", repo.join("tracked.txt")).unwrap();
    let preview = prepare(&temp, "snapshot", &[".env", "cache/selected"]);
    successful(&materialize(
        &temp,
        preview["snapshot_digest"].as_str().unwrap(),
        "work",
    ));
    assert_eq!(
        fs::read_to_string(temp.path().join("work/tracked.txt")).unwrap(),
        "committed\n"
    );
    assert_eq!(
        fs::read_to_string(temp.path().join("work/.env")).unwrap(),
        "explicit selection\n"
    );
    assert!(!temp.path().join("work/cache/secret").exists());
    assert!(
        preview["changes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["path"] == "cache/selected"
                && c["kind"] == "ignored"
                && c["included"] == true)
    );
}

#[test]
fn closed_records_reject_forged_paths_collisions_modes_and_limits_even_with_matching_digest() {
    let temp = fixture();
    prepare(&temp, "snapshot", &[]);
    let record_path = temp.path().join("snapshot/snapshot.json");
    fs::set_permissions(&record_path, fs::Permissions::from_mode(0o600)).unwrap();
    let original = fs::read_to_string(&record_path).unwrap();
    let parsed: Value = serde_json::from_str(&original).unwrap();
    for (index, field, value) in [
        (0, "path", Value::from("../outside")),
        (1, "path", Value::from(".git/config")),
        (2, "path", Value::from("TRACKED.txt")),
        (3, "size", Value::from(u64::MAX)),
        (4, "sha256", Value::from("bad")),
        (5, "executable", Value::from(true)),
        (6, "unknown", Value::from("rejected")),
    ] {
        // Preserve canonical field order so refusal exercises the altered
        // contract rather than merely JSON map reordering.
        let fragment = format!("\"{field}\":{}", parsed["files"][0][field]);
        let replacement = format!("\"{field}\":{value}");
        let forged = if field == "unknown" {
            original.replacen("\"files\":[{", "\"files\":[{\"unknown\":true,", 1)
        } else {
            assert!(original.contains(&fragment));
            original.replacen(&fragment, &replacement, 1)
        };
        let bytes = forged.as_bytes();
        fs::write(&record_path, bytes).unwrap();
        let digest = louiselm_skills::Digest::of(bytes).to_string();
        let destination = format!("forged-{index}");
        assert!(!materialize(&temp, &digest, &destination).status.success());
        assert!(!temp.path().join(destination).exists());
    }
}

#[test]
fn snapshot_aliases_and_oversized_selected_files_are_refused() {
    let temp = fixture();
    let repo = temp.path().join("repo");
    let large = fs::File::create(repo.join("large")).unwrap();
    large
        .set_len(louiselm_skills::workspace::MAX_FILE_BYTES as u64 + 1)
        .unwrap();
    let output = run(&[
        "prepare",
        "--repository",
        repo.to_str().unwrap(),
        "--output",
        temp.path().join("large-snapshot").to_str().unwrap(),
        "--include",
        "large",
    ]);
    assert!(!output.status.success());
    assert!(!temp.path().join("large-snapshot").exists());
    let preview = prepare(&temp, "snapshot", &[]);
    let file = temp.path().join("snapshot/files/tracked.txt");
    fs::hard_link(&file, temp.path().join("alias")).unwrap();
    assert!(
        !materialize(
            &temp,
            preview["snapshot_digest"].as_str().unwrap(),
            "aliased"
        )
        .status
        .success()
    );
    assert!(!temp.path().join("aliased").exists());
    fs::remove_file(&file).unwrap();
    symlink(repo.join("tracked.txt"), &file).unwrap();
    assert!(
        !materialize(
            &temp,
            preview["snapshot_digest"].as_str().unwrap(),
            "linked"
        )
        .status
        .success()
    );
    assert!(!temp.path().join("linked").exists());
}

#[test]
fn malformed_commands_never_echo_untrusted_values() {
    for args in [
        vec!["PRIVATE_MARKER"],
        vec!["prepare", "--PRIVATE_MARKER"],
        vec![
            "materialize",
            "--snapshot",
            "PRIVATE_MARKER",
            "--digest",
            "PRIVATE_MARKER",
            "--output",
            "PRIVATE_MARKER",
        ],
        vec![
            "prepare",
            "--repository",
            "PRIVATE_MARKER",
            "--repository",
            "PRIVATE_MARKER",
        ],
        vec!["prepare", "--robot-json", "--robot-json"],
    ] {
        let output = run(&args);
        assert_eq!(output.status.code(), Some(1));
        assert!(!String::from_utf8_lossy(&output.stderr).contains("PRIVATE_MARKER"));
        assert!(output.stdout.is_empty());
    }
}
