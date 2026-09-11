//! One cancellable tool tree, with no Agent home, runtime or capability mount.

#[cfg(test)]
#[path = "tool_execution_tests.rs"]
mod tests;

use super::{SupervisorCompletion, SupervisorError, command::CommandPermit};
use crate::{
    launch_protocol::{MAX_TOOL_OUTPUT_BYTES, ToolExecutionResult},
    registry::NetworkPolicy,
    sandbox::{
        BubblewrapBackend, ConfinementPlan, PreparedSession, SandboxedSession, default_system_roots,
    },
};
use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use std::{
    collections::BTreeMap,
    io::{self, Read},
    os::fd::AsFd,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub(super) struct ToolExecutor {
    backend: BubblewrapBackend,
    plan: ConfinementPlan,
    worker: Option<JoinHandle<Result<(), SupervisorError>>>,
    cancelled: Arc<AtomicBool>,
    closed: bool,
    finished: Arc<AtomicBool>,
    cleanup_unproven: bool,
    pub(super) measured_helper: Option<super::tool_helper::MeasuredHelper>,
    helper: Option<super::tool_helper::HelperRuntime>,
}

impl ToolExecutor {
    pub(super) fn new(
        backend: BubblewrapBackend,
        agent: &ConfinementPlan,
    ) -> Result<Self, SupervisorError> {
        let system_roots = default_system_roots();
        if system_roots.iter().any(|root| {
            agent.runtime_root.starts_with(root)
                || agent.home.starts_with(root)
                || agent.workspace.starts_with(root)
        }) || agent.home == agent.workspace
        {
            return Err(SupervisorError::ToolIsolationUnproven);
        }
        let home = agent
            .workspace
            .parent()
            .ok_or(SupervisorError::ToolIsolationUnproven)?
            .join("tool-home");
        if home == agent.home || home == agent.workspace {
            return Err(SupervisorError::ToolIsolationUnproven);
        }
        let plan = ConfinementPlan {
            session_id: String::new(),
            runtime_root: PathBuf::from("/usr"),
            executable: PathBuf::from("/bin/sh"),
            arguments: Vec::new(),
            environment: BTreeMap::from([
                ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
                ("HOME".to_owned(), home.to_string_lossy().into_owned()),
            ]),
            home,
            workspace: agent.workspace.clone(),
            system_roots,
            network: NetworkPolicy::Denied,
            identity: agent.identity,
            channels: Vec::new(),
        };
        Ok(Self {
            backend,
            plan,
            worker: None,
            cancelled: Arc::new(AtomicBool::new(false)),
            closed: false,
            finished: Arc::new(AtomicBool::new(true)),
            cleanup_unproven: false,
            measured_helper: None,
            helper: None,
        })
    }

    pub(super) fn launch_helper(
        &mut self,
        request: crate::launch_protocol::CommandMessage,
        enforcer: Arc<super::command::CommandEnforcer>,
        complete: SupervisorCompletion<super::HelperPrincipal>,
    ) -> Result<(), SupervisorError> {
        if self.closed || self.helper.is_some() {
            return Err(SupervisorError::ToolIsolationUnproven);
        }
        request
            .validate()
            .map_err(|_| SupervisorError::AuthorizationRejected)?;
        let measured = self
            .measured_helper
            .take()
            .ok_or(SupervisorError::ToolIsolationUnproven)?;
        self.helper = Some(super::tool_helper::HelperRuntime::launch(
            self.backend.clone(),
            self.plan.clone(),
            measured,
            request,
            enforcer,
            complete,
        )?);
        Ok(())
    }

    pub(super) fn cancel_helper(&mut self) -> Result<(), SupervisorError> {
        let result = self
            .helper
            .as_mut()
            .map_or(Ok(()), super::tool_helper::HelperRuntime::cancel);
        self.cleanup_unproven |= result.is_err();
        result
    }

    pub(super) fn execute(
        &mut self,
        permit: CommandPermit,
        complete: SupervisorCompletion<ToolExecutionResult>,
    ) -> Result<(), SupervisorError> {
        let request = permit.request();
        request
            .validate()
            .map_err(|_| SupervisorError::ToolIsolationUnproven)?;
        if self.closed || !permit.valid()? {
            return Err(SupervisorError::ToolIsolationUnproven);
        }
        if !self.finished.load(Ordering::Acquire) {
            return Err(SupervisorError::WorkerUnavailable);
        }
        self.join()?;
        self.cancelled.store(false, Ordering::Release);
        self.finished.store(false, Ordering::Release);
        let finished = Arc::clone(&self.finished);
        let cancelled = Arc::clone(&self.cancelled);
        let backend = self.backend.clone();
        let mut plan = self.plan.clone();
        plan.session_id = format!("tool-{}", permit.dispatch_sequence());
        plan.arguments = vec!["-c".to_owned(), request.command.clone()];
        let timeout = Duration::from_millis(u64::from(request.timeout_ms));
        self.worker = Some(
            thread::Builder::new()
                .name("louiselm-tool".to_owned())
                .spawn(move || {
                    let result = run(
                        &backend,
                        &plan,
                        timeout,
                        &cancelled,
                        || permit.valid(),
                        |prepared| start_command(prepared, &permit),
                    );
                    let cleanup = if matches!(&result, Err(SupervisorError::CleanupUnproven)) {
                        Err(SupervisorError::CleanupUnproven)
                    } else {
                        Ok(())
                    };
                    finished.store(true, Ordering::Release);
                    complete(result);
                    cleanup
                })
                .map_err(|_| {
                    self.finished.store(true, Ordering::Release);
                    SupervisorError::WorkerUnavailable
                })?,
        );
        Ok(())
    }

    pub(super) fn cancel(&mut self) -> Result<(), SupervisorError> {
        self.cancelled.store(true, Ordering::Release);
        self.join()
    }

    pub(super) fn dispose(&mut self) -> Result<(), SupervisorError> {
        self.closed = true;
        let helper = self.cancel_helper();
        let command = self.cancel();
        helper.and(command)
    }

    fn join(&mut self) -> Result<(), SupervisorError> {
        if let Some(worker) = self.worker.take() {
            self.cleanup_unproven |= !matches!(worker.join(), Ok(Ok(())));
        }
        if self.cleanup_unproven {
            self.closed = true;
            Err(SupervisorError::CleanupUnproven)
        } else {
            Ok(())
        }
    }
}

impl Drop for ToolExecutor {
    fn drop(&mut self) {
        // Explicit disposal propagates cleanup failure and poisons the lease.
        // Drop can retry cancellation but never releases any identity lease.
        let _ = self.dispose();
    }
}

fn run(
    backend: &BubblewrapBackend,
    plan: &ConfinementPlan,
    timeout: Duration,
    cancelled: &AtomicBool,
    alive: impl Fn() -> Result<bool, SupervisorError>,
    start: impl FnOnce(PreparedSession) -> Result<SandboxedSession, SupervisorError>,
) -> Result<ToolExecutionResult, SupervisorError> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or(SupervisorError::WorkerUnavailable)?;
    let mut prepared = backend.prepare(plan).map_err(super::system::map_sandbox)?;
    let ready = alive();
    if cancelled.load(Ordering::Acquire) || Instant::now() >= deadline || ready != Ok(true) {
        prepared
            .dispose()
            .map_err(|_| SupervisorError::CleanupUnproven)?;
        return Err(SupervisorError::AgentIdentityRejected);
    }
    let mut session = start(prepared)?;
    drop(session.take_stdin());
    let result = collect(&mut session, deadline, cancelled, &alive);
    session
        .dispose()
        .map_err(|_| SupervisorError::CleanupUnproven)?;
    result
}

fn start_command(
    prepared: PreparedSession,
    permit: &CommandPermit,
) -> Result<SandboxedSession, SupervisorError> {
    let mut prepared = Some(prepared);
    let result = permit.start(|| {
        prepared
            .take()
            .ok_or(SupervisorError::SpawnFailed)?
            .start()
            .map_err(super::system::map_sandbox)
    });
    // A denied final check still owns a prepared tree. Prove its cleanup;
    // merely dropping the unused closure cannot acknowledge cancellation.
    if let Some(mut prepared) = prepared {
        prepared
            .dispose()
            .map_err(|_| SupervisorError::CleanupUnproven)?;
    }
    result
}

fn nonblocking(stream: &impl AsFd) -> Result<(), SupervisorError> {
    let flags = fcntl_getfl(stream).map_err(|_| SupervisorError::RelayFailed)?;
    fcntl_setfl(stream, flags | OFlags::NONBLOCK).map_err(|_| SupervisorError::RelayFailed)
}

fn drain(
    stream: &mut impl Read,
    bytes: &mut Vec<u8>,
    truncated: &mut bool,
) -> Result<(), SupervisorError> {
    let mut buffer = [0; 4_096];
    // A writer that never blocks cannot starve cancellation or the deadline.
    for _ in 0..4 {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                let keep = count.min(MAX_TOOL_OUTPUT_BYTES.saturating_sub(bytes.len()));
                bytes.extend_from_slice(&buffer[..keep]);
                *truncated |= keep != count;
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return Err(SupervisorError::RelayFailed),
        }
    }
    Ok(())
}

fn text(bytes: &[u8], truncated: &mut bool) -> String {
    let mut value = String::from_utf8_lossy(bytes).into_owned();
    if value.len() > MAX_TOOL_OUTPUT_BYTES {
        let mut end = MAX_TOOL_OUTPUT_BYTES;
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        value.truncate(end);
        *truncated = true;
    }
    value
}

fn collect(
    session: &mut SandboxedSession,
    deadline: Instant,
    cancelled: &AtomicBool,
    alive: &impl Fn() -> Result<bool, SupervisorError>,
) -> Result<ToolExecutionResult, SupervisorError> {
    let mut stdout = session.take_stdout().ok_or(SupervisorError::RelayFailed)?;
    let mut stderr = session.take_stderr().ok_or(SupervisorError::RelayFailed)?;
    nonblocking(&stdout)?;
    nonblocking(&stderr)?;
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let mut truncated = false;
    let (exit_code, timed_out) = loop {
        if cancelled.load(Ordering::Acquire) || !alive()? {
            return Err(SupervisorError::AgentIdentityRejected);
        }
        drain(&mut stdout, &mut out, &mut truncated)?;
        drain(&mut stderr, &mut err, &mut truncated)?;
        if let Some(code) = session.try_wait().map_err(super::system::map_sandbox)? {
            drain(&mut stdout, &mut out, &mut truncated)?;
            drain(&mut stderr, &mut err, &mut truncated)?;
            break (code, false);
        }
        if Instant::now() >= deadline {
            break (-9, true);
        }
        thread::sleep(Duration::from_millis(5));
    };
    Ok(ToolExecutionResult {
        exit_code,
        stdout: text(&out, &mut truncated),
        stderr: text(&err, &mut truncated),
        truncated,
        timed_out,
    })
}
