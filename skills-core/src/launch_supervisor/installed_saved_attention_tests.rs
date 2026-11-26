//! Read back real broker effects after producer disposal and explicit acceptance.
use super::*;
use crate::broker::attention::{AttentionReason, AttentionSubject, Projection, ProjectionChange};

pub(super) fn wait_saved_pending(run_id: &str) -> Projection {
    let entries = Path::new(STATE).join("authorizations/attention-outbox/entries");
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let published = fs::read_dir(&entries).is_ok_and(|entries| {
            entries.filter_map(Result::ok).any(|entry| {
                fs::read(entry.path())
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<Projection>(&bytes).ok())
                    .is_some_and(|projection| {
                        matches!(projection.change,
                        ProjectionChange::Upsert(condition)
                            if condition.subject == AttentionSubject::Run(run_id.into())
                                && matches!(condition.reason, AttentionReason::PromotionPending(_)))
                    })
            })
        });
        if published {
            return assert_saved_pending(run_id);
        }
        assert!(
            Instant::now() < deadline,
            "saved Attention was not published"
        );
        thread::sleep(Duration::from_millis(25));
    }
}

pub(super) fn assert_saved_pending(run_id: &str) -> Projection {
    let state = Path::new(STATE).join("authorizations");
    let projections: Vec<Projection> = fs::read_dir(state.join("attention-outbox/entries"))
        .unwrap()
        .map(|entry| serde_json::from_slice(&fs::read(entry.unwrap().path()).unwrap()).unwrap())
        .collect();
    let pending: Vec<_> = projections
        .iter()
        .filter_map(|projection| match &projection.change {
            ProjectionChange::Upsert(condition)
                if condition.subject == AttentionSubject::Run(run_id.into())
                    && matches!(condition.reason, AttentionReason::PromotionPending(_)) =>
            {
                Some((projection, condition))
            }
            _ => None,
        })
        .collect();
    assert_eq!(pending.len(), 1);
    let (publication, condition) = pending[0];
    assert!(!projections.iter().any(
        |projection| matches!(&projection.change, ProjectionChange::Clear(key) if key == condition)
    ));
    assert!(!projections.iter().any(|projection| matches!(
        &projection.change, ProjectionChange::ClearSubject(subject) if subject == &condition.subject
    )));
    let AttentionReason::PromotionPending(result_id) = &condition.reason else {
        unreachable!()
    };
    let saved = state.join("saved-results").join(result_id);
    assert!(saved.join("published.json").is_file());
    assert!(!saved.join("accepted.json").exists());
    assert!(!saved.join("discarded.json").exists());
    eprintln!("SAVED_REVIEW_PROMOTION_PENDING_UNCHANGED");
    publication.clone()
}

pub(super) fn assert_saved_decision(run_id: &str, accepted: bool) {
    let state = Path::new(STATE).join("authorizations");
    let projections: Vec<Projection> = fs::read_dir(state.join("attention-outbox/entries"))
        .unwrap()
        .map(|entry| serde_json::from_slice(&fs::read(entry.unwrap().path()).unwrap()).unwrap())
        .collect();
    let pending: Vec<_> = projections
        .iter()
        .filter_map(|projection| match &projection.change {
            ProjectionChange::Upsert(condition)
                if condition.subject == AttentionSubject::Run(run_id.into())
                    && matches!(condition.reason, AttentionReason::PromotionPending(_)) =>
            {
                Some((projection, condition))
            }
            _ => None,
        })
        .collect();
    assert_eq!(pending.len(), 1);
    let (publication, condition) = pending[0];
    let cleared: Vec<_> = projections
        .iter()
        .filter(|projection| matches!(&projection.change, ProjectionChange::Clear(key) if key == condition))
        .collect();
    assert_eq!(cleared.len(), 1);
    assert!(cleared[0].sequence > publication.sequence);
    let AttentionReason::PromotionPending(result_id) = &condition.reason else {
        unreachable!();
    };
    let saved = state.join("saved-results").join(result_id);
    assert!(
        saved
            .join(if accepted {
                "accepted.json"
            } else {
                "discarded.json"
            })
            .is_file()
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(
            &fs::read(saved.join("attention.json")).unwrap()
        )
        .unwrap(),
        serde_json::to_value(condition).unwrap()
    );
    let wire = publication.wire();
    assert_eq!(wire["change"]["attention"]["result_id"], result_id.as_str());
    for field in ["source", "job", "verification", "changes", "prompt"] {
        assert!(wire["change"]["attention"].get(field).is_none());
    }
    eprintln!("saved Attention: durable pending publication, exact decision clear, no payload");
}
