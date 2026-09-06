//! Behavioral coverage for store.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Test fixtures abort on setup failure and assert failures directly."
)]

use std::io::{Cursor, Read};
use std::sync::{Arc, Barrier};

use louiselm_capture::{
    CaptureDraft, CaptureSource, IngestOutcome, MAX_CAPTURE_BYTES, Store, StoreError, Transcript,
};

fn draft(id: &str) -> CaptureDraft {
    CaptureDraft {
        id: id.to_owned(),
        source: CaptureSource::Android,
        recorded_at_ms: 1_765_000_000_000,
        duration_ms: 4_200,
        mime_type: "audio/mp4".to_owned(),
    }
}

#[test]
fn listing_is_stable_and_transcripts_are_immutable() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let store = Store::new(temporary.path()).expect("store");
    let later_id = "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee";
    let earlier_id = "11111111-1111-4111-8111-111111111111";
    store
        .ingest(&draft(later_id), Cursor::new(b"later"))
        .expect("later");
    store
        .ingest(&draft(earlier_id), Cursor::new(b"earlier"))
        .expect("earlier");

    let listed = store.list().expect("list");
    assert_eq!(
        listed
            .iter()
            .map(|capture| capture.record.id.as_str())
            .collect::<Vec<_>>(),
        vec![earlier_id, later_id]
    );

    let transcript = Transcript {
        schema_version: 1,
        capture_id: earlier_id.to_owned(),
        provider: "fake".to_owned(),
        model: "accurate".to_owned(),
        created_at_ms: 1_765_000_001_000,
        text: "an idea".to_owned(),
    };
    store
        .complete_transcription(earlier_id, &transcript)
        .expect("complete");
    assert_eq!(
        store.transcript(earlier_id).expect("transcript"),
        transcript
    );
    assert!(matches!(
        store.complete_transcription(
            earlier_id,
            &Transcript {
                text: "rewritten".to_owned(),
                ..transcript.clone()
            }
        ),
        Err(StoreError::Conflict(_))
    ));
}

#[test]
fn listing_preserves_capture_time_with_uuid_as_a_stable_tie_breaker() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let store = Store::new(temporary.path()).expect("store");
    let earlier_id = "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee";
    let later_id = "11111111-1111-4111-8111-111111111111";
    let mut later = draft(later_id);
    later.recorded_at_ms += 1_000;

    store.ingest(&later, Cursor::new(b"later")).expect("later");
    store
        .ingest(&draft(earlier_id), Cursor::new(b"earlier"))
        .expect("earlier");

    assert_eq!(
        store
            .list()
            .expect("list")
            .iter()
            .map(|capture| capture.record.id.as_str())
            .collect::<Vec<_>>(),
        vec![earlier_id, later_id]
    );
}

#[test]
fn ingest_preserves_audio_manifest_and_pending_state() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let store = Store::new(temporary.path()).expect("store");
    let capture_id = uuid::Uuid::new_v4().to_string();

    let outcome = store
        .ingest(&draft(&capture_id), Cursor::new(b"speech"))
        .expect("ingest");

    assert_eq!(outcome, IngestOutcome::Created);
    let capture = store.capture(&capture_id).expect("capture");
    assert_eq!(capture.record.id, capture_id);
    assert_eq!(capture.record.bytes, 6);
    assert_eq!(capture.record.sha256.len(), 64);
    assert_eq!(capture.state.transcription.status, "pending");
    assert_eq!(
        std::fs::read(&capture.audio_path).expect("audio"),
        b"speech"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let capture_directory = capture.audio_path.parent().expect("capture directory");
        assert_eq!(
            std::fs::metadata(temporary.path())
                .expect("root mode")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(capture_directory)
                .expect("capture mode")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(&capture.audio_path)
                .expect("audio mode")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[test]
fn repeated_identical_ingest_is_idempotent_but_conflicting_audio_is_rejected() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let store = Store::new(temporary.path()).expect("store");
    let capture_id = uuid::Uuid::new_v4().to_string();

    assert_eq!(
        store
            .ingest(&draft(&capture_id), Cursor::new(b"same"))
            .expect("first ingest"),
        IngestOutcome::Created
    );
    assert_eq!(
        store
            .ingest(&draft(&capture_id), Cursor::new(b"same"))
            .expect("repeat ingest"),
        IngestOutcome::Existing
    );
    assert!(matches!(
        store.ingest(&draft(&capture_id), Cursor::new(b"different")),
        Err(StoreError::Conflict(_))
    ));

    let mut changed_metadata = draft(&capture_id);
    changed_metadata.duration_ms += 1;
    assert!(matches!(
        store.ingest(&changed_metadata, Cursor::new(b"same")),
        Err(StoreError::Conflict(_))
    ));
}

#[test]
fn oversized_capture_is_rejected_without_a_partial_capture() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let store = Store::new(temporary.path()).expect("store");
    let capture_id = uuid::Uuid::new_v4().to_string();
    let audio = std::io::repeat(0).take(MAX_CAPTURE_BYTES + 1);

    assert!(matches!(
        store.ingest(&draft(&capture_id), audio),
        Err(StoreError::TooLarge { .. })
    ));
    assert!(matches!(
        store.capture(&capture_id),
        Err(StoreError::NotFound(_))
    ));
}

#[test]
fn untrusted_ids_and_audio_types_are_rejected() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let store = Store::new(temporary.path()).expect("store");
    let mut invalid = draft("../escape");

    assert!(matches!(
        store.ingest(&invalid.clone(), Cursor::new(b"speech")),
        Err(StoreError::InvalidCapture(_))
    ));
    invalid.id = uuid::Uuid::new_v4().to_string();
    "text/plain".clone_into(&mut invalid.mime_type);
    assert!(matches!(
        store.ingest(&invalid, Cursor::new(b"speech")),
        Err(StoreError::InvalidCapture(_))
    ));
}

#[test]
fn concurrent_identical_ingest_creates_one_capture_without_spurious_failure() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let store = Store::new(temporary.path()).expect("store");
    let capture_id = uuid::Uuid::new_v4().to_string();
    let barrier = Arc::new(Barrier::new(8));
    let mut threads = Vec::new();

    for _ in 0..8 {
        let store = store.clone();
        let capture_id = capture_id.clone();
        let barrier = Arc::clone(&barrier);
        threads.push(std::thread::spawn(move || {
            barrier.wait();
            store.ingest(&draft(&capture_id), Cursor::new(b"same"))
        }));
    }

    let outcomes = threads
        .into_iter()
        .map(|thread| thread.join().expect("thread").expect("ingest"))
        .collect::<Vec<_>>();
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| **outcome == IngestOutcome::Created)
            .count(),
        1
    );
    assert_eq!(store.list().expect("list").len(), 1);
}
