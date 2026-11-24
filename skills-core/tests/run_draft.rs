//! Draft intent has no authority and activates once after readiness, never on reopen.
#![allow(
    clippy::unwrap_used,
    reason = "Fixtures assert exact dispatch outcomes."
)]

use std::cell::Cell;

use louiselm_skills::{
    Digest,
    broker::{
        BrokerError,
        run_envelope::{ExecutionLimit, RunDraft, RunEnvelope, RunEnvelopeStore},
    },
};

fn draft() -> RunDraft {
    RunDraft {
        schema: "louiselm.broker.run-draft/1".into(),
        envelope: RunEnvelope {
            schema: "louiselm.broker.run-envelope/2".into(),
            run_id: "delayed-run".into(),
            envelope_id: "envelope".into(),
            envelope_revision: 1,
            controller_uid: 1000,
            bead_scope: None,
            provider_requests: None,
            commands: None,
            skill_requests: None,
            verification_plan_digest: Digest::of(b"plan").to_string(),
            max_sessions: 6,
            expires_at_ms: 0,
        },
        execution: ExecutionLimit::default(),
    }
}

#[test]
fn delayed_draft_survives_restart_and_dispatch_clock_follows_checks() {
    let root = tempfile::tempdir().unwrap();
    let saved = serde_json::to_vec(&draft()).unwrap();
    let store = RunEnvelopeStore::open(root.path()).unwrap();
    assert!(matches!(
        store.approve(&draft().envelope, 1),
        Err(BrokerError::InvalidGrant)
    ));
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    drop(store);
    let restored: RunDraft = serde_json::from_slice(&saved).unwrap();
    let store = RunEnvelopeStore::open(root.path()).unwrap();
    let clock = Cell::new(8_000_000_000);
    let activated = store
        .dispatch_with(
            &restored,
            || Ok(clock.get()),
            || {
                clock.set(clock.get() + 60_000);
                Ok(())
            },
        )
        .unwrap();
    assert_eq!(activated.dispatched_at_ms, 8_000_060_000);
    assert_eq!(activated.envelope.expires_at_ms, 8_001_860_000);
    assert_eq!(activated.envelope.max_sessions, 6);
    assert_eq!(
        activated.draft_digest,
        restored.digest().unwrap().to_string()
    );
    let restarted = RunEnvelopeStore::open(root.path()).unwrap();
    let mut changed_selection = restored.clone();
    changed_selection.envelope.max_sessions = 8;
    changed_selection.execution = ExecutionLimit::Duration {
        duration_ms: 3_600_000,
    };
    assert_ne!(
        changed_selection.digest().unwrap(),
        restored.digest().unwrap()
    );
    assert!(matches!(
        restarted.dispatch_with(&changed_selection, || Ok(clock.get()), || Ok(())),
        Err(BrokerError::InvalidGrant)
    ));
    assert!(matches!(
        restarted.dispatch_with(&restored, || Ok(clock.get() + 100), || Ok(())),
        Err(BrokerError::InvalidGrant)
    ));
    let mut renewed = activated.envelope.clone();
    renewed.envelope_revision += 1;
    renewed.expires_at_ms += 1;
    assert!(matches!(
        restarted.approve(&renewed, activated.dispatched_at_ms),
        Err(BrokerError::InvalidGrant)
    ));
}

#[test]
fn failed_readiness_and_elapsed_absolute_cutoff_create_no_authority() {
    let root = tempfile::tempdir().unwrap();
    let store = RunEnvelopeStore::open(root.path()).unwrap();
    assert!(matches!(
        store.dispatch_with(&draft(), || Ok(1), || Err(BrokerError::Installation)),
        Err(BrokerError::Installation)
    ));
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    let mut absolute = draft();
    absolute.execution = ExecutionLimit::StopBy {
        expires_at_ms: 5_000,
    };
    assert!(matches!(
        store.dispatch_with(&absolute, || Ok(5_000), || Ok(())),
        Err(BrokerError::InvalidGrant)
    ));
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    let dispatched = store
        .dispatch_with(&absolute, || Ok(4_000), || Ok(()))
        .unwrap();
    assert_eq!(dispatched.envelope.expires_at_ms, 5_000);
}

#[test]
fn draft_schema_rejects_ambiguous_limits_and_active_authority() {
    let value = serde_json::to_value(draft()).unwrap();
    for execution in [
        serde_json::json!({"kind":"duration", "duration_ms":0}),
        serde_json::json!({"kind":"duration", "duration_ms":u64::MAX}),
        serde_json::json!({"kind":"duration", "duration_ms":10, "expires_at_ms":100}),
        serde_json::json!({"kind":"stop_by", "expires_at_ms":100, "extra":true}),
    ] {
        let mut invalid = value.clone();
        invalid["execution"] = execution;
        if let Ok(invalid) = serde_json::from_value::<RunDraft>(invalid) {
            assert!(matches!(
                invalid.activate(1),
                Err(BrokerError::InvalidGrant)
            ));
        }
    }
    let mut active = draft();
    active.envelope.expires_at_ms = 100;
    assert!(matches!(active.activate(1), Err(BrokerError::InvalidGrant)));
    let mut extra = value;
    extra["extra"] = true.into();
    assert!(serde_json::from_value::<RunDraft>(extra).is_err());
}

#[test]
fn concurrent_start_spends_one_dispatch_and_never_rechecks_a_spent_draft() {
    use std::sync::{
        Arc, Barrier,
        atomic::{AtomicUsize, Ordering},
    };
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(RunEnvelopeStore::open(root.path()).unwrap());
    let checks = Arc::new(AtomicUsize::new(0));
    let start = Arc::new(Barrier::new(2));
    let children: Vec<_> = (0..2)
        .map(|_| {
            let (store, checks, start) =
                (Arc::clone(&store), Arc::clone(&checks), Arc::clone(&start));
            std::thread::spawn(move || {
                start.wait();
                store.dispatch_with(
                    &draft(),
                    || Ok(10_000),
                    || {
                        checks.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    },
                )
            })
        })
        .collect();
    let outcomes: Vec<_> = children
        .into_iter()
        .map(|child| child.join().unwrap())
        .collect();
    assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
    assert!(
        outcomes
            .iter()
            .any(|outcome| matches!(outcome, Err(BrokerError::InvalidGrant)))
    );
    assert_eq!(checks.load(Ordering::SeqCst), 1);
}
