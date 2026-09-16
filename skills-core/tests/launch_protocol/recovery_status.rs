//! Recovery status is bounded presentation, never evidence or authority.

use super::*;
use louiselm_skills::launch_protocol::{RecoveryReadiness, RecoveryUnavailableReason};

#[test]
fn recovery_status_round_trips_all_states_and_rejects_unbounded_fields() {
    for recovery in [
        unavailable_recovery(),
        RecoveryReadiness::Unavailable {
            reason: RecoveryUnavailableReason::PendingDurability,
        },
        RecoveryReadiness::Expired {},
        RecoveryReadiness::Quarantined {},
        RecoveryReadiness::Ready {
            operation_id: "retention-1".into(),
            expires_at_ms: 60000,
        },
    ] {
        let status = SessionStatus::compose(
            supervisor(SessionState::Running),
            status_posture(PostureSummary::Unverified),
            ConformanceEvidence::Unevaluated,
            recovery,
            vec![],
        )
        .unwrap();
        assert_eq!(
            SessionStatus::parse_canonical(&status.canonical_bytes()).unwrap(),
            status
        );
        let mut json = serde_json::to_value(&status).unwrap();
        json["recovery"]["path"] = "/private".into();
        assert!(serde_json::from_value::<SessionStatus>(json).is_err());
        let mut json = serde_json::to_value(&status).unwrap();
        json.as_object_mut().unwrap().remove("recovery");
        assert!(serde_json::from_value::<SessionStatus>(json).is_err());
    }
    for value in [
        serde_json::json!({"state": "ready", "operation_id": "point", "expires_at_ms": 1, "path": "/private"}),
        serde_json::json!({"state": "unavailable", "reason": "evidence_missing", "environment": "private"}),
        serde_json::json!({"state": "unavailable", "reason": "unsupported-producer-text"}),
        serde_json::json!({"state": "unavailable"}),
    ] {
        assert!(serde_json::from_value::<RecoveryReadiness>(value).is_err());
    }
    for (operation_id, expires_at_ms) in [("", 1), ("/private/path", 1), ("point", 0)] {
        assert!(
            SessionStatus::compose(
                supervisor(SessionState::Running),
                status_posture(PostureSummary::Unverified),
                ConformanceEvidence::Unevaluated,
                RecoveryReadiness::Ready {
                    operation_id: operation_id.into(),
                    expires_at_ms
                },
                vec![],
            )
            .is_err()
        );
    }
}
