//! Fixed measured helper launch, pinned channel and independently cancellable tree.

use super::{SupervisorCompletion, SupervisorError, command::CommandEnforcer};
use crate::{
    Digest,
    launch_protocol::{CommandMessage, CommandOperation, CommandPrincipal},
    launch_transport::{CredentialPin, KernelProcess, SeqpacketChannel, SeqpacketListener},
    sandbox::{BubblewrapBackend, Channel, ConfinementPlan, IdentityPlan, SandboxedSession},
};
use std::{
    fs,
    io::Write,
    os::unix::fs::{MetadataExt, PermissionsExt, chown},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub(super) struct MeasuredHelper {
    path: PathBuf,
    digest: Digest,
    device: u64,
    inode: u64,
}

impl MeasuredHelper {
    pub(super) fn measure(release: &Path, release_id: &str) -> Result<Self, SupervisorError> {
        let manifest = crate::release::read_manifest(release)
            .map_err(|_| SupervisorError::ToolIsolationUnproven)?;
        let component = manifest
            .component("louiselm-tool-test-helper")
            .ok_or(SupervisorError::ToolIsolationUnproven)?;
        if manifest.release_id != release_id
            || manifest.digest().to_string() != release_id
            || component.path != "bin/louiselm-tool-test-helper"
            || !component.executable
        {
            return Err(SupervisorError::ToolIsolationUnproven);
        }
        let path = release.join(&component.path);
        let metadata = fs::metadata(&path).map_err(|_| SupervisorError::ToolIsolationUnproven)?;
        let digest =
            Digest::of(&fs::read(&path).map_err(|_| SupervisorError::ToolIsolationUnproven)?);
        if metadata.len() != component.size || digest.hex() != component.sha256 {
            return Err(SupervisorError::ToolIsolationUnproven);
        }
        Ok(Self {
            path,
            digest,
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
}

/// Supervisor-created isolated process and its own authenticated channel.
/// Fields are private so an untrusted PID or first connector cannot construct it.
pub struct HelperPrincipal {
    pub(super) process: Arc<KernelProcess>,
    pub(super) channel: SeqpacketChannel,
    pub(super) principal: CommandPrincipal,
    deadline: Arc<Mutex<Instant>>,
    stopped: Arc<AtomicBool>,
}

impl HelperPrincipal {
    pub(super) fn close(&self) {
        self.stopped.store(true, Ordering::Release);
        self.channel.close();
    }
    pub(super) fn narrow_deadline(&self, deadline: Instant) -> Result<(), SupervisorError> {
        let mut current = self
            .deadline
            .lock()
            .map_err(|_| SupervisorError::CleanupUnproven)?;
        *current = (*current).min(deadline);
        Ok(())
    }
}

pub(super) struct HelperRuntime {
    worker: Option<JoinHandle<Result<(), SupervisorError>>>,
    stopped: Arc<AtomicBool>,
    cleanup_unproven: bool,
}

impl HelperRuntime {
    pub(super) fn launch(
        backend: BubblewrapBackend,
        mut plan: ConfinementPlan,
        measured: MeasuredHelper,
        request: CommandMessage,
        enforcer: Arc<CommandEnforcer>,
        complete: SupervisorCompletion<HelperPrincipal>,
    ) -> Result<Self, SupervisorError> {
        let CommandOperation::Delegate { grant, .. } = &request.operation else {
            return Err(SupervisorError::AuthorizationRejected);
        };
        let sequence = grant.sequence;
        plan.session_id = format!("helper-{sequence}");
        // Mount only this measured helper file, never the Agent's runtime tree.
        plan.runtime_root.clone_from(&measured.path);
        plan.executable.clone_from(&measured.path);
        plan.arguments.clear();
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&stopped);
        let worker = thread::Builder::new()
            .name("louiselm-tool-helper".into())
            .spawn(move || {
                let mut completion = Some(complete);
                let result = run(
                    &backend,
                    plan,
                    &measured,
                    &request,
                    &enforcer,
                    &stop,
                    &mut completion,
                );
                let cleanup_failed = matches!(&result, Err(SupervisorError::CleanupUnproven));
                if let Some(complete) = completion {
                    complete(Err(result.err().unwrap_or(SupervisorError::SpawnFailed)));
                }
                if cleanup_failed {
                    Err(SupervisorError::CleanupUnproven)
                } else {
                    Ok(())
                }
            })
            .map_err(|_| SupervisorError::WorkerUnavailable)?;
        Ok(Self {
            worker: Some(worker),
            stopped,
            cleanup_unproven: false,
        })
    }

    pub(super) fn cancel(&mut self) -> Result<(), SupervisorError> {
        self.stopped.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            self.cleanup_unproven |= !matches!(worker.join(), Ok(Ok(())));
        }
        if self.cleanup_unproven {
            Err(SupervisorError::CleanupUnproven)
        } else {
            Ok(())
        }
    }
}

impl Drop for HelperRuntime {
    fn drop(&mut self) {
        let _ = self.cancel();
    } // Explicit disposal propagates the poison; Drop never releases identity.
}

fn run(
    backend: &BubblewrapBackend,
    mut plan: ConfinementPlan,
    measured: &MeasuredHelper,
    request: &CommandMessage,
    enforcer: &CommandEnforcer,
    stopped: &Arc<AtomicBool>,
    complete: &mut Option<SupervisorCompletion<HelperPrincipal>>,
) -> Result<(), SupervisorError> {
    let IdentityPlan::HostIdentity { uid, gid } = plan.identity else {
        return Err(SupervisorError::ToolIsolationUnproven);
    };
    if !enforcer.agent_valid()?
        || stopped.load(Ordering::Acquire)
        || Digest::of(
            &fs::read(&measured.path).map_err(|_| SupervisorError::ToolIsolationUnproven)?,
        ) != measured.digest
    {
        return Err(SupervisorError::ToolIsolationUnproven);
    }
    let directory = tempfile::Builder::new()
        .prefix("helper-channel-")
        .tempdir_in(
            plan.home
                .parent()
                .ok_or(SupervisorError::ToolIsolationUnproven)?,
        )
        .map_err(|_| SupervisorError::CapabilityUnavailable)?;
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o711))
        .map_err(|_| SupervisorError::CapabilityUnavailable)?;
    let socket = directory.path().join("capability.sock");
    let bound = SeqpacketListener::bind_disabled(&socket)
        .map_err(|_| SupervisorError::CapabilityUnavailable)?;
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))
        .and_then(|()| chown(&socket, Some(uid), Some(gid)))
        .map_err(|_| SupervisorError::CapabilityUnavailable)?;
    plan.channels = vec![Channel::UnixSocket {
        id: plan.session_id.clone(),
        host_path: socket,
        guest_path: PathBuf::from("/tmp/louiselm-tool.sock"),
    }];
    let prepared = backend.prepare(&plan).map_err(super::system::map_sandbox)?;
    let mut session = prepared.start().map_err(super::system::map_sandbox)?;
    let result = serve(
        &mut session,
        bound,
        measured,
        request,
        enforcer,
        stopped,
        complete,
    );
    session
        .dispose()
        .map_err(|_| SupervisorError::CleanupUnproven)?;
    // A failed handshake with a proven empty tree is not failed cleanup.
    if let Some(complete) = complete.take() {
        complete(Err(result.err().unwrap_or(SupervisorError::SpawnFailed)));
    }
    Ok(())
}

fn serve(
    session: &mut SandboxedSession,
    bound: crate::launch_transport::BoundSeqpacketListener,
    measured: &MeasuredHelper,
    request: &CommandMessage,
    enforcer: &CommandEnforcer,
    stopped: &Arc<AtomicBool>,
    complete: &mut Option<SupervisorCompletion<HelperPrincipal>>,
) -> Result<(), SupervisorError> {
    let pin = session
        .agent_identity()
        .ok_or(SupervisorError::AgentIdentityRejected)?;
    let executable = fs::metadata(format!("/proc/{}/exe", pin.credentials().pid))
        .map_err(|_| SupervisorError::AgentIdentityRejected)?;
    if !pin
        .valid()
        .map_err(|_| SupervisorError::AgentIdentityRejected)?
        || executable.dev() != measured.device
        || executable.ino() != measured.inode
    {
        return Err(SupervisorError::AgentIdentityRejected);
    }
    let listener = bound
        .enable()
        .map_err(|_| SupervisorError::CapabilityUnavailable)?;
    let (sender, receiver) = mpsc::channel();
    listener
        .accept(
            CredentialPin::LiveProcess(Arc::clone(&pin)),
            Box::new(move |result| {
                let _ = sender.send(result);
            }),
        )
        .map_err(|_| SupervisorError::CapabilityUnavailable)?;
    let mut stdin = session.take_stdin().ok_or(SupervisorError::RelayFailed)?;
    stdin
        .write_all(&request.canonical_bytes())
        .map_err(|_| SupervisorError::RelayFailed)?;
    drop(stdin);
    let deadline = Arc::new(Mutex::new(Instant::now() + Duration::from_secs(30)));
    let channel = loop {
        if stopped.load(Ordering::Acquire)
            || !enforcer.agent_valid()?
            || Instant::now()
                >= *deadline
                    .lock()
                    .map_err(|_| SupervisorError::CleanupUnproven)?
        {
            return Err(SupervisorError::CapabilityUnavailable);
        }
        match receiver.recv_timeout(Duration::from_millis(5)) {
            Ok(result) => break result.map_err(|_| SupervisorError::CapabilityUnavailable)?,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => return Err(SupervisorError::CapabilityUnavailable),
        }
    };
    let CommandOperation::Delegate { grant, .. } = &request.operation else {
        return Err(SupervisorError::AuthorizationRejected);
    };
    let credentials = pin.credentials();
    if let Some(complete) = complete.take() {
        complete(Ok(HelperPrincipal {
            process: Arc::clone(&pin),
            channel: channel.clone(),
            deadline: Arc::clone(&deadline),
            stopped: Arc::clone(stopped),
            principal: CommandPrincipal {
                channel_id: format!("tool-capability-{}", grant.sequence),
                pid: credentials.pid,
                uid: credentials.uid,
                gid: credentials.gid,
            },
        }));
    }
    let result = loop {
        if stopped.load(Ordering::Acquire)
            || channel.is_closed()
            || enforcer.agent_valid() != Ok(true)
            || pin.valid().ok() != Some(true)
            || deadline
                .lock()
                .map_or(true, |deadline| Instant::now() >= *deadline)
        {
            break Ok(());
        }
        thread::sleep(Duration::from_millis(5));
    };
    channel.close();
    listener.close();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_helper_cleanup_remains_poisoned_after_worker_is_joined() {
        let mut runtime = HelperRuntime {
            worker: Some(thread::spawn(|| Err(SupervisorError::CleanupUnproven))),
            stopped: Arc::new(AtomicBool::new(false)),
            cleanup_unproven: false,
        };
        assert_eq!(runtime.cancel(), Err(SupervisorError::CleanupUnproven));
        assert!(runtime.worker.is_none());
        assert_eq!(runtime.cancel(), Err(SupervisorError::CleanupUnproven));
        assert!(runtime.stopped.load(Ordering::Acquire));
    }
}
