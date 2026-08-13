use std::{collections::VecDeque, io::Cursor, sync::Mutex};

use louiselm_capture::{
    CaptureDraft, CaptureSource, Store, Transcriber, TranscriptRequest, TranscriptionError,
    TranscriptionWorker,
};

struct FakeTranscriber {
    responses: Mutex<VecDeque<Result<String, TranscriptionError>>>,
}

impl Transcriber for FakeTranscriber {
    fn name(&self) -> &str {
        "fake"
    }

    fn model(&self) -> &str {
        "accurate"
    }

    fn transcribe(&self, request: TranscriptRequest<'_>) -> Result<String, TranscriptionError> {
        assert!(request.audio_path.is_file());
        self.responses
            .lock()
            .expect("responses")
            .pop_front()
            .expect("configured response")
    }
}

fn ingest(store: &Store, id: &str) {
    store
        .ingest(
            CaptureDraft {
                id: id.to_owned(),
                source: CaptureSource::Android,
                recorded_at_ms: 1_765_000_000_000,
                duration_ms: 1_000,
                mime_type: "audio/mp4".to_owned(),
            },
            Cursor::new(b"speech"),
        )
        .expect("ingest");
}

#[test]
fn worker_retries_transient_failures_after_backoff_and_completes_once() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let store = Store::new(temporary.path()).expect("store");
    let id = uuid::Uuid::new_v4().to_string();
    ingest(&store, &id);
    let provider = FakeTranscriber {
        responses: Mutex::new(VecDeque::from([
            Err(TranscriptionError::transient("service unavailable")),
            Ok("captured idea".to_owned()),
        ])),
    };
    let worker = TranscriptionWorker::new(&store, &provider);

    assert_eq!(worker.process_ready(1_000).expect("first pass"), 1);
    let retrying = store.capture(&id).expect("retrying");
    assert_eq!(retrying.state.transcription.status, "retrying");
    assert_eq!(retrying.state.transcription.attempts, 1);
    assert_eq!(
        retrying.state.transcription.last_error.as_deref(),
        Some("service unavailable")
    );
    assert_eq!(worker.process_ready(5_999).expect("too early"), 0);
    assert_eq!(worker.process_ready(6_000).expect("second pass"), 1);
    assert_eq!(
        store
            .capture(&id)
            .expect("completed")
            .state
            .transcription
            .status,
        "completed"
    );
    assert_eq!(
        store.transcript(&id).expect("transcript").text,
        "captured idea"
    );
    assert_eq!(worker.process_ready(7_000).expect("already complete"), 0);
}

#[test]
fn permanent_failure_waits_for_explicit_retry() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let store = Store::new(temporary.path()).expect("store");
    let id = uuid::Uuid::new_v4().to_string();
    ingest(&store, &id);
    let provider = FakeTranscriber {
        responses: Mutex::new(VecDeque::from([
            Err(TranscriptionError::permanent("authentication failed")),
            Ok("after repair".to_owned()),
        ])),
    };
    let worker = TranscriptionWorker::new(&store, &provider);

    assert_eq!(worker.process_ready(1_000).expect("failure"), 1);
    assert_eq!(
        store
            .capture(&id)
            .expect("failed")
            .state
            .transcription
            .status,
        "failed"
    );
    assert_eq!(worker.process_ready(99_000).expect("paused"), 0);
    store.retry_transcription(&id).expect("explicit retry");
    assert_eq!(worker.process_ready(99_000).expect("repaired"), 1);
    assert_eq!(
        store.transcript(&id).expect("transcript").text,
        "after repair"
    );
}
