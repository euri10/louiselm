//! Offline request-boundary experiment, not an installed Provider transport.
//!
//! The real `BrokerSession` owns serialization; the supervisor, sender enrollment,
//! clock, request reservations and upstream are explicit fixtures. Production
//! durable Run accounting belongs to qbr.5.1.3.2, composition to .3.9.

use super::*;

#[path = "buffered_requests/framing.rs"]
mod framing;
#[path = "buffered_requests/owner.rs"]
mod owner;
use owner::Proof;

const BODY: &str = r#"{"model":"fixture-model","input":"synthetic","stream":true}"#;

fn frame(body: &str) -> Vec<u8> {
    format!(
        "POST /v1/responses HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

#[test]
fn each_complete_request_spends_one_reservation_even_in_one_write() {
    let mut proof = Proof::new(2);
    let bytes = frame(BODY).repeat(2);
    proof.feed(&bytes).unwrap();
    assert_eq!(proof.admit().unwrap(), Some(0));
    assert_eq!(proof.admit().unwrap(), Some(1));
    assert_eq!(proof.admit().unwrap(), None);
    assert_eq!(proof.effects(), 2);
    proof.feed(&frame(BODY)).unwrap();
    assert_eq!(proof.admit().unwrap_err().code, ErrorCode::CapabilityDenied);
    assert_eq!(proof.effects(), 2);
}

#[test]
fn no_effect_before_the_last_byte_of_a_split_request() {
    let bytes = frame(BODY);
    for split in 1..bytes.len() {
        let mut proof = Proof::new(1);
        proof.feed(&bytes[..split]).unwrap();
        assert_eq!(proof.admit().unwrap(), None);
        assert_eq!(proof.effects(), 0);
        proof.feed(&bytes[split..]).unwrap();
        assert_eq!(proof.admit().unwrap(), Some(0));
        assert_eq!(proof.effects(), 1);
    }
}

#[test]
fn queued_and_partial_requests_recheck_every_authority_dimension() {
    let changes: &[fn(&mut Proof)] = &[
        Proof::stop,
        |p| p.invalidate_sender(),
        |p| p.binding.session_id = "other-session".into(),
        |p| p.binding.run_id = "other-run".into(),
        |p| p.binding.authorization_id = "replacement-runtime".into(),
        |p| p.binding.identity_slot += 1,
        |p| p.binding.envelope_revision += 1,
        |p| p.current.envelope_revision += 1,
        |p| p.current.channel_state = louiselm_skills::launch_protocol::ChannelState::Revoked,
        |p| p.expire(),
    ];
    for change in changes {
        for partial in [false, true] {
            let mut proof = Proof::new(3);
            let bytes = frame(BODY);
            let split = if partial {
                bytes.len() - 1
            } else {
                bytes.len()
            };
            proof
                .feed(&[bytes.as_slice(), &bytes[..split]].concat())
                .unwrap();
            assert_eq!(proof.admit().unwrap(), Some(0));
            change(&mut proof);
            proof.feed(&bytes[split..]).unwrap();
            let error = proof.admit().unwrap_err();
            assert_eq!(error.code, ErrorCode::CapabilityDenied);
            assert!(!error.retryable);
            assert_eq!(proof.effects(), 1);
            assert_eq!(proof.remaining(), 2);
        }
    }
}

#[test]
fn expiry_or_sender_loss_during_status_io_is_rechecked_before_the_effect() {
    use std::sync::atomic::Ordering;
    for expire in [false, true] {
        let mut proof = Proof::new(1);
        let clock = Arc::clone(&proof.clock);
        let sender = Arc::clone(&proof.sender_enrolled);
        let deadline = proof.binding.expires_at_ms;
        proof.before_status_reply = Some(Box::new(move || {
            if expire {
                clock.store(deadline, Ordering::SeqCst);
            } else {
                sender.store(false, Ordering::SeqCst);
            }
        }));
        proof.feed(&frame(BODY)).unwrap();
        assert_eq!(proof.admit().unwrap_err().code, ErrorCode::CapabilityDenied);
        assert_eq!(proof.effects(), 0);
        assert_eq!(proof.remaining(), 1);
    }
}

#[test]
fn malformed_or_unreviewed_operations_never_reach_the_upstream() {
    let valid = String::from_utf8(frame(BODY)).unwrap();
    let bad = [
        valid.replace("POST ", "GET "),
        valid.replace("/v1/responses", "http://other/v1/responses"),
        valid.replace("/v1/responses", "/v1/responses?redirect=http://other"),
        valid.replace("/v1/responses", "/v1/responses/compact"),
        valid.replace("localhost", "other"),
        valid.replace("Host: localhost", "Host: localhost\r\nHost: localhost"),
        valid.replace("Content-Length:", "Transfer-Encoding: chunked\r\nContent-Length:"),
        valid.replace("Content-Type:", "Content-Length: 3\r\nContent-Type:"),
        valid.replace("application/json", "text/plain"),
        valid.replace("HTTP/1.1", "HTTP/1.0"),
        valid.replace("POST /v1/responses HTTP/1.1\r\n", "POST /v1/responses HTTP/1.1\n"),
        String::from_utf8(frame(r#"{"model":"fixture-model","model":"other","input":"synthetic","stream":true}"#)).unwrap(),
        String::from_utf8(frame(r#"{"model":"other","input":"synthetic","stream":true}"#)).unwrap(),
        String::from_utf8(frame(r#"{"model":"fixture-model","input":"synthetic","stream":true,"url":"https://other"}"#)).unwrap(),
        String::from_utf8(frame(r#"{"model":"fixture-model","input":"synthetic","stream":false}"#)).unwrap(),
        String::from_utf8(frame("{}garbage")).unwrap(),
        "POST /v1/responses HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: 999999999999999999999999999\r\n\r\n".into(),
        "POST /v1/responses HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: +5\r\n\r\n".into(),
    ];
    for bytes in bad {
        let mut proof = Proof::new(2);
        proof.feed(bytes.as_bytes()).unwrap();
        assert!(proof.admit().is_err(), "accepted {bytes:?}");
        assert_eq!(proof.effects(), 0);
        // A malformed connection cannot resynchronize onto a later valid frame.
        proof.feed(&frame(BODY)).unwrap();
        assert!(proof.admit().is_err());
        assert_eq!(proof.effects(), 0);
    }
}

#[test]
fn cancellation_retains_output_and_spend_and_discards_late_callbacks() {
    use owner::Reply;
    for expire in [false, true] {
        let mut proof = Proof::new(2);
        proof.feed(&frame(BODY)).unwrap();
        let id = proof.admit().unwrap().unwrap();
        proof.reply(id, Reply::Chunk(b"received".to_vec()));
        proof.poll();
        assert_eq!(proof.output(id), b"received");
        if expire {
            proof.expire();
            proof.tick();
        } else {
            proof.stop();
        }
        proof.reply(id, Reply::Chunk(b"late".to_vec()));
        proof.reply(id, Reply::Complete);
        proof.poll();
        assert_eq!(proof.output(id), b"received");
        assert!(proof.unknown(id));
        assert_eq!(proof.remaining(), 1);
        assert_eq!(proof.effects(), 1);
    }
}

#[test]
fn unknown_outcomes_and_redirects_never_refund_or_retry() {
    use owner::Reply;
    for reply in [Reply::Unknown, Reply::Redirect, Reply::Chunk(vec![0; 4097])] {
        let mut proof = Proof::new(1);
        proof.feed(&frame(BODY)).unwrap();
        let id = proof.admit().unwrap().unwrap();
        proof.reply(id, reply);
        proof.poll();
        proof.poll();
        assert!(proof.unknown(id));
        assert_eq!(proof.effects(), 1);
        assert_eq!(proof.remaining(), 0);
    }
}

#[test]
fn completed_output_is_not_relabelled_unknown_by_later_disposal() {
    let mut proof = Proof::new(1);
    proof.feed(&frame(BODY)).unwrap();
    let id = proof.admit().unwrap().unwrap();
    proof.reply(id, owner::Reply::Chunk(b"done".to_vec()));
    proof.reply(id, owner::Reply::Complete);
    proof.poll();
    proof.stop();
    assert_eq!(proof.output(id), b"done");
    assert!(!proof.unknown(id));
    assert_eq!(proof.remaining(), 0);
}

#[test]
fn bounded_buffers_and_incomplete_eof_cannot_start_an_effect() {
    for bytes in [vec![b'x'; 4097], vec![0; 16385]] {
        let mut proof = Proof::new(1);
        assert!(proof.feed(&bytes).and_then(|()| proof.admit()).is_err());
        assert_eq!(proof.effects(), 0);
    }
    let mut proof = Proof::new(1);
    let bytes = frame(BODY);
    proof.feed(&bytes[..bytes.len() - 1]).unwrap();
    assert_eq!(proof.admit().unwrap(), None);
    proof.stop(); // EOF closes the local connection; no partial request is admitted.
    proof.feed(&bytes[bytes.len() - 1..]).unwrap();
    assert!(proof.admit().is_err());
    assert_eq!(proof.effects(), 0);
}

#[test]
fn concurrent_controls_and_admission_are_serialized_by_the_session_owner() {
    // Independent producers target the same worker. Exercise both ordered outcomes
    // deterministically, with acknowledgements rather than scheduler sleeps.
    for stop_first in [false, true] {
        let (events, receive) = mpsc::channel();
        let (ack, acknowledged) = mpsc::channel();
        let worker = thread::spawn(move || {
            let mut proof = Proof::new(2);
            proof.feed(&frame(BODY).repeat(2)).unwrap();
            for stop in receive {
                if stop {
                    proof.stop();
                } else {
                    let _result = proof.admit();
                }
                ack.send(proof.effects()).unwrap();
            }
            proof.effects()
        });
        let first = events.clone();
        thread::spawn(move || first.send(stop_first).unwrap())
            .join()
            .unwrap();
        assert_eq!(
            acknowledged.recv_timeout(Duration::from_secs(5)).unwrap(),
            usize::from(!stop_first)
        );
        let second = events.clone();
        thread::spawn(move || second.send(!stop_first).unwrap())
            .join()
            .unwrap();
        assert_eq!(
            acknowledged.recv_timeout(Duration::from_secs(5)).unwrap(),
            usize::from(!stop_first)
        );
        events.send(false).unwrap();
        assert_eq!(
            acknowledged.recv_timeout(Duration::from_secs(5)).unwrap(),
            usize::from(!stop_first)
        );
        drop(events);
        assert_eq!(worker.join().unwrap(), usize::from(!stop_first));
    }
}

#[test]
#[ignore = "requires the opt-in BPF sendmmsg driver inside a disposable KVM guest"]
fn kernel_sendmmsg() {
    use std::io::{Read, Write};
    assert_eq!(
        std::env::var("LOUISELM_REQUEST_PROOF_VM").as_deref(),
        Ok("1")
    );
    assert!(rustix::process::geteuid().is_root());
    let vm = std::process::Command::new("systemd-detect-virt")
        .arg("--vm")
        .output()
        .unwrap();
    assert!(vm.status.success());
    assert_eq!(vm.stdout, b"kvm\n");
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    println!(
        "REQUEST_PROOF_ENDPOINT={}",
        serde_json::json!({
            "port": listener.local_addr().unwrap().port(),
            "payload": String::from_utf8(frame(BODY)).unwrap(),
        })
    );
    std::io::stdout().flush().unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut connection = loop {
        match listener.accept() {
            Ok((connection, _)) => break connection,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "sender did not connect"
                );
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("accept: {error}"),
        }
    };
    connection
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    connection
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut proof = Proof::new(2);
    while proof.effects() < 2 {
        let mut bytes = [0; 4096];
        let count = connection.read(&mut bytes).unwrap();
        assert!(count > 0, "sender closed before two requests");
        proof.feed(&bytes[..count]).unwrap();
        while proof.admit().unwrap().is_some() {
            connection
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .unwrap();
        }
    }
    assert_eq!(proof.effects(), 2);
    assert_eq!(proof.remaining(), 0);
    println!("REQUEST_PROOF_ADMISSIONS=2");
}
