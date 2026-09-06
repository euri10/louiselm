use serde::{Deserialize, Serialize};

/// Source that produced an immutable raw capture.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureSource {
    /// `LouiseLM`'s Android recorder.
    Android,
    /// `LouiseLM`'s Neovim recorder adapter.
    Neovim,
}

/// Trusted metadata accompanying an audio stream before canonical ingestion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureDraft {
    /// Caller-generated UUID used for retry-safe ingestion.
    pub id: String,
    /// Surface that created the recording.
    pub source: CaptureSource,
    /// Unix epoch milliseconds at the start of recording.
    pub recorded_at_ms: u64,
    /// Recording duration in milliseconds.
    pub duration_ms: u64,
    /// Supported audio MIME type.
    pub mime_type: String,
}

/// Canonical immutable capture metadata written beside the original audio.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct CaptureRecord {
    /// Storage schema version.
    pub schema_version: u8,
    /// Capture UUID.
    pub id: String,
    /// Surface that created the recording.
    pub source: CaptureSource,
    /// Unix epoch milliseconds at the start of recording.
    pub recorded_at_ms: u64,
    /// Recording duration in milliseconds.
    pub duration_ms: u64,
    /// Canonical audio MIME type.
    pub mime_type: String,
    /// Original audio size in bytes.
    pub bytes: u64,
    /// Lowercase hexadecimal SHA-256 of the original audio.
    pub sha256: String,
}

/// Mutable processing state kept separate from the immutable capture.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct CaptureState {
    /// Storage schema version.
    pub schema_version: u8,
    /// Current transcription lifecycle.
    pub transcription: TranscriptionState,
}

/// Durable transcription lifecycle information.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct TranscriptionState {
    /// One of `pending`, `retrying`, `completed`, or `failed`.
    pub status: String,
    /// Completed provider calls, including failures.
    pub attempts: u32,
    /// Sanitized last failure message.
    pub last_error: Option<String>,
    /// Earliest Unix epoch milliseconds for the next attempt.
    pub next_attempt_at_ms: Option<u64>,
}

/// Immutable successful transcript associated with one capture.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct Transcript {
    /// Storage schema version.
    pub schema_version: u8,
    /// UUID of the source capture.
    pub capture_id: String,
    /// Provider implementation name.
    pub provider: String,
    /// Provider model identifier.
    pub model: String,
    /// Unix epoch milliseconds when transcription completed.
    pub created_at_ms: u64,
    /// Verbatim transcript returned by the provider.
    pub text: String,
}

impl Default for CaptureState {
    fn default() -> Self {
        Self {
            schema_version: 1,
            transcription: TranscriptionState {
                status: "pending".to_owned(),
                attempts: 0,
                last_error: None,
                next_attempt_at_ms: None,
            },
        }
    }
}
