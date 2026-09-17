//! Protected actual producer exports and cancellable verifier work owned by one supervisor.

mod execution;
mod transfer;

#[cfg(test)]
mod tests;

use super::{SupervisorCompletion, SupervisorError};
use crate::{
    Digest,
    launch::LaunchRequest,
    launch_protocol::{
        ResponseResult, VerificationExport, VerificationOperation, VerificationRequest,
    },
    sandbox::{BubblewrapBackend, ConfinementPlan, SandboxMechanicalState, SandboxedSession},
    workspace::{filesystem, verification},
};
use std::{
    fs::{self, File},
    os::unix::fs::{DirBuilderExt, MetadataExt},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub(super) struct Storage {
    directory: PathBuf,
    launch: LaunchRequest,
    integration_digest: String,
    input_root: PathBuf,
    broker_uid: u32,
    broker_gid: u32,
    backend: BubblewrapBackend,
    plan: ConfinementPlan,
}

impl Storage {
    pub(super) fn new(
        directory: &Path,
        launch: &LaunchRequest,
        integration_digest: String,
        input_root: PathBuf,
        broker_identity: (u32, u32),
        backend: BubblewrapBackend,
        plan: ConfinementPlan,
    ) -> Result<Self, SupervisorError> {
        protected(directory, 0, false)?;
        Ok(Self {
            directory: directory.into(),
            launch: launch.clone(),
            integration_digest,
            input_root,
            broker_uid: broker_identity.0,
            broker_gid: broker_identity.1,
            backend,
            plan,
        })
    }

    fn export(&self, request: &VerificationRequest) -> Result<VerificationExport, SupervisorError> {
        let VerificationOperation::Export {
            input_id,
            input_digest,
        } = &request.operation
        else {
            return Err(SupervisorError::AuthorizationRejected);
        };
        let retained = self.directory.join("inputs");
        protected(&retained, 0, true)?;
        let binding = crate::workspace::launch_inputs::retained_binding(
            &retained,
            &self.launch.session_input_manifest_id,
        )
        .map_err(|_| SupervisorError::ResolutionFailed)?;
        protected(&self.input_root, self.broker_uid, true)?;
        let input = self.input_root.join(input_id);
        protected(&input, self.broker_uid, true)?;
        let parent = self.directory.join("verification-exports");
        private_directory(&parent)?;
        let output = parent.join(&request.request_id);
        if output.exists() {
            let evidence = read_export(&output)?;
            if evidence.request != *request {
                return Err(SupervisorError::AuthorizationRejected);
            }
            if evidence.job.snapshot_digest != binding.snapshot_digest
                || evidence.job.base_digest != binding.base_digest
            {
                return Err(SupervisorError::ResolutionFailed);
            }
            verification::inspect(&output.join("job"), &digest(&evidence.job.job_digest)?)
                .map_err(|_| SupervisorError::ResolutionFailed)?;
            return Ok(evidence);
        }
        let job = verification::export_job(
            &input,
            &digest(input_digest)?,
            &self.directory.join("workspace"),
            &output,
            &digest(&binding.snapshot_digest)?,
        )
        .map_err(|_| SupervisorError::ResolutionFailed)?;
        let evidence = VerificationExport {
            request: request.clone(),
            job,
            integration_digest: self.integration_digest.clone(),
        };
        evidence
            .validate()
            .map_err(|_| SupervisorError::AuthorizationRejected)?;
        persist(&output.join("evidence.json"), &evidence)?;
        Ok(evidence)
    }
}

pub(super) struct Worker {
    cancelled: Arc<AtomicBool>,
    completed: Arc<AtomicBool>,
    start_gate: Arc<Mutex<()>>,
    cleanup_failed: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<(), SupervisorError>>>,
}

impl Worker {
    pub(super) fn spawn(
        storage: Arc<Storage>,
        request: VerificationRequest,
        session: Arc<Mutex<SandboxedSession>>,
        complete: SupervisorCompletion<ResponseResult>,
    ) -> Result<Self, SupervisorError> {
        request
            .validate()
            .map_err(|_| SupervisorError::AuthorizationRejected)?;
        if request.launch != storage.launch {
            return Err(SupervisorError::AuthorizationRejected);
        }
        let budget = request
            .expires_at_ms
            .checked_sub(now_ms()?)
            .filter(|ms| (1..=3_660_000).contains(ms))
            .ok_or(SupervisorError::AuthorizationRejected)?;
        let deadline = Instant::now()
            .checked_add(Duration::from_millis(budget))
            .ok_or(SupervisorError::AuthorizationRejected)?;
        let cancelled = Arc::new(AtomicBool::new(false));
        let completed = Arc::new(AtomicBool::new(false));
        let completion_flag = Arc::clone(&completed);
        let start_gate = Arc::new(Mutex::new(()));
        let cleanup_failed = Arc::new(AtomicBool::new(false));
        let cleanup_flag = Arc::clone(&cleanup_failed);
        let flag = Arc::clone(&cancelled);
        let gate = Arc::clone(&start_gate);
        let thread = thread::Builder::new()
            .name("louiselm-verification".into())
            .spawn(move || {
                let result = match request.operation {
                    VerificationOperation::Transfer { .. } => (|| {
                        if flag.load(Ordering::Acquire) || Instant::now() >= deadline {
                            return Err(SupervisorError::AuthorizationRejected);
                        }
                        let directory = storage.transfer(&request)?;
                        if flag.load(Ordering::Acquire) || Instant::now() >= deadline {
                            return Err(SupervisorError::AuthorizationRejected);
                        }
                        Ok(ResponseResult::VerificationTransfer {
                            request: Box::new(request.clone()),
                            directory,
                        })
                    })(),
                    VerificationOperation::Export { .. } => {
                        (|| {
                            // Keep the actual producer frozen throughout export, including its durable observation.
                            let mut session = session
                                .lock()
                                .map_err(|_| SupervisorError::CleanupUnproven)?;
                            if session
                                .mechanical_state()
                                .map_err(super::system::map_sandbox)?
                                != SandboxMechanicalState::Parked
                                || flag.load(Ordering::Acquire)
                                || Instant::now() >= deadline
                            {
                                return Err(SupervisorError::AuthorizationRejected);
                            }
                            let evidence = storage.export(&request)?;
                            if flag.load(Ordering::Acquire) || Instant::now() >= deadline {
                                return Err(SupervisorError::AuthorizationRejected);
                            }
                            Ok(ResponseResult::VerificationExport { evidence })
                        })()
                    }
                    VerificationOperation::Run { .. } => storage
                        .execute(&request, &session, &flag, &gate, &cleanup_flag, deadline)
                        .map(|evidence| ResponseResult::VerificationExecution { evidence }),
                };
                finish(result, &cleanup_flag, &completion_flag, complete)
            })
            .map_err(|_| SupervisorError::WorkerUnavailable)?;
        Ok(Self {
            cancelled,
            completed,
            start_gate,
            cleanup_failed,
            thread: Some(thread),
        })
    }

    pub(super) fn finished(&self) -> bool {
        self.completed.load(Ordering::Acquire)
            || self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }

    pub(super) fn cancel(&mut self) -> Result<(), SupervisorError> {
        {
            let _gate = self
                .start_gate
                .lock()
                .map_err(|_| SupervisorError::CleanupUnproven)?;
            self.cancelled.store(true, Ordering::Release);
        }
        let result = self.thread.take().map_or(Ok(()), |thread| {
            thread
                .join()
                .map_err(|_| SupervisorError::CleanupUnproven)?
        });
        if result.is_err() {
            self.cleanup_failed.store(true, Ordering::Release);
        }
        if self.cleanup_failed.load(Ordering::Acquire) {
            Err(SupervisorError::CleanupUnproven)
        } else {
            result
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        // Explicit disposal propagates failures; Drop never releases a host identity.
        let _ = self.cancel();
    }
}

fn finish(
    result: Result<ResponseResult, SupervisorError>,
    cleanup_failed: &AtomicBool,
    completed: &AtomicBool,
    complete: SupervisorCompletion<ResponseResult>,
) -> Result<(), SupervisorError> {
    let cleanup = match &result {
        Err(SupervisorError::CleanupUnproven) => Err(SupervisorError::CleanupUnproven),
        Ok(ResponseResult::VerificationExecution { evidence }) if !evidence.cleanup_proven => {
            Err(SupervisorError::CleanupUnproven)
        }
        _ => Ok(()),
    };
    if cleanup.is_err() {
        cleanup_failed.store(true, Ordering::Release);
    }
    // Consumers may request another job as soon as the callback delivers.
    // Publish the outcome first; cancel() still joins and refuses failed cleanup.
    completed.store(true, Ordering::Release);
    complete(result);
    cleanup
}

fn digest(value: &str) -> Result<Digest, SupervisorError> {
    Digest::parse(value).map_err(|_| SupervisorError::AuthorizationRejected)
}

fn now_ms() -> Result<u64, SupervisorError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| d.as_millis().try_into().ok())
        .ok_or(SupervisorError::AuthorizationRejected)
}

fn protected(path: &Path, uid: u32, private: bool) -> Result<File, SupervisorError> {
    let file = filesystem::open_directory(path).map_err(|_| SupervisorError::ResolutionFailed)?;
    let meta = file
        .metadata()
        .map_err(|_| SupervisorError::ResolutionFailed)?;
    if meta.uid() != uid || meta.mode() & 0o022 != 0 || (private && meta.mode() & 0o7777 != 0o700) {
        return Err(SupervisorError::ResolutionFailed);
    }
    Ok(file)
}

fn private_directory(path: &Path) -> Result<(), SupervisorError> {
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => (),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(_) => return Err(SupervisorError::DurabilityUnavailable),
    }
    protected(path, 0, true)?
        .sync_all()
        .map_err(|_| SupervisorError::DurabilityUnavailable)?;
    File::open(path.parent().ok_or(SupervisorError::ResolutionFailed)?)
        .and_then(|p| p.sync_all())
        .map_err(|_| SupervisorError::DurabilityUnavailable)
}

fn persist(path: &Path, value: &impl serde::Serialize) -> Result<(), SupervisorError> {
    let bytes = serde_json::to_vec(value).map_err(|_| SupervisorError::DurabilityUnavailable)?;
    filesystem::write_file(path, &bytes, 0o400)
        .map_err(|_| SupervisorError::DurabilityUnavailable)?;
    File::open(path.parent().ok_or(SupervisorError::ResolutionFailed)?)
        .and_then(|p| p.sync_all())
        .map_err(|_| SupervisorError::DurabilityUnavailable)
}

fn read_export(path: &Path) -> Result<VerificationExport, SupervisorError> {
    let root = protected(path, 0, true)?;
    let bytes = filesystem::read_source(&root, "evidence.json", 64 * 1024)
        .map_err(|_| SupervisorError::ResolutionFailed)?
        .ok_or(SupervisorError::ResolutionFailed)?
        .bytes;
    let evidence: VerificationExport =
        serde_json::from_slice(&bytes).map_err(|_| SupervisorError::ResolutionFailed)?;
    evidence
        .validate()
        .map_err(|_| SupervisorError::ResolutionFailed)?;
    if serde_json::to_vec(&evidence).map_err(|_| SupervisorError::ResolutionFailed)? != bytes {
        return Err(SupervisorError::ResolutionFailed);
    }
    Ok(evidence)
}
