//! Separate, durable voice jobs. This module does not grant phone authority.

use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

use fs2::FileExt;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use uuid::Uuid;

use crate::{
    Capture, CaptureDraft, IngestOutcome, Store, StoreError, Transcriber, Transcript,
    TranscriptRequest, TranscriptionErrorKind, permissions::set_private_permissions,
};

const MAX_JOB_BYTES: u64 = 16 * 1024;
const MAX_RESULT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_ATTEMPTS: u8 = 3;

/// Immutable identities captured with a voice recording.
///
/// The caller must derive and authorize the project, Agent and exact Session
/// using the trusted current owner, not trust these fields as authority.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VoiceAssociation {
    /// Receiver public-key identity, as lowercase SHA-256 hex.
    pub receiver: String,
    /// Exact controller identity, as lowercase 128-bit hex.
    pub controller: String,
    /// Exact Session identity, as lowercase 128-bit hex.
    pub subject: String,
    /// Authorized source project identity.
    pub project: String,
    /// Authorized source Agent identity.
    pub agent: String,
    /// Device-local draft identity, as lowercase SHA-256 hex.
    pub draft: String,
    /// Exact device-local draft revision, as lowercase SHA-256 hex.
    pub revision: String,
}

/// Durable status of one voice transcription cycle.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VoiceStatus {
    /// An admitted job is ready for its first call.
    Pending,
    /// A call slot has been durably reserved and may be in flight.
    Running,
    /// A temporary failure is waiting for backoff.
    Retrying,
    /// A transcript is retained.
    Completed,
    /// No further automatic calls are allowed without explicit retry.
    Failed,
    /// Cancellation fences all future calls, including retries.
    Cancelled,
}

/// Durable job metadata; audio and the original transcript are separate.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VoiceJob {
    /// Storage schema version.
    pub schema_version: u8,
    /// Exact immutable source and draft association.
    pub association: VoiceAssociation,
    /// Monotonic explicit-retry cycle, starting at zero.
    pub cycle: u64,
    /// Current cycle state.
    pub status: VoiceStatus,
    /// Provider call slots reserved in this cycle, including interrupted calls.
    pub attempts: u8,
    /// Earliest retry time, including recovery of a lost in-flight response.
    pub next_attempt_at_ms: Option<u64>,
    /// Sanitized provider failure, if any.
    pub last_error: Option<String>,
}

/// Isolated voice collection. It neither authenticates nor authorizes callers.
///
/// All methods perform blocking filesystem I/O. Callers must keep them off an
/// async executor and must authorize each operation at the current owner boundary.
#[derive(Clone, Debug)]
pub struct VoiceArchive {
    root: PathBuf,
    captures: Store,
}

impl VoiceArchive {
    /// Open a separate private voice collection.
    ///
    /// # Errors
    ///
    /// Returns filesystem failures.
    pub fn new(root: impl AsRef<Path>) -> Result<Self, StoreError> {
        let root = root.as_ref().to_path_buf();
        if fs::symlink_metadata(&root).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
            return Err(StoreError::InvalidCapture("voice root is a symlink".into()));
        }
        let captures = Store::new(&root)?;
        Ok(Self { root, captures })
    }

    /// Archive an authorized job and original audio, idempotently by UUID.
    ///
    /// This method is not an authorization boundary. The caller must stage
    /// network input privately first and serialize a fresh exact-request
    /// authority check with publication, including revocation and expiry.
    ///
    /// # Errors
    ///
    /// Rejects invalid identities, oversized audio, conflicting UUIDs and I/O failures.
    pub fn admit(
        &self,
        draft: &CaptureDraft,
        association: &VoiceAssociation,
        audio: impl Read,
    ) -> Result<IngestOutcome, StoreError> {
        validate_id(&draft.id)?;
        validate_association(association)?;
        if fs::symlink_metadata(self.root.join(&draft.id))
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return Err(StoreError::InvalidCapture(
                "voice directory is a symlink".into(),
            ));
        }
        let outcome = self.captures.ingest(draft, audio)?;
        self.locked(|| {
            let path = self.job_path(&draft.id)?;
            if path.exists() {
                let job = read_job(&path)?;
                if job.association != *association {
                    return Err(StoreError::Conflict(draft.id.clone()));
                }
            } else {
                write_atomic(
                    &path,
                    &VoiceJob {
                        schema_version: 1,
                        association: association.clone(),
                        cycle: 0,
                        status: VoiceStatus::Pending,
                        attempts: 0,
                        next_attempt_at_ms: None,
                        last_error: None,
                    },
                )?;
            }
            Ok(outcome)
        })
    }

    /// Load the immutable original capture.
    ///
    /// # Errors
    ///
    /// Returns invalid identifiers, missing records and filesystem failures.
    pub fn capture(&self, id: &str) -> Result<Capture, StoreError> {
        validate_id(id)?;
        self.captures.capture(id)
    }

    /// Load one job, without treating its association as read authority.
    ///
    /// # Errors
    ///
    /// Returns invalid identifiers, missing or malformed jobs and I/O failures.
    pub fn job(&self, id: &str) -> Result<VoiceJob, StoreError> {
        self.locked(|| read_job(&self.job_path(id)?))
    }

    /// Load the original unedited transcript, if one has been retained.
    ///
    /// # Errors
    ///
    /// Returns invalid identifiers, missing or malformed results and I/O failures.
    pub fn transcript(&self, id: &str) -> Result<Transcript, StoreError> {
        self.locked(|| self.read_transcript(id))
    }

    /// Fence a job against future calls; an in-flight result remains archival.
    ///
    /// # Errors
    ///
    /// Returns missing or malformed jobs and filesystem failures.
    pub fn cancel(&self, id: &str) -> Result<(), StoreError> {
        self.locked(|| {
            let path = self.job_path(id)?;
            let mut job = read_job(&path)?;
            job.status = VoiceStatus::Cancelled;
            job.next_attempt_at_ms = None;
            write_atomic(&path, &job)
        })
    }

    /// Start a new bounded cycle after an explicit operator retry.
    /// The exact request's stable lowercase 128-bit operation ID is retained so
    /// a delayed duplicate cannot start another cycle, even after a restart.
    ///
    /// # Errors
    ///
    /// Rejects jobs that are not failed, and reports storage failures.
    pub fn retry(&self, id: &str, operation_id: &str) -> Result<(), StoreError> {
        if !valid_hex(operation_id, 32) {
            return Err(StoreError::InvalidCapture(
                "voice retry identity is invalid".into(),
            ));
        }
        self.locked(|| {
            let path = self.job_path(id)?;
            let mut job = read_job(&path)?;
            let marker_path = path.with_file_name(format!("voice-retry-{operation_id}.json"));
            let cycle = if marker_path.exists() {
                let marker: RetryMarker = read_bounded(&marker_path, MAX_JOB_BYTES)?;
                if marker.schema_version != 1 || marker.cycle == 0 {
                    return Err(StoreError::InvalidCapture(
                        "voice retry marker is invalid".into(),
                    ));
                }
                if marker.cycle <= job.cycle {
                    return Ok(());
                }
                marker.cycle
            } else {
                if job.status != VoiceStatus::Failed {
                    return Err(StoreError::InvalidCapture("voice job is not failed".into()));
                }
                let cycle = job
                    .cycle
                    .checked_add(1)
                    .ok_or_else(|| StoreError::InvalidCapture("voice cycle exhausted".into()))?;
                write_atomic(
                    &marker_path,
                    &RetryMarker {
                        schema_version: 1,
                        cycle,
                    },
                )?;
                cycle
            };
            if job.status != VoiceStatus::Failed {
                return Err(StoreError::InvalidCapture("voice job is not failed".into()));
            }
            if job.cycle.checked_add(1) != Some(cycle) {
                return Err(StoreError::InvalidCapture(
                    "voice retry marker is inconsistent".into(),
                ));
            }
            job.cycle = cycle;
            job.status = VoiceStatus::Pending;
            job.attempts = 0;
            job.next_attempt_at_ms = None;
            job.last_error = None;
            write_atomic(&path, &job)
        })
    }

    fn job_path(&self, id: &str) -> Result<PathBuf, StoreError> {
        validate_id(id)?;
        let directory = self.root.join(id);
        if fs::symlink_metadata(&directory).is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return Err(StoreError::InvalidCapture(
                "voice directory is a symlink".into(),
            ));
        }
        if !directory.is_dir() {
            return Err(StoreError::NotFound(id.into()));
        }
        Ok(directory.join("voice-job.json"))
    }

    fn result_path(&self, id: &str) -> Result<PathBuf, StoreError> {
        Ok(self.job_path(id)?.with_file_name("voice-transcript.json"))
    }

    fn read_transcript(&self, id: &str) -> Result<Transcript, StoreError> {
        let path = self.result_path(id)?;
        if !path.exists() {
            return Err(StoreError::NotFound(id.into()));
        }
        let transcript: Transcript = read_bounded(&path, MAX_RESULT_BYTES)?;
        if transcript.schema_version != 1
            || transcript.capture_id != id
            || transcript.text.trim().is_empty()
            || transcript.provider.is_empty()
            || transcript.model.is_empty()
            || transcript.created_at_ms == 0
        {
            return Err(StoreError::InvalidCapture(
                "voice transcript is invalid".into(),
            ));
        }
        Ok(transcript)
    }

    fn locked<T>(&self, f: impl FnOnce() -> Result<T, StoreError>) -> Result<T, StoreError> {
        let file = lock_file(&self.root.join(".voice-lock"))?;
        file.lock_exclusive()?;
        let result = f();
        let unlocked = FileExt::unlock(&file);
        result.and_then(|value| {
            unlocked?;
            Ok(value)
        })
    }

    fn reserve(&self, id: &str, now_ms: u64) -> Result<bool, StoreError> {
        self.locked(|| {
            let path = self.job_path(id)?;
            if !path.exists() {
                return Ok(false);
            }
            let mut job = read_job(&path)?;
            if matches!(
                job.status,
                VoiceStatus::Cancelled | VoiceStatus::Completed | VoiceStatus::Failed
            ) {
                return Ok(false);
            }
            if self.result_path(id)?.exists() {
                self.read_transcript(id)?;
                job.status = VoiceStatus::Completed;
                job.next_attempt_at_ms = None;
                job.last_error = None;
                write_atomic(&path, &job)?;
                return Ok(false);
            }
            if job.next_attempt_at_ms.is_some_and(|due| due > now_ms) {
                return Ok(false);
            }
            if job.attempts >= MAX_ATTEMPTS {
                job.status = VoiceStatus::Failed;
                job.next_attempt_at_ms = None;
                write_atomic(&path, &job)?;
                return Ok(false);
            }
            job.attempts += 1;
            job.status = VoiceStatus::Running;
            let delay = 5_000_u64 << (job.attempts - 1);
            job.next_attempt_at_ms = Some(now_ms.saturating_add(delay));
            write_atomic(&path, &job)?;
            Ok(true)
        })
    }

    fn finish(
        &self,
        id: &str,
        result: Result<Transcript, (String, bool)>,
    ) -> Result<(), StoreError> {
        self.locked(|| {
            let path = self.job_path(id)?;
            let mut job = read_job(&path)?;
            let cancelled = job.status == VoiceStatus::Cancelled;
            match result {
                Ok(transcript) => {
                    let result_path = self.result_path(id)?;
                    if result_path.exists() {
                        if self.read_transcript(id)? != transcript {
                            return Err(StoreError::Conflict(id.into()));
                        }
                    } else {
                        write_atomic(&result_path, &transcript)?;
                    }
                    if !cancelled {
                        job.status = VoiceStatus::Completed;
                    }
                    job.next_attempt_at_ms = None;
                    job.last_error = None;
                }
                Err((message, transient)) if !cancelled => {
                    job.last_error = Some(message);
                    if transient && job.attempts < MAX_ATTEMPTS {
                        job.status = VoiceStatus::Retrying;
                    } else {
                        job.status = VoiceStatus::Failed;
                        job.next_attempt_at_ms = None;
                    }
                }
                Err(_) => {}
            }
            write_atomic(&path, &job)
        })
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RetryMarker {
    schema_version: u8,
    cycle: u64,
}

/// Controlled worker using the existing explicitly configured provider.
pub struct VoiceTranscriptionWorker<'a, T> {
    archive: &'a VoiceArchive,
    provider: &'a T,
}

impl<'a, T: Transcriber> VoiceTranscriptionWorker<'a, T> {
    /// Bind a separate voice archive and configured transcription provider.
    #[must_use]
    pub fn new(archive: &'a VoiceArchive, provider: &'a T) -> Self {
        Self { archive, provider }
    }

    /// Advance each due job once, reserving a call durably before dispatch.
    ///
    /// This method blocks, including while a provider request is in flight.
    /// A separate worker lock serializes processes without blocking cancellation.
    ///
    /// # Errors
    ///
    /// Returns storage failures. Provider errors become bounded durable states.
    pub fn process_ready(&self, now_ms: u64) -> Result<usize, StoreError> {
        if now_ms == 0 {
            return Err(StoreError::InvalidCapture("voice time is invalid".into()));
        }
        let lock = lock_file(&self.archive.root.join(".voice-worker-lock"))?;
        lock.lock_exclusive()?;
        let result = self.process_locked(now_ms);
        let unlocked = FileExt::unlock(&lock);
        result.and_then(|value| {
            unlocked?;
            Ok(value)
        })
    }

    fn process_locked(&self, now_ms: u64) -> Result<usize, StoreError> {
        let mut processed = 0;
        for capture in self.archive.captures.list()? {
            let id = &capture.record.id;
            if !self.archive.reserve(id, now_ms)? {
                continue;
            }
            let request = TranscriptRequest {
                audio_path: &capture.audio_path,
                mime_type: &capture.record.mime_type,
            };
            let result = match self.provider.transcribe(request) {
                Ok(text) => {
                    let transcript = Transcript {
                        schema_version: 1,
                        capture_id: id.clone(),
                        provider: self.provider.name().into(),
                        model: self.provider.model().into(),
                        created_at_ms: now_ms,
                        text,
                    };
                    let valid = !transcript.text.trim().is_empty()
                        && !transcript.provider.is_empty()
                        && !transcript.model.is_empty()
                        && (serde_json::to_vec(&transcript)?.len() as u64) < MAX_RESULT_BYTES;
                    if valid {
                        Ok(transcript)
                    } else {
                        Err((
                            "provider returned an invalid or oversized transcript".into(),
                            false,
                        ))
                    }
                }
                Err(error) => {
                    let transient = error.kind() == TranscriptionErrorKind::Transient;
                    let message = error.to_string();
                    Err((
                        if message.is_empty() || message.len() > 1024 {
                            "transcription provider failed".into()
                        } else {
                            message
                        },
                        transient,
                    ))
                }
            };
            self.archive.finish(id, result)?;
            processed += 1;
        }
        Ok(processed)
    }
}

fn validate_id(id: &str) -> Result<(), StoreError> {
    if Uuid::parse_str(id).is_ok_and(|value| value.to_string() == id) {
        return Ok(());
    }
    Err(StoreError::InvalidCapture(
        "voice id must be a canonical UUID".into(),
    ))
}

fn validate_association(value: &VoiceAssociation) -> Result<(), StoreError> {
    if !valid_hex(&value.receiver, 64)
        || !valid_hex(&value.controller, 32)
        || !valid_hex(&value.subject, 32)
        || value.controller.bytes().all(|byte| byte == b'0')
        || value.subject.bytes().all(|byte| byte == b'0')
        || !valid_hex(&value.draft, 64)
        || !valid_hex(&value.revision, 64)
        || [&value.project, &value.agent]
            .iter()
            .any(|item| item.is_empty() || item.len() > 512 || item.contains('\0'))
    {
        return Err(StoreError::InvalidCapture(
            "voice association is invalid".into(),
        ));
    }
    Ok(())
}

fn valid_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn read_job(path: &Path) -> Result<VoiceJob, StoreError> {
    let job: VoiceJob = read_bounded(path, MAX_JOB_BYTES)?;
    validate_association(&job.association)?;
    let valid_state = match job.status {
        VoiceStatus::Pending => {
            job.attempts == 0 && job.next_attempt_at_ms.is_none() && job.last_error.is_none()
        }
        VoiceStatus::Running => {
            (1..=MAX_ATTEMPTS).contains(&job.attempts) && job.next_attempt_at_ms.is_some()
        }
        VoiceStatus::Retrying => {
            (1..MAX_ATTEMPTS).contains(&job.attempts)
                && job.next_attempt_at_ms.is_some()
                && job.last_error.is_some()
        }
        VoiceStatus::Completed => {
            (1..=MAX_ATTEMPTS).contains(&job.attempts)
                && job.next_attempt_at_ms.is_none()
                && job.last_error.is_none()
        }
        VoiceStatus::Failed => {
            (1..=MAX_ATTEMPTS).contains(&job.attempts) && job.next_attempt_at_ms.is_none()
        }
        VoiceStatus::Cancelled => job.next_attempt_at_ms.is_none(),
    };
    if job.schema_version != 1
        || job.attempts > MAX_ATTEMPTS
        || !valid_state
        || job
            .last_error
            .as_ref()
            .is_some_and(|message| message.is_empty() || message.len() > 1024)
    {
        return Err(StoreError::InvalidCapture("voice job is invalid".into()));
    }
    Ok(job)
}

fn read_bounded<T: DeserializeOwned>(path: &Path, max: u64) -> Result<T, StoreError> {
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(StoreError::InvalidCapture(
            "voice record is a symlink".into(),
        ));
    }
    let mut bytes = Vec::new();
    File::open(path)?.take(max + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max {
        return Err(StoreError::InvalidCapture(
            "voice record is too large".into(),
        ));
    }
    Ok(serde_json::from_slice(&bytes)?)
}

fn lock_file(path: &Path) -> Result<File, StoreError> {
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(StoreError::InvalidCapture("voice lock is a symlink".into()));
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)?;
    set_private_permissions(path, false)?;
    Ok(file)
}

fn write_atomic(path: &Path, value: &impl Serialize) -> Result<(), StoreError> {
    let parent = path
        .parent()
        .ok_or_else(|| StoreError::InvalidCapture("voice path is invalid".into()))?;
    let temporary = parent.join(format!(".voice-{}", Uuid::new_v4()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        set_private_permissions(&temporary, false)?;
        serde_json::to_writer(&mut file, value)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if temporary.exists() {
        // A leftover private temporary is never treated as a committed job or result.
        let _ = fs::remove_file(temporary);
    }
    result
}
