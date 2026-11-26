//! Closed voice-job request boundaries use only synthetic data.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Fixture setup and expected refusals are asserted directly."
)]

use super::{
    PhoneControlError, Request,
    voice_job::{VoiceJobCommand, voice_job_command, voice_source_digest},
};
use crate::{MAX_CAPTURE_BYTES, hex::lower_hex};

fn request(action: u64, payload: &[u8]) -> Request {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../android/app/src/test/resources/phone-control-v1.json"
    ))
    .unwrap();
    Request {
        grant_id: "11".repeat(16),
        grant_revision: 1,
        period_id: "22".repeat(16),
        receiver_identity: "33".repeat(32),
        controller_id: "44".repeat(16),
        subject_id: "55".repeat(16),
        action,
        payload: lower_hex(payload),
        payload_hash: "66".repeat(32),
        operation_id: "77".repeat(16),
        expected_revision: 0,
        interaction: 0,
        signature: fixture["request"]["signature_der"].as_str().unwrap().into(),
    }
}

fn admission(length: u64) -> Vec<u8> {
    let mut bytes = vec![1];
    bytes.extend([1; 16]);
    bytes.extend([2; 32]);
    bytes.extend(100_u64.to_be_bytes());
    bytes.extend(50_u64.to_be_bytes());
    bytes.extend(length.to_be_bytes());
    bytes.extend([3; 32]);
    bytes.extend([4; 32]);
    bytes.extend([5; 32]);
    bytes
}

#[test]
fn admission_binds_exact_audio_and_draft_without_renewing_the_lease() {
    assert_eq!(
        voice_source_digest("/project", "fixture").unwrap(),
        "9aeebed9be9be18077fc10a85110e46761e00c7657bd0e5cdea5ea57ace9013d"
    );
    let request = request(83, &admission(MAX_CAPTURE_BYTES));
    let VoiceJobCommand::Admit {
        id,
        digest,
        recorded_at_ms,
        duration_ms,
        bytes,
        draft,
        revision,
        source_digest,
    } = voice_job_command(&request).unwrap()
    else {
        panic!("expected admission")
    };
    assert_eq!(id, uuid::Uuid::from_bytes([1; 16]).to_string());
    assert_eq!(digest, "02".repeat(32));
    assert_eq!(
        (recorded_at_ms, duration_ms, bytes),
        (100, 50, MAX_CAPTURE_BYTES)
    );
    assert_eq!(draft, "03".repeat(32));
    assert_eq!(revision, "04".repeat(32));
    assert_eq!(source_digest, "05".repeat(32));
    let mut changed = request;
    changed.interaction = 1;
    assert_eq!(
        voice_job_command(&changed).unwrap_err(),
        PhoneControlError::Malformed
    );
}

#[test]
fn job_actions_reject_wrong_subject_oversize_and_malformed_bytes() {
    let mut valid = request(83, &admission(1));
    valid.subject_id = "00".repeat(16);
    assert_eq!(
        voice_job_command(&valid).unwrap_err(),
        PhoneControlError::Malformed
    );
    assert_eq!(
        voice_job_command(&request(83, &admission(MAX_CAPTURE_BYTES + 1))).unwrap_err(),
        PhoneControlError::Malformed
    );
    assert_eq!(
        voice_job_command(&request(83, &admission(0))).unwrap_err(),
        PhoneControlError::Malformed
    );
    assert_eq!(
        voice_job_command(&request(83, &[1, 2])).unwrap_err(),
        PhoneControlError::Malformed
    );
    assert!(matches!(
        voice_job_command(&request(84, &[5; 16])).unwrap(),
        VoiceJobCommand::Cancel { .. }
    ));
    assert!(matches!(
        voice_job_command(&request(85, &[5; 32])).unwrap(),
        VoiceJobCommand::Retry { .. }
    ));
    assert_eq!(
        voice_job_command(&request(84, &[5; 15])).unwrap_err(),
        PhoneControlError::Malformed
    );
    assert_eq!(
        voice_job_command(&request(86, &[5; 16])).unwrap_err(),
        PhoneControlError::Unsupported
    );
}
