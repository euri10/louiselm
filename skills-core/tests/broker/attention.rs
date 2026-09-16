//! Broker outbox ordering and exact delivery identity.

use super::*;

#[test]
fn run_observations_pin_peer_subject_and_closed_response() {
    use louiselm_skills::broker::attention::AttentionEndpoint;
    use std::{
        io::{BufRead, BufReader, Write},
        os::unix::net::UnixListener,
    };
    let root = TempDir::new().unwrap();
    let capability = root.path().join("capability");
    fs::write(&capability, "broker-secret").unwrap();
    fs::set_permissions(&capability, fs::Permissions::from_mode(0o600)).unwrap();
    let socket = root.path().join("runs.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let endpoint = AttentionEndpoint {
        socket,
        capability_file: capability,
        receiver_uid: getuid().as_raw(),
    };
    let id = "12345678-1234-4234-8234-123456789abc";
    for (field, value, accepted) in [
        ("state", serde_json::json!("disposed"), true),
        ("state", serde_json::json!("unknown"), false),
        ("revision", serde_json::json!(0), false),
        (
            "run_id",
            serde_json::json!("87654321-1234-4234-8234-123456789abc"),
            false,
        ),
        ("token", serde_json::json!("secret"), false),
    ] {
        let (sender, receiver) = mpsc::channel();
        let worker = endpoint
            .read_run(
                id.into(),
                Box::new(move |result| {
                    sender.send(result).unwrap();
                }),
            )
            .unwrap();
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream);
        let mut request = String::new();
        reader.read_line(&mut request).unwrap();
        let request: serde_json::Value = serde_json::from_str(&request).unwrap();
        assert_eq!(request["run_id"], id);
        assert_eq!(request["capability"], "broker-secret");
        let mut result = serde_json::json!({"run_id":id,"revision":3,"state":"active"});
        result[field] = value;
        writeln!(reader.get_mut(), "{}", serde_json::json!({"type":"run_lifecycle_result","request_id":"run-lifecycle","result":result})).unwrap();
        worker.join().unwrap();
        assert_eq!(receiver.recv().unwrap().is_ok(), accepted);
    }
    let mut wrong_peer = endpoint;
    wrong_peer.receiver_uid += 1;
    let (sender, receiver) = mpsc::channel();
    let worker = wrong_peer
        .read_run(
            id.into(),
            Box::new(move |result| {
                sender.send(result).unwrap();
            }),
        )
        .unwrap();
    let (mut stream, _) = listener.accept().unwrap();
    worker.join().unwrap();
    assert!(receiver.recv().unwrap().is_err());
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut stream, &mut bytes).unwrap();
    assert!(
        bytes.is_empty(),
        "untrusted peer receives no credential or subject"
    );
}
use louiselm_skills::broker::attention::{
    AttentionCondition, AttentionReason, AttentionSubject, Outbox, ProjectionChange,
};

fn condition() -> AttentionCondition {
    AttentionCondition {
        subject: AttentionSubject::Session("session-1".into()),
        operation_id: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".into(),
        created_at_ms: 100,
        reason: AttentionReason::SessionFailed,
    }
}

#[test]
fn outbox_restarts_without_reordering_or_reopening_an_acknowledged_change() {
    let root = TempDir::new().unwrap();
    let outbox = Outbox::open(root.path()).unwrap();
    let first = outbox
        .enqueue("create", ProjectionChange::Upsert(condition()))
        .unwrap();
    let second = outbox
        .enqueue("clear", ProjectionChange::Clear(condition()))
        .unwrap();
    assert_eq!(first.sequence, 1);
    assert_eq!(
        first.wire(),
        serde_json::from_str::<serde_json::Value>(include_str!(
            "../../../tests/fixtures/broker_attention_projection.json"
        ))
        .unwrap()
    );
    assert_eq!(second.sequence, 2);
    assert_eq!(
        outbox
            .enqueue("create", ProjectionChange::Upsert(condition()))
            .unwrap(),
        first
    );
    assert!(
        outbox
            .enqueue("create", ProjectionChange::Clear(condition()))
            .is_err()
    );
    assert!(
        outbox
            .acknowledge(second.sequence, &second.digest())
            .is_err()
    );
    assert!(
        outbox
            .acknowledge(first.sequence, &Digest::of(b"substitution").to_string())
            .is_err()
    );
    assert_eq!(outbox.next().unwrap(), Some(first.clone()));
    outbox.acknowledge(first.sequence, &first.digest()).unwrap();
    drop(outbox);
    let restarted = Outbox::open(root.path()).unwrap();
    assert_eq!(restarted.next().unwrap(), Some(second.clone()));
    restarted
        .acknowledge(second.sequence, &second.digest())
        .unwrap();
    assert!(restarted.next().unwrap().is_none());
}

#[test]
fn direct_socket_delivery_retains_failed_entries_and_matches_the_exact_ack() {
    use louiselm_skills::broker::attention::AttentionEndpoint;
    use std::{
        io::{BufRead, BufReader, Write},
        os::unix::net::UnixListener,
    };
    let root = TempDir::new().unwrap();
    let endpoint = AttentionEndpoint {
        socket: root.path().join("attention.sock"),
        capability_file: root.path().join("attention-capability"),
        receiver_uid: getuid().as_raw(),
    };
    fs::write(&endpoint.capability_file, "private-test-capability").unwrap();
    fs::set_permissions(&endpoint.capability_file, fs::Permissions::from_mode(0o600)).unwrap();
    let listener = UnixListener::bind(&endpoint.socket).unwrap();
    let outbox = Outbox::open(&root.path().join("outbox")).unwrap();
    let entry = outbox
        .enqueue("create", ProjectionChange::Upsert(condition()))
        .unwrap();
    let expected = entry.clone();
    let peer = thread::spawn(move || {
        for valid in [false, true] {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            stream
                .write_all(
                    b"{\"type\":\"snapshot\",\"snapshot\":{\"generation\":0,\"items\":[]}}\n",
                )
                .unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let request: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request["projection"], expected.wire());
            assert_eq!(request["capability"], "private-test-capability");
            let reply = serde_json::json!({"type":"projection_result", "request_id":request["request_id"],
                "result":{"sequence":expected.sequence, "digest":if valid {expected.digest()} else {"wrong".into()}, "applied":true}});
            writeln!(reader.get_mut(), "{reply}").unwrap();
        }
    });
    assert!(outbox.deliver_next(&endpoint).is_err());
    assert_eq!(outbox.next().unwrap(), Some(entry));
    assert!(outbox.deliver_next(&endpoint).unwrap());
    assert!(outbox.next().unwrap().is_none());
    peer.join().unwrap();
}

#[test]
fn outbox_refuses_unbounded_or_hostile_condition_fields_before_persistence() {
    let root = TempDir::new().unwrap();
    let outbox = Outbox::open(root.path()).unwrap();
    let mut hostile = condition();
    hostile.operation_id = "not-a-canonical-uuid".into();
    assert!(
        outbox
            .enqueue("bad", ProjectionChange::Upsert(hostile))
            .is_err()
    );
    let mut hostile = condition();
    hostile.subject = AttentionSubject::Session("secret\nprompt".into());
    assert!(
        outbox
            .enqueue("bad", ProjectionChange::Upsert(hostile))
            .is_err()
    );
    assert!(outbox.next().unwrap().is_none());
}
