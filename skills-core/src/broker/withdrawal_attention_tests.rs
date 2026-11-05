//! Stable episodes, metadata changes, recurrence and durable outbox recovery.
#![allow(
    clippy::unwrap_used,
    reason = "Fixtures assert exact durable outcomes."
)]
use super::super::read_record;
use super::*;

fn alert() -> WithdrawalAlert {
    WithdrawalAlert {
        member: crate::Digest::of(b"member").to_string(),
        withdrawal: crate::Digest::of(b"withdrawal").to_string(),
        endorser: "Reviewer".into(),
        urgency: WithdrawalUrgency::SubjectOnly,
        next_action: "private CLI command must not reach Attention".into(),
    }
}

fn drain(outbox: &Outbox) -> Vec<super::super::attention::Projection> {
    let mut entries = vec![];
    while let Some(entry) = outbox.next().unwrap() {
        outbox.acknowledge(entry.sequence, &entry.digest()).unwrap();
        entries.push(entry);
    }
    entries
}

#[test]
fn stable_pair_updates_without_renotifying_and_resolved_recurrence_is_new() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("withdrawals");
    let outbox = Outbox::open(&root.path().join("outbox")).unwrap();
    let state = WithdrawalAttention::open(&path).unwrap();
    state
        .observe(1000, &[alert()], 100, &outbox, false)
        .unwrap();
    let first = drain(&outbox).remove(0).wire();
    let original = &first["change"]["attention"];
    assert!(!first.to_string().contains("private CLI"));
    let mut stronger = alert();
    stronger.urgency = WithdrawalUrgency::ReliedOn;
    stronger.endorser = "New local label".into();
    assert!(matches!(
        state.observe(1000, &[stronger.clone(), alert()], 200, &outbox, false),
        Err(BrokerError::RequestMismatch)
    ));
    assert!(
        outbox.next().unwrap().is_none(),
        "conflicting labels cannot partially project"
    );
    let mut weaker = stronger.clone();
    weaker.urgency = WithdrawalUrgency::SubjectOnly;
    state
        .observe(1000, &[stronger.clone(), weaker], 200, &outbox, false)
        .unwrap();
    let updated = drain(&outbox).remove(0).wire();
    let updated = &updated["change"]["attention"];
    assert_eq!(
        updated["source_operation_id"],
        original["source_operation_id"]
    );
    assert_eq!(updated["created_at_ms"], original["created_at_ms"]);
    assert_eq!(updated["withdrawal"]["urgency"], "relied_on");
    drop(state);
    let state = WithdrawalAttention::open(&path).unwrap();
    state.replay(&outbox).unwrap();
    state
        .observe(1000, &[stronger.clone()], 300, &outbox, false)
        .unwrap();
    assert!(outbox.next().unwrap().is_none());
    state.observe(1000, &[], 400, &outbox, false).unwrap();
    let clear = drain(&outbox).remove(0).wire();
    assert_eq!(
        clear["change"]["key"]["source_operation_id"],
        original["source_operation_id"]
    );
    state
        .observe(1000, &[stronger], 500, &outbox, false)
        .unwrap();
    let recurrence = drain(&outbox).remove(0).wire();
    assert_ne!(
        recurrence["change"]["attention"]["source_operation_id"],
        original["source_operation_id"]
    );
    assert_eq!(recurrence["change"]["attention"]["created_at_ms"], 500);
}

#[test]
fn intent_replays_after_outbox_storage_failure_without_duplicate_projection() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("withdrawals");
    let outbox_path = root.path().join("outbox");
    let state = WithdrawalAttention::open(&path).unwrap();
    let outbox = Outbox::open(&outbox_path).unwrap();
    let entries = outbox_path.join("entries");
    fs::remove_dir(&entries).unwrap();
    fs::write(&entries, b"simulate unavailable outbox storage").unwrap();
    assert!(matches!(
        state.observe(1000, &[alert()], 100, &outbox, false),
        Err(BrokerError::Storage(_))
    ));
    let intent: Record = read_record(&path.join("1000.json")).unwrap().unwrap();
    assert_eq!(intent.pending.len(), 1);
    fs::remove_file(&entries).unwrap();
    fs::create_dir(&entries).unwrap();
    drop(state);
    let state = WithdrawalAttention::open(&path).unwrap();
    state.replay(&outbox).unwrap();
    let delivered = drain(&outbox);
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].change, intent.pending[0]);
    state.replay(&outbox).unwrap();
    assert!(outbox.next().unwrap().is_none());
}

#[test]
fn malformed_observation_cannot_clear_an_existing_condition() {
    let root = tempfile::tempdir().unwrap();
    let state = WithdrawalAttention::open(&root.path().join("withdrawals")).unwrap();
    let outbox = Outbox::open(&root.path().join("outbox")).unwrap();
    state
        .observe(1000, &[alert()], 100, &outbox, false)
        .unwrap();
    drain(&outbox);
    let mut invalid = alert();
    invalid.member = "private/path".into();
    assert!(matches!(
        state.observe(1000, &[invalid], 200, &outbox, false),
        Err(BrokerError::InvalidGrant)
    ));
    assert!(outbox.next().unwrap().is_none());
    let record: Record = read_record(&state.path(1000)).unwrap().unwrap();
    assert_eq!(record.conditions.len(), 1);
}

#[test]
fn separate_withdrawals_of_one_member_have_separate_stable_conditions() {
    let root = tempfile::tempdir().unwrap();
    let state = WithdrawalAttention::open(&root.path().join("withdrawals")).unwrap();
    let outbox = Outbox::open(&root.path().join("outbox")).unwrap();
    let first = alert();
    let mut second = first.clone();
    second.withdrawal = crate::Digest::of(b"second withdrawal").to_string();
    state
        .observe(1000, &[first.clone(), second.clone()], 100, &outbox, false)
        .unwrap();
    let conditions = drain(&outbox);
    assert_eq!(conditions.len(), 2);
    let first_id = &conditions[0].wire()["change"]["attention"]["source_operation_id"];
    let second_id = &conditions[1].wire()["change"]["attention"]["source_operation_id"];
    assert_ne!(first_id, second_id);
    state
        .observe(1000, &[second.clone(), first], 200, &outbox, false)
        .unwrap();
    assert!(outbox.next().unwrap().is_none());
    state.observe(1000, &[second], 300, &outbox, false).unwrap();
    assert_eq!(drain(&outbox).len(), 1);
    let record: Record = read_record(&state.path(1000)).unwrap().unwrap();
    assert_eq!(record.conditions.len(), 1);
}

#[test]
fn interrupted_staging_files_never_replace_or_block_committed_state() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("withdrawals");
    let state = WithdrawalAttention::open(&path).unwrap();
    let outbox = Outbox::open(&root.path().join("outbox")).unwrap();
    state
        .observe(1000, &[alert()], 100, &outbox, false)
        .unwrap();
    drain(&outbox);
    fs::write(path.join(".withdrawal-incomplete"), b"{interrupted").unwrap();
    fs::write(
        path.join(".withdrawal-complete"),
        fs::read(state.path(1000)).unwrap(),
    )
    .unwrap();
    drop(state);
    let state = WithdrawalAttention::open(&path).unwrap();
    state.replay(&outbox).unwrap();
    state
        .observe(1000, &[alert()], 200, &outbox, false)
        .unwrap();
    assert!(outbox.next().unwrap().is_none());
    fs::write(state.path(1000), b"{corrupt committed record").unwrap();
    assert!(matches!(
        state.replay(&outbox),
        Err(BrokerError::Storage(_))
    ));
}

#[test]
fn aggregate_snapshot_supports_one_hundred_pairs_updates_and_clears() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("withdrawals");
    let state = WithdrawalAttention::open(&path).unwrap();
    let outbox = Outbox::open(&root.path().join("outbox")).unwrap();
    let mut alerts: Vec<_> = (0..100)
        .map(|index| WithdrawalAlert {
            member: crate::Digest::of(index.to_string().as_bytes()).to_string(),
            ..alert()
        })
        .collect();
    state.observe(1000, &alerts, 100, &outbox, false).unwrap();
    assert_eq!(drain(&outbox).len(), 100);
    drop(state);
    let state = WithdrawalAttention::open(&path).unwrap();
    state.replay(&outbox).unwrap();
    state.observe(1000, &alerts, 200, &outbox, false).unwrap();
    assert!(outbox.next().unwrap().is_none());
    for alert in &mut alerts {
        alert.urgency = WithdrawalUrgency::ReliedOn;
    }
    state.observe(1000, &alerts, 300, &outbox, false).unwrap();
    assert_eq!(drain(&outbox).len(), 100);
    state.observe(1000, &[], 400, &outbox, false).unwrap();
    assert_eq!(drain(&outbox).len(), 100);
}

#[test]
fn observed_local_layer_survives_empty_state_and_restart() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("withdrawals");
    let state = WithdrawalAttention::open(&path).unwrap();
    let outbox = Outbox::open(&root.path().join("outbox")).unwrap();
    state.observe(1000, &[], 100, &outbox, false).unwrap();
    assert!(!state.load(1000).unwrap().local_layer_observed);
    state.observe(1000, &[], 200, &outbox, true).unwrap();
    assert!(outbox.next().unwrap().is_none());
    drop(state);
    let state = WithdrawalAttention::open(&path).unwrap();
    state.replay(&outbox).unwrap();
    assert!(state.load(1000).unwrap().local_layer_observed);
    state.observe(1000, &[], 300, &outbox, false).unwrap();
    assert!(state.load(1000).unwrap().local_layer_observed);
    assert!(outbox.next().unwrap().is_none());
}

#[test]
fn aggregate_reader_refuses_oversized_state() {
    let root = tempfile::tempdir().unwrap();
    let state = WithdrawalAttention::open(&root.path().join("withdrawals")).unwrap();
    let outbox = Outbox::open(&root.path().join("outbox")).unwrap();
    fs::File::create(state.path(1000))
        .unwrap()
        .set_len(MAX_STATE_BYTES + 1)
        .unwrap();
    assert!(matches!(
        state.replay(&outbox),
        Err(BrokerError::Storage(_))
    ));
    assert!(outbox.next().unwrap().is_none());
}
