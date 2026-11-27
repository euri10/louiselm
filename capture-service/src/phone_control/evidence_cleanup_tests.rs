//! Disposable receiver evidence for Send cleanup.
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test fixtures and assertions"
)]

use super::*;

struct Fixture {
    root: tempfile::TempDir,
    receiver: String,
    generation: String,
    _registrar: super::super::barrier::ReceiverBarrier,
    store: Option<ActionEvidenceStore>,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let tls = root.path().join("tls");
        fs::create_dir(&tls).unwrap();
        let (receiver, generation, registrar) = super::super::barrier::receiver_fixture(&tls);
        let directory = root.path().join("actions");
        let guard = tls.join("phone-action-evidence.json");
        initialize_receiver_fixture(&directory, &guard, &receiver);
        bind_receiver_fixture(&directory, &guard, &receiver, &generation);
        let mut fixture = Self {
            root,
            receiver,
            generation,
            _registrar: registrar,
            store: None,
        };
        fixture.reopen();
        fixture
    }

    fn reopen(&mut self) {
        self.store = None;
        self.store = Some(
            ActionEvidenceStore::open_bound(
                self.root.path().join("actions"),
                self.root.path().join("tls/phone-action-evidence.json"),
                &self.receiver,
                ActionEvidenceKind::Archive,
            )
            .unwrap(),
        );
    }

    fn store(&self) -> &ActionEvidenceStore {
        self.store.as_ref().unwrap()
    }

    fn identity(&self, operation: u8, action: u8) -> ActionIdentity {
        ActionIdentity {
            operation_id: format!("{operation:02x}").repeat(16),
            receiver_identity: self.receiver.clone(),
            controller_id: "03".repeat(16),
            subject_id: "04".repeat(16),
            action,
            payload_hash: "05".repeat(32),
            grant_id: "06".repeat(16),
            grant_revision: 1,
            period_id: "07".repeat(16),
            expected_revision: 1,
            project: fs::canonicalize(self.root.path())
                .unwrap()
                .to_str()
                .unwrap()
                .into(),
            agent: "test-agent".into(),
            recorded_at_ms: 1,
        }
    }
}

#[test]
fn completed_send_cleanup_keeps_the_guard_across_restart() {
    let mut fixture = Fixture::new();
    let identity = fixture.identity(1, 3);
    let store = fixture.store();
    assert_eq!(
        store.reserve(&fixture.generation, &identity),
        Ok(ActionReservation::New)
    );
    for (at, outcome) in [
        ActionOutcome::Accepted {
            evidence: "11".repeat(32),
        },
        ActionOutcome::Dispatched {
            evidence: "22".repeat(32),
        },
        ActionOutcome::Completed {
            evidence: "33".repeat(32),
        },
    ]
    .iter()
    .enumerate()
    {
        store
            .report(&identity, outcome, u64::try_from(at + 2).unwrap())
            .unwrap();
    }
    assert_eq!(
        store.report(&identity, &ActionOutcome::DetailsRemoved, 5),
        Err(ActionEvidenceError::Malformed)
    );
    for _ in 0..2 {
        assert_eq!(
            store.remove_send_details(&identity.operation_id),
            Ok(DetailRemoval::Removed)
        );
    }
    let removed = store.lookup(&identity.operation_id).unwrap();
    assert_eq!(removed.identity, identity);
    assert_eq!(removed.outcome, ActionOutcome::DetailsRemoved);
    assert_eq!(removed.phases, Vec::<ActionOutcome>::new());
    assert!(
        !fixture
            .root
            .path()
            .join("actions/pending-before.json")
            .exists()
    );
    fixture.reopen();
    let store = fixture.store();
    assert_eq!(store.lookup(&identity.operation_id).unwrap(), removed);
    assert_eq!(
        store.reserve(&fixture.generation, &identity),
        Ok(ActionReservation::Existing)
    );
    let mut changed = identity.clone();
    changed.payload_hash = "ff".repeat(32);
    assert_eq!(
        store.reserve(&fixture.generation, &changed),
        Err(ActionEvidenceError::Conflict)
    );
    assert_eq!(
        store.report(
            &identity,
            &ActionOutcome::Completed {
                evidence: "33".repeat(32)
            },
            5
        ),
        Err(ActionEvidenceError::Conflict)
    );
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one scenario compares owner proof and storage refusals"
)]
fn fenced_send_cleanup_requires_matching_durable_owner_evidence() {
    for case in [
        "match",
        "lagged_archive",
        "missing",
        "unknown",
        "different_evidence",
        "different_identity",
        "damaged",
    ] {
        let root = tempfile::tempdir().unwrap();
        let capture = root.path().join("louiselm/capture");
        let tls = capture.join("tls");
        fs::create_dir_all(&tls).unwrap();
        let (receiver, generation, _registrar) = super::super::barrier::receiver_fixture(&tls);
        let directory = capture.join("phone-control/action-evidence");
        let guard = tls.join("phone-action-evidence.json");
        initialize_receiver_fixture(&directory, &guard, &receiver);
        bind_receiver_fixture(&directory, &guard, &receiver, &generation);
        let mut store = ActionEvidenceStore::open_bound(
            &directory,
            &guard,
            &receiver,
            ActionEvidenceKind::Archive,
        )
        .unwrap();
        let identity = ActionIdentity {
            operation_id: "01".repeat(16),
            receiver_identity: receiver.clone(),
            controller_id: "03".repeat(16),
            subject_id: "04".repeat(16),
            action: 3,
            payload_hash: "05".repeat(32),
            grant_id: "06".repeat(16),
            grant_revision: 1,
            period_id: "07".repeat(16),
            expected_revision: 1,
            project: fs::canonicalize(root.path())
                .unwrap()
                .to_str()
                .unwrap()
                .into(),
            agent: "test-agent".into(),
            recorded_at_ms: 1,
        };
        let fence = ActionOutcome::FencedNoEffect {
            evidence: "11".repeat(32),
        };
        store.reserve(&generation, &identity).unwrap();
        if case != "lagged_archive" {
            store.report(&identity, &fence, 2).unwrap();
        }
        let mut before = store.lookup(&identity.operation_id).unwrap();
        let mut other = identity.clone();
        other.operation_id = "12".repeat(16);
        other.action = 4;
        store.reserve(&generation, &other).unwrap();
        let other_outcome = ActionOutcome::Completed {
            evidence: "13".repeat(32),
        };
        store.report(&other, &other_outcome, 2).unwrap();
        let owner_path = root.path().join("louiselm/workflow/phone-action-owner");
        if case != "missing" {
            let owner = ActionEvidenceStore::open(&owner_path).unwrap();
            let mut owner_identity = identity.clone();
            if case == "different_identity" {
                owner_identity.payload_hash = "ff".repeat(32);
            }
            owner.reserve_owner(&owner_identity).unwrap();
            if case != "unknown" {
                let outcome = if case == "different_evidence" {
                    ActionOutcome::FencedNoEffect {
                        evidence: "22".repeat(32),
                    }
                } else {
                    fence.clone()
                };
                owner
                    .report_under_barrier(&owner_identity, &outcome, 2)
                    .unwrap();
            }
            drop(owner);
            if case == "damaged" {
                fs::write(owner_path.join("actions.json"), b"damaged").unwrap();
            }
        }
        if case == "lagged_archive" {
            assert_eq!(before.outcome, ActionOutcome::Unknown);
            super::super::http::refresh_action_for_cleanup(&store, &identity.operation_id).unwrap();
            before = store.lookup(&identity.operation_id).unwrap();
            assert_eq!(before.outcome, fence);
        }
        if !matches!(case, "match" | "lagged_archive") {
            let result = store.remove_send_details(&identity.operation_id);
            let expected = if matches!(case, "missing" | "damaged") {
                ActionEvidenceError::Unavailable
            } else {
                ActionEvidenceError::Conflict
            };
            assert_eq!(result, Err(expected), "{case}");
            assert_eq!(store.lookup(&identity.operation_id).unwrap(), before);
            continue;
        }
        let obstruction = directory.join("pending-before.json");
        fs::create_dir(&obstruction).unwrap();
        assert_eq!(
            store.remove_send_details(&identity.operation_id),
            Err(ActionEvidenceError::Unavailable)
        );
        drop(store);
        fs::remove_dir(obstruction).unwrap();
        store = ActionEvidenceStore::open_bound(
            &directory,
            &guard,
            &receiver,
            ActionEvidenceKind::Archive,
        )
        .unwrap();
        assert_eq!(store.lookup(&identity.operation_id).unwrap(), before);
        if case == "match" {
            let guard = store.guard.as_ref().unwrap();
            let barrier = Barrier::existing(
                guard.path.parent().unwrap(),
                &receiver,
                guard.barrier.clone().unwrap(),
            )
            .unwrap();
            let held_owner = root.path().join("louiselm/workflow/held-owner");
            let (started_tx, started_rx) = std::sync::mpsc::channel();
            let (done_tx, done_rx) = std::sync::mpsc::channel();
            std::thread::scope(|scope| {
                barrier
                    .with_generation(&generation, || {
                        scope.spawn(|| {
                            started_tx.send(()).unwrap();
                            done_tx
                                .send(store.remove_send_details(&identity.operation_id))
                                .unwrap();
                        });
                        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
                        assert_eq!(
                            done_rx.recv_timeout(Duration::from_millis(100)),
                            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                        );
                        fs::rename(&owner_path, &held_owner).unwrap();
                    })
                    .unwrap();
                assert_eq!(
                    done_rx.recv_timeout(Duration::from_secs(3)).unwrap(),
                    Err(ActionEvidenceError::Unavailable)
                );
            });
            fs::rename(&held_owner, &owner_path).unwrap();
            assert_eq!(store.lookup(&identity.operation_id).unwrap(), before);
        }
        assert_eq!(
            store.remove_send_details(&identity.operation_id),
            Ok(DetailRemoval::Removed)
        );
        assert_eq!(
            store.lookup(&identity.operation_id).unwrap().outcome,
            ActionOutcome::DetailsRemoved
        );
        assert_eq!(
            store.remove_send_details(&identity.operation_id),
            Ok(DetailRemoval::Removed)
        );
        drop(store);
        store = ActionEvidenceStore::open_bound(
            &directory,
            &guard,
            &receiver,
            ActionEvidenceKind::Archive,
        )
        .unwrap();
        assert_eq!(
            store.lookup(&identity.operation_id).unwrap().outcome,
            ActionOutcome::DetailsRemoved
        );
        assert_eq!(
            store.reserve(&generation, &identity),
            Ok(ActionReservation::Existing)
        );
        let mut changed = identity.clone();
        changed.payload_hash = "ff".repeat(32);
        assert_eq!(
            store.reserve(&generation, &changed),
            Err(ActionEvidenceError::Conflict)
        );
        assert_eq!(
            store.report(&identity, &fence, 3),
            Err(ActionEvidenceError::Conflict)
        );
        assert_eq!(
            store.lookup(&other.operation_id).unwrap().outcome,
            other_outcome
        );
        let owner = ActionEvidenceStore::open_existing(&owner_path).unwrap();
        assert_eq!(owner.lookup(&identity.operation_id).unwrap().outcome, fence);
    }
}

#[test]
fn cleanup_refuses_unresolved_and_other_action_types_without_changing_them() {
    let fixture = Fixture::new();
    let store = fixture.store();
    for (number, action, outcome) in [
        (1, 3, ActionOutcome::Unknown),
        (
            2,
            3,
            ActionOutcome::Accepted {
                evidence: "11".repeat(32),
            },
        ),
        (
            3,
            3,
            ActionOutcome::Dispatched {
                evidence: "22".repeat(32),
            },
        ),
        (
            4,
            3,
            ActionOutcome::Rejected {
                evidence: "33".repeat(32),
            },
        ),
        (
            6,
            4,
            ActionOutcome::Completed {
                evidence: "55".repeat(32),
            },
        ),
    ] {
        let identity = fixture.identity(number, action);
        store.reserve(&fixture.generation, &identity).unwrap();
        store.report(&identity, &outcome, 2).unwrap();
        let before = store.lookup(&identity.operation_id).unwrap();
        assert_eq!(
            store.remove_send_details(&identity.operation_id),
            Ok(if action == 3 {
                DetailRemoval::NotCompleted
            } else {
                DetailRemoval::Unsupported
            })
        );
        assert_eq!(store.lookup(&identity.operation_id).unwrap(), before);
    }
}

#[test]
fn interrupted_cleanup_recovers_the_marker_and_removes_staged_details() {
    for (replaced, fenced) in [(false, false), (true, false), (false, true), (true, true)] {
        let mut fixture = Fixture::new();
        let identity = fixture.identity(1, 3);
        let store = fixture.store();
        store.reserve(&fixture.generation, &identity).unwrap();
        store
            .report(
                &identity,
                &if fenced {
                    ActionOutcome::FencedNoEffect {
                        evidence: "33".repeat(32),
                    }
                } else {
                    ActionOutcome::Completed {
                        evidence: "33".repeat(32),
                    }
                },
                2,
            )
            .unwrap();
        let directory = fixture.root.path().join("actions");
        let guard_path = fixture.root.path().join("tls/phone-action-evidence.json");
        let before = fs::read(directory.join("actions.json")).unwrap();
        let mut after = parse_snapshot(&before).unwrap();
        let record = after.actions.get_mut(&identity.operation_id).unwrap();
        record.outcome = ActionOutcome::DetailsRemoved;
        record.phases.clear();
        let next = snapshot_bytes(&after).unwrap();
        replace(&directory, "pending-before.json", &before).unwrap();
        replace(&directory, "pending-after.json", &next).unwrap();
        let mut guard =
            read_guard(&guard_path, &fixture.receiver, ActionEvidenceKind::Archive).unwrap();
        guard.pending = Some(PendingUpdate {
            bytes: next.len() as u64,
            sha256: lower_hex(&Sha256::digest(&next)),
        });
        put_guard(&guard_path, &guard).unwrap();
        if replaced {
            persist_bytes(&directory, &next).unwrap();
        }
        fixture.reopen();
        assert_eq!(
            fixture
                .store()
                .lookup(&identity.operation_id)
                .unwrap()
                .outcome,
            ActionOutcome::DetailsRemoved
        );
        assert_eq!(
            fixture.store().reserve(&fixture.generation, &identity),
            Ok(ActionReservation::Existing)
        );
        assert!(!directory.join("pending-before.json").exists());
        assert!(!directory.join("pending-after.json").exists());
    }
}

#[test]
fn storage_failure_does_not_remove_the_original_evidence() {
    let mut fixture = Fixture::new();
    let identity = fixture.identity(1, 3);
    fixture
        .store()
        .reserve(&fixture.generation, &identity)
        .unwrap();
    fixture
        .store()
        .report(
            &identity,
            &ActionOutcome::Completed {
                evidence: "33".repeat(32),
            },
            2,
        )
        .unwrap();
    let before = fixture.store().lookup(&identity.operation_id).unwrap();
    let obstruction = fixture.root.path().join("actions/pending-before.json");
    fs::create_dir(&obstruction).unwrap();
    assert_eq!(
        fixture.store().remove_send_details(&identity.operation_id),
        Err(ActionEvidenceError::Unavailable)
    );
    fixture.store = None;
    fs::remove_dir(obstruction).unwrap();
    fixture.reopen();
    assert_eq!(
        fixture.store().lookup(&identity.operation_id).unwrap(),
        before
    );
    assert_eq!(
        fixture.store().reserve(&fixture.generation, &identity),
        Ok(ActionReservation::Existing)
    );
}

#[test]
fn restart_discards_staged_details_left_after_a_committed_cleanup() {
    let mut fixture = Fixture::new();
    let identity = fixture.identity(1, 3);
    fixture
        .store()
        .reserve(&fixture.generation, &identity)
        .unwrap();
    fixture
        .store()
        .report(
            &identity,
            &ActionOutcome::Completed {
                evidence: "33".repeat(32),
            },
            2,
        )
        .unwrap();
    let directory = fixture.root.path().join("actions");
    let old = fs::read(directory.join("actions.json")).unwrap();
    fixture
        .store()
        .remove_send_details(&identity.operation_id)
        .unwrap();
    let marker = fs::read(directory.join("actions.json")).unwrap();
    replace(&directory, "pending-before.json", &old).unwrap();
    replace(&directory, "pending-after.json", &marker).unwrap();
    fixture.reopen();
    assert_eq!(
        fixture
            .store()
            .lookup(&identity.operation_id)
            .unwrap()
            .outcome,
        ActionOutcome::DetailsRemoved
    );
    assert!(!directory.join("pending-before.json").exists());
    assert!(!directory.join("pending-after.json").exists());
    assert_eq!(
        fixture.store().reserve(&fixture.generation, &identity),
        Ok(ActionReservation::Existing)
    );
}

#[test]
fn recovery_rejects_cleanup_of_unresolved_or_native_evidence() {
    let fixture = Fixture::new();
    let identity = fixture.identity(1, 3);
    fixture
        .store()
        .reserve(&fixture.generation, &identity)
        .unwrap();
    let original =
        parse_snapshot(&fs::read(fixture.root.path().join("actions/actions.json")).unwrap())
            .unwrap();
    let mut removed = parse_snapshot(&snapshot_bytes(&original).unwrap()).unwrap();
    removed
        .actions
        .get_mut(&identity.operation_id)
        .unwrap()
        .outcome = ActionOutcome::DetailsRemoved;
    assert!(!preserves_evidence(
        &original,
        &removed,
        &fixture.receiver,
        ActionEvidenceKind::Archive
    ));
    fixture
        .store()
        .report(
            &identity,
            &ActionOutcome::Completed {
                evidence: "33".repeat(32),
            },
            2,
        )
        .unwrap();
    let complete =
        parse_snapshot(&fs::read(fixture.root.path().join("actions/actions.json")).unwrap())
            .unwrap();
    removed
        .actions
        .get_mut(&identity.operation_id)
        .unwrap()
        .updated_at_ms = 2;
    assert!(preserves_evidence(
        &complete,
        &removed,
        &fixture.receiver,
        ActionEvidenceKind::Archive
    ));
    assert!(!preserves_evidence(
        &complete,
        &removed,
        &fixture.receiver,
        ActionEvidenceKind::Owner
    ));
}

#[test]
fn damaged_interrupted_cleanup_cannot_be_read_as_an_absent_action() {
    let mut fixture = Fixture::new();
    let identity = fixture.identity(1, 3);
    fixture
        .store()
        .reserve(&fixture.generation, &identity)
        .unwrap();
    fixture
        .store()
        .report(
            &identity,
            &ActionOutcome::Completed {
                evidence: "33".repeat(32),
            },
            2,
        )
        .unwrap();
    let directory = fixture.root.path().join("actions");
    let guard_path = fixture.root.path().join("tls/phone-action-evidence.json");
    let before = fs::read(directory.join("actions.json")).unwrap();
    let mut after = parse_snapshot(&before).unwrap();
    let record = after.actions.get_mut(&identity.operation_id).unwrap();
    record.outcome = ActionOutcome::DetailsRemoved;
    record.phases.clear();
    let next = snapshot_bytes(&after).unwrap();
    replace(&directory, "pending-before.json", &before).unwrap();
    replace(&directory, "pending-after.json", b"damaged").unwrap();
    let mut guard =
        read_guard(&guard_path, &fixture.receiver, ActionEvidenceKind::Archive).unwrap();
    guard.pending = Some(PendingUpdate {
        bytes: next.len() as u64,
        sha256: lower_hex(&Sha256::digest(&next)),
    });
    put_guard(&guard_path, &guard).unwrap();
    fixture.store = None;
    assert!(matches!(
        ActionEvidenceStore::open_bound(
            &directory,
            &guard_path,
            &fixture.receiver,
            ActionEvidenceKind::Archive
        ),
        Err(ActionEvidenceError::Unavailable)
    ));
    assert_eq!(fs::read(directory.join("actions.json")).unwrap(), before);
}

#[test]
fn cleanup_waits_for_the_same_barrier_as_delivery_and_reports() {
    use std::sync::mpsc;
    let fixture = Fixture::new();
    let identity = fixture.identity(1, 3);
    fixture
        .store()
        .reserve(&fixture.generation, &identity)
        .unwrap();
    fixture
        .store()
        .report(
            &identity,
            &ActionOutcome::Completed {
                evidence: "33".repeat(32),
            },
            2,
        )
        .unwrap();
    let guard = fixture.store().guard.as_ref().unwrap();
    let barrier = Barrier::existing(
        guard.path.parent().unwrap(),
        &guard.receiver,
        guard.barrier.clone().unwrap(),
    )
    .unwrap();
    let (started_tx, started_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::scope(|scope| {
        barrier
            .with_generation(&fixture.generation, || {
                scope.spawn(|| {
                    started_tx.send(()).unwrap();
                    done_tx
                        .send(fixture.store().remove_send_details(&identity.operation_id))
                        .unwrap();
                });
                started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
                assert_eq!(
                    done_rx.recv_timeout(Duration::from_millis(100)),
                    Err(mpsc::RecvTimeoutError::Timeout)
                );
            })
            .unwrap();
        assert_eq!(
            done_rx.recv_timeout(Duration::from_secs(3)).unwrap(),
            Ok(DetailRemoval::Removed)
        );
    });
}
