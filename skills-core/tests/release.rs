//! Behavioral coverage for release.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Test fixtures abort on setup failure and assert failures directly."
)]

//! The trusted release: what may become one, and what may install one.
//!
//! Bundles here are assembled from fake component files rather than by running
//! `cargo build`, so the tests cover identity, signing, tampering, install
//! atomicity, and downgrade without a nested build. CI also runs the
//! `installed_release_` cases as root in the initial user namespace. These
//! software-key fixtures do not replace genuine hardware-signing acceptance.

mod support;

use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
};

use louiselm_skills::{
    Policy,
    install::{self, InstallError},
    launch, launch_protocol, launch_receipt,
    release::{self, ReleaseError, SourceIdentity, ToolchainIdentity},
    sshsig::SkPolicy,
    trust::{Role, TrustStore},
};
use support::{Fixture, SshKey, write_file};

fn toolchain() -> ToolchainIdentity {
    ToolchainIdentity {
        rustc: "1.97.1".to_owned(),
        cargo: "1.97.1".to_owned(),
        target: "x86_64-linux".to_owned(),
    }
}

fn source() -> SourceIdentity {
    SourceIdentity {
        commit: "0123456789abcdef0123456789abcdef01234567".to_owned(),
        clean: true,
        describe: "v0.1.0".to_owned(),
        dependencies_digest: louiselm_skills::Digest::of(b"Cargo.lock").to_string(),
    }
}

fn assemble(fixture: &Fixture, name: &str, contents: &str, built_at_ms: u64) -> std::path::PathBuf {
    let staging = fixture.path(&format!("staging/{name}"));
    write_file(&staging.join("louiselm-skills"), contents);
    fs::set_permissions(
        staging.join("louiselm-skills"),
        fs::Permissions::from_mode(0o755),
    )
    .expect("mode is settable");

    let bundle = fixture.path(&format!("bundles/{name}"));
    release::assemble(
        &release::AssembleRequest {
            source: source(),
            toolchain: toolchain(),
            policy: &Policy::embedded(),
            components: vec![release::ComponentInput {
                name: "louiselm-skills".to_owned(),
                path: staging.join("louiselm-skills"),
                kind: release::ComponentKind::Executable,
            }],
            built_at_ms,
        },
        &bundle,
    )
    .expect("the bundle assembles");
    bundle
}

fn enrol_release_key(fixture: &Fixture) -> SshKey {
    let primary = SshKey::generate(fixture, "primary");
    let release_key = SshKey::generate(fixture, "release");
    let store = fixture.store();
    TrustStore::bootstrap(
        &store,
        "louiselm/skills",
        &primary.public_key(),
        &release_key.public_key(),
        SkPolicy::none(),
        1,
    )
    .expect("trust bootstraps");
    release_key
}

fn sign_bundle(bundle: &Path, key: &SshKey) {
    let manifest = fs::read(bundle.join("manifest.json")).expect("the manifest is readable");
    let signature = key.sign(release::RELEASE_NAMESPACE, &manifest);
    fs::write(bundle.join("manifest.sig"), signature).expect("the signature is writable");
}

#[test]
fn recorded_release_history_survives_retirement_but_new_old_key_signatures_fail() {
    let fixture = Fixture::new();
    let key = enrol_release_key(&fixture);
    let store = fixture.store();
    let bundle = assemble(&fixture, "approved", "approved bytes", 1);
    release::sign_bundle(
        &store,
        &bundle,
        &louiselm_skills::SshKeygenSigner::new(key.private_key_path()),
    )
    .expect("recorded release signature");
    let trust = TrustStore::load(&store).expect("trust").expect("enrolled");
    let replacement = SshKey::generate(&fixture, "release-replacement");
    let change = support::key_change(&trust, Role::Release, &replacement.public_key());
    let signature = key.sign(
        louiselm_skills::trust::recovery::RECOVERY_NAMESPACE,
        &change.canonical_bytes(),
    );
    let rotated =
        support::apply_key_change(&store, &change, &signature, &replacement, 2).expect("rotation");
    release::verify_bundle(&bundle, &rotated).expect("recorded historical release still verifies");
    let forged = assemble(&fixture, "after-retirement", "new bytes", 0);
    sign_bundle(&forged, &key);
    assert!(
        release::verify_bundle(&forged, &rotated).is_err(),
        "old key cannot authorize a new release"
    );
    assert!(
        release::sign_bundle(
            &store,
            &forged,
            &louiselm_skills::SshKeygenSigner::new(key.private_key_path())
        )
        .is_err()
    );
}

#[test]
fn a_bundle_binds_its_source_toolchain_dependencies_policy_and_bytes() {
    let fixture = Fixture::new();
    let bundle = assemble(
        &fixture,
        "first",
        "#!/bin/sh\necho one\n",
        1_756_800_000_000,
    );

    let manifest = release::read_manifest(&bundle).expect("the manifest is readable");

    assert_eq!(manifest.source.commit, source().commit);
    assert_eq!(manifest.toolchain.rustc, "1.97.1");
    assert_eq!(
        manifest.policy.digest,
        Policy::embedded().digest().to_string()
    );
    assert_eq!(
        manifest
            .components
            .iter()
            .map(|component| component.name.as_str())
            .collect::<Vec<_>>(),
        vec![
            "louiselm-skills",
            "policy/policy.json",
            "schemas/schemas.json"
        ],
        "the policy and schemas are components, not decoration",
    );
    assert!(manifest.components[0].executable);
    assert!(!manifest.components[1].executable);
    assert_eq!(manifest.release_id, manifest.digest().to_string());
    assert!(
        bundle.join("policy/policy.json").is_file(),
        "the policy travels with the bundle it governs",
    );
    assert!(
        manifest
            .schemas
            .contains(&louiselm_skills::trust::recovery::RECOVERY_NAMESPACE.to_owned()),
        "the recovery change schema is bound by the release"
    );
    for schema in [
        louiselm_skills::trust::TRUST_SCHEMA,
        louiselm_skills::trust::onboarding::SETUP_NAMESPACE,
        louiselm_skills::trust::status::STATUS_SCHEMA,
    ] {
        assert!(
            manifest.schemas.contains(&schema.to_owned()),
            "missing {schema}"
        );
    }
    assert!(
        bundle.join("schemas/schemas.json").is_file(),
        "the schemas a release implements are part of its identity",
    );
    for schema in [
        launch::REQUEST_SCHEMA,
        launch_protocol::LAUNCH_AUTHORIZATION_SCHEMA,
        launch_protocol::LIFECYCLE_REQUEST_SCHEMA,
        launch_protocol::STATUS_REQUEST_SCHEMA,
        launch_protocol::RECEIPT_ACK_SCHEMA,
        launch_protocol::SUPERVISOR_STATUS_SCHEMA,
        launch_protocol::SESSION_STATUS_SCHEMA,
        launch_protocol::RESPONSE_SCHEMA,
        launch_receipt::RECEIPT_SCHEMA,
        launch_receipt::SIGNED_RECEIPT_SCHEMA,
    ] {
        assert!(
            manifest.schemas.iter().any(|candidate| candidate == schema),
            "trusted release omitted launcher schema {schema}",
        );
    }
}

#[test]
fn a_dirty_source_tree_cannot_produce_a_release() {
    let fixture = Fixture::new();
    let repository = fixture.path("repository");
    fs::create_dir_all(&repository).expect("directory is creatable");
    for arguments in [
        vec!["init", "-q", "-b", "main"],
        vec!["config", "user.name", "test"],
        vec!["config", "user.email", "test@localhost"],
    ] {
        assert!(
            std::process::Command::new("git")
                .current_dir(&repository)
                .args(&arguments)
                .status()
                .expect("git runs")
                .success()
        );
    }
    write_file(&repository.join("file.txt"), "committed\n");
    for arguments in [vec!["add", "file.txt"], vec!["commit", "-q", "-m", "first"]] {
        assert!(
            std::process::Command::new("git")
                .current_dir(&repository)
                .args(&arguments)
                .status()
                .expect("git runs")
                .success()
        );
    }

    let clean = SourceIdentity::of(&repository, "digest").expect("a clean tree is identifiable");
    assert!(clean.clean);

    write_file(&repository.join("file.txt"), "uncommitted change\n");
    let error = SourceIdentity::of(&repository, "digest").expect_err("a dirty tree is refused");
    assert!(
        matches!(error, ReleaseError::DirtySource(_)),
        "unexpected error: {error}",
    );

    write_file(&repository.join("file.txt"), "committed\n");
    write_file(&repository.join("untracked.txt"), "untracked\n");
    let error =
        SourceIdentity::of(&repository, "digest").expect_err("an untracked file is refused too");
    assert!(
        matches!(error, ReleaseError::DirtySource(_)),
        "unexpected error: {error}",
    );
}

#[test]
fn only_the_release_role_authorizes_a_bundle() {
    let fixture = Fixture::new();
    let release_key = enrol_release_key(&fixture);
    let primary = SshKey::generate(&fixture, "primary-2");
    let bundle = assemble(
        &fixture,
        "first",
        "#!/bin/sh\necho one\n",
        1_756_800_000_000,
    );
    let store = fixture.store();
    let trust = TrustStore::load(&store)
        .expect("trust is readable")
        .expect("trust exists");

    let error = release::verify_bundle(&bundle, &trust).expect_err("an unsigned bundle is refused");
    assert!(
        matches!(error, ReleaseError::Unsigned),
        "unexpected error: {error}",
    );

    sign_bundle(&bundle, &primary);
    let error = release::verify_bundle(&bundle, &trust)
        .expect_err("another role cannot authorize a release");
    assert!(
        matches!(error, ReleaseError::Signature(_)),
        "unexpected error: {error}",
    );

    sign_bundle(&bundle, &release_key);
    release::verify_bundle(&bundle, &trust).expect("the release role authorizes it");
}

#[test]
fn a_byte_changed_after_signing_is_caught_by_name() {
    let fixture = Fixture::new();
    let release_key = enrol_release_key(&fixture);
    let bundle = assemble(
        &fixture,
        "first",
        "#!/bin/sh\necho one\n",
        1_756_800_000_000,
    );
    sign_bundle(&bundle, &release_key);
    let store = fixture.store();
    let trust = TrustStore::load(&store)
        .expect("trust is readable")
        .expect("trust exists");

    let component = bundle.join("bin/louiselm-skills");
    fs::set_permissions(&component, fs::Permissions::from_mode(0o755)).expect("mode is settable");
    fs::write(&component, "#!/bin/sh\necho tampered\n").expect("the component is writable");

    let error = release::verify_bundle(&bundle, &trust).expect_err("a tampered bundle is refused");

    match error {
        ReleaseError::ComponentMismatch { name, .. } => assert_eq!(name, "louiselm-skills"),
        other => panic!("unexpected error: {other}"),
    }
}

#[test]
fn a_missing_component_is_refused_rather_than_installed_in_part() {
    let fixture = Fixture::new();
    let release_key = enrol_release_key(&fixture);
    let bundle = assemble(
        &fixture,
        "first",
        "#!/bin/sh\necho one\n",
        1_756_800_000_000,
    );
    sign_bundle(&bundle, &release_key);
    let store = fixture.store();
    let trust = TrustStore::load(&store)
        .expect("trust is readable")
        .expect("trust exists");
    fs::remove_file(bundle.join("bin/louiselm-skills")).expect("the component is removable");

    let error = release::verify_bundle(&bundle, &trust).expect_err("a partial bundle is refused");

    assert!(
        matches!(error, ReleaseError::ComponentMissing { .. }),
        "unexpected error: {error}",
    );
}

#[test]
fn installing_flips_one_symlink_and_leaves_the_prior_release_intact() {
    let fixture = Fixture::new();
    let release_key = enrol_release_key(&fixture);
    let store = fixture.store();
    let prefix = fixture.path("prefix");

    let first = assemble(
        &fixture,
        "first",
        "#!/bin/sh\necho one\n",
        1_756_800_000_000,
    );
    sign_bundle(&first, &release_key);
    let first_state = install::install(&store, &first, &prefix, 1_756_800_000_010)
        .expect("the first install succeeds");

    let second = assemble(
        &fixture,
        "second",
        "#!/bin/sh\necho two\n",
        1_756_800_100_000,
    );
    sign_bundle(&second, &release_key);
    let second_state = install::install(&store, &second, &prefix, 1_756_800_100_010)
        .expect("the second install succeeds");

    assert_ne!(first_state.release_id, second_state.release_id);
    assert_eq!(
        fs::read_link(prefix.join("current")).expect("current is a symlink"),
        Path::new("releases").join(&second_state.release_id),
    );
    assert!(
        prefix
            .join("releases")
            .join(&first_state.release_id)
            .join("bin/louiselm-skills")
            .is_file(),
        "the prior release stays usable where safe",
    );
    assert_eq!(
        fs::read_to_string(prefix.join("current/bin/louiselm-skills"))
            .expect("the current binary is readable"),
        "#!/bin/sh\necho two\n",
    );
}

#[test]
fn a_downgrade_is_refused_and_changes_nothing() {
    let fixture = Fixture::new();
    let release_key = enrol_release_key(&fixture);
    let store = fixture.store();
    let prefix = fixture.path("prefix");

    let old = assemble(&fixture, "old", "#!/bin/sh\necho old\n", 1_756_800_000_000);
    sign_bundle(&old, &release_key);
    let new = assemble(&fixture, "new", "#!/bin/sh\necho new\n", 1_756_800_100_000);
    sign_bundle(&new, &release_key);

    install::install(&store, &new, &prefix, 1).expect("the new release installs");
    let error = install::install(&store, &old, &prefix, 2).expect_err("a downgrade is refused");

    assert!(
        matches!(error, InstallError::Downgrade { .. }),
        "unexpected error: {error}",
    );
    assert_eq!(
        fs::read_to_string(prefix.join("current/bin/louiselm-skills"))
            .expect("the current binary is readable"),
        "#!/bin/sh\necho new\n",
        "a refused downgrade leaves the newer release current",
    );
}

#[test]
fn an_unsigned_bundle_never_reaches_the_prefix() {
    let fixture = Fixture::new();
    enrol_release_key(&fixture);
    let store = fixture.store();
    let prefix = fixture.path("prefix");
    let bundle = assemble(
        &fixture,
        "first",
        "#!/bin/sh\necho one\n",
        1_756_800_000_000,
    );

    let error =
        install::install(&store, &bundle, &prefix, 1).expect_err("an unsigned bundle is refused");

    assert!(
        matches!(error, InstallError::Release(ReleaseError::Unsigned)),
        "unexpected error: {error}",
    );
    assert!(
        !prefix.join("current").exists(),
        "a refused install leaves no partial prefix",
    );
}

#[test]
fn status_reports_ownership_as_evidence_rather_than_claiming_it() {
    let fixture = Fixture::new();
    let release_key = enrol_release_key(&fixture);
    let store = fixture.store();
    let prefix = fixture.path("prefix");
    let bundle = assemble(
        &fixture,
        "first",
        "#!/bin/sh\necho one\n",
        1_756_800_000_000,
    );
    sign_bundle(&bundle, &release_key);
    install::install(&store, &bundle, &prefix, 1).expect("the install succeeds");

    let status = install::status(&prefix).expect("status is readable");

    assert!(status.installed.is_some());
    assert!(
        !status.ownership.root_owned,
        "an install by an ordinary user is not root-owned, and says so",
    );
    assert!(
        !status.trusted,
        "a prefix an ordinary user can write is not a trust boundary",
    );
    assert_eq!(
        status.failure_code.as_deref(),
        Some("prefix_not_root_owned")
    );
    assert_eq!(status.next_action.id, "install_as_root");
}

fn installed_fixture() -> (Fixture, PathBuf, PathBuf) {
    if std::env::var_os("LOUISELM_REQUIRE_ROOT_RELEASE").is_some() {
        assert!(
            rustix::process::geteuid().is_root(),
            "root gate requires root"
        );
        assert_eq!(
            fs::read_to_string("/proc/self/uid_map")
                .expect("UID map")
                .split_whitespace()
                .collect::<Vec<_>>(),
            ["0", "0", "4294967295"],
            "root gate requires the initial user namespace",
        );
    }
    let fixture = Fixture::new();
    let key = enrol_release_key(&fixture);
    let bundle = assemble(&fixture, "ownership", "#!/bin/sh\nexit 0\n", 1);
    sign_bundle(&bundle, &key);
    let prefix = fixture.path("prefix");
    let state = install::install(&fixture.store(), &bundle, &prefix, 1).expect("install");
    let installed = prefix.join("releases").join(state.release_id);
    // Match the observed guest install, independent of the runner's umask.
    for directory in [
        &prefix,
        &prefix.join("releases"),
        &installed,
        &installed.join("bin"),
        &installed.join("policy"),
        &installed.join("schemas"),
    ] {
        fs::set_permissions(directory, fs::Permissions::from_mode(0o755))
            .expect("fixed directory mode");
    }
    fs::set_permissions(prefix.join("state.json"), fs::Permissions::from_mode(0o644))
        .expect("fixed state mode");
    let executable = installed.join("bin/louiselm-skills");
    (fixture, prefix, executable)
}

fn assert_installed_permissions(prefix: &Path, executable: &Path, writable: bool) {
    let status = install::status(prefix).expect("status");
    let identity = release::identity_of(executable);
    let root_owned = rustix::process::geteuid().is_root();
    assert_eq!(status.ownership.root_owned, root_owned);
    assert_eq!(status.ownership.world_writable, writable);
    assert_eq!(status.trusted, root_owned && !writable, "{status:?}");
    assert_eq!(identity.verified, status.trusted, "{identity:?}");
    if identity.verified {
        assert_eq!(
            identity.release_id,
            status.installed.map(|state| state.release_id)
        );
    }
}

#[test]
fn installed_release_checks_real_file_directory_and_link_ownership() {
    // louiselm-se9g: genuine root install in the lm70 guest falsely refused
    // current's 0777 symlink mode. Exercise both public consumers, not chmod
    // on the symlink (which would modify the release directory instead).
    let (_fixture, prefix, executable) = installed_fixture();
    assert_installed_permissions(&prefix, &executable, false);
    for target in [
        &prefix,
        &prefix.join("releases"),
        &executable,
        &prefix.join("state.json"),
    ] {
        let permissions = fs::metadata(target).expect("metadata").permissions();
        for added in [0o020, 0o002] {
            fs::set_permissions(
                target,
                fs::Permissions::from_mode(permissions.mode() | added),
            )
            .expect("add write permission");
            assert_installed_permissions(&prefix, &executable, true);
        }
        fs::set_permissions(target, permissions).expect("restore permissions");
        assert_installed_permissions(&prefix, &executable, false);
    }
    if rustix::process::geteuid().is_root() {
        for target in [&executable, &prefix.join("current")] {
            // lchown preserves the symlink target while changing its owner.
            rustix::fs::chownat(
                rustix::fs::CWD,
                target,
                Some(rustix::fs::Uid::from_raw(1000)),
                None,
                rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
            )
            .expect("change entry owner");
            let status = install::status(&prefix).expect("status");
            assert!(!status.ownership.root_owned);
            assert!(!status.trusted);
            assert!(!release::identity_of(&executable).verified);
            rustix::fs::chownat(
                rustix::fs::CWD,
                target,
                Some(rustix::fs::Uid::ROOT),
                None,
                rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
            )
            .expect("restore entry owner");
        }
        assert_installed_permissions(&prefix, &executable, false);
    }
}

#[test]
fn installed_release_accepts_only_the_expected_current_symlink() {
    let (fixture, prefix, executable) = installed_fixture();
    let current = prefix.join("current");
    let expected = fs::read_link(&current).expect("installed target");
    let outside = fixture.path("outside");
    fs::create_dir(&outside).expect("outside directory");
    let other_release =
        Path::new("releases").join(louiselm_skills::Digest::of(b"other").to_string());
    fs::create_dir(prefix.join(&other_release)).expect("other release directory");
    fs::set_permissions(
        prefix.join(&other_release),
        fs::Permissions::from_mode(0o755),
    )
    .expect("fixed other release mode");
    for target in [
        outside,
        other_release,
        prefix.join(&expected),
        PathBuf::from("releases/missing"),
        PathBuf::from("current"),
        PathBuf::from("releases"),
        PathBuf::from("state.json"),
    ] {
        fs::remove_file(&current).expect("remove fixture link");
        symlink(target, &current).expect("replace current");
        assert_installed_permissions(&prefix, &executable, true);
    }
    fs::remove_file(&current).expect("remove fixture link");
    symlink(&expected, &current).expect("restore current");
    let unexpected = executable
        .parent()
        .expect("bin directory")
        .join("unexpected");
    for target in [
        Path::new("."),
        Path::new("missing"),
        Path::new("unexpected"),
    ] {
        symlink(target, &unexpected).expect("unexpected link");
        assert_installed_permissions(&prefix, &executable, true);
        fs::remove_file(&unexpected).expect("remove fixture link");
    }
    // A current link with the right text must not hide a redirected release.
    let installed = prefix.join(&expected);
    let moved = fixture.path("moved-release");
    fs::rename(&installed, &moved).expect("move release");
    symlink(&moved, &installed).expect("redirect release");
    assert_installed_permissions(&prefix, &executable, true);
    fs::remove_file(&installed).expect("remove redirect");
    fs::rename(moved, installed).expect("restore release");
    assert_installed_permissions(&prefix, &executable, false);
}

#[test]
fn status_preserves_a_preexisting_uid_probe_and_its_target() {
    let fixture = Fixture::new();
    let sentinel = fixture.path("must-survive");
    write_file(&sentinel, "preserve this file\n");
    let probe = std::env::temp_dir().join(format!("louiselm-skills-uid-{}", std::process::id()));
    // symlink refuses an existing path; never overwrite another process's data.
    std::os::unix::fs::symlink(&sentinel, &probe).expect("the probe path is unused");

    let status = install::status(&fixture.path(""));
    let remaining_link = fs::read_link(&probe);
    if remaining_link.is_ok() {
        fs::remove_file(&probe).expect("the test removes its own symlink");
    }

    assert!(
        status.is_ok(),
        "an empty prefix still has a readable status"
    );
    assert_eq!(
        fs::read_to_string(&sentinel).expect("the sentinel remains readable"),
        "preserve this file\n",
        "status must not truncate a file while discovering the invoker UID",
    );
    assert_eq!(
        remaining_link.expect("status must not remove a pre-existing path"),
        sentinel,
    );
}

#[test]
fn a_development_build_is_labeled_unverified_and_says_why() {
    let identity = release::running_identity();

    assert!(
        !identity.verified,
        "a cargo-built binary is never a trusted release",
    );
    assert_eq!(identity.failure_code.as_deref(), Some("development_build"));
    assert!(identity.release_id.is_none());
}

#[test]
fn a_development_build_may_not_activate_a_generation_in_a_trusted_store() {
    let fixture = Fixture::new();
    let store = fixture.store();
    // Stand in for a store a verified release created. Nothing in a
    // development build can produce this record honestly, which is the point.
    write_file(
        &fixture.store_root().join("provenance.json"),
        r#"{"schema":"louiselm.skills.store-provenance/1","trusted":true,"created_by_release":"sha256:0"}"#,
    );

    assert!(store.is_trusted());
    let error = louiselm_skills::admission::activate(
        &store,
        &louiselm_skills::Digest::of(b"any generation"),
        1,
    )
    .expect_err("a development build cannot activate into a trusted store");

    assert!(
        matches!(
            error,
            louiselm_skills::admission::AdmissionError::NotTrustedRelease { .. }
        ),
        "unexpected error: {error}",
    );
}

#[test]
fn a_store_a_development_build_created_is_marked_untrusted_forever() {
    let fixture = Fixture::new();

    let provenance = fixture
        .store()
        .provenance()
        .expect("provenance is recorded");

    assert!(!provenance.trusted);
    assert_eq!(provenance.created_by_release, None);
}

#[test]
fn a_component_changed_after_install_is_reported_as_tampered() {
    let fixture = Fixture::new();
    let release_key = enrol_release_key(&fixture);
    let store = fixture.store();
    let prefix = fixture.path("prefix");
    let bundle = assemble(
        &fixture,
        "first",
        "#!/bin/sh\necho one\n",
        1_756_800_000_000,
    );
    sign_bundle(&bundle, &release_key);
    let state = install::install(&store, &bundle, &prefix, 1).expect("the install succeeds");

    let installed = prefix
        .join("releases")
        .join(&state.release_id)
        .join("bin/louiselm-skills");
    fs::set_permissions(&installed, fs::Permissions::from_mode(0o755)).expect("mode is settable");
    fs::write(&installed, "#!/bin/sh\necho swapped\n").expect("the component is writable");

    let status = install::status(&prefix).expect("status is readable");

    assert!(!status.trusted);
    assert!(
        status.failure_code.as_deref() == Some("release_tampered")
            || status.failure_code.as_deref() == Some("prefix_not_root_owned"),
        "unexpected failure code: {:?}",
        status.failure_code,
    );
}

#[test]
fn a_release_installed_where_an_agent_can_write_makes_no_trusted_claim() {
    // Regression for louiselm-jqj5: matching bytes are not a boundary if
    // someone other than root can replace them a moment later. Every automated
    // install here runs as an ordinary user, so this is the case that must
    // report unverified.
    let fixture = Fixture::new();
    let release_key = enrol_release_key(&fixture);
    let store = fixture.store();
    let prefix = fixture.path("prefix");
    let bundle = assemble(
        &fixture,
        "first",
        "#!/bin/sh\necho one\n",
        1_756_800_000_000,
    );
    sign_bundle(&bundle, &release_key);
    let state = install::install(&store, &bundle, &prefix, 1).expect("the install succeeds");

    let installed = prefix
        .join("releases")
        .join(&state.release_id)
        .join("bin/louiselm-skills");
    let identity = release::identity_of(&installed);

    assert!(
        !identity.verified,
        "an install an ordinary user owns is not a trusted release",
    );
    assert_eq!(
        identity.failure_code.as_deref(),
        Some("prefix_not_root_owned")
    );
}
