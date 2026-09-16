//! Detailed status is closed, bounded and descriptive, never evidence input.
use super::*;
use louiselm_skills::{
    launch_protocol::{FreshnessBasis, PostureStatus},
    posture::{DimensionName, DimensionState, EvidenceKind, FailureCode},
};

#[test]
fn posture_rejects_contradictory_or_unbounded_explanations() {
    let valid = status_posture(PostureSummary::FullyVerified);
    valid.validate().unwrap();
    let invalidations: &[fn(&mut PostureStatus)] = &[
        |p| p.state = PostureSummary::Unverified,
        |p| p.dimensions.swap(0, 1),
        |p| p.dimensions[0].dimension = DimensionName::Network,
        |p| p.dimensions[0].state = DimensionState::Failed,
        |p| p.dimensions[0].failure_code = Some(FailureCode::RuntimeDrift),
        |p| p.dimensions[0].evidence.clear(),
        |p| p.dimensions[0].evidence[0].kind = EvidenceKind::BrokerReceipt,
        |p| p.dimensions[0].evidence[0].id = "/private/key".into(),
        |p| p.dimensions[0].evidence = vec![p.dimensions[0].evidence[0].clone(); 17],
        |p| p.dimensions[0].next_action.detail = "run untrusted payload".into(),
        |p| p.dimensions[0].freshness.basis = FreshnessBasis::Missing,
        |p| p.dimensions[0].freshness.basis = FreshnessBasis::Invalidated,
        |p| p.dimensions[0].freshness.last_verified_at_ms = None,
        |p| p.provider_disclosure_notice.clear(),
        |p| p.embedded_instructions_notice.clear(),
    ];
    for (index, invalidate) in invalidations.iter().enumerate() {
        let mut candidate = valid.clone();
        invalidate(&mut candidate);
        assert!(
            candidate.validate().is_err(),
            "accepted invalid explanation {index}"
        );
    }
    let mut missing = status_posture(PostureSummary::Unverified);
    missing.dimensions[0].freshness.last_verified_at_ms = Some(1000);
    assert!(
        missing.validate().is_err(),
        "missing evidence cannot invent a successful check"
    );
}

#[test]
fn detailed_status_requires_exactly_six_closed_dimensions() {
    let status = SessionStatus::compose(
        supervisor(SessionState::Running),
        status_posture(PostureSummary::Unverified),
        ConformanceEvidence::Unevaluated,
        unavailable_recovery(),
        vec![],
    )
    .unwrap();
    let json = serde_json::to_value(&status).unwrap();
    assert_eq!(
        json["recovery"],
        serde_json::json!({
            "state": "unavailable", "reason": "evidence_missing"
        })
    );
    assert_eq!(
        json["posture"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        [
            "dimensions",
            "embedded_instructions_notice",
            "provider_disclosure_notice",
            "state"
        ]
    );
    assert_eq!(
        json["posture"]["dimensions"][2],
        serde_json::json!({
            "dimension": "runtime",
            "state": "failed",
            "requirement": "measured_runtime",
            "evidence": [],
            "failure_code": "evidence_missing",
            "next_action": {
                "id": "collect_trusted_evidence",
                "detail": "Collect trusted evidence for this dimension before launch."
            },
            "freshness": {"basis": "missing", "last_verified_at_ms": null}
        })
    );
    let mut missing = json.clone();
    missing["posture"]["dimensions"]
        .as_array_mut()
        .unwrap()
        .pop();
    assert!(serde_json::from_value::<SessionStatus>(missing).is_err());
    let mut extra = json.clone();
    extra["posture"]["dimensions"][0]["environment"] = serde_json::json!({"secret":"payload"});
    assert!(serde_json::from_value::<SessionStatus>(extra).is_err());
    let mut action = json;
    action["posture"]["dimensions"][0]["next_action"]["command"] = "untrusted".into();
    assert!(serde_json::from_value::<SessionStatus>(action).is_err());
    assert_eq!(
        SessionStatus::parse_canonical(&status.canonical_bytes()).unwrap(),
        status
    );
}

#[test]
fn pending_only_describes_startup_and_waived_never_means_verified() {
    let mut pending = status_posture(PostureSummary::Unverified);
    pending.state = PostureSummary::Pending;
    pending.validate().unwrap();
    let mut starting = supervisor(SessionState::Starting);
    starting.launcher_head.as_mut().unwrap().sequence = 0;
    starting.broker_head.as_mut().unwrap().sequence = 0;
    SessionStatus::compose(
        starting,
        pending.clone(),
        ConformanceEvidence::Unevaluated,
        unavailable_recovery(),
        vec![],
    )
    .unwrap();
    assert!(
        SessionStatus::compose(
            supervisor(SessionState::Running),
            pending,
            ConformanceEvidence::Unevaluated,
            unavailable_recovery(),
            vec![]
        )
        .is_err()
    );
    let mut waived = status_posture(PostureSummary::Waived);
    waived.validate().unwrap();
    assert_eq!(waived.state, PostureSummary::Waived);
    waived.state = PostureSummary::FullyVerified;
    assert!(waived.validate().is_err());
}
