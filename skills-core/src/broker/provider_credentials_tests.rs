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

fn store() -> ProviderCredentialStore {
    ProviderCredentialStore::in_memory([("acme", SECRET)])
}

fn facts(uid: u32, gid: u32, mode: u32) -> FileFacts {
    FileFacts {
        uid,
        gid,
        mode,
        is_dir: false,
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
