//! First-release bootstrap is provisional, never installed recovery readiness.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Disposable trust fixtures and assertions."
)]

mod support;

use louiselm_skills::{
    sshsig::SkPolicy,
    trust::{Role, TrustStore},
};
use support::{Fixture, SshKey};

#[test]
fn published_first_release_recipe_enforces_hardware_policy() {
    assert!(include_str!("../README.md").contains("../docs/recovery-ceremony.md"));
    // Execute the documented bootstrap, substituting only disposable paths.
    // Do not run key generation, jq, signing or installation from the guide.
    let guide = include_str!("../../docs/recovery-ceremony.md");
    let start = guide.find("\"$DEV_TOOL\" trust bootstrap").unwrap();
    let command = guide[start..].split_once(" |\n").unwrap().0;
    let command = command
        .replace("$HOME/.ssh/id_louiselm_primary.pub", "$PRIMARY")
        .replace("$HOME/.ssh/id_louiselm_release.pub", "$RELEASE");
    let fixture = Fixture::new();
    let primary = SshKey::generate(&fixture, "primary");
    let release = SshKey::generate(&fixture, "release");
    let output = std::process::Command::new("/bin/sh")
        .args(["-eu", "-c", &command])
        .env("DEV_TOOL", env!("CARGO_BIN_EXE_louiselm-skills"))
        .env("provisional_store", fixture.store_root())
        .env("PRIMARY", primary.private_key_path().with_extension("pub"))
        .env("RELEASE", release.private_key_path().with_extension("pub"))
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let trust: TrustStore = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(trust.keys.len(), 2);
    for key in &trust.keys {
        assert_eq!(key.sk_policy, SkPolicy::require_presence_and_verification());
    }
    let signature = release.sign("louiselm.skills.release/1", b"fixture");
    assert!(matches!(
        louiselm_skills::sshsig::verify(
            &signature,
            "louiselm.skills.release/1",
            b"fixture",
            &release.public_key(),
            trust.key_for(Role::Release).unwrap().sk_policy,
        ),
        Err(louiselm_skills::sshsig::SignatureError::NotHardwareBacked { .. })
    ));
    assert!(!fixture.store().is_trusted());
}

#[test]
fn provisional_bootstrap_enrolls_distinct_ordinary_signers_without_recovery_role() {
    let fixture = Fixture::new();
    let primary = SshKey::generate(&fixture, "primary");
    let release = SshKey::generate(&fixture, "release");
    let trust = TrustStore::bootstrap(
        &fixture.store(),
        "test/setup",
        &primary.public_key(),
        &release.public_key(),
        SkPolicy::none(),
        1,
    )
    .unwrap();
    assert_eq!(trust.keys.len(), 2);
    assert_eq!(
        trust.key_for(Role::Release).unwrap().public_key,
        release.public_key()
    );
    assert_eq!(
        trust.admission_key().unwrap().public_key,
        primary.public_key()
    );
    assert!(Role::parse("recovery").is_none());
    assert!(!fixture.store().is_trusted());
    assert!(trust.paper_verifier.is_none() && trust.passkey.is_none());
    assert!(
        TrustStore::bootstrap(
            &fixture.store(),
            "test/setup",
            &primary.public_key(),
            &release.public_key(),
            SkPolicy::none(),
            2
        )
        .is_err()
    );
    assert_eq!(TrustStore::load(&fixture.store()).unwrap(), Some(trust));
}

#[test]
fn provisional_bootstrap_refuses_production_provenance_and_same_key_roles() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let primary = SshKey::generate(&fixture, "primary");
    assert!(
        TrustStore::bootstrap(
            &store,
            "test/setup",
            &primary.public_key(),
            &primary.public_key(),
            SkPolicy::none(),
            1
        )
        .is_err()
    );
    assert!(TrustStore::load(&store).unwrap().is_none());
    // Public fixture metadata cannot make this test executable an installed tool.
    // It must nevertheless forbid the provisional API from writing such a store.
    std::fs::write(fixture.path("store/provenance.json"), r#"{"schema":"louiselm.skills.store-provenance/1","trusted":true,"created_by_release":"fixture"}"#).unwrap();
    let release = SshKey::generate(&fixture, "release");
    assert!(
        TrustStore::bootstrap(
            &store,
            "test/setup",
            &primary.public_key(),
            &release.public_key(),
            SkPolicy::require_presence_and_verification(),
            1
        )
        .is_err()
    );
    assert!(TrustStore::load(&store).unwrap().is_none());
    assert!(TrustStore::reset(&store).is_err());
    assert!(
        louiselm_skills::trust::onboarding::reset(
            &store,
            &louiselm_skills::canonical::Digest::of(b"fixture")
        )
        .is_err()
    );
}

#[test]
fn public_status_never_mistakes_unset_or_provisional_state_for_ready() {
    use louiselm_skills::trust::status;
    let fixture = Fixture::new();
    let empty = status::read(&fixture.store()).unwrap();
    assert!(!empty.recovery_ready);
    assert_eq!(
        empty.next_action,
        "install_signed_release_and_setup_fresh_store"
    );
    let primary = SshKey::generate(&fixture, "primary");
    let release = SshKey::generate(&fixture, "release");
    TrustStore::bootstrap(
        &fixture.store(),
        "test/status",
        &primary.public_key(),
        &release.public_key(),
        SkPolicy::none(),
        1,
    )
    .unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_louiselm-skills"))
        .args(["recovery", "status", "--store"])
        .arg(fixture.store_root())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["recovery_ready"], false);
    assert_eq!(value["paper_enrolled"], false);
    assert_eq!(value["passkey_enrolled"], false);
    assert!(value.get("paper_verifier").is_none());
    assert!(!String::from_utf8_lossy(&output.stdout).contains(&primary.public_key()));
}
