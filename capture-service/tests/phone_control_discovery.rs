//! Discovery requests use the same signed authority as Session reads.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "test fixtures and assertions"
)]

use aws_lc_rs::{
    rand::SystemRandom,
    signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair},
};
use louiselm_capture::{PhoneControlError, verify_phone_control_frame};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

fn unhex(text: &str) -> Vec<u8> {
    text.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
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

fn frame(action: u8) -> (Value, String) {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../android/app/src/test/resources/phone-control-v1.json"
    ))
    .unwrap();
    let mut proof = fixture["proof"].clone();
    let fields = proof.as_object_mut().unwrap();
    let signature = fields.remove("signature_der").unwrap();
    fields.insert("signature".into(), signature);
    fields.remove("signature_raw");
    fields.remove("transcript");
    let receiver = proof["receiver_identity"].as_str().unwrap().to_owned();
    let controller = if action == 48 {
        "00".repeat(16)
    } else {
        "22".repeat(16)
    };
    let subject = "00".repeat(16);
    let payload = format!("{}32", "00".repeat(16));
    let digest = hex(&Sha256::digest(unhex(&payload)));
    let request = &fixture["request"];
    let mut transcript = b"louiselm-phone-control\0\x01\x02".to_vec();
    transcript.extend(unhex(request["grant_id"].as_str().unwrap()));
    transcript.extend(7_u64.to_be_bytes());
    transcript.extend(unhex(request["period_id"].as_str().unwrap()));
    transcript.extend(unhex(&receiver));
    transcript.extend(unhex(&controller));
    transcript.extend(unhex(&subject));
    transcript.push(action);
    transcript.extend(unhex(&digest));
    transcript.extend(unhex(request["operation_id"].as_str().unwrap()));
    transcript.extend(0_u64.to_be_bytes());
    transcript.push(1);
    let key = EcdsaKeyPair::from_pkcs8(
        &ECDSA_P256_SHA256_ASN1_SIGNING,
        &unhex(fixture["keys"]["working"]["pkcs8"].as_str().unwrap()),
    )
    .unwrap();
    let signature = key.sign(&SystemRandom::new(), &transcript).unwrap();
    (
        json!({"v":1,"type":if action == 48 { "list_controllers" } else { "list_sessions" },
            "controller_id":controller,
            "grant":{"grant_id":proof["grant_id"],"grant_revision":7,
                "enrolled_key":fixture["keys"]["enrolled"]["sec1"],
                "projects":["/work"],"agents":["codex"],"groups":["read"]},
            "period":{"period_id":request["period_id"],"challenge_id":proof["challenge_id"],
                "idle_expires_at_ms":2000,"absolute_expires_at_ms":3000},
            "proof":proof,
            "request":{"grant_id":request["grant_id"],"grant_revision":7,"period_id":request["period_id"],
                "receiver_identity":receiver,"controller_id":controller,"subject_id":subject,
                "action":action,"payload":payload,"payload_hash":digest,"operation_id":request["operation_id"],
                "expected_revision":0,"interaction":1,"signature":hex(signature.as_ref())}
        }),
        receiver,
    )
}

#[test]
fn discovery_verifies_signed_request_and_rejects_altered_target() {
    for action in [48, 49] {
        let (mut value, receiver) = frame(action);
        let verify = |value: &Value| {
            verify_phone_control_frame(&serde_json::to_vec(value).unwrap(), &receiver)
        };
        assert_eq!(verify(&value), Ok(()));
        value["request"]["payload"] = json!(format!("{}01", "00".repeat(16)));
        assert_eq!(verify(&value), Err(PhoneControlError::Unauthenticated));
        let (mut value, _) = frame(action);
        value["request"]["subject_id"] = json!("44".repeat(16));
        assert_eq!(verify(&value), Err(PhoneControlError::Malformed));
    }
}
