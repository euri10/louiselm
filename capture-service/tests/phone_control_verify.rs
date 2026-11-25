//! Controller verifier checks the shared cross-language signing vectors.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "test fixtures and assertions"
)]

use std::{
    io::Write,
    process::{Command, Stdio},
};

use aws_lc_rs::{
    rand::SystemRandom,
    signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair},
};
use louiselm_capture::{PhoneControlError, TlsIdentity, verify_phone_control_frame};
use serde_json::{Value, json};

const FIXTURE: &str = include_str!("../../android/app/src/test/resources/phone-control-v1.json");

fn frame() -> (Value, String) {
    let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    let mut proof = fixture["proof"].clone();
    let proof_fields = proof.as_object_mut().unwrap();
    let signature = proof_fields.remove("signature_der").unwrap();
    proof_fields.insert("signature".into(), signature);
    proof_fields.remove("signature_raw");
    proof_fields.remove("transcript");
    let mut request = fixture["request"].clone();
    let request_fields = request.as_object_mut().unwrap();
    let signature = request_fields.remove("signature_der").unwrap();
    request_fields.insert("signature".into(), signature);
    for name in ["before", "limit", "transcript", "signature_raw"] {
        request_fields.remove(name);
    }
    let receiver = proof["receiver_identity"].as_str().unwrap().to_owned();
    (
        json!({
            "v": 1, "type": "read_session",
            "controller_id": request["controller_id"],
            "grant": {
                "grant_id": proof["grant_id"], "grant_revision": proof["grant_revision"],
                "enrolled_key": fixture["keys"]["enrolled"]["sec1"],
                "projects": ["/work"], "agents": ["codex"], "groups": ["read"]
            },
            "period": {
                "period_id": request["period_id"], "challenge_id": proof["challenge_id"],
                "idle_expires_at_ms": 2000, "absolute_expires_at_ms": 3000
            },
            "proof": proof, "request": request
        }),
        receiver,
    )
}

fn verify(frame: &Value, receiver: &str) -> Result<(), PhoneControlError> {
    verify_phone_control_frame(&serde_json::to_vec(frame).unwrap(), receiver)
}

#[test]
fn authenticates_fixture_and_rejects_altered_bindings() {
    let (good, receiver) = frame();
    assert_eq!(verify(&good, &receiver), Ok(()));
    let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    let mut wrong_key = good.clone();
    wrong_key["grant"]["enrolled_key"] = fixture["keys"]["wrong"]["sec1"].clone();
    assert_eq!(
        verify(&wrong_key, &receiver),
        Err(PhoneControlError::Unauthenticated)
    );
    let cases = [
        (
            "/request/payload",
            json!("00000000000000000015"),
            PhoneControlError::Unauthenticated,
        ),
        (
            "/request/subject_id",
            json!("ffffffffffffffffffffffffffffffff"),
            PhoneControlError::Unauthenticated,
        ),
        (
            "/request/grant_revision",
            json!(8),
            PhoneControlError::Unauthenticated,
        ),
        (
            "/proof/challenge",
            json!("00".repeat(32)),
            PhoneControlError::Unauthenticated,
        ),
        (
            "/period/challenge_id",
            json!("ff".repeat(16)),
            PhoneControlError::Unauthenticated,
        ),
        (
            "/request/period_id",
            json!("ff".repeat(16)),
            PhoneControlError::Unauthenticated,
        ),
        (
            "/grant/enrolled_key",
            json!("04".to_owned() + &"00".repeat(64)),
            PhoneControlError::Malformed,
        ),
    ];
    for (path, value, error) in cases {
        let mut bad = good.clone();
        *bad.pointer_mut(path).unwrap() = value;
        assert_eq!(verify(&bad, &receiver), Err(error), "{path}");
    }
    assert_eq!(
        verify(&good, &"ff".repeat(32)),
        Err(PhoneControlError::Unauthenticated)
    );
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    bytes
        .iter()
        .flat_map(|byte| {
            [
                DIGITS[usize::from(byte >> 4)],
                DIGITS[usize::from(byte & 15)],
            ]
        })
        .map(char::from)
        .collect()
}

fn unhex(text: &str) -> Vec<u8> {
    text.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

fn sign_for_receiver(frame: &mut Value, receiver: &str) {
    let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    let original = fixture["proof"]["receiver_identity"].as_str().unwrap();
    for (name, key) in [("proof", "enrolled"), ("request", "working")] {
        frame[name]["receiver_identity"] = json!(receiver);
        let encoded = fixture[name]["transcript"]
            .as_str()
            .unwrap()
            .replace(original, receiver);
        let pair = EcdsaKeyPair::from_pkcs8(
            &ECDSA_P256_SHA256_ASN1_SIGNING,
            &unhex(fixture["keys"][key]["pkcs8"].as_str().unwrap()),
        )
        .unwrap();
        let signature = pair.sign(&SystemRandom::new(), &unhex(&encoded)).unwrap();
        frame[name]["signature"] = json!(hex(signature.as_ref()));
    }
}

fn cli(state: &std::path::Path, input: &[u8]) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_louiselm-capture"))
        .args(["phone-control", "verify"])
        .env("LOUISELM_CAPTURE_STATE_DIR", state)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn cli_uses_its_local_receiver_and_emits_only_the_closed_result() {
    let state = tempfile::tempdir().unwrap();
    let tls = state.path().join("louiselm/capture/tls");
    let identity = TlsIdentity::load_or_create(&tls).unwrap();
    let (mut frame, _) = frame();
    sign_for_receiver(&mut frame, identity.public_key_sha256());
    let good = cli(state.path(), &serde_json::to_vec(&frame).unwrap());
    assert!(good.status.success());
    assert_eq!(good.stdout, b"{\"v\":1,\"type\":\"verified\"}\n");
    assert_eq!(good.stderr, [] as [u8; 0]);
    frame["request"]["payload"] = json!("00000000000000000015");
    let bad = cli(state.path(), &serde_json::to_vec(&frame).unwrap());
    assert!(bad.status.success());
    assert_eq!(
        bad.stdout,
        b"{\"v\":1,\"type\":\"error\",\"code\":\"unauthenticated\"}\n"
    );
    assert_eq!(bad.stderr, [] as [u8; 0]);
    let missing = tempfile::tempdir().unwrap();
    let absent = cli(missing.path(), &serde_json::to_vec(&frame).unwrap());
    assert!(absent.status.success());
    assert_eq!(
        absent.stdout,
        b"{\"v\":1,\"type\":\"error\",\"code\":\"unavailable\"}\n"
    );
    assert!(!missing.path().join("louiselm").exists());
}

#[test]
fn refuses_ambiguous_or_unsupported_wire_values() {
    let (good, receiver) = frame();
    for (path, value, code) in [
        ("/request/action", json!(2), PhoneControlError::Unsupported),
        (
            "/request/action",
            json!(9_007_199_254_740_992_u64),
            PhoneControlError::Malformed,
        ),
        (
            "/request/interaction",
            json!(2),
            PhoneControlError::Unsupported,
        ),
        (
            "/request/signature",
            json!("ab".repeat(64)),
            PhoneControlError::Malformed,
        ),
        (
            "/grant/groups",
            json!(["read", "read"]),
            PhoneControlError::Malformed,
        ),
        (
            "/grant/groups",
            json!(["future"]),
            PhoneControlError::Unsupported,
        ),
        (
            "/grant/agents",
            json!(["bad\u{0}agent"]),
            PhoneControlError::Malformed,
        ),
        (
            "/request/expected_revision",
            json!(9_007_199_254_740_992_u64),
            PhoneControlError::Malformed,
        ),
        ("/v", json!(2), PhoneControlError::Unsupported),
        (
            "/v",
            json!(9_007_199_254_740_992_u64),
            PhoneControlError::Malformed,
        ),
        (
            "/type",
            json!("x".repeat(8193)),
            PhoneControlError::Malformed,
        ),
    ] {
        let mut bad = good.clone();
        *bad.pointer_mut(path).unwrap() = value;
        assert_eq!(verify(&bad, &receiver), Err(code), "{path}");
    }
    let mut bad = good.clone();
    bad["request"]["unlocked"] = json!(true);
    assert_eq!(verify(&bad, &receiver), Err(PhoneControlError::Malformed));
    let duplicate = serde_json::to_string(&good).unwrap().replace(
        "\"type\":\"read_session\"",
        "\"type\":\"read_session\",\"type\":\"read_session\"",
    );
    assert_eq!(
        verify_phone_control_frame(duplicate.as_bytes(), &receiver),
        Err(PhoneControlError::Malformed)
    );
}

#[test]
fn signatures_require_minimal_der_and_valid_positive_scalars() {
    let (good, receiver) = frame();
    for signature in [
        "3006020100020101",   // zero scalar
        "3006020180020101",   // negative scalar
        "300702020001020101", // unnecessary leading zero
        "300602010102010100", // trailing byte
        "ff",                 // not DER
    ] {
        let mut bad = good.clone();
        bad["request"]["signature"] = json!(signature);
        assert_eq!(
            verify(&bad, &receiver),
            Err(PhoneControlError::Malformed),
            "{signature}"
        );
    }
}
