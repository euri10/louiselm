//! Confined plan execution with one durable observation per completed command.

use super::{
    Arc, AtomicBool, ConfinementPlan, Duration, File, Instant, Mutex, Ordering, Path, PathBuf,
    SandboxMechanicalState, SandboxedSession, Storage, SupervisorError, VerificationOperation,
    VerificationRequest, digest, filesystem, fs, persist, protected, read_export, verification,
};
use crate::{
    launch_protocol::{VerificationExecution, VerificationStep},
    sandbox::{IdentityPlan, PreparedSession},
};
use std::os::unix::fs::{PermissionsExt, chown};

impl Storage {
    fn load_job(
        &self,
        request: &VerificationRequest,
    ) -> Result<(PathBuf, verification::LoadedJob), SupervisorError> {
        let VerificationOperation::Run {
            producer_session_id,
            export_request_id,
            export_digest,
            job_digest,
        } = &request.operation
        else {
            return Err(SupervisorError::AuthorizationRejected);
        };
        let root = self
            .directory
            .parent()
            .ok_or(SupervisorError::ResolutionFailed)?;
        protected(root, 0, false)?;
        let producer = root.join(producer_session_id);
        protected(&producer, 0, false)?;
        protected(&producer.join("verification-exports"), 0, true)?;
        let exported = producer
            .join("verification-exports")
            .join(export_request_id);
        let source = read_export(&exported)?;
        if source.request.launch.session_id != *producer_session_id
            || source.request.request_id != *export_request_id
            || source
                .digest()
                .map_err(|_| SupervisorError::ResolutionFailed)?
                .to_string()
                != *export_digest
            || source.job.job_digest != *job_digest
        {
            return Err(SupervisorError::AuthorizationRejected);
        }
        let loaded = verification::load(&exported.join("job"), &digest(job_digest)?)
            .map_err(|_| SupervisorError::ResolutionFailed)?;
        if loaded.preview != source.job {
            return Err(SupervisorError::ResolutionFailed);
        }
        Ok((exported, loaded))
    }

    fn materialize(
        &self,
        request: &VerificationRequest,
        loaded: &verification::LoadedJob,
    ) -> Result<ConfinementPlan, SupervisorError> {
        let directory = self.directory.join("verification-run");
        let IdentityPlan::HostIdentity { uid, gid } = self.plan.identity else {
            return Err(SupervisorError::IdentityAssignmentInvalid);
        };
        // Root keeps the publication barrier until the complete independent copy is owned correctly.
        filesystem::publish(&directory, |staging| {
            filesystem::write_files(&staging.join("workspace"), &loaded.files, false)?;
            fs::create_dir(staging.join("home"))?;
            filesystem::write_file(
                &staging.join("request.json"),
                &request.canonical_bytes(),
                0o400,
            )?;
            own_copy(&staging.join("workspace"), uid, gid)?;
            own_copy(&staging.join("home"), uid, gid)?;
            Ok(())
        })
        .map_err(|_| SupervisorError::DurabilityUnavailable)?;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o711))
            .map_err(|_| SupervisorError::DurabilityUnavailable)?;
        File::open(&directory)
            .and_then(|p| p.sync_all())
            .map_err(|_| SupervisorError::DurabilityUnavailable)?;
        let mut plan = self.plan.clone();
        plan.workspace = directory.join("workspace");
        plan.home = directory.join("home");
        plan.environment
            .insert("HOME".into(), plan.home.to_string_lossy().into_owned());
        Ok(plan)
    }

    pub(super) fn execute(
        &self,
        request: &VerificationRequest,
        session: &Arc<Mutex<SandboxedSession>>,
        cancelled: &AtomicBool,
        start_gate: &Mutex<()>,
        cleanup_failed: &AtomicBool,
        deadline: Instant,
    ) -> Result<VerificationExecution, SupervisorError> {
        let (exported, loaded) = self.load_job(request)?;
        let process = {
            let mut session = session
                .lock()
                .map_err(|_| SupervisorError::CleanupUnproven)?;
            if session
                .mechanical_state()
                .map_err(super::super::system::map_sandbox)?
                != SandboxMechanicalState::Running
            {
                return Err(SupervisorError::AgentIdentityRejected);
            }
            session
                .agent_identity()
                .ok_or(SupervisorError::AgentIdentityRejected)?
        };
        let mut plan = self.materialize(request, &loaded)?;
        let directory = self.directory.join("verification-run");
        let mut evidence = VerificationExecution {
            request: request.clone(),
            job: loaded.preview,
            integration_digest: self.integration_digest.clone(),
            steps: Vec::new(),
            cleanup_proven: true,
            interrupted: false,
        };
        let alive = || {
            Ok(Instant::now() < deadline
                && process
                    .valid()
                    .map_err(|_| SupervisorError::AgentIdentityRejected)?)
        };
        for (index, command) in loaded.commands.iter().enumerate() {
            if cancelled.load(Ordering::Acquire) || alive() != Ok(true) {
                evidence.interrupted = true;
                break;
            }
            plan.session_id = format!("verification-{index}");
            plan.arguments = vec!["-c".into(), shell(command)];
            let timeout = Duration::from_millis(command.timeout_ms)
                .min(deadline.saturating_duration_since(Instant::now()));
            let result = super::super::tool_execution::run(
                &self.backend,
                &plan,
                timeout,
                cancelled,
                alive,
                |prepared| start(prepared, cancelled, start_gate, &alive),
            );
            let step = match result {
                Ok(output) => VerificationStep::Completed {
                    exit_code: output.exit_code,
                    timed_out: output.timed_out,
                },
                Err(error) => {
                    evidence.cleanup_proven = error != SupervisorError::CleanupUnproven;
                    if !evidence.cleanup_proven {
                        cleanup_failed.store(true, Ordering::Release);
                    }
                    VerificationStep::Unknown
                }
            };
            let successful = matches!(
                step,
                VerificationStep::Completed {
                    exit_code: 0,
                    timed_out: false
                }
            );
            // A later crash cannot erase an earlier observed result or refund its authority.
            persist(&directory.join(format!("step-{index}.json")), &step)?;
            evidence.steps.push(step);
            if !successful {
                break;
            }
        }
        evidence.interrupted |= cancelled.load(Ordering::Acquire) || alive() != Ok(true);
        // Commands never receive the retained job mount. Recheck it before publishing success.
        let unchanged =
            verification::inspect(&exported.join("job"), &digest(&evidence.job.job_digest)?)
                .map_err(|_| SupervisorError::ResolutionFailed)?;
        if unchanged != evidence.job {
            return Err(SupervisorError::ResolutionFailed);
        }
        persist(&directory.join("result.json"), &evidence)?;
        Ok(evidence)
    }
}

fn start(
    prepared: PreparedSession,
    cancelled: &AtomicBool,
    gate: &Mutex<()>,
    alive: &impl Fn() -> Result<bool, SupervisorError>,
) -> Result<SandboxedSession, SupervisorError> {
    let mut prepared = Some(prepared);
    let result = (|| {
        let _guard = gate.lock().map_err(|_| SupervisorError::CleanupUnproven)?;
        if cancelled.load(Ordering::Acquire) || !alive()? {
            return Err(SupervisorError::AuthorizationRejected);
        }
        prepared
            .take()
            .ok_or(SupervisorError::SpawnFailed)?
            .start()
            .map_err(super::super::system::map_sandbox)
    })();
    if let Some(mut prepared) = prepared {
        prepared
            .dispose()
            .map_err(|_| SupervisorError::CleanupUnproven)?;
    }
    result
}

fn own_copy(path: &Path, uid: u32, gid: u32) -> Result<(), std::io::Error> {
    // Only newly created regular files/directories below an unpublished root-owned barrier.
    let meta = fs::symlink_metadata(path)?;
    if meta.is_dir() {
        for entry in fs::read_dir(path)? {
            own_copy(&entry?.path(), uid, gid)?;
        }
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    chown(path, Some(uid), Some(gid))
}

fn shell(command: &verification::Command) -> String {
    fn quote(value: &str) -> String {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
    format!(
        "cd -- {} && exec {}",
        quote(&command.cwd),
        command
            .argv
            .iter()
            .map(|arg| quote(arg))
            .collect::<Vec<_>>()
            .join(" ")
    )
}
