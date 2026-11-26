//! Isolated voice archive and controlled transcription behavior.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Fixtures abort on setup failure and assert failures directly."
)]

use std::{
    collections::VecDeque,
    io::{Cursor, Read},
    process::Command,
    sync::{Arc, Mutex, mpsc},
    thread,
};

use louiselm_capture::{
    CaptureDraft, CaptureSource, Store, StoreError, Transcriber, TranscriptRequest,
    TranscriptionError, VoiceArchive, VoiceAssociation, VoiceStatus, VoiceTranscriptionWorker,
};

struct Fake(Mutex<VecDeque<Result<String, TranscriptionError>>>);

impl Transcriber for Fake {
    fn name(&self) -> &'static str {
        "fake"
    }
    fn model(&self) -> &'static str {
        "fixture"
    }
    fn transcribe(&self, request: TranscriptRequest<'_>) -> Result<String, TranscriptionError> {
        assert_eq!(std::fs::read(request.audio_path).unwrap(), b"dummy audio");
        self.0
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected provider call")
    }
}

fn capture(id: &str) -> CaptureDraft {
    CaptureDraft {
        id: id.into(),
        source: CaptureSource::Android,
        recorded_at_ms: 10,
        duration_ms: 20,
        mime_type: "audio/mp4".into(),
    }
}

fn association() -> VoiceAssociation {
    VoiceAssociation {
        receiver: "aa".repeat(32),
        controller: "11".repeat(16),
        subject: "22".repeat(16),
        project: "project".into(),
        agent: "agent".into(),
        draft: "33".repeat(32),
        revision: "44".repeat(32),
    }
}

#[test]
fn retries_are_bounded_across_reopen_duplicate_and_explicit_retry() {
    let root = tempfile::tempdir().unwrap();
    let ordinary = Store::new(root.path().join("ordinary")).unwrap();
    let voice = VoiceArchive::new(root.path().join("voice")).unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let draft = capture(&id);
    voice
        .admit(&draft, &association(), Cursor::new(b"dummy audio"))
        .unwrap();
    let provider = Fake(Mutex::new(VecDeque::from([
        Err(TranscriptionError::transient("temporary")),
        Err(TranscriptionError::transient("temporary")),
        Err(TranscriptionError::transient("temporary")),
        Ok("original transcript".into()),
    ])));
    for time in [1_000, 6_000, 16_000] {
        let reopened = VoiceArchive::new(root.path().join("voice")).unwrap();
        assert_eq!(
            VoiceTranscriptionWorker::new(&reopened, &provider)
                .process_ready(time)
                .unwrap(),
            1
        );
    }
    let job = voice.job(&id).unwrap();
    assert_eq!(job.status, VoiceStatus::Failed);
    assert_eq!(job.attempts, 3);
    voice
        .admit(&draft, &association(), Cursor::new(b"dummy audio"))
        .unwrap();
    assert_eq!(
        VoiceTranscriptionWorker::new(&voice, &provider)
            .process_ready(100_000)
            .unwrap(),
        0
    );
    voice.retry(&id, &"66".repeat(16)).unwrap();
    assert_eq!(
        VoiceTranscriptionWorker::new(&voice, &provider)
            .process_ready(100_000)
            .unwrap(),
        1
    );
    assert_eq!(voice.transcript(&id).unwrap().text, "original transcript");
    assert_eq!(voice.job(&id).unwrap().status, VoiceStatus::Completed);
    assert_eq!(ordinary.list().unwrap().len(), 0);
    assert_eq!(
        VoiceTranscriptionWorker::new(&voice, &provider)
            .process_ready(200_000)
            .unwrap(),
        0
    );
    let mut wrong = association();
    wrong.subject = "55".repeat(16);
    assert!(
        matches!(voice.admit(&draft, &wrong, Cursor::new(b"dummy audio")),
        Err(StoreError::Conflict(ref value)) if value == &id)
    );
    assert_eq!(voice.transcript(&id).unwrap().text, "original transcript");
}

struct BlockingFake {
    started: mpsc::Sender<()>,
    finish: Mutex<mpsc::Receiver<()>>,
}
impl Transcriber for BlockingFake {
    fn name(&self) -> &'static str {
        "fake"
    }
    fn model(&self) -> &'static str {
        "fixture"
    }
    fn transcribe(&self, _: TranscriptRequest<'_>) -> Result<String, TranscriptionError> {
        self.started.send(()).unwrap();
        self.finish.lock().unwrap().recv().unwrap();
        Ok("late original".into())
    }
}

#[test]
fn cancellation_fences_future_calls_but_keeps_an_inflight_result() {
    let root = tempfile::tempdir().unwrap();
    let voice = Arc::new(VoiceArchive::new(root.path().join("voice")).unwrap());
    let id = uuid::Uuid::new_v4().to_string();
    voice
        .admit(&capture(&id), &association(), Cursor::new(b"dummy audio"))
        .unwrap();
    let (started_tx, started_rx) = mpsc::channel();
    let (finish_tx, finish_rx) = mpsc::channel();
    let worker_voice = Arc::clone(&voice);
    let worker = thread::spawn(move || {
        let provider = BlockingFake {
            started: started_tx,
            finish: Mutex::new(finish_rx),
        };
        VoiceTranscriptionWorker::new(&worker_voice, &provider)
            .process_ready(1_000)
            .unwrap()
    });
    started_rx.recv().unwrap();
    voice.cancel(&id).unwrap();
    finish_tx.send(()).unwrap();
    assert_eq!(worker.join().unwrap(), 1);
    assert_eq!(voice.job(&id).unwrap().status, VoiceStatus::Cancelled);
    assert_eq!(voice.transcript(&id).unwrap().text, "late original");
    assert!(matches!(voice.retry(&id, &"66".repeat(16)),
        Err(StoreError::InvalidCapture(ref message)) if message == "voice job is not failed"));
    assert_eq!(
        std::fs::read(voice.capture(&id).unwrap().audio_path).unwrap(),
        b"dummy audio"
    );
}

struct ExitDuringCall;
impl Transcriber for ExitDuringCall {
    fn name(&self) -> &'static str {
        "fake"
    }
    fn model(&self) -> &'static str {
        "fixture"
    }
    fn transcribe(&self, _: TranscriptRequest<'_>) -> Result<String, TranscriptionError> {
        std::process::exit(42);
    }
}

#[test]
fn interrupted_worker_fixture() {
    let Some(root) = std::env::var_os("LOUISELM_VOICE_CRASH_ROOT") else {
        return;
    };
    let voice = VoiceArchive::new(root).unwrap();
    VoiceTranscriptionWorker::new(&voice, &ExitDuringCall)
        .process_ready(1_000)
        .unwrap();
    panic!("fixture did not exit inside the provider");
}

#[test]
fn interrupted_call_consumes_a_slot_after_process_restart() {
    let root = tempfile::tempdir().unwrap();
    let voice = VoiceArchive::new(root.path()).unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    voice
        .admit(&capture(&id), &association(), Cursor::new(b"dummy audio"))
        .unwrap();
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "interrupted_worker_fixture"])
        .env_clear()
        .env("LOUISELM_VOICE_CRASH_ROOT", root.path())
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(42));
    let reopened = VoiceArchive::new(root.path()).unwrap();
    assert_eq!(reopened.job(&id).unwrap().attempts, 1);
    assert_eq!(reopened.job(&id).unwrap().status, VoiceStatus::Running);
    let provider = Fake(Mutex::new(VecDeque::from([
        Err(TranscriptionError::transient("temporary")),
        Err(TranscriptionError::transient("temporary")),
    ])));
    let worker = VoiceTranscriptionWorker::new(&reopened, &provider);
    assert_eq!(worker.process_ready(5_999).unwrap(), 0);
    assert_eq!(worker.process_ready(6_000).unwrap(), 1);
    assert_eq!(worker.process_ready(16_000).unwrap(), 1);
    assert_eq!(reopened.job(&id).unwrap().attempts, 3);
    assert_eq!(reopened.job(&id).unwrap().status, VoiceStatus::Failed);
    assert_eq!(worker.process_ready(100_000).unwrap(), 0);
}

#[test]
fn cancelled_queued_work_permanent_failure_and_invalid_data_fail_closed() {
    let root = tempfile::tempdir().unwrap();
    let voice = VoiceArchive::new(root.path()).unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let draft = capture(&id);
    let mut invalid = association();
    invalid.controller = "00".repeat(16);
    assert!(
        matches!(voice.admit(&draft, &invalid, Cursor::new(b"dummy audio")),
        Err(StoreError::InvalidCapture(ref message)) if message == "voice association is invalid")
    );
    assert!(
        matches!(voice.admit(&draft, &association(), std::io::repeat(0).take(20 * 1024 * 1024 + 1)),
        Err(StoreError::TooLarge { limit, received }) if limit == 20 * 1024 * 1024 && received == limit + 1)
    );
    voice
        .admit(&draft, &association(), Cursor::new(b"dummy audio"))
        .unwrap();
    voice.cancel(&id).unwrap();
    let provider = Fake(Mutex::new(VecDeque::new()));
    assert_eq!(
        VoiceTranscriptionWorker::new(&voice, &provider)
            .process_ready(1_000)
            .unwrap(),
        0
    );
    assert!(matches!(voice.retry(&id, &"66".repeat(16)),
        Err(StoreError::InvalidCapture(ref message)) if message == "voice job is not failed"));
    assert_eq!(
        std::fs::read(voice.capture(&id).unwrap().audio_path).unwrap(),
        b"dummy audio"
    );

    let second = uuid::Uuid::new_v4().to_string();
    voice
        .admit(
            &capture(&second),
            &association(),
            Cursor::new(b"dummy audio"),
        )
        .unwrap();
    let provider = Fake(Mutex::new(VecDeque::from([Err(
        TranscriptionError::permanent("credentials rejected"),
    )])));
    assert_eq!(
        VoiceTranscriptionWorker::new(&voice, &provider)
            .process_ready(1_000)
            .unwrap(),
        1
    );
    assert_eq!(voice.job(&second).unwrap().status, VoiceStatus::Failed);
    assert_eq!(
        VoiceTranscriptionWorker::new(&voice, &provider)
            .process_ready(100_000)
            .unwrap(),
        0
    );
}

#[test]
fn contradictory_persisted_job_is_never_dispatched() {
    let root = tempfile::tempdir().unwrap();
    let voice = VoiceArchive::new(root.path()).unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    voice
        .admit(&capture(&id), &association(), Cursor::new(b"dummy audio"))
        .unwrap();
    let path = root.path().join(&id).join("voice-job.json");
    let mut json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    json["attempts"] = 3.into();
    std::fs::write(path, serde_json::to_vec(&json).unwrap()).unwrap();
    assert!(
        matches!(voice.job(&id), Err(StoreError::InvalidCapture(ref message)) if message == "voice job is invalid")
    );
    let provider = Fake(Mutex::new(VecDeque::new()));
    assert!(
        matches!(VoiceTranscriptionWorker::new(&voice, &provider).process_ready(1_000),
        Err(StoreError::InvalidCapture(ref message)) if message == "voice job is invalid")
    );
}

#[test]
fn duplicate_retry_cannot_restart_a_later_exhausted_cycle() {
    let root = tempfile::tempdir().unwrap();
    let voice = VoiceArchive::new(root.path()).unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    voice
        .admit(&capture(&id), &association(), Cursor::new(b"dummy audio"))
        .unwrap();
    let provider = Fake(Mutex::new(VecDeque::from(
        (0..6)
            .map(|_| Err(TranscriptionError::transient("temporary")))
            .collect::<Vec<_>>(),
    )));
    let worker = VoiceTranscriptionWorker::new(&voice, &provider);
    for time in [1_000, 6_000, 16_000] {
        assert_eq!(worker.process_ready(time).unwrap(), 1);
    }
    let operation = "66".repeat(16);
    // Simulate a process dying after the retry marker is durable but before
    // the failed job is advanced to its next cycle.
    std::fs::write(
        root.path()
            .join(&id)
            .join(format!("voice-retry-{operation}.json")),
        br#"{"schema_version":1,"cycle":1}"#,
    )
    .unwrap();
    voice.retry(&id, &operation).unwrap();
    for time in [20_000, 25_000, 35_000] {
        assert_eq!(worker.process_ready(time).unwrap(), 1);
    }
    assert_eq!(voice.job(&id).unwrap().status, VoiceStatus::Failed);
    VoiceArchive::new(root.path())
        .unwrap()
        .retry(&id, &operation)
        .unwrap();
    assert_eq!(voice.job(&id).unwrap().status, VoiceStatus::Failed);
    assert_eq!(worker.process_ready(100_000).unwrap(), 0);
}
