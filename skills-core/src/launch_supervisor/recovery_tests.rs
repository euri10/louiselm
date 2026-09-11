//! Deterministic retention evidence; these fixtures do not prove vendor support.
#![allow(
    clippy::unwrap_used,
    reason = "Tests assert disposable fixture setup and outcomes."
)]

use super::*;
use crate::{
    Digest,
    launch::{PROTOCOL_VERSION, REQUEST_SCHEMA},
};
use std::{fs, os::unix::fs::PermissionsExt};

fn fixture() -> tempfile::TempDir {
    let fixture = tempfile::tempdir().unwrap();
    fs::create_dir(fixture.path().join("home")).unwrap();
    fs::create_dir(fixture.path().join("workspace")).unwrap();
    fs::write(
        fixture.path().join(CHECKPOINT),
        br#"{"schema":"louiselm.test-recovery/1","acp_session_id":"conversation","counter":7}"#,
    )
    .unwrap();
    fs::write(fixture.path().join(WORKSPACE), b"7").unwrap();
    fixture
}

fn operation() -> RetentionRequest {
    RetentionRequest {
        request_id: "retain".into(),
        acp_session_id: "conversation".into(),
        expires_at_ms: 200,
    }
}

fn capture(
    fixture: &tempfile::TempDir,
    request: &RetentionRequest,
    now: u64,
) -> Result<RetentionEvidence, RecoveryError> {
    retain(
        &File::open(fixture.path())?,
        fixture.path(),
        &launch(),
        &Digest::of(b"fixture").to_string(),
        request,
        now,
    )
}

fn launch() -> LaunchRequest {
    LaunchRequest {
        schema: REQUEST_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        request_id: "launch".into(),
        authorization_id: "authorization".into(),
        session_id: "session".into(),
        run_id: "run".into(),
        agent_id: "test-agent".into(),
        envelope_id: "empty".into(),
        envelope_revision: 1,
        skill_generation_id: Digest::of(b"generation").to_string(),
        session_input_manifest_id: Digest::of(b"input").to_string(),
    }
}

#[test]
fn retained_checkpoint_survives_source_loss_without_home_secrets() {
    let fixture = tempfile::tempdir().unwrap();
    fs::create_dir(fixture.path().join("home")).unwrap();
    fs::create_dir(fixture.path().join("workspace")).unwrap();
    fs::write(
        fixture.path().join("home/recovery.json"),
        br#"{"schema":"louiselm.test-recovery/1","acp_session_id":"conversation","counter":7}"#,
    )
    .unwrap();
    fs::write(fixture.path().join("workspace/recovery-counter.json"), b"7").unwrap();
    fs::write(
        fixture.path().join("home/credentials"),
        b"never-retain-this",
    )
    .unwrap();
    let root = File::open(fixture.path()).unwrap();
    let request = launch();
    let operation = RetentionRequest {
        request_id: "retain".into(),
        acp_session_id: "conversation".into(),
        expires_at_ms: 200,
    };
    let integration = Digest::of(b"fixture-integration").to_string();
    let retained = retain(
        &root,
        fixture.path(),
        &request,
        &integration,
        &operation,
        100,
    )
    .unwrap();
    fs::remove_file(fixture.path().join("home/recovery.json")).unwrap();
    fs::remove_file(fixture.path().join("workspace/recovery-counter.json")).unwrap();
    assert_eq!(
        retain(
            &root,
            fixture.path(),
            &request,
            &integration,
            &operation,
            101
        )
        .unwrap(),
        retained
    );
    assert!(
        !fixture
            .path()
            .join("retained-recovery/home/credentials")
            .exists()
    );
    assert_eq!(
        read(
            &File::open(fixture.path().join("retained-recovery")).unwrap(),
            WORKSPACE
        )
        .unwrap(),
        b"7"
    );
    assert_eq!(
        fs::metadata(fixture.path().join("retained-recovery"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
}

#[test]
fn retry_cannot_renew_expiry_replace_binding_or_ignore_corruption() {
    let fixture = fixture();
    let request = operation();
    let evidence = capture(&fixture, &request, 100).unwrap();
    assert_eq!(capture(&fixture, &request, 199).unwrap(), evidence);
    assert!(matches!(
        capture(&fixture, &request, 200),
        Err(RecoveryError::Expired)
    ));
    for changed in [
        RetentionRequest {
            expires_at_ms: 300,
            ..request.clone()
        },
        RetentionRequest {
            request_id: "another".into(),
            ..request.clone()
        },
        RetentionRequest {
            acp_session_id: "another".into(),
            ..request.clone()
        },
    ] {
        assert!(matches!(
            capture(&fixture, &changed, 101),
            Err(RecoveryError::Conflict)
        ));
    }
    let mut another_launch = launch();
    another_launch.authorization_id = "different-authorization".into();
    assert!(matches!(
        retain(
            &File::open(fixture.path()).unwrap(),
            fixture.path(),
            &another_launch,
            &Digest::of(b"fixture").to_string(),
            &request,
            101
        ),
        Err(RecoveryError::Conflict)
    ));
    fs::write(fixture.path().join(DIRECTORY).join(WORKSPACE), b"8").unwrap();
    assert!(matches!(
        capture(&fixture, &request, 101),
        Err(RecoveryError::Invalid)
    ));
}

#[test]
fn unknown_layout_secret_fields_and_torn_checkpoints_are_not_recovery() {
    for bytes in [
        br#"{"schema":"vendor-guess","acp_session_id":"conversation","counter":7}"#.as_slice(),
        br#"{"schema":"louiselm.test-recovery/1","acp_session_id":"conversation","counter":7,"token":"secret"}"#,
        br#"{"schema":"louiselm.test-recovery/1","acp_session_id":"wrong","counter":7}"#,
        br#"{"schema":"louiselm.test-recovery/1","acp_session_id":"conversation","counter":8}"#,
        b"{",
    ] {
        let fixture = fixture();
        fs::write(fixture.path().join(CHECKPOINT), bytes).unwrap();
        assert!(capture(&fixture, &operation(), 100).is_err());
        assert!(!fixture.path().join(DIRECTORY).exists());
    }
    let fixture = fixture();
    fs::write(fixture.path().join(CHECKPOINT), vec![b'x'; MAX_BYTES + 1]).unwrap();
    assert!(capture(&fixture, &operation(), 100).is_err());
}

#[test]
fn links_partial_publication_and_unprotected_retained_state_fail_closed() {
    use std::os::unix::fs::symlink;
    for link in [CHECKPOINT, WORKSPACE, "home"] {
        let fixture = fixture();
        let original = fixture.path().join(link);
        let moved = fixture.path().join("untrusted-target");
        fs::rename(&original, &moved).unwrap();
        symlink(&moved, &original).unwrap();
        assert!(capture(&fixture, &operation(), 100).is_err());
        assert!(!fixture.path().join(DIRECTORY).exists());
    }
    let fixture = fixture();
    fs::hard_link(
        fixture.path().join(CHECKPOINT),
        fixture.path().join("alias"),
    )
    .unwrap();
    assert!(capture(&fixture, &operation(), 100).is_err());

    let partial = self::fixture();
    fs::create_dir(partial.path().join(DIRECTORY)).unwrap();
    fs::write(partial.path().join(DIRECTORY).join("sentinel"), b"keep").unwrap();
    assert!(capture(&partial, &operation(), 100).is_err());
    assert_eq!(
        fs::read(partial.path().join(DIRECTORY).join("sentinel")).unwrap(),
        b"keep"
    );

    let protected = self::fixture();
    capture(&protected, &operation(), 100).unwrap();
    fs::set_permissions(
        protected.path().join(DIRECTORY),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    assert!(matches!(
        capture(&protected, &operation(), 100),
        Err(RecoveryError::Invalid)
    ));
}

#[test]
fn sealing_uses_the_original_directory_handle_and_is_repeatable() {
    let fixture = fixture();
    let directory = fixture.path().join("original");
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o711)).unwrap();
    let storage = SessionStorage {
        root: File::open(&directory).unwrap(),
        directory: directory.clone(),
        launch: launch(),
        integration_digest: Digest::of(b"fixture").to_string(),
    };
    let moved = fixture.path().join("moved");
    fs::rename(&directory, &moved).unwrap();
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o711)).unwrap();
    FAIL_SEAL_SYNC.set(true);
    assert!(matches!(storage.seal(), Err(RecoveryError::Io(_))));
    storage.seal().unwrap();
    assert_eq!(fs::metadata(moved).unwrap().mode() & 0o777, 0o700);
    assert_eq!(fs::metadata(directory).unwrap().mode() & 0o777, 0o711);
}

#[test]
fn interrupted_publication_yields_no_evidence_and_retry_can_complete() {
    let fixture = fixture();
    FAIL_PUBLICATION.set(true);
    assert!(matches!(
        capture(&fixture, &operation(), 100),
        Err(RecoveryError::Storage(_))
    ));
    assert!(!fixture.path().join(DIRECTORY).exists());
    let evidence = capture(&fixture, &operation(), 101).unwrap();
    assert_eq!(evidence.request, operation());
}
