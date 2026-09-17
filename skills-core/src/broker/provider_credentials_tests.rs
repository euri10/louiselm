#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "Fixtures assert exact custody outcomes."
)]
use super::{
    CredentialHandle, FileFacts, ProviderCredentialStore, credential_file_trusted,
    credential_root_trusted,
};
use crate::launch_protocol::{ErrorCode, NextAction};

const SECRET: &str = "sk-fixture-must-never-escape-the-broker";
const OWNER: u32 = 4242;
const GROUP: u32 = 4243;

fn disk_store() -> (tempfile::TempDir, std::path::PathBuf) {
    use std::{fs, os::unix::fs::PermissionsExt};
    let state = tempfile::tempdir().unwrap();
    let root = ProviderCredentialStore::root_in(state.path());
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let path = root.join("acme");
    fs::write(&path, SECRET).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    (state, path)
}

fn open(
    state: &std::path::Path,
) -> Result<ProviderCredentialStore, crate::launch_protocol::ProtocolError> {
    ProviderCredentialStore::open(
        state,
        rustix::process::geteuid().as_raw(),
        rustix::process::getegid().as_raw(),
    )
}

#[test]
fn multiply_linked_credentials_are_unavailable() {
    let (state, path) = disk_store();
    std::fs::hard_link(path, state.path().join("outside-store")).unwrap();
    assert_eq!(
        open(state.path()).unwrap_err().code,
        ErrorCode::CredentialUnavailable
    );
}

#[test]
fn special_permission_bits_are_not_private_credential_modes() {
    use std::{fs, os::unix::fs::PermissionsExt};
    let (state, path) = disk_store();
    fs::set_permissions(path, fs::Permissions::from_mode(0o4600)).unwrap();
    assert_eq!(
        open(state.path()).unwrap_err().code,
        ErrorCode::CredentialUnavailable
    );
}

#[test]
fn installed_store_provisions_empty_and_reopens_without_changing_acls() {
    use std::{
        fs,
        os::unix::fs::{MetadataExt, PermissionsExt},
    };
    let state = tempfile::tempdir().unwrap();
    let uid = rustix::process::geteuid().as_raw();
    let gid = rustix::process::getegid().as_raw();
    let store = ProviderCredentialStore::installed(state.path(), uid, gid).unwrap();
    assert!(store.providers().is_empty());
    let root = ProviderCredentialStore::root_in(state.path());
    assert_eq!(fs::metadata(&root).unwrap().mode() & 0o7777, 0o700);
    fs::set_permissions(&root, fs::Permissions::from_mode(0o750)).unwrap();
    assert_eq!(
        ProviderCredentialStore::installed(state.path(), uid, gid)
            .unwrap_err()
            .code,
        ErrorCode::CredentialUnavailable
    );
    assert_eq!(fs::metadata(root).unwrap().mode() & 0o7777, 0o750);
}

#[test]
fn filesystem_failures_are_typed_and_never_echo_material() {
    use std::{
        fs,
        os::unix::fs::{PermissionsExt, symlink},
    };
    let missing = tempfile::tempdir().unwrap();
    assert_eq!(
        open(missing.path()).unwrap_err().code,
        ErrorCode::CredentialUnavailable
    );
    for bytes in [
        b"".as_slice(),
        b" \n\t",
        b"\xff",
        &vec![b'x'; 16 * 1024 + 1],
    ] {
        let (state, path) = disk_store();
        fs::write(path, bytes).unwrap();
        let error = open(state.path()).unwrap_err();
        assert_eq!(error.code, ErrorCode::CredentialUnavailable);
        error.validate().unwrap();
        assert!(!serde_json::to_string(&error).unwrap().contains(SECRET));
    }
    for mode in [0o000, 0o400, 0o640, 0o604] {
        let (state, path) = disk_store();
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
        assert_eq!(
            open(state.path()).unwrap_err().code,
            ErrorCode::CredentialUnavailable
        );
    }
    let (state, path) = disk_store();
    let outside = state.path().join("outside");
    fs::rename(&path, &outside).unwrap();
    symlink(outside, &path).unwrap();
    assert_eq!(
        open(state.path()).unwrap_err().code,
        ErrorCode::CredentialUnavailable
    );
    fs::remove_file(&path).unwrap();
    rustix::fs::mknodat(
        rustix::fs::CWD,
        &path,
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        0,
    )
    .unwrap();
    assert_eq!(
        open(state.path()).unwrap_err().code,
        ErrorCode::CredentialUnavailable
    );
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    assert_eq!(
        open(state.path()).unwrap_err().code,
        ErrorCode::CredentialUnavailable
    );
}

#[test]
fn loading_keeps_material_private_and_rejects_redirected_roots() {
    use std::{fs, os::unix::fs::symlink};
    let (state, path) = disk_store();
    let loaded = open(state.path()).unwrap();
    let handle = loaded.handle("acme").unwrap();
    assert!(
        loaded
            .with_secret(&handle, |secret| secret == SECRET)
            .unwrap()
    );
    assert!(!format!("{loaded:?} {handle:?}").contains(SECRET));
    let root = path.parent().unwrap();
    let outside = state.path().join("outside");
    fs::rename(root, &outside).unwrap();
    symlink(outside, root).unwrap();
    assert_eq!(
        open(state.path()).unwrap_err().code,
        ErrorCode::CredentialUnavailable
    );
}

fn store() -> ProviderCredentialStore {
    ProviderCredentialStore::in_memory([("acme", SECRET)])
}

fn facts(uid: u32, gid: u32, mode: u32) -> FileFacts {
    FileFacts {
        uid,
        gid,
        mode,
        is_dir: false,
        is_file: true,
        links: 1,
        is_symlink: false,
    }
}

#[test]
fn handle_names_a_provider_and_carries_no_secret() {
    let handle = store().handle("acme").unwrap();
    assert_eq!(handle.provider(), "acme");
    let encoded = serde_json::to_string(&handle).unwrap();
    assert!(
        !encoded.contains(SECRET),
        "serialized handle leaked the secret: {encoded}"
    );
}

#[test]
fn debug_of_handle_redacts_the_secret() {
    let handle = store().handle("acme").unwrap();
    let rendered = format!("{handle:?}");
    assert!(
        !rendered.contains(SECRET),
        "Debug leaked the secret: {rendered}"
    );
}

#[test]
fn debug_of_store_redacts_every_secret() {
    let rendered = format!("{:?}", store());
    assert!(
        !rendered.contains(SECRET),
        "Debug leaked the secret: {rendered}"
    );
}

#[test]
fn handle_does_not_round_trip_into_a_secret() {
    let handle = store().handle("acme").unwrap();
    let encoded = serde_json::to_string(&handle).unwrap();
    let decoded: CredentialHandle = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded.provider(), "acme");
    // A handle decoded from Session-supplied bytes must still resolve only
    // through the store that actually holds the material.
    let other = ProviderCredentialStore::in_memory([("other", "different")]);
    let error = other.with_secret(&decoded, |_| ()).unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidRequest);
}

#[test]
fn broker_internal_accessor_reaches_the_secret() {
    let seen = store()
        .with_secret(&store().handle("acme").unwrap(), str::to_owned)
        .unwrap();
    assert_eq!(seen, SECRET);
}

#[test]
fn unknown_provider_is_an_invalid_request() {
    let error = store().handle("nope").unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidRequest);
    error.validate().unwrap();
}

#[test]
fn unusable_credential_material_is_typed_not_a_panic() {
    let store = ProviderCredentialStore::in_memory([("blank", "")]);
    let error = store.handle("blank").unwrap_err();
    assert_eq!(error.code, ErrorCode::CredentialUnavailable);
    assert!(!error.retryable);
    assert_eq!(error.next_action, NextAction::ContactOperator);
    error.validate().unwrap();
}

#[test]
fn credential_file_must_be_private_to_the_broker() {
    assert!(credential_file_trusted(
        &facts(OWNER, GROUP, 0o600),
        OWNER,
        GROUP
    ));
}

#[test]
fn credential_file_refuses_wider_modes_and_foreign_identity() {
    for mode in [0o640, 0o604, 0o660, 0o666, 0o700] {
        assert!(
            !credential_file_trusted(&facts(OWNER, GROUP, mode), OWNER, GROUP),
            "mode {mode:o} must be refused"
        );
    }
    assert!(!credential_file_trusted(
        &facts(OWNER + 1, GROUP, 0o600),
        OWNER,
        GROUP
    ));
    assert!(!credential_file_trusted(
        &facts(OWNER, GROUP + 1, 0o600),
        OWNER,
        GROUP
    ));
}

#[test]
fn credential_file_refuses_a_symlink() {
    let mut symlink = facts(OWNER, GROUP, 0o600);
    symlink.is_symlink = true;
    assert!(!credential_file_trusted(&symlink, OWNER, GROUP));
}

#[test]
fn credential_root_must_be_a_private_directory() {
    let mut root = facts(OWNER, GROUP, 0o700);
    root.is_dir = true;
    assert!(credential_root_trusted(&root, OWNER, GROUP));
    for mode in [0o750, 0o770, 0o777, 0o600] {
        let mut wider = facts(OWNER, GROUP, mode);
        wider.is_dir = true;
        assert!(
            !credential_root_trusted(&wider, OWNER, GROUP),
            "mode {mode:o} must be refused"
        );
    }
    assert!(!credential_root_trusted(
        &facts(OWNER, GROUP, 0o700),
        OWNER,
        GROUP
    ));
}
