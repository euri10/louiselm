use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::permissions::set_private_permissions;

use crate::{CaptureDraft, CaptureRecord, CaptureState, Transcript};

/// Maximum accepted original audio size: 20 MiB.
pub const MAX_CAPTURE_BYTES: u64 = 20 * 1024 * 1024;

/// Result of retry-safe capture ingestion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IngestOutcome {
    /// A new immutable capture was created.
    Created,
    /// The same UUID and audio digest already existed.
    Existing,
}

/// A capture loaded from the filesystem store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capture {
    /// Immutable manifest.
    pub record: CaptureRecord,
    /// Mutable processing state.
    pub state: CaptureState,
    /// Path to the immutable original audio.
    pub audio_path: PathBuf,
}

/// Durable capture-store failure.
#[derive(Debug, Error)]
pub enum StoreError {
    /// The submitted capture metadata is unsafe or unsupported.
    #[error("invalid capture: {0}")]
    InvalidCapture(String),
    /// The audio exceeds [`MAX_CAPTURE_BYTES`].
    #[error("capture is too large: limit {limit} bytes, received at least {received} bytes")]
    TooLarge {
        /// Configured maximum.
        limit: u64,
        /// Bytes observed before aborting.
        received: u64,
    },
    /// The UUID exists with different audio.
    #[error("capture conflicts with existing UUID: {0}")]
    Conflict(String),
    /// The requested capture does not exist.
    #[error("capture not found: {0}")]
    NotFound(String),
    /// Filesystem operation failed.
    #[error("capture storage failed: {0}")]
    Io(#[from] io::Error),
    /// Persisted JSON could not be decoded.
    #[error("capture data is malformed: {0}")]
    Json(#[from] serde_json::Error),
}

/// Filesystem-backed owner of immutable audio and capture manifests.
#[derive(Clone, Debug)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    /// Open or create a capture root.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the root cannot be created.
    pub fn new(root: impl AsRef<Path>) -> Result<Self, StoreError> {
        fs::create_dir_all(root.as_ref())?;
        set_private_permissions(root.as_ref(), true)?;
        Ok(Self {
            root: root.as_ref().to_path_buf(),
        })
    }

    /// Atomically ingest one original audio stream.
    ///
    /// # Errors
    ///
    /// Rejects unsafe metadata, oversized audio, conflicting UUID reuse, read
    /// failures, and filesystem failures. Partial incoming directories are
    /// removed before returning.
    pub fn ingest(
        &self,
        draft: CaptureDraft,
        mut audio: impl Read,
    ) -> Result<IngestOutcome, StoreError> {
        let extension = validate_draft(&draft)?;
        let incoming = self.root.join(format!(".incoming-{}", Uuid::new_v4()));
        fs::create_dir(&incoming)?;
        set_private_permissions(&incoming, true)?;

        let result = self.ingest_into(&draft, extension, &mut audio, &incoming);
        if result.is_err() || incoming.exists() {
            let _ = fs::remove_dir_all(&incoming);
        }
        result
    }

    fn ingest_into(
        &self,
        draft: &CaptureDraft,
        extension: &str,
        audio: &mut impl Read,
        incoming: &Path,
    ) -> Result<IngestOutcome, StoreError> {
        let audio_name = format!("audio.{extension}");
        let audio_path = incoming.join(&audio_name);
        let audio_file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&audio_path)?;
        set_private_permissions(&audio_path, false)?;
        let mut writer = BufWriter::new(audio_file);
        let mut hasher = Sha256::new();
        let mut bytes = 0_u64;
        let mut buffer = [0_u8; 16 * 1024];

        loop {
            let read = audio.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            bytes += read as u64;
            if bytes > MAX_CAPTURE_BYTES {
                return Err(StoreError::TooLarge {
                    limit: MAX_CAPTURE_BYTES,
                    received: bytes,
                });
            }
            writer.write_all(&buffer[..read])?;
            hasher.update(&buffer[..read]);
        }
        writer.flush()?;
        writer.get_ref().sync_all()?;

        let record = CaptureRecord {
            schema_version: 1,
            id: draft.id.clone(),
            source: draft.source,
            recorded_at_ms: draft.recorded_at_ms,
            duration_ms: draft.duration_ms,
            mime_type: draft.mime_type.clone(),
            bytes,
            sha256: format!("{:x}", hasher.finalize()),
        };
        let destination = self.root.join(&draft.id);
        if destination.exists() {
            let existing = self.capture(&draft.id)?;
            return if existing.record == record {
                Ok(IngestOutcome::Existing)
            } else {
                Err(StoreError::Conflict(draft.id.clone()))
            };
        }

        write_json(&incoming.join("capture.json"), &record)?;
        write_json(&incoming.join("state.json"), &CaptureState::default())?;
        File::open(incoming)?.sync_all()?;
        if let Err(error) = fs::rename(incoming, &destination) {
            if destination.is_dir() {
                let existing = self.capture(&draft.id)?;
                return if existing.record == record {
                    Ok(IngestOutcome::Existing)
                } else {
                    Err(StoreError::Conflict(draft.id.clone()))
                };
            }
            return Err(StoreError::Io(error));
        }
        File::open(&self.root)?.sync_all()?;
        Ok(IngestOutcome::Created)
    }

    /// Load one capture and resolve its original audio path.
    ///
    /// # Errors
    ///
    /// Returns `NotFound` for an unknown or invalid UUID, and reports malformed
    /// or unreadable persisted data explicitly.
    pub fn capture(&self, id: &str) -> Result<Capture, StoreError> {
        validate_id(id).map_err(|_| StoreError::NotFound(id.to_owned()))?;
        let directory = self.root.join(id);
        if !directory.is_dir() {
            return Err(StoreError::NotFound(id.to_owned()));
        }
        let record: CaptureRecord = read_json(&directory.join("capture.json"))?;
        let state: CaptureState = read_json(&directory.join("state.json"))?;
        let extension = extension_for_mime(&record.mime_type).ok_or_else(|| {
            StoreError::InvalidCapture("stored MIME type is unsupported".to_owned())
        })?;
        let audio_path = directory.join(format!("audio.{extension}"));
        if !audio_path.is_file() {
            return Err(StoreError::Io(io::Error::new(
                io::ErrorKind::NotFound,
                "original audio is missing",
            )));
        }
        Ok(Capture {
            record,
            state,
            audio_path,
        })
    }

    /// List every durable capture in recording order with UUID tie-breaking.
    ///
    /// # Errors
    ///
    /// Returns an error when the root or a capture cannot be read.
    pub fn list(&self) -> Result<Vec<Capture>, StoreError> {
        let mut ids = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(id) = name.to_str() else {
                continue;
            };
            if entry.file_type()?.is_dir() && validate_id(id).is_ok() {
                ids.push(id.to_owned());
            }
        }
        let mut captures = ids
            .into_iter()
            .map(|id| self.capture(&id))
            .collect::<Result<Vec<_>, StoreError>>()?;
        captures.sort_by(|left, right| {
            left.record
                .recorded_at_ms
                .cmp(&right.record.recorded_at_ms)
                .then_with(|| left.record.id.cmp(&right.record.id))
        });
        Ok(captures)
    }

    /// Load an immutable successful transcript.
    ///
    /// # Errors
    ///
    /// Returns `NotFound` until a transcript exists and reports malformed data.
    pub fn transcript(&self, id: &str) -> Result<Transcript, StoreError> {
        validate_id(id).map_err(|_| StoreError::NotFound(id.to_owned()))?;
        let path = self.root.join(id).join("transcript.json");
        if !path.is_file() {
            return Err(StoreError::NotFound(id.to_owned()));
        }
        read_json(&path)
    }

    /// Persist a transcript once and mark its capture completed.
    ///
    /// # Errors
    ///
    /// Rejects mismatched metadata and attempts to rewrite an existing
    /// transcript. Filesystem and serialization failures are returned.
    pub fn complete_transcription(
        &self,
        id: &str,
        transcript: &Transcript,
    ) -> Result<(), StoreError> {
        let capture = self.capture(id)?;
        if transcript.capture_id != id
            || transcript.schema_version != 1
            || transcript.provider.is_empty()
            || transcript.model.is_empty()
            || transcript.text.trim().is_empty()
            || transcript.created_at_ms == 0
        {
            return Err(StoreError::InvalidCapture(
                "transcript metadata is invalid".to_owned(),
            ));
        }
        let path = capture
            .audio_path
            .parent()
            .ok_or_else(|| StoreError::InvalidCapture("capture directory is invalid".to_owned()))?;
        let transcript_path = path.join("transcript.json");
        if transcript_path.exists() {
            return if self.transcript(id)? == *transcript {
                self.mark_completed(id)
            } else {
                Err(StoreError::Conflict(id.to_owned()))
            };
        }
        write_json(&transcript_path, transcript)?;
        File::open(path)?.sync_all()?;
        self.mark_completed(id)
    }

    /// Reset a failed or retrying capture to pending after explicit operator action.
    ///
    /// # Errors
    ///
    /// Returns capture read or state persistence failures.
    pub fn retry_transcription(&self, id: &str) -> Result<(), StoreError> {
        let capture = self.capture(id)?;
        let mut state = capture.state;
        if state.transcription.status == "completed" {
            return Ok(());
        }
        state.transcription.status = "pending".to_owned();
        state.transcription.last_error = None;
        state.transcription.next_attempt_at_ms = None;
        self.write_state(id, &state)
    }

    pub(crate) fn record_failure(
        &self,
        id: &str,
        now_ms: u64,
        message: &str,
        retry: bool,
    ) -> Result<(), StoreError> {
        let capture = self.capture(id)?;
        let mut state = capture.state;
        state.transcription.attempts = state.transcription.attempts.saturating_add(1);
        state.transcription.last_error = Some(message.to_owned());
        if retry {
            let shift = state.transcription.attempts.saturating_sub(1).min(9);
            let delay_ms = 5_000_u64.saturating_mul(1_u64 << shift);
            state.transcription.status = "retrying".to_owned();
            state.transcription.next_attempt_at_ms = Some(now_ms.saturating_add(delay_ms));
        } else {
            state.transcription.status = "failed".to_owned();
            state.transcription.next_attempt_at_ms = None;
        }
        self.write_state(id, &state)
    }

    pub(crate) fn mark_completed(&self, id: &str) -> Result<(), StoreError> {
        let capture = self.capture(id)?;
        let mut state = capture.state;
        state.transcription.status = "completed".to_owned();
        state.transcription.last_error = None;
        state.transcription.next_attempt_at_ms = None;
        self.write_state(id, &state)
    }

    fn write_state(&self, id: &str, state: &CaptureState) -> Result<(), StoreError> {
        let directory = self.root.join(id);
        write_json_atomic(&directory.join("state.json"), state)?;
        File::open(directory)?.sync_all()?;
        Ok(())
    }
}

fn validate_draft(draft: &CaptureDraft) -> Result<&'static str, StoreError> {
    validate_id(&draft.id)?;
    if draft.recorded_at_ms == 0 {
        return Err(StoreError::InvalidCapture(
            "recorded_at_ms must be positive".to_owned(),
        ));
    }
    if draft.duration_ms == 0 {
        return Err(StoreError::InvalidCapture(
            "duration_ms must be positive".to_owned(),
        ));
    }
    extension_for_mime(&draft.mime_type).ok_or_else(|| {
        StoreError::InvalidCapture(format!("unsupported audio MIME type: {}", draft.mime_type))
    })
}

fn validate_id(id: &str) -> Result<(), StoreError> {
    let parsed = Uuid::parse_str(id)
        .map_err(|_| StoreError::InvalidCapture("id must be a UUID".to_owned()))?;
    if parsed.to_string() != id.to_ascii_lowercase() {
        return Err(StoreError::InvalidCapture(
            "id must use canonical UUID text".to_owned(),
        ));
    }
    Ok(())
}

fn extension_for_mime(mime_type: &str) -> Option<&'static str> {
    match mime_type {
        "audio/mp4" => Some("m4a"),
        "audio/ogg" => Some("ogg"),
        "audio/wav" | "audio/x-wav" => Some("wav"),
        "audio/webm" => Some("webm"),
        _ => None,
    }
}

fn write_json(path: &Path, value: &impl serde::Serialize) -> Result<(), StoreError> {
    let file = OpenOptions::new().create_new(true).write(true).open(path)?;
    set_private_permissions(path, false)?;
    let mut writer = BufWriter::new(file);
    serde_json::to_writer_pretty(&mut writer, value)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    writer.get_ref().sync_all()?;
    Ok(())
}

fn write_json_atomic(path: &Path, value: &impl serde::Serialize) -> Result<(), StoreError> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| StoreError::InvalidCapture("state path is invalid".to_owned()))?;
    let temporary = path.with_file_name(format!(".{file_name}.{}", Uuid::new_v4()));
    let result = (|| {
        write_json(&temporary, value)?;
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    if temporary.exists() {
        let _ = fs::remove_file(temporary);
    }
    result
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, StoreError> {
    let reader = BufReader::new(File::open(path)?);
    Ok(serde_json::from_reader(reader)?)
}
