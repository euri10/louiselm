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
    android.complete("restart");
    android.finish();
    second.stop();
    assert_eq!(
        completed_owner_evidence(&desktop, &phone, operation),
        native
    );
}
