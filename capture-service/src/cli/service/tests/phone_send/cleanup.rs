//! Completed Send cleanup through the real desktop and disposable Android client.

use super::*;

#[test]
#[ignore = "requires the Android SDK, Gradle, stock Neovim, and built capture executable"]
fn completed_send_cleanup_survives_restart_without_replaying() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let desktop = Desktop {
        paths: paths(root.path()),
        project: fs::canonicalize(concat!(env!("CARGO_MANIFEST_DIR"), "/..")).unwrap(),
        binary: PathBuf::from(env::var_os("LOUISELM_TEST_CAPTURE").unwrap()),
    };
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!(
        "https://localhost:{}",
        listener.local_addr().unwrap().port()
    );
    let first = desktop.start_receiver(&listener, &root.path().join("receiver-one"), false);
    let phone = desktop.enroll(&url);
    let editor = Editor::start(&desktop, &root.path().join("editor"), FIRST_TEXT);
    let mut android = phone.start_android(
        &desktop,
        root.path(),
        &editor.targets,
        "dev.louiselm.capture.PhoneCleanupEndToEndTest",
    );
    android.wait_for("cleanup");
    let operation = fs::read_to_string(root.path().join("first-operation")).unwrap();
    let operation = operation.trim();
    let native = completed_owner_evidence(&desktop, &phone, operation);
    for _ in 0..2 {
        let result = desktop.admin(&["remove-action-details", "--operation-id", operation]);
        assert_eq!(result["type"], "action_details_removed");
        assert_eq!(result["operation_id"], operation);
    }
    assert_eq!(
        completed_owner_evidence(&desktop, &phone, operation),
        native
    );
    android.complete("cleanup");
    android.wait_for("restart");
    editor.finish(1); // The original and its repeat produced exactly one Agent message.
    first.stop();
    let second = desktop.start_receiver(&listener, &root.path().join("receiver-two"), false);
    let restarted_editor = Editor::start(&desktop, &root.path().join("editor-two"), FIRST_TEXT);
    android.complete("restart");
    android.finish();
    restarted_editor.finish(0);
    second.stop();
    assert_eq!(
        completed_owner_evidence(&desktop, &phone, operation),
        native
    );
}

#[test]
#[ignore = "requires the Android SDK, Gradle, stock Neovim, and built capture executable"]
#[expect(
    clippy::too_many_lines,
    reason = "one Send is followed through both restarts"
)]
fn owner_fenced_send_cleanup_survives_restart_without_replaying() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let desktop = Desktop {
        paths: paths(root.path()),
        project: fs::canonicalize(concat!(env!("CARGO_MANIFEST_DIR"), "/..")).unwrap(),
        binary: PathBuf::from(env::var_os("LOUISELM_TEST_CAPTURE").unwrap()),
    };
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!(
        "https://localhost:{}",
        listener.local_addr().unwrap().port()
    );
    let first = desktop.start_receiver(&listener, &root.path().join("receiver-one"), false);
    let phone = desktop.enroll(&url);
    let mut editor = Editor::start_inner(
        &desktop,
        &root.path().join("editor"),
        FIRST_TEXT,
        None,
        false,
        true,
    );
    let mut android = phone.start_android_inner(
        &desktop,
        root.path(),
        &editor.targets,
        "dev.louiselm.capture.PhoneCleanupEndToEndTest",
        true,
    );
    wait_for_file(
        &desktop.paths.state.join("fence-begun"),
        &mut android.child,
        Duration::from_secs(60),
    );
    editor.desktop_turn("fence-desktop");
    fs::write(desktop.paths.state.join("fence-release"), []).unwrap();
    android.wait_for("cleanup");
    let operation = fs::read_to_string(root.path().join("first-operation")).unwrap();
    let evidence = fs::read_to_string(root.path().join("fence-evidence")).unwrap();
    let owner = ActionEvidenceStore::open_bound(
        desktop
            .paths
            .state
            .join("louiselm/workflow/phone-action-owner"),
        desktop.paths.tls().join("phone-action-owner.json"),
        &phone.identity,
        ActionEvidenceKind::Owner,
    )
    .unwrap();
    let native = owner.lookup(operation.trim()).unwrap();
    assert_eq!(
        native.outcome,
        ActionOutcome::FencedNoEffect {
            evidence: evidence.clone()
        }
    );
    assert_eq!(native.phases, Vec::<ActionOutcome>::new());
    drop(owner);
    let archive: Value = serde_json::from_slice(
        &fs::read(
            desktop
                .paths
                .state
                .join("louiselm/capture/phone-control/action-evidence/actions.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let record = &archive["actions"][operation.trim()];
    assert_eq!(record["outcome"]["fenced_no_effect"]["evidence"], evidence);
    let mut archived_identity =
        serde_json::from_value::<crate::phone_control::ActionIdentity>(record["identity"].clone())
            .unwrap();
    archived_identity.recorded_at_ms = native.identity.recorded_at_ms;
    assert_eq!(archived_identity, native.identity);
    eprintln!(
        "positive proof: real Session fenced the exact Send; native journal and receiver archive agree"
    );
    let owner_path = desktop
        .paths
        .state
        .join("louiselm/workflow/phone-action-owner");
    let held_owner = desktop.paths.state.join("louiselm/workflow/held-owner");
    fs::rename(&owner_path, &held_owner).unwrap();
    let refusal = desktop
        .isolated(&desktop.binary)
        .args([
            "phone-control",
            "remove-action-details",
            "--operation-id",
            operation.trim(),
        ])
        .output()
        .unwrap();
    fs::rename(&held_owner, &owner_path).unwrap();
    assert!(!refusal.status.success());
    assert!(String::from_utf8_lossy(&refusal.stderr).contains("unavailable"));
    for _ in 0..2 {
        assert_eq!(
            desktop.admin(&["remove-action-details", "--operation-id", operation.trim()])["type"],
            "action_details_removed"
        );
    }
    android.complete("cleanup");
    android.wait_for("restart");
    editor.finish(1); // Exactly the desktop turn; the phone Send never prompted the Agent.
    first.stop();
    let second = desktop.start_receiver(&listener, &root.path().join("receiver-two"), false);
    let restarted_editor = Editor::start(&desktop, &root.path().join("editor-two"), FIRST_TEXT);
    let generation: Value = serde_json::from_slice(
        &fs::read(desktop.paths.tls().join("phone-action-owner.json")).unwrap(),
    )
    .unwrap();
    let mut child = desktop
        .isolated(&desktop.binary)
        .args(["phone-control", "owner-journal"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let command = json!({"v":2, "type":"begin", "generation":generation["generation"], "identity":native.identity});
    writeln!(child.stdin.take().unwrap(), "{command}").unwrap();
    let reply = child.wait_with_output().unwrap();
    assert!(reply.status.success());
    let reply: Value = serde_json::from_slice(&reply.stdout).unwrap();
    assert_eq!(reply["type"], "existing");
    assert_eq!(reply["outcome"]["fenced_no_effect"]["evidence"], evidence);
    android.complete("restart");
    android.finish();
    restarted_editor.finish(0);
    second.stop();
    let owner = ActionEvidenceStore::open_bound(
        desktop
            .paths
            .state
            .join("louiselm/workflow/phone-action-owner"),
        desktop.paths.tls().join("phone-action-owner.json"),
        &phone.identity,
        ActionEvidenceKind::Owner,
    )
    .unwrap();
    assert_eq!(owner.lookup(operation.trim()).unwrap(), native);
}
