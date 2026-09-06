//! Behavioral coverage for signature.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Test fixtures abort on setup failure and assert failures directly."
)]

//! SSH signature parsing and verification, including FIDO assertion flags.
//!
//! The signatures here are real: the tests generate software keys and sign
//! with `ssh-keygen`, so the crypto path is exercised rather than mocked. The
//! FIDO flag cases use crafted blobs, because a hardware assertion cannot be
//! produced without hardware — which is why the flag check runs before the
//! cryptographic check, where a crafted blob can still reach it.

mod support;

use louiselm_skills::sshsig::{self, SignatureError, SkPolicy};
use support::{Fixture, SshKey};

const NAMESPACE: &str = "louiselm.skills.admission/1";

#[test]
fn a_real_signature_verifies_against_the_key_that_made_it() {
    let fixture = Fixture::new();
    let key = SshKey::generate(&fixture, "primary");

    let armored = key.sign(NAMESPACE, b"payload bytes");
    let verified = sshsig::verify(
        &armored,
        NAMESPACE,
        b"payload bytes",
        &key.public_key(),
        SkPolicy::none(),
    )
    .expect("a real signature verifies");

    assert_eq!(verified.namespace, NAMESPACE);
    assert_eq!(verified.algorithm, "ssh-ed25519");
    assert_eq!(verified.openssh_public_key(), key.public_key());
    assert!(
        verified.sk_flags.is_none(),
        "a software key carries no assertion flags"
    );
}

#[test]
fn a_signature_over_other_bytes_is_refused() {
    let fixture = Fixture::new();
    let key = SshKey::generate(&fixture, "primary");
    let armored = key.sign(NAMESPACE, b"payload bytes");

    let error = sshsig::verify(
        &armored,
        NAMESPACE,
        b"different bytes",
        &key.public_key(),
        SkPolicy::none(),
    )
    .expect_err("a signature over other bytes is refused");

    assert!(
        matches!(error, SignatureError::Cryptographic(_)),
        "unexpected error: {error}",
    );
}

#[test]
fn a_signature_from_another_namespace_is_refused() {
    let fixture = Fixture::new();
    let key = SshKey::generate(&fixture, "primary");
    let armored = key.sign("louiselm.skills.release/1", b"payload bytes");

    let error = sshsig::verify(
        &armored,
        NAMESPACE,
        b"payload bytes",
        &key.public_key(),
        SkPolicy::none(),
    )
    .expect_err("a signature from another namespace is refused");

    assert!(
        matches!(error, SignatureError::NamespaceMismatch { .. }),
        "unexpected error: {error}",
    );
}

#[test]
fn a_signature_from_an_unenrolled_key_is_refused() {
    let fixture = Fixture::new();
    let enrolled = SshKey::generate(&fixture, "primary");
    let stranger = SshKey::generate(&fixture, "stranger");
    let armored = stranger.sign(NAMESPACE, b"payload bytes");

    let error = sshsig::verify(
        &armored,
        NAMESPACE,
        b"payload bytes",
        &enrolled.public_key(),
        SkPolicy::none(),
    )
    .expect_err("a signature from an unenrolled key is refused");

    assert!(
        matches!(error, SignatureError::KeyMismatch { .. }),
        "unexpected error: {error}",
    );
}

#[test]
fn a_mangled_signature_is_refused_rather_than_half_parsed() {
    let fixture = Fixture::new();
    let key = SshKey::generate(&fixture, "primary");
    let armored = key.sign(NAMESPACE, b"payload bytes");

    for mangled in [
        armored.replace("-----BEGIN SSH SIGNATURE-----", "-----BEGIN NOTHING-----"),
        armored.replace('A', "B"),
        armored.lines().take(2).collect::<Vec<_>>().join("\n"),
        String::new(),
    ] {
        let error = sshsig::verify(
            &mangled,
            NAMESPACE,
            b"payload bytes",
            &key.public_key(),
            SkPolicy::none(),
        )
        .expect_err("a mangled signature is refused");
        assert!(
            !matches!(error, SignatureError::ToolMissing(_)),
            "unexpected error: {error}",
        );
    }
}

#[test]
fn a_hardware_assertion_without_the_required_flags_is_refused_before_the_crypto() {
    let present_and_verified = support::crafted_sk_signature(NAMESPACE, 0x05);
    let present_only = support::crafted_sk_signature(NAMESPACE, 0x01);
    let neither = support::crafted_sk_signature(NAMESPACE, 0x00);
    let key = support::crafted_sk_public_key();
    let policy = SkPolicy::require_presence_and_verification();

    let parsed = sshsig::parse(&present_and_verified).expect("the crafted blob parses");
    let flags = parsed
        .sk_flags
        .expect("an sk signature carries assertion flags");
    assert!(flags.user_presence);
    assert!(flags.user_verification);

    for (armored, missing) in [
        (present_only, "user verification"),
        (neither, "user presence"),
    ] {
        let error = sshsig::verify(&armored, NAMESPACE, b"payload bytes", &key, policy)
            .expect_err("an assertion missing a required flag is refused");
        match error {
            SignatureError::AssertionFlags { missing: found } => {
                assert!(
                    found.contains(missing),
                    "expected {missing} to be named, got {found}",
                );
            }
            other => panic!("unexpected error: {other}"),
        }
    }
}

#[test]
fn a_software_key_cannot_satisfy_a_hardware_requirement() {
    let fixture = Fixture::new();
    let key = SshKey::generate(&fixture, "primary");
    let armored = key.sign(NAMESPACE, b"payload bytes");

    let error = sshsig::verify(
        &armored,
        NAMESPACE,
        b"payload bytes",
        &key.public_key(),
        SkPolicy::require_presence_and_verification(),
    )
    .expect_err("a software key cannot satisfy a hardware requirement");

    assert!(
        matches!(error, SignatureError::NotHardwareBacked { .. }),
        "unexpected error: {error}",
    );
}
