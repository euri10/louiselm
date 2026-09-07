//! Public Chrome virtual-authenticator fixture, captured 2026-09-07 in isolated
//! Playwright session passkey-fixture, codex/01a07753-51de-7733-bfeb-8a3f26fa907e.
//! No maintainer credential. Synthetic assertions use its disposable private key;
//! they test verifier semantics, not an observed Android event order.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Public test fixtures and assertions."
)]

mod support;

use louiselm_skills::sshsig::SkPolicy;
use louiselm_skills::trust::{
    Role, TrustStore,
    paper::PaperPhrase,
    passkey::{PendingAuthentication, PendingRegistration},
    recovery::{
        self, RecoveryAuthorization, RecoveryChange, RecoveryConfirmation, ReplacementKey,
        ReplacementProof,
    },
};
use openssl::{hash::MessageDigest, pkey::PKey, sign::Signer};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::process::Command;
use support::{Fixture, SshKey};
use webauthn_rs::prelude::{Base64UrlSafeData, PublicKeyCredential, RegisterPublicKeyCredential};

const CREDENTIAL_ID: &str = "hoiqR0MK1DUJTtd2xBHlXfc4wfM5_TZsnieCSbxIt0Y";
const ATTESTATION: &str = "o2NmbXRkbm9uZWdhdHRTdG10oGhhdXRoRGF0YVikSZYN5YgOjGh0NBcPZHZgW4_krrmihjLHmVzzuoMdl2NdAAAAAQECAwQFBgcIAQIDBAUGBwgAIIaIqkdDCtQ1CU7XdsQR5V33OMHzOf02bJ4ngkm8SLdGpQECAyYgASFYILcsYYG9SVzlzOsF8pwv6wzq79e_v1QPZmN0WI-CTBUnIlggjnfeVMLi3XpUvrTEKHOUQ7SF43bylxnH3Htjzp2mjhw";
// PUBLIC disposable virtual authenticator key, never use outside these fixtures.
const PRIVATE_KEY: &str = "-----BEGIN PRIVATE KEY-----\nMIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgtwtOuiZnq9p713fxEm7r\nD4z+6QtyswqIbksMKa4CNJmhRANCAAS3LGGBvUlc5czrBfKcL+sM6u/Xv79UD2Zj\ndFiPgkwVJ4533lTC4t16VL60xChzlEO0heN28pcZx9x7Y86dpo4c\n-----END PRIVATE KEY-----\n";

fn trust(fixture: &Fixture) -> (TrustStore, SshKey) {
    let primary = SshKey::generate(fixture, "primary");
    let recovery = SshKey::generate(fixture, "recovery");
    (
        TrustStore::bootstrap(
            &fixture.store(),
            "test/passkey",
            &primary.public_key(),
            &recovery.public_key(),
            SkPolicy::none(),
            1,
        )
        .unwrap(),
        primary,
    )
}

fn encoded(bytes: Vec<u8>) -> Value {
    serde_json::to_value(Base64UrlSafeData::from(bytes)).unwrap()
}

fn registration(challenge: &Value, origin: &str) -> RegisterPublicKeyCredential {
    serde_json::from_value(json!({
        "id": CREDENTIAL_ID, "rawId": CREDENTIAL_ID, "type":"public-key",
        "response": {"attestationObject":ATTESTATION,
            "clientDataJSON": encoded(serde_json::to_vec(&json!({"type":"webauthn.create", "challenge":challenge, "origin":origin, "crossOrigin":false})).unwrap()),
            "transports":["internal"]}, "extensions":{}
    })).unwrap()
}

#[test]
fn real_verifier_accepts_backed_up_registration_and_consumes_each_attempt() {
    let fixture = Fixture::new();
    let (trust, _) = trust(&fixture);
    let (mut pending, options) = PendingRegistration::start(&trust, 45081).unwrap();
    let options = serde_json::to_value(options).unwrap();
    assert_eq!(
        options["publicKey"]["authenticatorSelection"]["userVerification"],
        "required"
    );
    let response = registration(&options["publicKey"]["challenge"], "http://localhost:45081");
    let registered = pending.finish(&trust, &response).unwrap();
    assert!(!registered.credential().fingerprint().unwrap().is_empty());
    assert!(pending.finish(&trust, &response).is_err());
    assert_eq!(TrustStore::load(&fixture.store()).unwrap(), Some(trust));
}

#[test]
fn registration_refuses_wrong_origin_challenge_and_stale_snapshot() {
    let fixture = Fixture::new();
    let (trust, _) = trust(&fixture);
    for origin in [
        "http://localhost:45082",
        "https://attacker.example",
        "http://127.0.0.1:45081",
    ] {
        let (mut pending, options) = PendingRegistration::start(&trust, 45081).unwrap();
        let options = serde_json::to_value(options).unwrap();
        assert!(
            pending
                .finish(
                    &trust,
                    &registration(&options["publicKey"]["challenge"], origin)
                )
                .is_err()
        );
        assert!(
            pending
                .finish(
                    &trust,
                    &registration(&options["publicKey"]["challenge"], "http://localhost:45081")
                )
                .is_err()
        );
    }
    let (mut pending, _) = PendingRegistration::start(&trust, 45081).unwrap();
    assert!(
        pending
            .finish(
                &trust,
                &registration(&encoded(vec![1; 32]), "http://localhost:45081")
            )
            .is_err()
    );
    let (mut pending, options) = PendingRegistration::start(&trust, 45081).unwrap();
    let mut changed = trust.clone();
    changed.sequence += 1;
    assert!(
        pending
            .finish(
                &changed,
                &registration(
                    &serde_json::to_value(options).unwrap()["publicKey"]["challenge"],
                    "http://localhost:45081"
                )
            )
            .is_err()
    );
    assert_eq!(TrustStore::load(&fixture.store()).unwrap(), Some(trust));
}

#[test]
fn registration_refuses_missing_uv_and_wrong_rp_hash() {
    let fixture = Fixture::new();
    let (trust, _) = trust(&fixture);
    for fault in ["uv", "rp"] {
        let (mut pending, options) = PendingRegistration::start(&trust, 45081).unwrap();
        let mut response = registration(
            &serde_json::to_value(options).unwrap()["publicKey"]["challenge"],
            "http://localhost:45081",
        );
        let mut bytes = response.response.attestation_object.as_ref().to_vec();
        let hash = Sha256::digest(b"localhost");
        // Locate the observed authData in this fixed fmt=none fixture, not a
        // production CBOR parser. Keep length/framing and alter one security bit.
        let start = bytes
            .windows(32)
            .position(|window| window == hash.as_slice())
            .unwrap();
        if fault == "uv" {
            bytes[start + 32] &= !4;
        } else {
            bytes[start] ^= 1;
        }
        response.response.attestation_object = bytes.into();
        assert!(pending.finish(&trust, &response).is_err(), "{fault}");
    }
    assert_eq!(TrustStore::load(&fixture.store()).unwrap(), Some(trust));
}

#[test]
fn passkey_cli_refuses_uninstalled_authority_and_never_echoes_unknown_values() {
    let fixture = Fixture::new();
    for command in ["passkey-enroll", "passkey-recover"] {
        let mut process = Command::new(env!("CARGO_BIN_EXE_louiselm-skills"));
        process
            .args(["recovery", command, "--store"])
            .arg(fixture.path("must-not-create"));
        if command == "passkey-enroll" {
            process.args(["--authorizer", "unused-fixture-key"]);
        }
        let output = process.output().unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr).contains("trusted installed tool"));
        assert!(!fixture.path("must-not-create").exists());
    }
    let output = Command::new(env!("CARGO_BIN_EXE_louiselm-skills"))
        .args([
            "recovery",
            "passkey-recover",
            "--phrase",
            "private-fixture-input",
        ])
        .output()
        .unwrap();
    assert!(!String::from_utf8_lossy(&output.stderr).contains("private-fixture-input"));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("private-fixture-input"));
}

fn enrolled(fixture: &Fixture) -> (TrustStore, Value) {
    let (trust, primary) = trust(fixture);
    let paper = PaperPhrase::generate().unwrap();
    let change = RecoveryChange::new(&trust, vec![], Some(&paper)).unwrap();
    let trust = recovery::apply(
        &fixture.store(),
        &change,
        RecoveryAuthorization::SigningKey {
            role: Role::Primary,
            signature: &primary.sign(recovery::RECOVERY_NAMESPACE, &change.canonical_bytes()),
        },
        &[],
        RecoveryConfirmation {
            paper: Some(&paper),
            registration: None,
        },
        2,
    )
    .unwrap();
    let (mut pending, options) = PendingRegistration::start(&trust, 45081).unwrap();
    let options = serde_json::to_value(options).unwrap();
    let registered = pending
        .finish(
            &trust,
            &registration(&options["publicKey"]["challenge"], "http://localhost:45081"),
        )
        .unwrap();
    let change = RecoveryChange::enroll_passkey(&trust, &registered).unwrap();
    let signature = primary.sign(recovery::RECOVERY_NAMESPACE, &change.canonical_bytes());
    assert!(
        recovery::apply(
            &fixture.store(),
            &change,
            RecoveryAuthorization::SigningKey {
                role: Role::Primary,
                signature: &signature
            },
            &[],
            RecoveryConfirmation::default(),
            3
        )
        .is_err(),
        "registration possession cannot be omitted"
    );
    let enrolled = recovery::apply(
        &fixture.store(),
        &change,
        RecoveryAuthorization::SigningKey {
            role: Role::Primary,
            signature: &signature,
        },
        &[],
        RecoveryConfirmation {
            paper: None,
            registration: Some(&registered),
        },
        3,
    )
    .unwrap();
    assert_eq!(enrolled.paper_verifier, trust.paper_verifier);
    (enrolled, options["publicKey"]["user"]["id"].clone())
}

fn assertion(challenge: &Value, user: &Value, fault: &str, counter: u32) -> PublicKeyCredential {
    let mut auth_data = Sha256::digest(b"localhost").to_vec();
    auth_data.push(if fault == "uv" { 0x19 } else { 0x1d }); // UP, UV, BE and BS.
    auth_data.extend_from_slice(&counter.to_be_bytes());
    let client = serde_json::to_vec(&json!({"type":"webauthn.get", "challenge": if fault == "challenge" {encoded(vec![1;32])} else {challenge.clone()}, "origin":if fault == "origin" {"http://localhost:45082"} else {"http://localhost:45081"}, "crossOrigin":false})).unwrap();
    let mut payload = auth_data.clone();
    payload.extend_from_slice(&Sha256::digest(&client));
    let key = PKey::private_key_from_pem(PRIVATE_KEY.as_bytes()).unwrap();
    let mut signer = Signer::new(MessageDigest::sha256(), &key).unwrap();
    signer.update(&payload).unwrap();
    let mut signature = signer.sign_to_vec().unwrap();
    if fault == "signature" {
        signature[10] ^= 1;
    }
    serde_json::from_value(json!({"id":if fault=="credential" {"AQID"} else {CREDENTIAL_ID}, "rawId":if fault=="credential" {"AQID"} else {CREDENTIAL_ID}, "type":"public-key", "response":{"authenticatorData":encoded(auth_data), "clientDataJSON":encoded(client), "signature":encoded(signature), "userHandle":if fault=="user" {encoded(vec![0;16])} else {user.clone()}}, "extensions":{}})).unwrap()
}

#[test]
fn passkey_alone_replaces_signing_key_and_preserves_paper() {
    let fixture = Fixture::new();
    let (trust, user) = enrolled(&fixture);
    let replacement = SshKey::generate(&fixture, "replacement");
    let change = RecoveryChange::new(
        &trust,
        vec![ReplacementKey {
            role: Role::Primary,
            public_key: replacement.public_key(),
        }],
        None,
    )
    .unwrap();
    let proofs = [ReplacementProof {
        role: Role::Primary,
        signature: replacement.sign(recovery::POSSESSION_NAMESPACE, &change.canonical_bytes()),
    }];
    for fault in [
        "uv",
        "origin",
        "challenge",
        "credential",
        "user",
        "signature",
    ] {
        let (mut pending, options) = PendingAuthentication::start(&trust, &change, 45081).unwrap();
        let options = serde_json::to_value(options).unwrap();
        assert!(
            pending
                .finish(&assertion(
                    &options["publicKey"]["challenge"],
                    &user,
                    fault,
                    2
                ))
                .is_err(),
            "{fault}"
        );
        assert!(
            pending
                .finish(&assertion(&options["publicKey"]["challenge"], &user, "", 2))
                .is_err(),
            "failed attempts are consumed"
        );
        assert_eq!(
            TrustStore::load(&fixture.store()).unwrap(),
            Some(trust.clone())
        );
    }
    let (mut pending, options) = PendingAuthentication::start(&trust, &change, 45081).unwrap();
    let response = assertion(
        &serde_json::to_value(options).unwrap()["publicKey"]["challenge"],
        &user,
        "",
        2,
    );
    let approval = pending.finish(&response).unwrap();
    assert!(pending.finish(&response).is_err());
    let recovered = recovery::apply(
        &fixture.store(),
        &change,
        RecoveryAuthorization::Passkey(approval),
        &proofs,
        RecoveryConfirmation::default(),
        4,
    )
    .unwrap();
    assert_eq!(recovered.paper_verifier, trust.paper_verifier);
    assert_eq!(
        recovered.admission_key().unwrap().public_key,
        replacement.public_key()
    );
    assert_ne!(
        recovered.passkey, trust.passkey,
        "counter metadata publishes atomically"
    );
    assert!(PendingAuthentication::start(&recovered, &change, 45081).is_err());
}

#[test]
fn approval_binds_exact_change_and_passkey_remains_reusable() {
    let fixture = Fixture::new();
    let (trust, user) = enrolled(&fixture);
    let next = PaperPhrase::generate().unwrap();
    let change = RecoveryChange::new(&trust, vec![], Some(&next)).unwrap();
    let (mut pending, options) = PendingAuthentication::start(&trust, &change, 45081).unwrap();
    let approval = pending
        .finish(&assertion(
            &serde_json::to_value(options).unwrap()["publicKey"]["challenge"],
            &user,
            "",
            2,
        ))
        .unwrap();
    let different = PaperPhrase::generate().unwrap();
    let altered = RecoveryChange::new(&trust, vec![], Some(&different)).unwrap();
    assert!(
        recovery::apply(
            &fixture.store(),
            &altered,
            RecoveryAuthorization::Passkey(approval),
            &[],
            RecoveryConfirmation {
                paper: Some(&different),
                registration: None
            },
            4
        )
        .is_err()
    );
    assert_eq!(
        TrustStore::load(&fixture.store()).unwrap(),
        Some(trust.clone())
    );
    let mut current = trust;
    for counter in 2..=3 {
        let next = PaperPhrase::generate().unwrap();
        let change = RecoveryChange::new(&current, vec![], Some(&next)).unwrap();
        let (mut pending, options) =
            PendingAuthentication::start(&current, &change, 45081).unwrap();
        let approval = pending
            .finish(&assertion(
                &serde_json::to_value(options).unwrap()["publicKey"]["challenge"],
                &user,
                "",
                counter,
            ))
            .unwrap();
        let updated = recovery::apply(
            &fixture.store(),
            &change,
            RecoveryAuthorization::Passkey(approval),
            &[],
            RecoveryConfirmation {
                paper: Some(&next),
                registration: None,
            },
            u64::from(counter) + 2,
        )
        .unwrap();
        assert_eq!(updated.keys, current.keys);
        assert_eq!(updated.paper_verifier, change.next_verifier);
        assert!(
            updated
                .retired_paper_verifiers
                .contains(current.paper_verifier.as_ref().unwrap())
        );
        assert_eq!(
            updated.passkey.as_ref().unwrap().fingerprint().unwrap(),
            current.passkey.as_ref().unwrap().fingerprint().unwrap()
        );
        current = updated;
    }
    assert_eq!(TrustStore::load(&fixture.store()).unwrap(), Some(current));
}
