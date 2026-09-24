//! Trusted worker observations, never display reads, own posture Attention.
use super::*;
use louiselm_skills::broker::attention::{Outbox, ProjectionChange};
use louiselm_skills::launch_protocol::ChannelState;

pub(super) fn drain(outbox: &Outbox) -> Vec<ProjectionChange> {
    let mut changes = vec![];
    while let Some(item) = outbox.next().unwrap() {
        outbox.acknowledge(item.sequence, &item.digest()).unwrap();
        changes.push(item.change);
    }
    changes
}

#[test]
fn posture_attention_starts_only_when_waiting_and_rechecks_keep_episode_ids() {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("control.sock");
    let mut request = request("posture-attention");
    request.run_id = "00000000-0000-4000-8000-000000000001".into();
    let service = lifecycle::bound_service(root.path(), &socket, &request);
    let outbox = Outbox::open(&root.path().join("authorizations/attention-outbox")).unwrap();
    let peer = thread::spawn(move || {
        let (authorization, channel) = fake_supervisor(&socket, &request, 2000);
        let mut status = lifecycle::status(&authorization);
        lifecycle::answer_one_status_query(&channel, &status);
        status.state = SessionState::Parked;
        status.channel_state = ChannelState::Revoked;
        lifecycle::answer_one_status_query(&channel, &status);
        lifecycle::answer_one_status_query(&channel, &status);
    });
    let mut session = service
        .serve_launch(2000, verify_fixture_signature)
        .unwrap();
    service
        .project_posture_attention(&mut session, 3000, verify_fixture_signature)
        .unwrap();
    assert!(
        outbox.next().unwrap().is_none(),
        "running partial posture is not waiting"
    );
    service
        .project_posture_attention(&mut session, 4000, verify_fixture_signature)
        .unwrap();
    let mut operations = std::collections::BTreeSet::new();
    while let Some(item) = outbox.next().unwrap() {
        let ProjectionChange::Upsert(condition) = &item.change else {
            panic!("expected failure");
        };
        assert_eq!(
            item.wire()["change"]["attention"]["linked_run_id"],
            "00000000-0000-4000-8000-000000000001"
        );
        assert!(operations.insert(condition.operation_id.clone()));
        outbox.acknowledge(item.sequence, &item.digest()).unwrap();
    }
    assert_eq!(operations.len(), 5);
    service
        .project_posture_attention(&mut session, 5000, verify_fixture_signature)
        .unwrap();
    assert!(
        outbox.next().unwrap().is_none(),
        "unchanged failures create no duplicate items"
    );
    peer.join().unwrap();
}
