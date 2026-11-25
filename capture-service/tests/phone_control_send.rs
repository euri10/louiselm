//! Signed Session Send fixture shared with Android.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "test fixtures and assertions"
)]

use louiselm_capture::{PhoneControlError, verify_phone_control_frame};
use serde_json::{Value, json};

fn frame() -> (Value, String) {
    let original: Value = serde_json::from_str(include_str!(
        "../../android/app/src/test/resources/phone-control-v1.json"
    ))
    .unwrap();
    let send: Value = serde_json::from_str(include_str!(
        "../../android/app/src/test/resources/phone-control-send-v1.json"
    ))
    .unwrap();
    let mut proof = original["proof"].clone();
    let proof_fields = proof.as_object_mut().unwrap();
    let signature = proof_fields.remove("signature_der").unwrap();
    proof_fields.insert("signature".into(), signature);
    for field in ["signature_raw", "transcript"] {
        proof_fields.remove(field);
    }
    let mut request = original["request"].clone();
    let fields = request.as_object_mut().unwrap();
    for field in [
        "before",
        "limit",
        "transcript",
        "signature_raw",
        "signature_der",
    ] {
        fields.remove(field);
    }
    fields.insert("action".into(), json!(3));
    fields.insert("payload".into(), send["payload"].clone());
    fields.insert("payload_hash".into(), send["payload_hash"].clone());
    fields.insert("signature".into(), send["signature_der"].clone());
    fields.insert("expected_revision".into(), json!(1));
    fields.insert("interaction".into(), json!(1));
    let receiver = proof["receiver_identity"].as_str().unwrap().to_owned();
    (
        json!({"v":1,"type":"send_prompt","controller_id":request["controller_id"],
        "grant":{"grant_id":proof["grant_id"],"grant_revision":proof["grant_revision"],
            "enrolled_key":original["keys"]["enrolled"]["sec1"],"projects":["/work"],
            "agents":["codex"],"groups":["operate_sessions"]},
        "period":{"period_id":request["period_id"],"challenge_id":proof["challenge_id"],
            "idle_expires_at_ms":2000,"absolute_expires_at_ms":3000},"proof":proof,"request":request}),
        receiver,
    )
}

fn verify(frame: &Value, receiver: &str) -> Result<(), PhoneControlError> {
    verify_phone_control_frame(&serde_json::to_vec(frame).unwrap(), receiver)
}

#[test]
fn verifies_the_exact_phone_send_and_rejects_tampering() {
    let (good, receiver) = frame();
    assert_eq!(verify(&good, &receiver), Ok(()));
    for (path, value, expected) in [
        (
            "/request/payload",
            json!("015265706c79"),
            PhoneControlError::Unauthenticated,
        ),
        (
            "/request/subject_id",
            json!("ff".repeat(16)),
            PhoneControlError::Unauthenticated,
        ),
        (
            "/request/expected_revision",
            json!(0),
            PhoneControlError::Malformed,
        ),
        (
            "/request/expected_revision",
            json!(2),
            PhoneControlError::Unauthenticated,
        ),
        (
            "/request/interaction",
            json!(0),
            PhoneControlError::Malformed,
        ),
        (
            "/request/payload",
            json!("02aa"),
            PhoneControlError::Unsupported,
        ),
        (
            "/request/payload",
            json!("01ff"),
            PhoneControlError::Malformed,
        ),
        (
            "/request/payload",
            json!("0100"),
            PhoneControlError::Malformed,
        ),
        (
            "/request/payload",
            json!("01"),
            PhoneControlError::Malformed,
        ),
        (
            "/request/payload",
            json!("01".to_owned() + &"61".repeat(4096)),
            PhoneControlError::Malformed,
        ),
    ] {
        let mut bad = good.clone();
        *bad.pointer_mut(path).unwrap() = value;
        assert_eq!(verify(&bad, &receiver), Err(expected), "{path}");
    }
}
