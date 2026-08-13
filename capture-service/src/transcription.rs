use std::path::Path;

use thiserror::Error;

use crate::{Store, StoreError, Transcript};

/// Borrowed input passed to a transcription provider.
#[derive(Clone, Copy, Debug)]
pub struct TranscriptRequest<'a> {
    /// Original immutable audio file.
    pub audio_path: &'a Path,
    /// Canonical MIME type from the capture manifest.
    pub mime_type: &'a str,
}

/// Whether a transcription failure may succeed without operator action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TranscriptionErrorKind {
    /// Network, rate-limit, or service failure suitable for backoff.
    Transient,
    /// Authentication, configuration, or unsupported-input failure.
    Permanent,
}

/// Sanitized provider failure safe to persist and display.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("{message}")]
pub struct TranscriptionError {
    kind: TranscriptionErrorKind,
    message: String,
}

impl TranscriptionError {
    /// Construct a retryable provider failure.
    #[must_use]
    pub fn transient(message: impl Into<String>) -> Self {
        Self {
            kind: TranscriptionErrorKind::Transient,
            message: message.into(),
        }
    }

    /// Construct a provider failure that requires explicit intervention.
    #[must_use]
    pub fn permanent(message: impl Into<String>) -> Self {
        Self {
            kind: TranscriptionErrorKind::Permanent,
            message: message.into(),
        }
    }

    /// Failure classification used by the durable worker.
    #[must_use]
    pub fn kind(&self) -> TranscriptionErrorKind {
        self.kind
    }
}

/// Narrow provider boundary for deterministic transcription-worker tests.
pub trait Transcriber {
    /// Stable provider name recorded in successful transcripts.
    fn name(&self) -> &str;
    /// Effective provider model identifier.
    fn model(&self) -> &str;
    /// Transcribe one immutable original recording.
    ///
    /// # Errors
    ///
    /// Returns a sanitized transient or permanent provider failure.
    fn transcribe(&self, request: TranscriptRequest<'_>) -> Result<String, TranscriptionError>;
}

/// Restart-safe state machine that advances ready captures.
pub struct TranscriptionWorker<'a, T> {
    store: &'a Store,
    provider: &'a T,
}

impl<'a, T: Transcriber> TranscriptionWorker<'a, T> {
    /// Bind one durable store to one configured provider.
    #[must_use]
    pub fn new(store: &'a Store, provider: &'a T) -> Self {
        Self { store, provider }
    }

    /// Attempt every pending or due-retry capture once.
    ///
    /// # Errors
    ///
    /// Returns durable store failures. Provider failures are persisted as
    /// retrying or failed state and do not abort the remaining captures.
    pub fn process_ready(&self, now_ms: u64) -> Result<usize, StoreError> {
        let mut processed = 0;
        for capture in self.store.list()? {
            let transcription = &capture.state.transcription;
            let ready = transcription.status == "pending"
                || (transcription.status == "retrying"
                    && transcription
                        .next_attempt_at_ms
                        .is_some_and(|due| due <= now_ms));
            if !ready {
                continue;
            }

            if self.store.transcript(&capture.record.id).is_ok() {
                self.store.mark_completed(&capture.record.id)?;
                processed += 1;
                continue;
            }

            let request = TranscriptRequest {
                audio_path: &capture.audio_path,
                mime_type: &capture.record.mime_type,
            };
            match self.provider.transcribe(request) {
                Ok(text) if !text.trim().is_empty() => {
                    let transcript = Transcript {
                        schema_version: 1,
                        capture_id: capture.record.id.clone(),
                        provider: self.provider.name().to_owned(),
                        model: self.provider.model().to_owned(),
                        created_at_ms: now_ms,
                        text,
                    };
                    self.store
                        .complete_transcription(&capture.record.id, &transcript)?;
                }
                Ok(_) => self.store.record_failure(
                    &capture.record.id,
                    now_ms,
                    "provider returned an empty transcript",
                    false,
                )?,
                Err(error) => self.store.record_failure(
                    &capture.record.id,
                    now_ms,
                    &error.message,
                    error.kind == TranscriptionErrorKind::Transient,
                )?,
            }
            processed += 1;
        }
        Ok(processed)
    }
}
