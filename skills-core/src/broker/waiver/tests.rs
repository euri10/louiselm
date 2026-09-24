#![allow(
    clippy::unwrap_used,
    reason = "Disposable fixtures assert exact transaction outcomes."
)]
use super::*;

#[test]
fn approved_receipt_survives_restart_and_retry_cannot_revive_revocation() {
    let root = tempfile::tempdir().unwrap();
    let context = context();
    let store = Waivers::open(root.path()).unwrap();
    let plan = store
        .control(
            &context,
            &Request::Plan {
                proposal: proposal(),
            },
            100,
        )
        .unwrap()
        .plan
        .unwrap();
    assert!(store.current("session", 100).unwrap().is_none());
    let apply = Request::Apply {
        plan_digest: plan.digest.clone(),
    };
    let first = store.control(&context, &apply, 110).unwrap();
    drop(store);
    let store = Waivers::open(root.path()).unwrap();
    assert_eq!(store.control(&context, &apply, 120).unwrap(), first);
    let receipt = first.receipt.unwrap();
    store
        .control(
            &context,
            &Request::Revoke {
                receipt_digest: receipt.digest.clone(),
            },
            120,
        )
        .unwrap();
    assert!(!store.control(&context, &apply, 130).unwrap().active);
    assert!(store.current("session", 130).unwrap().is_none());
    assert_eq!(
        store
            .control(
                &context,
                &Request::Result {
                    plan_digest: plan.digest
                },
                210
            )
            .unwrap()
            .receipt,
        Some(receipt)
    );
}

#[test]
fn stale_preview_and_conflicting_retry_do_not_write_approval() {
    let root = tempfile::tempdir().unwrap();
    let store = Waivers::open(root.path()).unwrap();
    let mut context = context();
    let plan = store
        .control(
            &context,
            &Request::Plan {
                proposal: proposal(),
            },
            100,
        )
        .unwrap()
        .plan
        .unwrap();
    let mut conflict = proposal();
    conflict.rationale = "different intent".into();
    assert_eq!(
        store.control(&context, &Request::Plan { proposal: conflict }, 110),
        Err(WaiverError::Conflict)
    );
    context.receipt_head = crate::Digest::of(b"later lifecycle").to_string();
    assert_eq!(
        store.control(
            &context,
            &Request::Apply {
                plan_digest: plan.digest
            },
            120
        ),
        Err(WaiverError::StalePlan)
    );
    assert!(store.current("session", 130).unwrap().is_none());
}

#[test]
fn unattended_and_containment_failures_cannot_be_planned() {
    let mut context = context();
    context.attendance = Attendance::Unattended;
    assert_eq!(
        validate_plan(&context, &proposal(), 100),
        Err(WaiverError::Unattended)
    );
    context.attendance = Attendance::Interactive;
    context.condition = Condition::ContainmentFailure;
    assert_eq!(
        validate_plan(&context, &proposal(), 100),
        Err(WaiverError::NotWaivable)
    );
}

#[test]
fn proposal_binds_the_current_condition_and_exclusive_expiry() {
    let context = context();
    assert_eq!(validate_plan(&context, &proposal(), 100), Ok(()));
    assert_eq!(
        validate_plan(&context, &proposal(), 200),
        Err(WaiverError::Expired)
    );
    let mut changed = proposal();
    changed.condition = Condition::Stale;
    assert_eq!(
        validate_plan(&context, &changed, 100),
        Err(WaiverError::StalePlan)
    );
}

#[test]
fn receipts_cannot_be_read_or_replayed_under_another_launch_identity() {
    let root = tempfile::tempdir().unwrap();
    let store = Waivers::open(root.path()).unwrap();
    let original = context();
    let plan = store
        .control(
            &original,
            &Request::Plan {
                proposal: proposal(),
            },
            100,
        )
        .unwrap()
        .plan
        .unwrap();
    let apply = Request::Apply {
        plan_digest: plan.digest.clone(),
    };
    store.control(&original, &apply, 110).unwrap();
    for field in 0..5 {
        let mut foreign = original.clone();
        match field {
            0 => foreign.operator_uid += 1,
            1 => foreign.run_id = "other-run".into(),
            2 => foreign.authorization_id = "other-authorization".into(),
            3 => foreign.request_digest = crate::Digest::of(b"other-request").to_string(),
            _ => foreign.envelope_revision += 1,
        }
        assert!(store.control(&foreign, &apply, 120).is_err());
        assert!(
            store
                .control(
                    &foreign,
                    &Request::Result {
                        plan_digest: plan.digest.clone()
                    },
                    120
                )
                .is_err()
        );
    }
}

#[test]
fn expiry_and_competing_plans_never_renew_or_replace_an_approval() {
    let root = tempfile::tempdir().unwrap();
    let store = Waivers::open(root.path()).unwrap();
    let context = context();
    let first = store
        .control(
            &context,
            &Request::Plan {
                proposal: proposal(),
            },
            100,
        )
        .unwrap()
        .plan
        .unwrap();
    let mut second = proposal();
    second.request_id = "competing".into();
    let second = store
        .control(&context, &Request::Plan { proposal: second }, 100)
        .unwrap()
        .plan
        .unwrap();
    let apply = Request::Apply {
        plan_digest: first.digest,
    };
    let original = store.control(&context, &apply, 110).unwrap().receipt;
    assert_eq!(
        store.control(
            &context,
            &Request::Apply {
                plan_digest: second.digest
            },
            120
        ),
        Err(WaiverError::StalePlan)
    );
    assert!(store.control(&context, &apply, 199).unwrap().active);
    let expired = store.control(&context, &apply, 200).unwrap();
    assert!(!expired.active);
    assert_eq!(expired.receipt, original);
    assert!(store.current("session", 200).unwrap().is_none());
}

fn context() -> Context {
    Context {
        preparation: None,
        session_id: "session".into(),
        run_id: "run".into(),
        authorization_id: "authorization".into(),
        request_digest: crate::Digest::of(b"launch").to_string(),
        envelope_revision: 1,
        operator_uid: 1000,
        attendance: Attendance::Interactive,
        condition: Condition::Missing,
        receipt_head: crate::Digest::of(b"park").to_string(),
    }
}

fn proposal() -> Proposal {
    Proposal {
        request_id: "waive-1".into(),
        condition: Condition::Missing,
        rationale: "Inspect this Session while certification is unavailable".into(),
        expires_at_ms: 200,
    }
}
