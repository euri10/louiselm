//! Real daemon delivery with a disposable, exact-ACK capture-service peer.

use super::*;
use crate::broker::attention::{AttentionSubject, Outbox, ProjectionChange};
use std::{
    io::Read,
    os::unix::net::{UnixListener, UnixStream},
};

const OUTBOX: &str = "/var/lib/louiselm/broker/authorizations/attention-outbox";

pub(super) fn assert_waiver_expiry_projects_without_reads(session: &LaunchedSession) {
    use crate::broker::attention::AttentionReason;
    let outbox = Outbox::open(Path::new(OUTBOX)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut operations = std::collections::BTreeSet::new();
    while operations.len() < 5 {
        assert!(
            Instant::now() < deadline,
            "expiry did not produce five independent conditions"
        );
        if let Some(item) = outbox.next().unwrap() {
            if let ProjectionChange::Upsert(condition) = &item.change {
                assert_eq!(
                    condition.subject,
                    AttentionSubject::Session(session.receipt().payload.session_id.clone())
                );
                assert!(matches!(
                    condition.reason,
                    AttentionReason::SkillUnverified(_)
                ));
                assert!(operations.insert(condition.operation_id.clone()));
            }
            // Fixture consumes the real persisted outbox; no status calls or UI.
            outbox.acknowledge(item.sequence, &item.digest()).unwrap();
            chown(
                Path::new(OUTBOX).join(format!("acks/{:020}.json", item.sequence)),
                Some(BROKER_UID),
                Some(BROKER_UID),
            )
            .unwrap();
        } else {
            thread::sleep(Duration::from_millis(50));
        }
    }
}

pub(super) fn assert_posture_cleared(session: &str) {
    let outbox = Outbox::open(Path::new(OUTBOX)).unwrap();
    let mut cleared = 0;
    while let Some(item) = outbox.next().unwrap() {
        if let ProjectionChange::Clear(condition) = &item.change
            && condition.subject == AttentionSubject::Session(session.into())
        {
            cleared += 1;
        }
        outbox.acknowledge(item.sequence, &item.digest()).unwrap();
    }
    assert_eq!(
        cleared, 5,
        "terminal receipt clears only its posture conditions"
    );
}

pub(super) fn enqueue(name: &str) -> u64 {
    let entry = Outbox::open(Path::new(OUTBOX))
        .unwrap()
        .enqueue(
            name,
            ProjectionChange::ClearSubject(AttentionSubject::Session(name.into())),
        )
        .unwrap();
    // The fixture may enqueue as root while the actual daemon runs as its
    // dedicated UID. Production producers already run as the broker.
    if rustix::process::geteuid().is_root() {
        chown(
            Path::new(OUTBOX).join(format!("entries/{:020}.json", entry.sequence)),
            Some(BROKER_UID),
            Some(BROKER_UID),
        )
        .unwrap();
    }
    entry.sequence
}

pub(super) fn configure() {
    let capability = Path::new(STATE).join("attention-capability");
    fs::write(&capability, "test-projection-capability").unwrap();
    fs::set_permissions(&capability, fs::Permissions::from_mode(0o600)).unwrap();
    chown(&capability, Some(BROKER_UID), Some(BROKER_UID)).unwrap();
    write_json(
        Path::new("/etc/louiselm-broker-attention.json"),
        &serde_json::json!({
            "socket": format!("{STATE}/attention.sock"),
            "capability_file": capability,
            "receiver_uid": 0
        }),
    );
}

pub(super) fn listen() -> UnixListener {
    let path = Path::new(STATE).join("attention.sock");
    let listener = UnixListener::bind(&path).unwrap();
    // The parent is broker-private; only this fixture receiver is root.
    chown(&path, Some(BROKER_UID), Some(BROKER_UID)).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    listener.set_nonblocking(true).unwrap();
    listener
}

pub(super) fn receive(listener: &UnixListener) -> (UnixStream, serde_json::Value) {
    let deadline = Instant::now() + Duration::from_secs(35);
    let stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    Instant::now() < deadline,
                    "daemon never delivered pending Attention"
                );
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("fixture accept: {error}"),
        }
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let request: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(request["capability"], "test-projection-capability");
    (reader.into_inner(), request)
}

pub(super) fn reply(mut stream: UnixStream, request: &serde_json::Value, valid: bool) {
    let digest = Digest::of(request["projection"].to_string().as_bytes());
    let response = serde_json::json!({
        "type": "projection_result", "request_id": request["request_id"],
        "result": {"sequence": request["projection"]["sequence"],
            "digest": if valid {digest.hex()} else {"wrong"}, "applied": true}
    });
    writeln!(stream, "{response}").unwrap();
}

pub(super) fn wait_ack(sequence: u64) {
    let deadline = Instant::now() + Duration::from_secs(2);
    let path = Path::new(OUTBOX).join(format!("acks/{sequence:020}.json"));
    while !path.exists() {
        assert!(Instant::now() < deadline, "exact ACK was not persisted");
        thread::sleep(Duration::from_millis(10));
    }
}

pub(super) fn assert_stopped(stream: &mut UnixStream) {
    assert_eq!(
        stream.read(&mut [0]).unwrap(),
        0,
        "SIGTERM closes delivery I/O"
    );
}

pub(super) fn assert_pending(sequence: u64) {
    assert_eq!(
        Outbox::open(Path::new(OUTBOX))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .sequence,
        sequence
    );
}
