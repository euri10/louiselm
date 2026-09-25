#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "Assertions on a disposable ledger fixture"
)]

use super::*;
use crate::broker::provider_extension::{Extension, ExtensionError, ExtensionRequest};
use std::{sync::Arc, thread};

fn ledger() -> (tempfile::TempDir, ProviderLedger) {
    let root = tempfile::tempdir().unwrap();
    let ledger = ProviderLedger::open(&root.path().join("provider-requests")).unwrap();
    (root, ledger)
}

#[test]
fn each_attempt_durably_spends_one_unit_until_the_run_total() {
    let (_root, ledger) = ledger();
    assert_eq!(ledger.reserve("run-a", "session-a", 2, 10).unwrap(), 0);
    assert_eq!(ledger.reserve("run-a", "session-a", 2, 11).unwrap(), 1);
    assert!(matches!(
        ledger.reserve("run-a", "session-a", 2, 12),
        Err(BrokerError::ProviderBudgetExhausted)
    ));
    assert_eq!(ledger.spent("run-a").unwrap(), 2);
}

#[test]
fn every_session_of_a_run_shares_one_total_and_runs_are_independent() {
    let (_root, ledger) = ledger();
    ledger.reserve("run-a", "session-a", 2, 10).unwrap();
    ledger.reserve("run-a", "session-b", 2, 10).unwrap();
    assert!(matches!(
        ledger.reserve("run-a", "session-c", 2, 10),
        Err(BrokerError::ProviderBudgetExhausted)
    ));
    assert_eq!(ledger.reserve("run-b", "session-d", 2, 10).unwrap(), 0);
}

#[test]
fn restart_cannot_reset_the_spent_total() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("provider-requests");
    ProviderLedger::open(&path)
        .unwrap()
        .reserve("run-a", "s", 1, 10)
        .unwrap();
    let reopened = ProviderLedger::open(&path).unwrap();
    assert_eq!(reopened.spent("run-a").unwrap(), 1);
    assert!(matches!(
        reopened.reserve("run-a", "s", 1, 10),
        Err(BrokerError::ProviderBudgetExhausted)
    ));
}

#[test]
fn a_grant_stating_a_different_run_total_is_refused() {
    let (_root, ledger) = ledger();
    ledger.reserve("run-a", "session-a", 2, 10).unwrap();
    assert!(matches!(
        ledger.reserve("run-a", "session-b", 50, 10),
        Err(BrokerError::InvalidGrant)
    ));
    assert_eq!(ledger.spent("run-a").unwrap(), 1);
}

#[test]
fn concurrent_reservations_never_overspend_the_last_units() {
    let (_root, ledger) = ledger();
    let ledger = Arc::new(ledger);
    let workers: Vec<_> = (0..24)
        .map(|index| {
            let ledger = Arc::clone(&ledger);
            thread::spawn(move || {
                ledger
                    .reserve("run-a", &format!("session-{index}"), 10, 10)
                    .is_ok()
            })
        })
        .collect();
    let granted = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .filter(|granted| *granted)
        .count();
    assert_eq!(granted, 10);
    assert_eq!(ledger.spent("run-a").unwrap(), 10);
}

#[test]
fn unexpected_ledger_entries_fail_closed() {
    let (root, ledger) = ledger();
    ledger.reserve("run-a", "s", 5, 10).unwrap();
    let run = std::fs::read_dir(root.path().join("provider-requests/runs"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    std::fs::write(run.join("stray"), b"{}").unwrap();
    assert!(ledger.reserve("run-a", "s", 5, 10).is_err());
    assert!(ledger.spent("run-a").is_err());
}

#[test]
fn the_first_hold_wins_survives_restart_and_stays_per_run() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("provider-requests");
    let ledger = ProviderLedger::open(&path).unwrap();
    assert_eq!(ledger.held("run-a").unwrap(), None);
    let first = ledger.hold("run-a", HoldReason::Exhausted, 10).unwrap();
    // A sibling observing expiry later cannot rewrite the recorded cause or time.
    assert_eq!(
        ledger.hold("run-a", HoldReason::Expired, 99).unwrap(),
        first
    );
    let reopened = ProviderLedger::open(&path).unwrap();
    assert_eq!(reopened.held("run-a").unwrap(), Some(first));
    assert_eq!(reopened.held("run-b").unwrap(), None);
    // The hold lives beside the attempt records, never among them.
    reopened.reserve("run-a", "s", 5, 11).unwrap();
    assert_eq!(reopened.spent("run-a").unwrap(), 1);
}

fn lift(ledger: &ProviderLedger, id: &str, units: u32) -> Result<Extension, BrokerError> {
    ledger.extend(
        "run-a",
        &ExtensionRequest {
            request_id: id.into(),
            additional_requests: units,
            expires_at_ms: None,
        },
        7,
        20,
        |_, _, _| Ok(()),
    )
}

#[test]
fn extensions_lift_one_hold_generation_raise_the_total_and_survive_restart() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("provider-requests");
    let ledger = ProviderLedger::open(&path).unwrap();
    ledger.reserve("run-a", "s", 1, 10).unwrap();
    let first = ledger.hold("run-a", HoldReason::Exhausted, 11).unwrap();
    assert_eq!(lift(&ledger, "ext-1", 2).unwrap().lifts, first);
    assert_eq!(ledger.held("run-a").unwrap(), None);
    let reopened = ProviderLedger::open(&path).unwrap();
    assert_eq!(reopened.extensions("run-a").unwrap().len(), 1);
    assert_eq!(reopened.reserve("run-a", "s", 1, 21).unwrap(), 1);
    assert_eq!(reopened.reserve("run-a", "s", 1, 22).unwrap(), 2);
    assert!(matches!(
        reopened.reserve("run-a", "s", 1, 23),
        Err(BrokerError::ProviderBudgetExhausted)
    ));
    // The next hold is a new generation; the lifted one is never rewritten.
    let second = reopened.hold("run-a", HoldReason::Exhausted, 24).unwrap();
    assert_ne!(second, first);
    assert_eq!(reopened.held("run-a").unwrap(), Some(second));
}

#[test]
fn an_extension_needs_a_standing_hold_and_a_tampered_record_fails_closed() {
    let (root, ledger) = ledger();
    assert!(matches!(
        lift(&ledger, "ext-1", 1),
        Err(BrokerError::ProviderExtension(ExtensionError::NotHeld))
    ));
    ledger.hold("run-a", HoldReason::Expired, 11).unwrap();
    lift(&ledger, "ext-1", 1).unwrap();
    let holds = std::fs::read_dir(root.path().join("provider-requests/holds"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    // Rewriting the lifted hold breaks the binding: nothing re-grants units.
    std::fs::remove_file(holds.join("0.json")).unwrap();
    assert!(ledger.extensions("run-a").is_err());
    assert!(ledger.reserve("run-a", "s", 5, 12).is_err());
}
