//! Verified occupancy reads must not observe an in-progress receipt append.
#![allow(clippy::unwrap_used, reason = "Tests own disposable receipt fixtures.")]

use super::*;
use std::{sync::mpsc, thread, time::Duration};

#[test]
fn verification_waits_for_in_progress_receipt_publication() {
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(
        ReceiptStore::open(
            root.path(),
            TrustedRelease {
                release_id: crate::Digest::of(b"release").to_string(),
                signing_key_id: "key".into(),
            },
        )
        .unwrap(),
    );
    let authorization = LaunchAuthorization {
        role: crate::launch_protocol::LaunchRole::Agent,
        conformance: crate::launch_protocol::ConformanceAuthorization::default(),
        schema: crate::launch_protocol::LAUNCH_AUTHORIZATION_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        authorization_id: "authorization".into(),
        request_id: "request".into(),
        request_digest: crate::Digest::of(b"request").to_string(),
        controller_uid: 1000,
        session_id: "session".into(),
        run_id: "run".into(),
        envelope_revision: 1,
        identity_slot: 0,
        assigned_uid: 2000,
        assigned_gid: 2000,
        provider_expires_at_ms: None,
        expires_at_ms: 50_000,
        broker_loss_grace_ms: 5000,
    };
    let appending = lock(&store.appending);
    let directory = store.session_directory("session").unwrap();
    fs::create_dir(&directory).unwrap();
    let pending = receipt_path(&directory, 0);
    // write_new_bytes creates this name before it writes and fsyncs the bytes.
    fs::write(&pending, []).unwrap();
    let (send, receive) = mpsc::channel();
    let reading = Arc::clone(&store);
    let started = Arc::new(std::sync::Barrier::new(2));
    let reader_started = Arc::clone(&started);
    let reader = thread::spawn(move || {
        reader_started.wait();
        send.send(reading.verified_chain(&authorization, &mut |_, _, _| true))
            .unwrap();
    });
    started.wait();
    let premature = receive.recv_timeout(Duration::from_millis(100));
    // Withdraw this test-only unfinished append: no durable receipt was made.
    fs::remove_file(pending).unwrap();
    drop(appending);
    reader.join().unwrap();
    assert!(matches!(premature, Err(mpsc::RecvTimeoutError::Timeout)));
    assert_eq!(
        receive.recv().unwrap().unwrap(),
        Vec::<SignedReceipt>::new()
    );
}
