//! Cross-language phone-control contract vectors; no authority effects are wired here.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "test fixtures and assertions"
)]

use aws_lc_rs::{
    rand::SystemRandom,
    signature::{
        ECDSA_P256_SHA256_ASN1, ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair, UnparsedPublicKey,
    },
};
use serde_json::Value;
use sha2::{Digest, Sha256};

const FIXTURE: &str = include_str!("../../android/app/src/test/resources/phone-control-v1.json");
const DOMAIN: &[u8] = b"louiselm-phone-control\0";

fn hex(value: &str) -> Vec<u8> {
    assert_eq!(value.len() % 2, 0);
    value
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

fn bytes(object: &Value, key: &str, length: usize) -> Vec<u8> {
    let value = hex(object[key].as_str().unwrap());
    assert_eq!(value.len(), length, "{key}");
    value
}

fn number(object: &Value, key: &str) -> u64 {
    object[key].as_u64().unwrap()
}

fn transcript(message: &Value, kind: &str) -> Vec<u8> {
    let mut out = DOMAIN.to_vec();
    out.extend([1, if kind == "proof" { 1 } else { 2 }]);
    out.extend(bytes(message, "grant_id", 16));
    out.extend(number(message, "grant_revision").to_be_bytes());
    if kind == "proof" {
        out.extend(bytes(message, "enrolled_key_hash", 32));
        out.extend(bytes(message, "receiver_identity", 32));
        out.extend(bytes(message, "challenge_id", 16));
        out.extend(bytes(message, "challenge", 32));
        out.extend(bytes(message, "working_key", 65));
    } else {
        out.extend(bytes(message, "period_id", 16));
        out.extend(bytes(message, "receiver_identity", 32));
        out.extend(bytes(message, "controller_id", 16));
        out.extend(bytes(message, "subject_id", 16));
        out.push(u8::try_from(number(message, "action")).unwrap());
        out.extend(bytes(message, "payload_hash", 32));
        out.extend(bytes(message, "operation_id", 16));
        out.extend(number(message, "expected_revision").to_be_bytes());
        out.push(u8::try_from(number(message, "interaction")).unwrap());
    }
    out
}

fn valid_signature(key: &[u8], message: &[u8], signature: &[u8]) -> bool {
    UnparsedPublicKey::new(&ECDSA_P256_SHA256_ASN1, key)
        .verify(message, signature)
        .is_ok()
}

// This models fixture expectations only. Production must enforce them at the owner boundary.
fn outcome(fixture: &Value, case: &Value) -> &'static str {
    let kind = case["kind"].as_str().unwrap();
    let message = &fixture[kind];
    let key_name = case["key"].as_str().unwrap();
    let key = bytes(&fixture["keys"][key_name], "sec1", 65);
    let signature = hex(message["signature_der"].as_str().unwrap());
    if !valid_signature(&key, &transcript(message, kind), &signature) {
        return "unauthenticated";
    }
    if kind == "proof" {
        if case["current_challenge_id"]
            .as_str()
            .is_some_and(|id| id != message["challenge_id"].as_str().unwrap())
        {
            return "expired";
        }
    } else {
        let payload = hex(case["payload"]
            .as_str()
            .unwrap_or_else(|| message["payload"].as_str().unwrap()));
        if Sha256::digest(payload).as_slice() != bytes(message, "payload_hash", 32) {
            return "unauthenticated";
        }
        if case["current_subject_id"]
            .as_str()
            .is_some_and(|id| id != message["subject_id"].as_str().unwrap())
        {
            return "stale_subject";
        }
    }
    let identity_field = if kind == "proof" {
        "challenge_id"
    } else {
        "operation_id"
    };
    if case["consumed_id"].as_str() == message[identity_field].as_str() {
        return "unauthenticated";
    }
    "ok"
}

#[test]
fn shared_vectors_bind_the_exact_bytes_and_refuse_invalid_contexts() {
    let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    assert_eq!(fixture["version"], 1);
    let enrolled = bytes(&fixture["keys"]["enrolled"], "sec1", 65);
    assert_eq!(
        Sha256::digest(enrolled).as_slice(),
        bytes(&fixture["proof"], "enrolled_key_hash", 32)
    );
    assert_eq!(
        bytes(&fixture["keys"]["working"], "sec1", 65),
        bytes(&fixture["proof"], "working_key", 65)
    );
    for field in ["grant_id", "grant_revision", "receiver_identity"] {
        assert_eq!(fixture["proof"][field], fixture["request"][field]);
    }
    let request = &fixture["request"];
    let mut payload = number(request, "before").to_be_bytes().to_vec();
    payload.extend(
        u16::try_from(number(request, "limit"))
            .unwrap()
            .to_be_bytes(),
    );
    assert_eq!(payload, hex(request["payload"].as_str().unwrap()));
    for kind in ["proof", "request"] {
        let message = &fixture[kind];
        let encoded = transcript(message, kind);
        assert_eq!(encoded, hex(message["transcript"].as_str().unwrap()));
        let key_name = if kind == "proof" {
            "enrolled"
        } else {
            "working"
        };
        let key = bytes(&fixture["keys"][key_name], "sec1", 65);
        let signature = hex(message["signature_der"].as_str().unwrap());
        assert!(valid_signature(&key, &encoded, &signature));
        assert!(!valid_signature(
            &key,
            &encoded,
            &bytes(message, "signature_raw", 64)
        ));
        let mut trailing = signature.clone();
        trailing.push(0);
        assert!(!valid_signature(&key, &encoded, &trailing));
        for index in 0..encoded.len() {
            let mut changed = encoded.clone();
            changed[index] ^= 1;
            assert!(
                !valid_signature(&key, &changed, &signature),
                "{kind} byte {index}"
            );
        }
    }
    for case in fixture["cases"].as_array().unwrap() {
        assert_eq!(
            outcome(&fixture, case),
            case["outcome"].as_str().unwrap(),
            "{}",
            case["name"]
        );
    }
}

#[test]
fn rust_can_sign_with_the_fixture_working_key() {
    let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    let key = &fixture["keys"]["working"];
    let pair = EcdsaKeyPair::from_pkcs8(
        &ECDSA_P256_SHA256_ASN1_SIGNING,
        &hex(key["pkcs8"].as_str().unwrap()),
    )
    .unwrap();
    let message = transcript(&fixture["request"], "request");
    let signature = pair.sign(&SystemRandom::new(), &message).unwrap();
    assert!(valid_signature(
        &bytes(key, "sec1", 65),
        &message,
        signature.as_ref()
    ));
}
