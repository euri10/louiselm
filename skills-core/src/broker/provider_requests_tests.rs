#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "Assertions on a disposable ledger fixture"
)]

use super::*;
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
