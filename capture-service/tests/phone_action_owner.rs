//! Durable owner admission versus no-effect fencing.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "test fixtures and assertions"
)]

use std::{
    fs,
    io::Write,
    process::{Child, Command, Stdio},
    sync::{Arc, Barrier},
    thread,
};

use louiselm_capture::ActionIdentity;

#[path = "support/phone_evidence.rs"]
#[allow(
    dead_code,
    reason = "other evidence fixtures share this support module"
)]
mod phone_evidence;

fn initialize(root: &std::path::Path) -> String {
    let tls = root.join("louiselm/capture/tls");
    let identity = louiselm_capture::TlsIdentity::load_or_create(&tls).unwrap();
    let receiver = identity.public_key_sha256().to_owned();
    phone_evidence::initialize(
        &root.join("louiselm/workflow/phone-action-owner"),
        &tls.join("phone-action-owner.json"),
        &receiver,
    );
    receiver
}

fn identity(project: &std::path::Path, id: u8) -> ActionIdentity {
    ActionIdentity {
        operation_id: format!("{id:02x}").repeat(16),
        receiver_identity: louiselm_capture::TlsIdentity::load_or_create(
            project.join("louiselm/capture/tls"),
        )
        .unwrap()
        .public_key_sha256()
        .to_owned(),
        controller_id: "03".repeat(16),
        subject_id: "04".repeat(16),
        action: 3,
        payload_hash: "05".repeat(32),
        grant_id: "06".repeat(16),
        grant_revision: 1,
        period_id: "07".repeat(16),
        expected_revision: 2,
        project: fs::canonicalize(project).unwrap().to_str().unwrap().into(),
        agent: "codex".into(),
        recorded_at_ms: 1_000,
    }
}

fn spawn(root: &std::path::Path) -> Child {
    Command::new(env!("CARGO_BIN_EXE_louiselm-capture"))
        .args(["phone-control", "owner-journal"])
        .env_clear()
        .env("LOUISELM_CAPTURE_CONFIG_DIR", root)
        .env("LOUISELM_CAPTURE_DATA_DIR", root)
        .env("LOUISELM_CAPTURE_STATE_DIR", root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap()
}

fn send(mut child: Child, message: &serde_json::Value) -> serde_json::Value {
    child
        .stdin
        .take()
        .unwrap()
        .write_all(message.to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    serde_json::from_slice(&output.stdout).unwrap()
}

fn invoke(root: &std::path::Path, message: &serde_json::Value) -> serde_json::Value {
    send(spawn(root), message)
}

#[test]
fn losing_journal_and_lock_never_reopens_the_same_action() {
    let temp = tempfile::tempdir().unwrap();
    initialize(temp.path());
    let request = identity(temp.path(), 9);
    let begin = serde_json::json!({"v":1,"type":"begin","identity":request});
    assert_eq!(invoke(temp.path(), &begin)["type"], "begun");

    let owner = temp.path().join("louiselm/workflow/phone-action-owner");
    fs::remove_dir_all(&owner).unwrap();
    fs::remove_file(owner.with_extension("lock")).unwrap();

    let retry = invoke(temp.path(), &begin);
    assert_eq!(retry["type"], "error");
    assert_eq!(retry["code"], "unavailable");
}

#[test]
fn owner_commands_refuse_a_missing_or_corrupt_receiver_guard() {
    let temp = tempfile::tempdir().unwrap();
    initialize(temp.path());
    let request = identity(temp.path(), 10);
    let guard = temp
        .path()
        .join("louiselm/capture/tls/phone-action-owner.json");
    let messages = [
        serde_json::json!({"v":1,"type":"begin","identity":request}),
        serde_json::json!({"v":1,"type":"inspect","identity":request}),
        serde_json::json!({"v":1,"type":"fence","identity":request,"at_ms":2000}),
        serde_json::json!({"v":1,"type":"report","identity":request,"outcome":"unknown","at_ms":2000}),
    ];
    fs::remove_file(&guard).unwrap();
    for message in &messages {
        assert_eq!(invoke(temp.path(), message)["code"], "unavailable");
    }
    fs::write(&guard, b"{}").unwrap();
    for message in &messages {
        assert_eq!(invoke(temp.path(), message)["code"], "unavailable");
    }
}

#[test]
fn local_owner_command_persists_a_fence_across_processes() {
    let temp = tempfile::tempdir().unwrap();
    initialize(temp.path());
    let request = identity(temp.path(), 5);
    let fenced = invoke(
        temp.path(),
        &serde_json::json!({"v":1,"type":"fence","identity":request,"at_ms":2000}),
    );
    assert_eq!(fenced["type"], "fenced");
    let late = invoke(
        temp.path(),
        &serde_json::json!({"v":1,"type":"begin","identity":request}),
    );
    assert_eq!(late["type"], "existing");
    assert_eq!(
        late["outcome"]["fenced_no_effect"]["evidence"],
        fenced["evidence"]
    );
}

#[test]
fn inspection_preserves_missing_and_owner_proved_outcomes_across_processes() {
    let temp = tempfile::tempdir().unwrap();
    initialize(temp.path());
    let request = identity(temp.path(), 6);
    let inspect = |request: &ActionIdentity| {
        invoke(
            temp.path(),
            &serde_json::json!({"v":1,"type":"inspect","identity":request}),
        )
    };
    assert_eq!(inspect(&request)["code"], "unavailable");
    let begin = serde_json::json!({"v":1,"type":"begin","identity":request});
    assert_eq!(invoke(temp.path(), &begin)["type"], "begun");
    assert_eq!(inspect(&request)["outcome"], "unknown");

    let mut altered = request.clone();
    altered.controller_id = "0a".repeat(16);
    assert_eq!(inspect(&altered)["code"], "conflict");
    let fence = serde_json::json!({"fenced_no_effect":{"evidence":"0b".repeat(32)}});
    let report =
        serde_json::json!({"v":1,"type":"report","identity":request,"outcome":fence,"at_ms":2000});
    assert_eq!(invoke(temp.path(), &report)["type"], "recorded");
    assert_eq!(inspect(&request)["outcome"], fence);
    assert_eq!(invoke(temp.path(), &begin)["outcome"], fence);
}

#[test]
fn separate_helpers_cannot_both_begin_and_fence() {
    let temp = tempfile::tempdir().unwrap();
    initialize(temp.path());
    let seed = identity(temp.path(), 7);
    assert_eq!(
        invoke(
            temp.path(),
            &serde_json::json!({"v":1,"type":"begin","identity":seed})
        )["type"],
        "begun"
    );
    let request = identity(temp.path(), 8);
    let begin = serde_json::json!({"v":1,"type":"begin","identity":request});
    let fence = serde_json::json!({"v":1,"type":"fence","identity":request,"at_ms":2000});
    let barrier = Arc::new(Barrier::new(2));
    let children = [spawn(temp.path()), spawn(temp.path())];
    let handles: Vec<_> = children
        .into_iter()
        .zip([begin.clone(), fence])
        .map(|(child, message)| {
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                send(child, &message)
            })
        })
        .collect();
    let results: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    let won_begin = results[0]["type"] == "begun";
    let fenced = results[1]["type"] == "fenced";
    assert_ne!(
        won_begin, fenced,
        "exactly one helper must win: {results:?}"
    );
    let retained = invoke(temp.path(), &begin);
    assert_eq!(retained["type"], "existing");
    if fenced {
        assert_eq!(
            retained["outcome"]["fenced_no_effect"]["evidence"],
            results[1]["evidence"]
        );
    } else {
        assert_eq!(retained["outcome"], "unknown");
        assert_eq!(
            invoke(
                temp.path(),
                &serde_json::json!({"v":1,"type":"fence","identity":request,"at_ms":3000})
            )["type"],
            "uncertain"
        );
    }
}
