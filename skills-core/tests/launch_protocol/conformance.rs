//! Closed conformance authority and bounded exact-byte report framing.

use super::*;
use louiselm_skills::{
    conformance::{
        MAX_REPORT_BYTES,
        admission::{Attendance, Condition},
    },
    launch_protocol::{
        CONFORMANCE_REPORT_CHUNK_BYTES, CONFORMANCE_REPORT_CHUNK_SCHEMA, ConformanceAuthorization,
        ConformanceReportChunk, ConformanceWaiver,
    },
};

fn waiver(request: &LaunchRequest) -> ConformanceAuthorization {
    ConformanceAuthorization {
        attendance: Attendance::Interactive,
        waiver: Some(ConformanceWaiver {
            session_id: request.session_id.clone(),
            request_digest: request.digest().to_string(),
            operator_uid: 1000,
            condition: Condition::Missing,
            expires_at_ms: 1500,
            receipt_digest: digest(b"broker-approved-waiver"),
        }),
    }
}

#[test]
fn conformance_waivers_bind_exact_authenticated_launch_and_exclusive_expiry() {
    let request = launch_request();
    let mut authorization = launch_authorization(&request);
    authorization.conformance = waiver(&request);
    assert!(authorization.validate_for(&request, 1000, 1499).is_ok());
    assert!(authorization.validate_for(&request, 1000, 1500).is_err());
    // Historical receipt inspection does not renew this expired approval.
    assert!(authorization.validate().is_ok());

    for change in [
        "session",
        "request",
        "operator",
        "root",
        "condition",
        "digest",
        "unattended",
    ] {
        let mut changed = authorization.clone();
        let waiver = changed.conformance.waiver.as_mut().unwrap();
        match change {
            "session" => waiver.session_id = "foreign-session".into(),
            "request" => waiver.request_digest = digest(b"different-envelope-revision"),
            "operator" => waiver.operator_uid = 1001,
            "root" => waiver.operator_uid = 0,
            "condition" => waiver.condition = Condition::ContainmentFailure,
            "digest" => waiver.receipt_digest = "not-a-receipt".into(),
            "unattended" => changed.conformance.attendance = Attendance::Unattended,
            _ => unreachable!(),
        }
        assert!(
            changed.validate_for(&request, 1000, 1499).is_err(),
            "{change}"
        );
    }
}

#[test]
fn conformance_authority_cannot_be_omitted_or_smuggled_into_launch_requests() {
    let request = launch_request();
    let mut value = serde_json::to_value(launch_authorization(&request)).unwrap();
    value.as_object_mut().unwrap().remove("conformance");
    assert!(serde_json::from_value::<LaunchAuthorization>(value).is_err());
    for value in [
        serde_json::json!({"waiver": null}),
        serde_json::json!({"attendance": "interactive", "waiver": null, "enforced": false}),
    ] {
        assert!(serde_json::from_value::<ConformanceAuthorization>(value).is_err());
    }
    let mut value = serde_json::to_value(request).unwrap();
    value["conformance"] = serde_json::json!({"attendance": "interactive", "waiver": null});
    assert!(serde_json::from_value::<LaunchRequest>(value).is_err());
}

fn chunk() -> ConformanceReportChunk {
    ConformanceReportChunk {
        schema: CONFORMANCE_REPORT_CHUNK_SCHEMA.into(),
        receipt_digest: digest(b"exact-signed-receipt"),
        offset: 0,
        total_bytes: MAX_REPORT_BYTES,
        bytes: vec![255; CONFORMANCE_REPORT_CHUNK_BYTES],
    }
}

#[test]
fn conformance_fragments_fit_packets_and_preserve_every_byte() {
    let first = chunk();
    let bytes = first.canonical_bytes().unwrap();
    assert!(bytes.len() < MAX_PROTOCOL_MESSAGE_BYTES);
    assert_eq!(
        ConformanceReportChunk::parse_canonical(&bytes).unwrap(),
        first
    );
    let mut last = first;
    last.offset = MAX_REPORT_BYTES - CONFORMANCE_REPORT_CHUNK_BYTES;
    last.total_bytes = last.offset + 1;
    last.bytes = vec![0];
    assert_eq!(
        ConformanceReportChunk::parse_canonical(&last.canonical_bytes().unwrap()).unwrap(),
        last
    );
}

#[test]
fn conformance_fragments_reject_unbounded_noncanonical_or_contradictory_input() {
    for change in [
        "schema",
        "digest",
        "empty",
        "oversized",
        "offset",
        "end",
        "short",
        "long",
    ] {
        let mut value = chunk();
        match change {
            "schema" => value.schema.push('x'),
            "digest" => value.receipt_digest = "foreign".into(),
            "empty" => value.total_bytes = 0,
            "oversized" => value.total_bytes = MAX_REPORT_BYTES + 1,
            "offset" => value.offset = 1,
            "end" => value.offset = value.total_bytes,
            "short" => {
                value.bytes.pop();
            }
            "long" => value.bytes.push(0),
            _ => unreachable!(),
        }
        assert!(value.canonical_bytes().is_err(), "{change}");
        assert!(
            ConformanceReportChunk::parse_canonical(&serde_json::to_vec(&value).unwrap()).is_err(),
            "{change}"
        );
    }
    let mut bytes = chunk().canonical_bytes().unwrap();
    bytes.push(b'\n');
    assert!(ConformanceReportChunk::parse_canonical(&bytes).is_err());
    let mut value = serde_json::to_value(chunk()).unwrap();
    value["unexpected"] = true.into();
    assert!(ConformanceReportChunk::parse_canonical(&serde_json::to_vec(&value).unwrap()).is_err());
    assert!(
        ConformanceReportChunk::parse_canonical(&vec![b' '; MAX_PROTOCOL_MESSAGE_BYTES + 1])
            .is_err()
    );
}
