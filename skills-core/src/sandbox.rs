//! Confinement backends and the lifecycle of a confined Session.
//!
//! The launcher's job is narrow: validate, materialize confinement, execute,
//! supervise, report. This module owns the middle three. It knows how to build
//! a confinement plan into a running process tree and how to freeze, interrupt,
//! and dispose of that tree — and nothing about policy, egress, credentials, or
//! what the Agent is for.
//!
//! Two design points worth stating, because both are places a sandbox usually
//! leaks:
//!
//! * **The tree is enclosed before it exists.** The child joins its cgroup from
//!   `pre_exec`, between fork and exec, so there is no window in which it can
//!   fork a descendant that lands outside. Moving a process into a cgroup after
//!   spawning it leaves exactly that window, and anything already forked stays
//!   behind.
//! * **A backend is not trusted for its name.** What [`Backend::spawn`] returns
//!   is the mechanisms it used. Whether those mechanisms actually hold is
//!   decided by the conformance suite running hostile probes inside the thing,
//!   not by this module asserting it passed a flag.

mod host_identity;

use std::{
    collections::BTreeMap,
    fs, io,
    io::Write,
    os::unix::{
        fs::{DirBuilderExt, MetadataExt, PermissionsExt, chown},
        net::UnixStream,
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use self::host_identity::{Gate as HostIdentityGate, Observation as HostIdentityObservation};

use crate::{
    isolation::{
        CONTRACT_VERSION, Dimension, DimensionEvidence, IsolationEvidence, KernelPrerequisites,
    },
    registry::NetworkPolicy,
};

/// How long disposal waits for a process tree to actually be gone.
pub const DISPOSAL_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a lifecycle call waits for the kernel to confirm a cgroup state.
const LIFECYCLE_CONFIRM_TIMEOUT: Duration = Duration::from_secs(5);

/// How long `interrupt` waits, once frozen, for the tree's membership to stop
/// growing before it enumerates who to signal.
const SIGNAL_SETTLE_TIMEOUT: Duration = Duration::from_millis(300);

/// The identity a Session runs under.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityPlan {
    /// A distinct host uid and gid. Requires the launcher to be root.
    HostIdentity {
        /// Host uid the Session runs as.
        uid: u32,
        /// Host gid the Session runs as.
        gid: u32,
    },
    /// Namespaces only: the host identity is still the operator's.
    ///
    /// Enough to develop and to run conformance; never enough to be verified.
    NamespaceOnly,
}

/// A channel the launcher creates for the Session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Channel {
    /// The ACP conversation, over the Session's own stdin and stdout.
    AcpStdio {
        /// Channel identifier recorded in the receipt.
        id: String,
    },
    /// A Unix socket the launcher created and bound into the Session.
    UnixSocket {
        /// Channel identifier recorded in the receipt.
        id: String,
        /// Where the socket lives outside the Session.
        host_path: PathBuf,
        /// Where the Session sees it.
        guest_path: PathBuf,
    },
}

impl Channel {
    /// Returns the channel identifier.
    pub fn id(&self) -> &str {
        match self {
            Self::AcpStdio { id } => id,
            Self::UnixSocket { id, .. } => id,
        }
    }
}

/// Everything needed to materialize one confined Session.
#[derive(Clone, Debug)]
pub struct ConfinementPlan {
    /// Session this confinement is for.
    pub session_id: String,
    /// Runtime directory, mounted read-only.
    pub runtime_root: PathBuf,
    /// Absolute path of the executable inside the runtime.
    pub executable: PathBuf,
    /// Arguments, fixed by the Agent's registration.
    pub arguments: Vec<String>,
    /// The complete environment; nothing is inherited.
    pub environment: BTreeMap<String, String>,
    /// Private, writable home.
    pub home: PathBuf,
    /// Private, writable workspace, and the working directory.
    pub workspace: PathBuf,
    /// System directories mounted read-only.
    pub system_roots: Vec<PathBuf>,
    /// Whether the Session may reach the network.
    pub network: NetworkPolicy,
    /// The identity it runs under.
    pub identity: IdentityPlan,
    /// Channels the launcher created.
    pub channels: Vec<Channel>,
}

/// A confinement that could not be created or controlled.
#[derive(Debug, Error)]
pub enum SandboxError {
    /// A filesystem operation failed.
    #[error("sandbox I/O failed at '{path}': {source}")]
    Io {
        /// Path being operated on.
        path: String,
        /// Underlying failure.
        source: io::Error,
    },
    /// The backend program is missing.
    #[error("{program} is required by the {backend} backend: {reason}")]
    BackendMissing {
        /// The backend.
        backend: &'static str,
        /// The program it needs.
        program: String,
        /// Why it could not be used.
        reason: String,
    },
    /// The backend refused to start the Session.
    #[error("{backend} failed to start the Session: {reason}")]
    SpawnFailed {
        /// The backend.
        backend: &'static str,
        /// What it reported.
        reason: String,
    },
    /// A post-fork failure could not prove the blocked process tree gone.
    #[error("cleanup of session {session_id} was not proven: {reason}")]
    CleanupUnproven {
        /// Session whose identity must remain reserved.
        session_id: String,
        /// What prevented a zero-survivor proof.
        reason: String,
    },
    /// No usable cgroup, so the process tree could not be enclosed.
    #[error("no writable cgroup v2 hierarchy: {0}")]
    NoCgroup(String),
    /// Disposal did not finish within [`DISPOSAL_TIMEOUT`].
    #[error("{survivors} process(es) survived disposal of session {session_id}")]
    Survivors {
        /// The Session being disposed of.
        session_id: String,
        /// How many processes were still alive.
        survivors: usize,
    },
    /// The plan asks for something this build refuses.
    #[error("confinement plan is not admissible: {0}")]
    Refused(String),
}

/// A cgroup v2 group holding exactly one Session's processes.
#[derive(Clone, Debug)]
pub struct Cgroup {
    path: PathBuf,
}

/// Read-only handle to one Session's live cgroup membership.
///
/// Clones observe the cgroup again on every call; they do not retain a PID
/// snapshot that could become stale before a capability peer is checked.
#[derive(Clone, Debug)]
pub struct ProcessTree {
    cgroup: Cgroup,
}

impl ProcessTree {
    /// Returns every process currently enclosed in this Session's cgroup.
    pub fn processes(&self) -> Result<Vec<u32>, SandboxError> {
        self.cgroup.processes()
    }

    /// Reports whether `pid` is currently enclosed in this Session's cgroup.
    pub fn contains(&self, pid: u32) -> Result<bool, SandboxError> {
        Ok(self.processes()?.contains(&pid))
    }
}

impl Cgroup {
    /// Returns the cgroup this process belongs to, when it is delegated.
    ///
    /// Under a systemd user session the user's own scope is delegated, which
    /// is what lets an unprivileged conformance run exercise freeze and kill
    /// for real instead of skipping them.
    pub fn delegated_parent() -> Option<PathBuf> {
        let own = fs::read_to_string("/proc/self/cgroup").ok()?;
        let relative = own
            .lines()
            .find_map(|line| line.strip_prefix("0::"))?
            .trim()
            .trim_start_matches('/');
        let path = Path::new("/sys/fs/cgroup").join(relative);
        usable_delegated_parent(&path).then_some(path)
    }

    /// Creates a child cgroup named for one Session.
    pub fn create(parent: &Path, session_id: &str) -> Result<Self, SandboxError> {
        let path = parent.join(format!("louiselm-session-{session_id}"));
        if path.exists() {
            let _ = fs::remove_dir(&path);
        }
        fs::create_dir(&path).map_err(|source| {
            SandboxError::NoCgroup(format!("cannot create {}: {source}", path.display()))
        })?;
        Ok(Self { path })
    }

    /// Returns the cgroup directory.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns every process currently in the cgroup.
    ///
    /// Fails when `cgroup.procs` cannot be read or contains a malformed PID.
    pub fn processes(&self) -> Result<Vec<u32>, SandboxError> {
        let path = self.path.join("cgroup.procs");
        self.try_processes().map_err(|source| SandboxError::Io {
            path: path.display().to_string(),
            source,
        })
    }

    fn try_processes(&self) -> io::Result<Vec<u32>> {
        let path = self.path.join("cgroup.procs");
        fs::read_to_string(&path)?
            .lines()
            .map(|line| {
                line.trim().parse().map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("{} contains a malformed process id", path.display()),
                    )
                })
            })
            .collect()
    }

    /// Reports whether the cgroup supports freezing.
    pub fn supports_freeze(&self) -> bool {
        self.path.join("cgroup.freeze").is_file()
    }

    /// Reports whether every process in the cgroup is currently frozen.
    pub fn is_frozen(&self) -> bool {
        self.frozen_state().unwrap_or(false)
    }

    fn requested_frozen_state(&self) -> Result<bool, SandboxError> {
        let path = self.path.join("cgroup.freeze");
        let value = fs::read_to_string(&path).map_err(|source| SandboxError::Io {
            path: path.display().to_string(),
            source,
        })?;
        match value.trim() {
            "0" => Ok(false),
            "1" => Ok(true),
            _ => Err(SandboxError::SpawnFailed {
                backend: "cgroup v2",
                reason: "cgroup.freeze did not report a requested freeze state".to_owned(),
            }),
        }
    }

    fn frozen_state(&self) -> Result<bool, SandboxError> {
        let path = self.path.join("cgroup.events");
        let contents = fs::read_to_string(&path).map_err(|source| SandboxError::Io {
            path: path.display().to_string(),
            source,
        })?;
        let value = contents
            .lines()
            .find_map(|line| line.strip_prefix("frozen "))
            .map(str::trim);
        match value {
            Some("0") => Ok(false),
            Some("1") => Ok(true),
            _ => Err(SandboxError::SpawnFailed {
                backend: "cgroup v2",
                reason: "cgroup.events did not report freeze state".to_owned(),
            }),
        }
    }

    fn preflight_lifecycle(&self) -> Result<(), SandboxError> {
        let processes = self.processes()?;
        if !processes.is_empty() {
            return Err(SandboxError::NoCgroup(format!(
                "new Session cgroup {} already contains {} process(es)",
                self.path.display(),
                processes.len(),
            )));
        }
        self.freeze()?;
        self.thaw()?;
        self.kill_all()
    }

    /// Freezes every process in the cgroup, including ones forked since.
    pub fn freeze(&self) -> Result<(), SandboxError> {
        self.write("cgroup.freeze", "1")
    }

    fn request_freeze_and_wait(&self, timeout: Duration) -> Result<(), SandboxError> {
        self.request_frozen_state_and_wait(true, timeout)
    }

    fn request_thaw_and_wait(&self, timeout: Duration) -> Result<(), SandboxError> {
        self.request_frozen_state_and_wait(false, timeout)
    }

    fn request_frozen_state_and_wait(
        &self,
        frozen: bool,
        timeout: Duration,
    ) -> Result<(), SandboxError> {
        self.write("cgroup.freeze", if frozen { "1" } else { "0" })?;
        let deadline = Instant::now() + timeout;
        loop {
            if self.frozen_state()? == frozen {
                return Ok(());
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(SandboxError::SpawnFailed {
                    backend: "cgroup v2",
                    reason: format!(
                        "the kernel did not confirm the {} state before its deadline",
                        if frozen { "frozen" } else { "running" },
                    ),
                });
            }
            thread::sleep(Duration::from_millis(20).min(deadline - now));
        }
    }

    /// Thaws a frozen cgroup.
    pub fn thaw(&self) -> Result<(), SandboxError> {
        self.write("cgroup.freeze", "0")
    }

    /// Kills every process in the cgroup, including ones forked since.
    pub fn kill_all(&self) -> Result<(), SandboxError> {
        self.write("cgroup.kill", "1")
    }

    /// Removes the cgroup once it is empty.
    pub fn remove(&self) {
        let _ = fs::remove_dir(&self.path);
    }

    fn write(&self, file: &str, value: &str) -> Result<(), SandboxError> {
        let path = self.path.join(file);
        fs::write(&path, value).map_err(|source| SandboxError::Io {
            path: path.display().to_string(),
            source,
        })
    }
}

fn preflight_cgroup(
    cgroup: Option<Cgroup>,
    identity: IdentityPlan,
) -> Result<Option<Cgroup>, SandboxError> {
    let Some(cgroup) = cgroup else {
        return if matches!(identity, IdentityPlan::HostIdentity { .. }) {
            Err(SandboxError::Refused(
                "a distinct host identity requires a writable cgroup for fail-closed startup"
                    .to_owned(),
            ))
        } else {
            Ok(None)
        };
    };
    match cgroup.preflight_lifecycle() {
        Ok(()) => Ok(Some(cgroup)),
        Err(error @ SandboxError::NoCgroup(_)) => {
            cgroup.remove();
            Err(error)
        }
        Err(error) => {
            cgroup.remove();
            if matches!(identity, IdentityPlan::HostIdentity { .. }) {
                Err(error)
            } else {
                Ok(None)
            }
        }
    }
}

fn usable_delegated_parent(path: &Path) -> bool {
    use rustix::fs::{Access, AtFlags, CWD, accessat};

    let effective = AtFlags::EACCESS;
    accessat(CWD, path, Access::WRITE_OK | Access::EXEC_OK, effective).is_ok()
        && accessat(CWD, path.join("cgroup.procs"), Access::WRITE_OK, effective).is_ok()
}

/// A running, confined Session.
#[derive(Debug)]
pub struct SandboxedSession {
    /// Session identifier.
    pub session_id: String,
    /// Backend that created it.
    pub backend: &'static str,
    /// What the backend established.
    pub evidence: IsolationEvidence,
    child: Child,
    cgroup: Option<Cgroup>,
    sandbox_leader_pid: Option<u32>,
    // Keep Bubblewrap's status reader alive for its terminal status write.
    _status_guard: Option<UnixStream>,
}

/// Kernel-proved execution state of one live sandbox process tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SandboxMechanicalState {
    /// The live process tree may execute.
    Running,
    /// The live process tree is completely frozen.
    Parked,
    /// The supervised process exited with this sanitized launcher-side code.
    Exited(i32),
}

/// A confined process tree whose workload is still blocked before `exec`.
#[derive(Debug)]
pub struct PreparedSession {
    session: Option<SandboxedSession>,
    startup_gate: Option<HostIdentityGate>,
}

/// What disposal actually did.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DisposalReport {
    /// The Session disposed of.
    pub session_id: String,
    /// Processes still alive when disposal was asked for.
    pub processes_before: usize,
    /// Processes still alive afterwards. Anything but zero is a failure.
    pub survivors: usize,
    /// Whether the Session's identity was released.
    pub identity_released: bool,
}

impl SandboxedSession {
    /// Returns the host PID of Bubblewrap's outer monitor process.
    pub fn monitor_pid(&self) -> u32 {
        self.child.id()
    }

    /// Returns Bubblewrap's host-view PID-namespace leader, when observed.
    ///
    /// This is Bubblewrap's reaper, not necessarily the Agent process itself.
    pub fn sandbox_leader_pid(&self) -> Option<u32> {
        self.sandbox_leader_pid
    }

    /// Returns every process in the Session's tree.
    ///
    /// Fails when the Session's cgroup membership cannot be read.
    pub fn processes(&self) -> Result<Vec<u32>, SandboxError> {
        match &self.cgroup {
            Some(cgroup) => cgroup.processes(),
            None => Ok(Vec::new()),
        }
    }

    /// Returns a dynamic read-only handle to this Session's process tree.
    ///
    /// Namespace-only Sessions without lifecycle cgroup control return
    /// `None`; a verified host-identity Session always has one.
    pub fn process_tree(&self) -> Option<ProcessTree> {
        self.cgroup
            .as_ref()
            .cloned()
            .map(|cgroup| ProcessTree { cgroup })
    }

    /// Borrows the Session's stdin, when the ACP channel is stdio.
    pub fn stdin(&mut self) -> Option<&mut std::process::ChildStdin> {
        self.child.stdin.as_mut()
    }

    /// Takes the Session's stdin for an owned ACP relay.
    pub fn take_stdin(&mut self) -> Option<std::process::ChildStdin> {
        self.child.stdin.take()
    }

    /// Takes the Session's stdout, when the ACP channel is stdio.
    pub fn take_stdout(&mut self) -> Option<std::process::ChildStdout> {
        self.child.stdout.take()
    }

    /// Takes the Session's stderr.
    ///
    /// The backend always pipes stderr rather than inheriting the launcher's,
    /// so it must be drained by someone: an unread pipe fills its OS buffer
    /// and blocks the Session the first time it writes past that limit.
    pub fn take_stderr(&mut self) -> Option<std::process::ChildStderr> {
        self.child.stderr.take()
    }

    /// Observes a settled process-tree state without guessing through a kernel transition.
    ///
    /// Fails when membership, exit, requested freeze, and observed freeze cannot prove one state.
    pub fn mechanical_state(&mut self) -> Result<SandboxMechanicalState, SandboxError> {
        if let Some(code) = self.try_wait()? {
            return Ok(SandboxMechanicalState::Exited(code));
        }
        let cgroup = self
            .cgroup
            .as_ref()
            .ok_or_else(|| SandboxError::NoCgroup("this Session has no cgroup".to_owned()))?;
        if cgroup.processes()?.is_empty() {
            return Err(SandboxError::SpawnFailed {
                backend: self.backend,
                reason: "the Session tree is empty without an observed process exit".to_owned(),
            });
        }
        let requested = cgroup.requested_frozen_state()?;
        let observed = cgroup.frozen_state()?;
        if requested != observed {
            return Err(SandboxError::SpawnFailed {
                backend: "cgroup v2",
                reason: "the Session freeze transition is not settled".to_owned(),
            });
        }
        Ok(if observed {
            SandboxMechanicalState::Parked
        } else {
            SandboxMechanicalState::Running
        })
    }

    /// Freezes the whole tree, preserving in-flight work.
    ///
    /// Freezing rather than stopping the direct child is the point: a Park that
    /// only stopped the process the launcher knows about would leave every
    /// descendant running.
    pub fn park(&mut self) -> Result<(), SandboxError> {
        let cgroup = self
            .cgroup
            .as_ref()
            .ok_or_else(|| SandboxError::NoCgroup("this Session has no cgroup".to_owned()))?;
        cgroup.request_freeze_and_wait(LIFECYCLE_CONFIRM_TIMEOUT)?;
        if cgroup.processes()?.is_empty() {
            return Err(SandboxError::SpawnFailed {
                backend: self.backend,
                reason: "the Session tree exited while Park was being applied".to_owned(),
            });
        }
        match self.child.try_wait().map_err(|source| SandboxError::Io {
            path: "child".to_owned(),
            source,
        })? {
            None => Ok(()),
            Some(_) => Err(SandboxError::SpawnFailed {
                backend: self.backend,
                reason: "the Session exited while Park was being applied".to_owned(),
            }),
        }
    }

    /// Thaws a parked Session.
    pub fn resume(&mut self) -> Result<(), SandboxError> {
        self.resume_with_timeout(LIFECYCLE_CONFIRM_TIMEOUT)
    }

    fn resume_with_timeout(&mut self, timeout: Duration) -> Result<(), SandboxError> {
        let cgroup = self
            .cgroup
            .as_ref()
            .ok_or_else(|| SandboxError::NoCgroup("this Session has no cgroup".to_owned()))?;
        let resumed = (|| {
            if !cgroup.frozen_state()? {
                return Err(SandboxError::SpawnFailed {
                    backend: self.backend,
                    reason: "the Session tree was not frozen before Resume".to_owned(),
                });
            }
            if cgroup.processes()?.is_empty() {
                return Err(SandboxError::SpawnFailed {
                    backend: self.backend,
                    reason: "the Session tree exited before Resume was applied".to_owned(),
                });
            }
            if self
                .child
                .try_wait()
                .map_err(|source| SandboxError::Io {
                    path: "child".to_owned(),
                    source,
                })?
                .is_some()
            {
                return Err(SandboxError::SpawnFailed {
                    backend: self.backend,
                    reason: "the Session exited before Resume was applied".to_owned(),
                });
            }
            cgroup.request_thaw_and_wait(timeout)?;
            if cgroup.processes()?.is_empty() {
                return Err(SandboxError::SpawnFailed {
                    backend: self.backend,
                    reason: "the Session tree exited while Resume was being applied".to_owned(),
                });
            }
            match self.child.try_wait().map_err(|source| SandboxError::Io {
                path: "child".to_owned(),
                source,
            })? {
                None => Ok(()),
                Some(_) => Err(SandboxError::SpawnFailed {
                    backend: self.backend,
                    reason: "the Session exited while Resume was being applied".to_owned(),
                }),
            }
        })();
        if resumed.is_err() {
            let _ = cgroup.request_freeze_and_wait(timeout);
        }
        resumed
    }

    /// Reports whether a parked Session is currently frozen.
    ///
    /// `false` when the Session has no cgroup, matching what `park` itself
    /// would refuse rather than claiming a state nothing can actually hold.
    pub fn is_parked(&self) -> bool {
        self.cgroup.as_ref().is_some_and(Cgroup::is_frozen)
    }

    /// Sends `SIGINT` to every process interrupt can actually reach.
    ///
    /// Turn-level Cancellation is delivered by the adapter over ACP; this is
    /// the floor under it, for an Agent that ignores the protocol or has
    /// already forked something that will not hear it.
    ///
    /// Freezes the cgroup before enumerating who to signal. bwrap's own setup
    /// keeps forking for a short moment after the launcher-visible child
    /// starts, so a plain "read `processes()` then `kill`" can run before that
    /// chain finishes and permanently miss the descendant that ends up
    /// actually running the Agent — freezing first stops anything already
    /// alive from being missed, and cgroup v2 freezes newly forked tasks too,
    /// so a brief settle wait closes the rest of the window before the signal
    /// goes out.
    ///
    /// This does not guarantee zero survivors. The backend's own
    /// namespace-init helper is, by the kernel's own pid-namespace rules,
    /// immune to a default-action signal it never explicitly handled — only
    /// `SIGKILL`/`SIGSTOP` reach it regardless of what runs inside the
    /// Session. The Agent and anything it forked are not namespace-init and
    /// die normally. Call [`SandboxedSession::dispose`] for an actual
    /// zero-survivors guarantee.
    pub fn interrupt(&mut self) -> Result<usize, SandboxError> {
        self.interrupt_with_timeout(LIFECYCLE_CONFIRM_TIMEOUT)
    }

    fn interrupt_with_timeout(&mut self, timeout: Duration) -> Result<usize, SandboxError> {
        let cgroup = self
            .cgroup
            .as_ref()
            .filter(|cgroup| cgroup.supports_freeze())
            .cloned();
        let was_frozen = cgroup.as_ref().map(Cgroup::frozen_state).transpose()?;
        let outcome = (|| {
            let processes = if let Some(cgroup) = &cgroup {
                cgroup.request_freeze_and_wait(timeout)?;
                wait_for_stable_membership(cgroup)?
            } else {
                self.processes()?
            };
            if processes.is_empty() {
                return Err(SandboxError::SpawnFailed {
                    backend: self.backend,
                    reason: "the Session tree exited before Interrupt was applied".to_owned(),
                });
            }
            if self
                .child
                .try_wait()
                .map_err(|source| SandboxError::Io {
                    path: "child".to_owned(),
                    source,
                })?
                .is_some()
            {
                return Err(SandboxError::SpawnFailed {
                    backend: self.backend,
                    reason: "the Session exited before Interrupt was applied".to_owned(),
                });
            }
            signal(&processes, "-INT", self.backend)
        })();
        if let Some(cgroup) = &cgroup {
            let restored = if was_frozen == Some(true) {
                cgroup.request_freeze_and_wait(timeout)
            } else {
                cgroup.request_thaw_and_wait(timeout)
            };
            restored?;
        }
        outcome
    }

    /// Terminates the whole tree and releases the Session's identity.
    pub fn dispose(&mut self) -> Result<DisposalReport, SandboxError> {
        let deadline = Instant::now() + DISPOSAL_TIMEOUT;
        let processes_before = self.processes().map(|processes| processes.len());
        let cgroup_kill = if let Some(cgroup) = &self.cgroup {
            // Thaw first: a frozen cgroup cannot process the kill.
            let _ = cgroup.thaw();
            cgroup.kill_all()
        } else {
            Ok(())
        };
        let _ = self.child.kill();

        let mut original_error = None;
        let processes_before = match processes_before {
            Ok(processes) => Some(processes),
            Err(error) => {
                original_error = Some(error);
                None
            }
        };
        if let Err(error) = cgroup_kill
            && original_error.is_none()
        {
            original_error = Some(error);
        }

        let mut child_reaped = false;
        let mut child_reap_failed = false;
        let survivors = loop {
            if !child_reaped && !child_reap_failed {
                match self.child.try_wait() {
                    Ok(Some(_)) => child_reaped = true,
                    Ok(None) => {}
                    Err(source) => {
                        child_reap_failed = true;
                        if original_error.is_none() {
                            original_error = Some(SandboxError::CleanupUnproven {
                                session_id: self.session_id.clone(),
                                reason: format!(
                                    "launcher-side child could not be reaped: {source}"
                                ),
                            });
                        }
                    }
                }
            }
            let survivors = match self.processes() {
                Ok(processes) => Some(processes.len()),
                Err(error) => {
                    if original_error.is_none() {
                        original_error = Some(error);
                    }
                    None
                }
            };
            let membership_settled = survivors.is_none_or(|survivors| survivors == 0);
            if (child_reaped || child_reap_failed) && membership_settled {
                break survivors;
            }
            let now = Instant::now();
            if now >= deadline {
                break survivors;
            }
            thread::sleep(Duration::from_millis(20).min(deadline - now));
        };
        if let Some(error) = original_error {
            return Err(error);
        }
        let survivors = survivors.ok_or_else(|| SandboxError::CleanupUnproven {
            session_id: self.session_id.clone(),
            reason: "Session cgroup membership could not be proved empty".to_owned(),
        })?;
        if survivors > 0 {
            return Err(SandboxError::Survivors {
                session_id: self.session_id.clone(),
                survivors,
            });
        }
        if !child_reaped {
            return Err(SandboxError::CleanupUnproven {
                session_id: self.session_id.clone(),
                reason: "launcher-side child did not exit before the disposal deadline".to_owned(),
            });
        }
        if let Some(cgroup) = self.cgroup.take() {
            cgroup.remove();
        }
        self._status_guard = None;
        Ok(DisposalReport {
            session_id: self.session_id.clone(),
            processes_before: processes_before
                .expect("successful disposal observed the initial process count"),
            survivors: 0,
            identity_released: true,
        })
    }

    /// Waits for the Session to exit on its own.
    pub fn wait(&mut self) -> Result<i32, SandboxError> {
        self.child
            .wait()
            .map(|status| status.code().unwrap_or(-1))
            .map_err(|source| SandboxError::Io {
                path: "child".to_owned(),
                source,
            })
    }

    /// Reports the Session's exit status without waiting.
    pub fn try_wait(&mut self) -> Result<Option<i32>, SandboxError> {
        self.child
            .try_wait()
            .map(|status| status.map(|status| status.code().unwrap_or(-1)))
            .map_err(|source| SandboxError::Io {
                path: "child".to_owned(),
                source,
            })
    }
}

impl PreparedSession {
    fn session(&self) -> &SandboxedSession {
        self.session
            .as_ref()
            .expect("a prepared Session remains owned until start or disposal")
    }

    /// Returns the Session identifier.
    pub fn session_id(&self) -> &str {
        &self.session().session_id
    }

    /// Returns the backend that prepared the Session.
    pub fn backend(&self) -> &'static str {
        self.session().backend
    }

    /// Returns the confinement evidence established before the workload runs.
    pub fn evidence(&self) -> &IsolationEvidence {
        &self.session().evidence
    }

    /// Returns the host PID of Bubblewrap's outer monitor process.
    pub fn monitor_pid(&self) -> u32 {
        self.session().monitor_pid()
    }

    /// Returns Bubblewrap's verified host-view PID-namespace leader, when available.
    pub fn sandbox_leader_pid(&self) -> Option<u32> {
        self.session().sandbox_leader_pid()
    }

    /// Returns every process currently enclosed in the Session cgroup.
    pub fn processes(&self) -> Result<Vec<u32>, SandboxError> {
        self.session().processes()
    }

    /// Returns a dynamic read-only handle to the still-blocked process tree.
    pub fn process_tree(&self) -> Option<ProcessTree> {
        self.session().process_tree()
    }

    /// Releases the startup gate and returns the now-running Session.
    pub fn start(mut self) -> Result<SandboxedSession, SandboxError> {
        let release = self
            .startup_gate
            .as_mut()
            .expect("a prepared Session owns its startup gate")
            .release();
        if let Err(source) = release {
            let session_id = self.session_id().to_owned();
            return match self.dispose() {
                Ok(_) => Err(SandboxError::SpawnFailed {
                    backend: "bubblewrap",
                    reason: format!("startup gate release failed: {source}"),
                }),
                Err(cleanup) => Err(SandboxError::CleanupUnproven {
                    session_id,
                    reason: format!("startup gate release failed: {source}; {cleanup}"),
                }),
            };
        }

        let gate = self
            .startup_gate
            .take()
            .expect("the released startup gate remains owned");
        let mut session = self
            .session
            .take()
            .expect("the prepared Session remains owned");
        session._status_guard = Some(gate.into_status());
        Ok(session)
    }

    /// Kills the still-blocked process tree and proves it has no survivors.
    pub fn dispose(&mut self) -> Result<DisposalReport, SandboxError> {
        let report = self
            .session
            .as_mut()
            .ok_or_else(|| {
                SandboxError::Refused("prepared Session is already disposed".to_owned())
            })?
            .dispose()?;
        self.startup_gate = None;
        self.session = None;
        Ok(report)
    }
}

impl Drop for PreparedSession {
    fn drop(&mut self) {
        if self.session.is_some() {
            let _ = self.dispose();
        }
    }
}

/// Waits for `cgroup`'s membership to stop changing, or gives up.
///
/// A frozen cgroup still admits newly forked tasks — they are simply born
/// frozen — so this only closes the startup-fork race before one enumeration;
/// it says nothing about descendants forked later in the Session's life.
fn wait_for_stable_membership(cgroup: &Cgroup) -> Result<Vec<u32>, SandboxError> {
    let deadline = Instant::now() + SIGNAL_SETTLE_TIMEOUT;
    let mut previous = cgroup.processes()?;
    loop {
        thread::sleep(Duration::from_millis(20));
        let current = cgroup.processes()?;
        if current == previous || Instant::now() >= deadline {
            return Ok(current);
        }
        previous = current;
    }
}

/// Sends one signal to every pid in `processes` with a single `kill` call.
fn signal(processes: &[u32], signal: &str, backend: &'static str) -> Result<usize, SandboxError> {
    if processes.is_empty() {
        return Ok(0);
    }
    let mut command = Command::new("/usr/bin/kill");
    command.arg(signal);
    for pid in processes {
        command.arg(pid.to_string());
    }
    let status = command.status().map_err(|source| SandboxError::Io {
        path: "/usr/bin/kill".to_owned(),
        source,
    })?;
    if !status.success() {
        return Err(SandboxError::SpawnFailed {
            backend,
            reason: "kill refused to signal the Session tree".to_owned(),
        });
    }
    Ok(processes.len())
}

fn identity_start_failed(
    backend: &'static str,
    session_id: &str,
    child: &mut Child,
    cgroup: &Cgroup,
    source: io::Error,
) -> SandboxError {
    let deadline = Instant::now() + DISPOSAL_TIMEOUT;
    let _ = cgroup.thaw();
    let cgroup_kill_error = cgroup.kill_all().err();
    let _ = child.kill();

    let mut child_reaped = false;
    let mut membership = cgroup.try_processes();
    loop {
        if !child_reaped {
            child_reaped = matches!(child.try_wait(), Ok(Some(_)));
        }
        if child_reaped && membership.as_ref().is_ok_and(Vec::is_empty) {
            cgroup.remove();
            return SandboxError::SpawnFailed {
                backend,
                reason: format!("host identity verification failed: {source}"),
            };
        }
        if Instant::now() >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(20));
        membership = cgroup.try_processes();
    }
    let membership = match membership {
        Ok(processes) => format!("{} process(es) remain", processes.len()),
        Err(error) => format!("membership is unreadable: {error}"),
    };
    let reaping = if child_reaped {
        "launcher-side child was reaped"
    } else {
        "launcher-side child was not reaped"
    };
    let killing = cgroup_kill_error.map_or_else(
        || "cgroup kill was issued".to_owned(),
        |error| format!("cgroup kill failed: {error}"),
    );
    SandboxError::CleanupUnproven {
        session_id: session_id.to_owned(),
        reason: format!(
            "{backend} host identity verification failed: {source}; {killing}; {membership}; {reaping}",
        ),
    }
}

fn materialize_writable(path: &Path, identity: Option<(u32, u32)>) -> Result<(), SandboxError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| SandboxError::Io {
            path: parent.display().to_string(),
            source,
        })?;
    }
    let created = match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => true,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => false,
        Err(source) => {
            return Err(SandboxError::Io {
                path: path.display().to_string(),
                source,
            });
        }
    };
    let metadata = fs::symlink_metadata(path).map_err(|source| SandboxError::Io {
        path: path.display().to_string(),
        source,
    })?;
    if !metadata.is_dir() {
        return Err(SandboxError::Refused(format!(
            "writable path '{}' is not a directory",
            path.display(),
        )));
    }
    if created {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|source| {
            SandboxError::Io {
                path: path.display().to_string(),
                source,
            }
        })?;
    }
    let Some((uid, gid)) = identity else {
        return Ok(());
    };
    if created {
        return chown(path, Some(uid), Some(gid)).map_err(|source| SandboxError::Io {
            path: path.display().to_string(),
            source,
        });
    }
    if metadata.uid() != uid || metadata.gid() != gid || metadata.mode() & 0o777 != 0o700 {
        return Err(SandboxError::Refused(format!(
            "existing writable path '{}' is not private and writable by the Session identity",
            path.display(),
        )));
    }
    Ok(())
}

fn materialize_session_root(home: &Path, workspace: &Path) -> Result<(), SandboxError> {
    let home_parent = home.parent();
    if home_parent.is_none() || home_parent != workspace.parent() {
        return Err(SandboxError::Refused(
            "HostIdentity home and workspace must share one Session root".to_owned(),
        ));
    }
    let root = home_parent.expect("the Session root was checked above");
    let parent = root.parent().ok_or_else(|| {
        SandboxError::Refused("HostIdentity Session root has no parent".to_owned())
    })?;
    let parent_metadata = fs::symlink_metadata(parent).map_err(|source| SandboxError::Io {
        path: parent.display().to_string(),
        source,
    })?;
    if !parent_metadata.is_dir()
        || parent_metadata.uid() != 0
        || parent_metadata.mode() & 0o777 != 0o711
    {
        return Err(SandboxError::Refused(format!(
            "Sessions root '{}' must be a root-owned 0711 directory",
            parent.display(),
        )));
    }
    let created = match fs::DirBuilder::new().mode(0o711).create(root) {
        Ok(()) => true,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => false,
        Err(source) => {
            return Err(SandboxError::Io {
                path: root.display().to_string(),
                source,
            });
        }
    };
    let metadata = fs::symlink_metadata(root).map_err(|source| SandboxError::Io {
        path: root.display().to_string(),
        source,
    })?;
    if !metadata.is_dir() || metadata.uid() != 0 {
        return Err(SandboxError::Refused(format!(
            "Session root '{}' must be a root-owned directory",
            root.display(),
        )));
    }
    if !created && metadata.mode() & 0o777 != 0o711 {
        return Err(SandboxError::Refused(format!(
            "existing Session root '{}' must have mode 0711",
            root.display(),
        )));
    }
    if created {
        fs::set_permissions(root, fs::Permissions::from_mode(0o711)).map_err(|source| {
            SandboxError::Io {
                path: root.display().to_string(),
                source,
            }
        })?;
    }
    Ok(())
}

/// A way of confining a Session.
pub trait Backend {
    /// The backend's name, as recorded in evidence.
    fn name(&self) -> &'static str;

    /// The backend's version, as it reports it.
    fn version(&self) -> Result<String, SandboxError>;

    /// What the kernel offers this backend right now.
    fn prerequisites(&self) -> KernelPrerequisites;

    /// Starts a confined Session.
    fn spawn(&self, plan: &ConfinementPlan) -> Result<SandboxedSession, SandboxError>;
}

/// Confinement via bubblewrap.
///
/// A candidate, not a trusted name: what it establishes is decided by the
/// conformance suite, and a launch that has no passing conformance report for
/// this backend on this kernel is refused.
#[derive(Clone, Debug)]
pub struct BubblewrapBackend {
    program: PathBuf,
    cgroup_parent: Option<PathBuf>,
    cached_version: Option<String>,
}

impl Default for BubblewrapBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl BubblewrapBackend {
    /// Uses `bwrap` from the system.
    pub fn new() -> Self {
        Self {
            program: PathBuf::from("bwrap"),
            cgroup_parent: None,
            cached_version: None,
        }
    }

    /// Uses a specific `bwrap` binary.
    pub fn at(program: &Path) -> Self {
        Self {
            program: program.to_path_buf(),
            cgroup_parent: None,
            cached_version: None,
        }
    }

    /// Uses only the launcher's pinned binary, cgroup parent, and measured version.
    pub(crate) fn for_launcher(
        program: &Path,
        cgroup_parent: &Path,
        backend_version: String,
    ) -> Self {
        Self {
            program: program.to_path_buf(),
            cgroup_parent: Some(cgroup_parent.to_path_buf()),
            cached_version: Some(backend_version),
        }
    }

    fn cgroup_parent(&self) -> Result<Option<PathBuf>, SandboxError> {
        let Some(parent) = &self.cgroup_parent else {
            return Ok(Cgroup::delegated_parent());
        };
        let metadata = fs::symlink_metadata(parent).map_err(|source| {
            SandboxError::NoCgroup(format!(
                "cannot inspect configured cgroup parent '{}': {source}",
                parent.display(),
            ))
        })?;
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || metadata.uid() != 0
            || metadata.mode() & 0o7777 != 0o700
        {
            return Err(SandboxError::NoCgroup(format!(
                "configured cgroup parent '{}' must be a root-owned non-symlink directory with mode 0700",
                parent.display(),
            )));
        }
        if usable_delegated_parent(parent) {
            return Ok(Some(parent.clone()));
        }
        Err(SandboxError::NoCgroup(format!(
            "configured cgroup parent '{}' is not writable",
            parent.display(),
        )))
    }

    fn arguments(
        &self,
        plan: &ConfinementPlan,
        identity_gate: Option<&HostIdentityGate>,
    ) -> Vec<String> {
        let mut arguments = vec![
            "--unshare-all".to_owned(),
            "--die-with-parent".to_owned(),
            "--new-session".to_owned(),
            "--clearenv".to_owned(),
        ];
        if let Some(gate) = identity_gate {
            arguments.push("--json-status-fd".to_owned());
            arguments.push(gate.status_fd().to_string());
            arguments.push("--block-fd".to_owned());
            arguments.push(gate.block_fd().to_string());
        }
        if let IdentityPlan::HostIdentity { uid, gid } = plan.identity {
            // These select the namespace-visible ids. `CommandExt` separately
            // applies the same ids to Bubblewrap on the host side.
            arguments.push("--uid".to_owned());
            arguments.push(uid.to_string());
            arguments.push("--gid".to_owned());
            arguments.push(gid.to_string());
        }
        // The generic skeleton is mounted first so every plan-specific bind
        // below can win the mountpoint it names. A plan's home or workspace
        // plausibly lives under the host's /tmp; binding it before this
        // tmpfs would only get shadowed the moment /tmp is mounted fresh.
        arguments.push("--proc".to_owned());
        arguments.push("/proc".to_owned());
        arguments.push("--dev".to_owned());
        arguments.push("/dev".to_owned());
        arguments.push("--tmpfs".to_owned());
        arguments.push("/tmp".to_owned());
        for root in &plan.system_roots {
            if root.exists() {
                arguments.push("--ro-bind".to_owned());
                arguments.push(root.display().to_string());
                arguments.push(root.display().to_string());
            }
        }
        arguments.push("--ro-bind".to_owned());
        arguments.push(plan.runtime_root.display().to_string());
        arguments.push(plan.runtime_root.display().to_string());
        for writable in [&plan.home, &plan.workspace] {
            arguments.push("--bind".to_owned());
            arguments.push(writable.display().to_string());
            arguments.push(writable.display().to_string());
        }
        for channel in &plan.channels {
            if let Channel::UnixSocket {
                host_path,
                guest_path,
                ..
            } = channel
            {
                arguments.push("--bind".to_owned());
                arguments.push(host_path.display().to_string());
                arguments.push(guest_path.display().to_string());
            }
        }
        for (key, value) in &plan.environment {
            arguments.push("--setenv".to_owned());
            arguments.push(key.clone());
            arguments.push(value.clone());
        }
        arguments.push("--chdir".to_owned());
        arguments.push(plan.workspace.display().to_string());
        arguments.push("--".to_owned());
        arguments.push(plan.executable.display().to_string());
        arguments.extend(plan.arguments.iter().cloned());
        arguments
    }

    /// Materializes a confined Session while keeping its workload blocked.
    pub fn prepare(&self, plan: &ConfinementPlan) -> Result<PreparedSession, SandboxError> {
        if plan.network != NetworkPolicy::Denied {
            return Err(SandboxError::Refused(
                "this build confines only Sessions with no network; brokered egress belongs to the control service".to_owned(),
            ));
        }
        let host_identity = if let IdentityPlan::HostIdentity { uid, gid } = plan.identity {
            if !rustix::process::geteuid().is_root() {
                return Err(SandboxError::Refused(
                    "a distinct host identity requires a root launcher".to_owned(),
                ));
            }
            if uid == 0 || gid == 0 {
                return Err(SandboxError::Refused(
                    "a Session host identity must be non-root".to_owned(),
                ));
            }
            Some((uid, gid))
        } else {
            None
        };
        if host_identity.is_some() {
            materialize_session_root(&plan.home, &plan.workspace)?;
        }
        for writable in [&plan.home, &plan.workspace] {
            materialize_writable(writable, host_identity)?;
        }

        let mut startup_gate = HostIdentityGate::new().map_err(|source| SandboxError::Io {
            path: "startup gate".to_owned(),
            source,
        })?;
        let cgroup_parent = self.cgroup_parent()?;
        if host_identity.is_some() && cgroup_parent.is_none() {
            return Err(SandboxError::Refused(
                "a distinct host identity requires a writable cgroup for fail-closed startup"
                    .to_owned(),
            ));
        }
        let cgroup = cgroup_parent
            .map(|parent| Cgroup::create(&parent, &plan.session_id))
            .transpose()?;
        let cgroup = preflight_cgroup(cgroup, plan.identity)?;

        let mut command = Command::new(&self.program);
        command
            .args(self.arguments(plan, Some(&startup_gate)))
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some((uid, gid)) = host_identity {
            command.gid(gid).uid(uid);
        }
        startup_gate.inherit_fds(&mut command);

        if let Some(cgroup) = &cgroup {
            let procs = cgroup.path().join("cgroup.procs");
            let file = match fs::OpenOptions::new().write(true).open(&procs) {
                Ok(file) => file,
                Err(source) => {
                    cgroup.remove();
                    return Err(SandboxError::Io {
                        path: procs.display().to_string(),
                        source,
                    });
                }
            };
            // The file is deliberately opened before the uid/gid drop. cgroup
            // v2 authorizes migration against its open-time credentials.
            // Between fork and exec the child writes itself into the cgroup:
            // "0" means "the writing process". Doing this from the parent
            // after spawn would leave a window in which the child could fork a
            // descendant that never joins, and disposal could not prove it
            // gone.
            unsafe {
                command.pre_exec(move || {
                    (&file).write_all(b"0\n")?;
                    Ok(())
                });
            }
        }

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                if let Some(cgroup) = &cgroup {
                    cgroup.remove();
                }
                return Err(SandboxError::SpawnFailed {
                    backend: self.name(),
                    reason: error.to_string(),
                });
            }
        };
        startup_gate.child_spawned();

        let (identity, sandbox_leader_pid) = if let Some((uid, gid)) = host_identity {
            let cgroup = cgroup
                .as_ref()
                .expect("HostIdentity requires a cgroup before spawning");
            let observation = match startup_gate.verify(child.id(), cgroup, uid, gid) {
                Ok(observation) => observation,
                Err(error) => {
                    return Err(identity_start_failed(
                        self.name(),
                        &plan.session_id,
                        &mut child,
                        cgroup,
                        error,
                    ));
                }
            };
            (Some(observation), Some(observation.sandbox_leader_pid))
        } else {
            (None, None)
        };

        Ok(PreparedSession {
            session: Some(SandboxedSession {
                session_id: plan.session_id.clone(),
                backend: self.name(),
                evidence: self.evidence(plan.network, cgroup.as_ref(), identity),
                child,
                cgroup,
                sandbox_leader_pid,
                _status_guard: None,
            }),
            startup_gate: Some(startup_gate),
        })
    }

    fn evidence(
        &self,
        network: NetworkPolicy,
        cgroup: Option<&Cgroup>,
        identity: Option<HostIdentityObservation>,
    ) -> IsolationEvidence {
        let identity_satisfied = identity.is_some_and(|observed| observed.initial_user_namespace);
        // `spawn` retains a cgroup only after its empty-tree lifecycle probe
        // has successfully frozen, thawed, and killed it.
        let lifecycle_satisfied = cgroup.is_some();
        let dimensions = vec![
            DimensionEvidence {
                dimension: Dimension::FilesystemVisibility,
                satisfied: true,
                mechanism: "mount namespace".to_owned(),
                detail: "Only the measured runtime, private home, workspace, and declared system roots are bound.".to_owned(),
            },
            DimensionEvidence {
                dimension: Dimension::FilesystemMutation,
                satisfied: true,
                mechanism: "read-only bind mounts".to_owned(),
                detail: "Everything but the private home and workspace is bound read-only.".to_owned(),
            },
            DimensionEvidence {
                dimension: Dimension::ProcessSeparation,
                satisfied: true,
                mechanism: "pid namespace".to_owned(),
                detail: "The Session is its own pid namespace and sees no process outside it.".to_owned(),
            },
            DimensionEvidence {
                dimension: Dimension::ProcessInheritance,
                satisfied: true,
                mechanism: "--clearenv, --new-session, close-on-exec".to_owned(),
                detail: "No environment, controlling terminal, or descriptor beyond the declared channels is inherited.".to_owned(),
            },
            DimensionEvidence {
                dimension: Dimension::IpcAccess,
                satisfied: true,
                mechanism: "ipc namespace and unbound sockets".to_owned(),
                detail: "No host socket is bound except the launcher's own channels.".to_owned(),
            },
            DimensionEvidence {
                dimension: Dimension::NetworkDenial,
                satisfied: network == NetworkPolicy::Denied,
                mechanism: "network namespace".to_owned(),
                detail: "The Session has an empty network namespace with no route out.".to_owned(),
            },
            DimensionEvidence {
                dimension: Dimension::Identity,
                satisfied: identity_satisfied,
                mechanism: match identity {
                    Some(_) if identity_satisfied => "observed host uid/gid".to_owned(),
                    Some(_) => "current user namespace only".to_owned(),
                    None => "namespace only".to_owned(),
                },
                detail: match identity {
                    Some(observed) if identity_satisfied => {
                        format!(
                            "Host process {} was observed as uid {} and gid {} with no supplementary groups.",
                            observed.sandbox_leader_pid, observed.uid, observed.gid,
                        )
                    }
                    Some(observed) => format!(
                        "Process {} adopted uid {} and gid {} only relative to the launcher's mapped user namespace.",
                        observed.sandbox_leader_pid, observed.uid, observed.gid,
                    ),
                    None => "The Session shares the launcher's host identity.".to_owned(),
                },
            },
            DimensionEvidence {
                dimension: Dimension::Lifecycle,
                satisfied: lifecycle_satisfied,
                mechanism: if lifecycle_satisfied {
                    "cgroup v2 freeze and kill".to_owned()
                } else {
                    "none".to_owned()
                },
                detail: if lifecycle_satisfied {
                    "The whole tree can be frozen and killed through its own cgroup.".to_owned()
                } else {
                    "No usable cgroup v2 freeze-and-kill controls, so the tree cannot be frozen or proven gone.".to_owned()
                },
            },
            DimensionEvidence {
                dimension: Dimension::Evidence,
                satisfied: true,
                mechanism: "backend report".to_owned(),
                detail: "Mechanisms reported are the ones the backend actually invoked.".to_owned(),
            },
        ];
        IsolationEvidence {
            contract_version: CONTRACT_VERSION.to_owned(),
            backend: self.name().to_owned(),
            backend_version: self.version().unwrap_or_else(|_| "unknown".to_owned()),
            kernel: self.prerequisites(),
            dimensions,
        }
    }
}

impl Backend for BubblewrapBackend {
    fn name(&self) -> &'static str {
        "bubblewrap"
    }

    fn version(&self) -> Result<String, SandboxError> {
        if let Some(version) = &self.cached_version {
            return Ok(version.clone());
        }
        let output = Command::new(&self.program)
            .arg("--version")
            .output()
            .map_err(|error| SandboxError::BackendMissing {
                backend: "bubblewrap",
                program: self.program.display().to_string(),
                reason: error.to_string(),
            })?;
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    fn prerequisites(&self) -> KernelPrerequisites {
        let user_namespaces = Path::new("/proc/self/uid_map").is_file()
            && fs::read_to_string("/proc/sys/user/max_user_namespaces")
                .ok()
                .and_then(|value| value.trim().parse::<u64>().ok())
                .is_some_and(|count| count > 0);
        let cgroup_v2 = self.cgroup_parent().is_ok_and(|parent| parent.is_some());
        KernelPrerequisites {
            user_namespaces,
            pid_namespaces: Path::new("/proc/self/ns/pid").exists(),
            network_namespaces: Path::new("/proc/self/ns/net").exists(),
            cgroup_v2,
            details: vec![format!(
                "bwrap at {}",
                crate::scan::escape(&self.program.display().to_string())
            )],
        }
    }

    fn spawn(&self, plan: &ConfinementPlan) -> Result<SandboxedSession, SandboxError> {
        self.prepare(plan)?.start()
    }
}

/// Returns the system directories a Session may see read-only.
///
/// Fixed here rather than taken from the request: a caller who can add a mount
/// can add the one directory that undoes the rest.
pub fn default_system_roots() -> Vec<PathBuf> {
    [
        "/usr",
        "/bin",
        "/sbin",
        "/lib",
        "/lib64",
        "/etc/alternatives",
    ]
    .iter()
    .map(PathBuf::from)
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn freeze_wait_requires_kernel_confirmation() {
        let fixture = tempfile::tempdir().expect("fixture opens");
        fs::write(fixture.path().join("cgroup.freeze"), "0")
            .expect("freeze control fixture writes");
        fs::write(
            fixture.path().join("cgroup.events"),
            "populated 1\nfrozen 1\n",
        )
        .expect("event fixture writes");
        let cgroup = Cgroup {
            path: fixture.path().to_owned(),
        };

        cgroup
            .request_freeze_and_wait(Duration::ZERO)
            .expect("reported kernel freeze completes");
        assert_eq!(
            fs::read_to_string(fixture.path().join("cgroup.freeze")).expect("freeze request reads"),
            "1",
        );

        fs::write(
            fixture.path().join("cgroup.events"),
            "populated 1\nfrozen 0\n",
        )
        .expect("unfrozen event fixture writes");
        assert!(matches!(
            cgroup.request_freeze_and_wait(Duration::ZERO),
            Err(SandboxError::SpawnFailed { .. }),
        ));
    }

    #[test]
    fn mechanical_state_requires_live_membership_and_a_settled_freeze_request() {
        let fixture = tempfile::tempdir().expect("fixture opens");
        let mut session = test_session("mechanical-state", fixture.path(), None);
        fs::write(
            fixture.path().join("cgroup.procs"),
            format!("{}\n", session.child.id()),
        )
        .expect("membership fixture writes");
        fs::write(fixture.path().join("cgroup.freeze"), "1")
            .expect("freeze request fixture writes");
        fs::write(
            fixture.path().join("cgroup.events"),
            "populated 1\nfrozen 0\n",
        )
        .expect("freeze event fixture writes");

        assert!(
            session.mechanical_state().is_err(),
            "an accepted but incomplete kernel transition is ambiguous",
        );

        fs::write(fixture.path().join("cgroup.procs"), "")
            .expect("empty membership fixture writes");
        fs::write(fixture.path().join("cgroup.freeze"), "0")
            .expect("running request fixture writes");
        assert!(
            session.mechanical_state().is_err(),
            "a live launcher child outside the proven tree is ambiguous",
        );
        clean_up_test_child(&mut session);
    }

    #[test]
    fn mechanical_state_reports_settled_running_parked_and_exit() {
        let fixture = tempfile::tempdir().expect("fixture opens");
        let freeze = fixture.path().join("cgroup.freeze");
        let events = fixture.path().join("cgroup.events");
        let mut session = test_session("mechanical-state", fixture.path(), None);
        fs::write(
            fixture.path().join("cgroup.procs"),
            format!("{}\n", session.child.id()),
        )
        .expect("membership fixture writes");

        fs::write(&freeze, "0").expect("running request fixture writes");
        fs::write(&events, "populated 1\nfrozen 0\n").expect("running event fixture writes");
        assert_eq!(
            session.mechanical_state().expect("running state proves"),
            SandboxMechanicalState::Running,
        );

        fs::write(&freeze, "1").expect("park request fixture writes");
        fs::write(&events, "populated 1\nfrozen 1\n").expect("park event fixture writes");
        assert_eq!(
            session.mechanical_state().expect("parked state proves"),
            SandboxMechanicalState::Parked,
        );

        session.child.kill().expect("child exits");
        session.child.wait().expect("child is reaped");
        assert_eq!(
            session.mechanical_state().expect("exit state proves"),
            SandboxMechanicalState::Exited(-1),
        );
    }

    #[test]
    fn resume_waits_for_kernel_thaw_confirmation_and_refreezes_on_timeout() {
        let fixture = tempfile::tempdir().expect("fixture opens");
        let freeze = fixture.path().join("cgroup.freeze");
        fs::write(&freeze, "1").expect("freeze control fixture writes");
        fs::write(
            fixture.path().join("cgroup.events"),
            "populated 1\nfrozen 1\n",
        )
        .expect("event fixture writes");
        let mut session = test_session("resume-timeout", fixture.path(), None);
        fs::write(
            fixture.path().join("cgroup.procs"),
            format!("{}\n", session.child.id()),
        )
        .expect("membership fixture writes");

        let error = session
            .resume_with_timeout(Duration::ZERO)
            .expect_err("Resume waits for the kernel to report frozen=0");
        let final_request = fs::read_to_string(&freeze).expect("freeze request reads");
        session.child.kill().expect("child can be cleaned up");
        session.child.wait().expect("child is reaped");

        assert!(
            matches!(error, SandboxError::SpawnFailed { backend: "cgroup v2", ref reason }
                if reason.contains("running")),
            "unexpected error: {error}",
        );
        assert_eq!(
            final_request, "1",
            "a failed thaw must request and confirm a re-freeze",
        );
    }

    #[test]
    fn resume_rejects_an_empty_tree_without_thawing_it() {
        let fixture = tempfile::tempdir().expect("fixture opens");
        let freeze = fixture.path().join("cgroup.freeze");
        fs::write(&freeze, "1").expect("freeze control fixture writes");
        fs::write(
            fixture.path().join("cgroup.events"),
            "populated 0\nfrozen 1\n",
        )
        .expect("event fixture writes");
        fs::write(fixture.path().join("cgroup.procs"), "")
            .expect("empty membership fixture writes");
        let mut session = test_session("resume-empty", fixture.path(), None);

        let error = session
            .resume_with_timeout(Duration::ZERO)
            .expect_err("an empty Session tree cannot Resume");
        let final_request = fs::read_to_string(&freeze).expect("freeze request reads");
        session.child.kill().expect("child can be cleaned up");
        session.child.wait().expect("child is reaped");

        assert!(
            matches!(error, SandboxError::SpawnFailed { backend: "test", ref reason }
                if reason.contains("tree exited")),
            "unexpected error: {error}",
        );
        assert_eq!(final_request, "1", "an empty tree must remain frozen");
    }

    #[test]
    fn resume_rejects_an_exited_launcher_child_without_thawing_the_tree() {
        let fixture = tempfile::tempdir().expect("fixture opens");
        let freeze = fixture.path().join("cgroup.freeze");
        fs::write(&freeze, "1").expect("freeze control fixture writes");
        fs::write(
            fixture.path().join("cgroup.events"),
            "populated 1\nfrozen 1\n",
        )
        .expect("event fixture writes");
        let mut session = test_session("resume-exited", fixture.path(), None);
        fs::write(
            fixture.path().join("cgroup.procs"),
            format!("{}\n", session.child.id()),
        )
        .expect("membership fixture writes");
        session.child.kill().expect("child exits");
        session.child.wait().expect("child is reaped");

        let error = session
            .resume_with_timeout(Duration::ZERO)
            .expect_err("an exited Session cannot Resume");

        assert!(
            matches!(error, SandboxError::SpawnFailed { backend: "test", ref reason }
                if reason.contains("Session exited")),
            "unexpected error: {error}",
        );
        assert_eq!(
            fs::read_to_string(freeze).expect("freeze request reads"),
            "1",
            "an exited Session tree must remain frozen",
        );
    }

    fn replace_freeze_event(cgroup_path: &Path, frozen: bool) {
        let pending = cgroup_path.join("cgroup.events.next");
        fs::write(
            &pending,
            format!("populated 1\nfrozen {}\n", u8::from(frozen)),
        )
        .expect("next event fixture writes");
        fs::rename(pending, cgroup_path.join("cgroup.events"))
            .expect("event fixture changes atomically");
    }

    fn wait_for_freeze_request(path: &Path, expected: &str) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while fs::read_to_string(path).ok().as_deref() != Some(expected) {
            assert!(
                Instant::now() < deadline,
                "freeze control never received request {expected}",
            );
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn clean_up_test_child(session: &mut SandboxedSession) {
        let _ = session.child.kill();
        session.child.wait().expect("child is reaped");
    }

    #[test]
    fn interrupt_restores_and_confirms_a_running_tree() {
        let fixture = tempfile::tempdir().expect("fixture opens");
        let freeze = fixture.path().join("cgroup.freeze");
        fs::write(&freeze, "0").expect("freeze control fixture writes");
        replace_freeze_event(fixture.path(), false);
        let mut session = test_session("interrupt-running", fixture.path(), None);
        fs::write(
            fixture.path().join("cgroup.procs"),
            format!("{}\n", session.child.id()),
        )
        .expect("membership fixture writes");
        let kernel_path = fixture.path().to_owned();
        let kernel_freeze = freeze.clone();
        let kernel = thread::spawn(move || {
            wait_for_freeze_request(&kernel_freeze, "1");
            replace_freeze_event(&kernel_path, true);
            wait_for_freeze_request(&kernel_freeze, "0");
            replace_freeze_event(&kernel_path, false);
        });

        let result = session.interrupt_with_timeout(Duration::from_secs(1));
        kernel.join().expect("kernel fixture completes");
        let is_parked = session.is_parked();
        let final_request = fs::read_to_string(&freeze).expect("freeze request reads");
        clean_up_test_child(&mut session);

        assert_eq!(result.expect("interrupt succeeds"), 1);
        assert!(
            !is_parked,
            "a Running tree must return to confirmed running"
        );
        assert_eq!(final_request, "0");
    }

    #[test]
    fn interrupt_keeps_a_parked_tree_confirmed_frozen() {
        let fixture = tempfile::tempdir().expect("fixture opens");
        let freeze = fixture.path().join("cgroup.freeze");
        fs::write(&freeze, "1").expect("freeze control fixture writes");
        replace_freeze_event(fixture.path(), true);
        let mut session = test_session("interrupt-parked", fixture.path(), None);
        fs::write(
            fixture.path().join("cgroup.procs"),
            format!("{}\n", session.child.id()),
        )
        .expect("membership fixture writes");

        let result = session.interrupt_with_timeout(Duration::ZERO);
        let is_parked = session.is_parked();
        let final_request = fs::read_to_string(&freeze).expect("freeze request reads");
        clean_up_test_child(&mut session);

        assert_eq!(result.expect("interrupt succeeds"), 1);
        assert!(is_parked, "a Parked tree must remain confirmed frozen");
        assert_eq!(final_request, "1");
    }

    #[test]
    fn interrupt_rejects_an_empty_tree_and_restores_running_state() {
        let fixture = tempfile::tempdir().expect("fixture opens");
        let freeze = fixture.path().join("cgroup.freeze");
        fs::write(&freeze, "0").expect("freeze control fixture writes");
        replace_freeze_event(fixture.path(), false);
        fs::write(fixture.path().join("cgroup.procs"), "")
            .expect("empty membership fixture writes");
        let mut session = test_session("interrupt-empty", fixture.path(), None);
        let kernel_path = fixture.path().to_owned();
        let kernel_freeze = freeze.clone();
        let kernel = thread::spawn(move || {
            wait_for_freeze_request(&kernel_freeze, "1");
            replace_freeze_event(&kernel_path, true);
            wait_for_freeze_request(&kernel_freeze, "0");
            replace_freeze_event(&kernel_path, false);
        });

        let result = session.interrupt_with_timeout(Duration::from_secs(1));
        kernel.join().expect("kernel fixture completes");
        let is_parked = session.is_parked();
        let final_request = fs::read_to_string(&freeze).expect("freeze request reads");
        clean_up_test_child(&mut session);
        let error = result.expect_err("an empty Session tree cannot be interrupted");

        assert!(
            matches!(error, SandboxError::SpawnFailed { backend: "test", ref reason }
                if reason.contains("tree exited")),
            "unexpected error: {error}",
        );
        assert!(!is_parked, "the prior Running state must be restored");
        assert_eq!(final_request, "0");
    }

    #[test]
    fn interrupt_rejects_an_exited_launcher_child_without_signalling_members() {
        let fixture = tempfile::tempdir().expect("fixture opens");
        let freeze = fixture.path().join("cgroup.freeze");
        fs::write(&freeze, "1").expect("freeze control fixture writes");
        replace_freeze_event(fixture.path(), true);
        let mut session = test_session("interrupt-exited", fixture.path(), None);
        session.child.kill().expect("launcher child exits");
        session.child.wait().expect("launcher child is reaped");
        let mut member = Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("membership fixture process starts");
        fs::write(
            fixture.path().join("cgroup.procs"),
            format!("{}\n", member.id()),
        )
        .expect("membership fixture writes");

        let result = session.interrupt_with_timeout(Duration::ZERO);
        let is_parked = session.is_parked();
        let final_request = fs::read_to_string(&freeze).expect("freeze request reads");
        let member_was_running = member
            .try_wait()
            .expect("membership fixture process can be observed")
            .is_none();
        let _ = member.kill();
        member.wait().expect("membership fixture process is reaped");
        let error = result.expect_err("an exited Session cannot be interrupted");

        assert!(
            matches!(error, SandboxError::SpawnFailed { backend: "test", ref reason }
                if reason.contains("Session exited")),
            "unexpected error: {error}",
        );
        assert!(
            member_was_running,
            "an unrelated member must not be signalled after the launcher child exited",
        );
        assert!(is_parked, "the prior Parked state must be preserved");
        assert_eq!(final_request, "1");
    }

    #[test]
    fn interrupt_reports_mechanic_failure_after_restoring_state() {
        let fixture = tempfile::tempdir().expect("fixture opens");
        let freeze = fixture.path().join("cgroup.freeze");
        fs::write(&freeze, "1").expect("freeze control fixture writes");
        replace_freeze_event(fixture.path(), true);
        fs::write(fixture.path().join("cgroup.procs"), "not-a-pid\n")
            .expect("malformed membership fixture writes");
        let mut session = test_session("interrupt-mechanic-error", fixture.path(), None);

        let error = session
            .interrupt_with_timeout(Duration::ZERO)
            .expect_err("malformed membership fails interrupt");
        let is_parked = session.is_parked();
        let final_request = fs::read_to_string(&freeze).expect("freeze request reads");
        clean_up_test_child(&mut session);

        assert!(matches!(error, SandboxError::Io { .. }));
        assert!(is_parked, "mechanic failure restores the original state");
        assert_eq!(final_request, "1");
    }

    #[test]
    fn interrupt_reports_unconfirmed_restoration() {
        let fixture = tempfile::tempdir().expect("fixture opens");
        let freeze = fixture.path().join("cgroup.freeze");
        fs::write(&freeze, "0").expect("freeze control fixture writes");
        replace_freeze_event(fixture.path(), false);
        let mut session = test_session("interrupt-restore-error", fixture.path(), None);
        fs::write(
            fixture.path().join("cgroup.procs"),
            format!("{}\n", session.child.id()),
        )
        .expect("membership fixture writes");
        let kernel_path = fixture.path().to_owned();
        let kernel_freeze = freeze.clone();
        let kernel = thread::spawn(move || {
            wait_for_freeze_request(&kernel_freeze, "1");
            replace_freeze_event(&kernel_path, true);
        });

        let error = session
            .interrupt_with_timeout(Duration::from_secs(1))
            .expect_err("unconfirmed running restoration fails interrupt");
        kernel.join().expect("kernel fixture completes");
        let final_request = fs::read_to_string(&freeze).expect("freeze request reads");
        clean_up_test_child(&mut session);

        assert!(
            matches!(error, SandboxError::SpawnFailed { backend: "cgroup v2", ref reason }
                if reason.contains("running")),
            "unexpected error: {error}",
        );
        assert_eq!(final_request, "0", "restoration was requested");
    }

    fn test_session(
        session_id: &str,
        cgroup_path: &Path,
        status_guard: Option<UnixStream>,
    ) -> SandboxedSession {
        SandboxedSession {
            session_id: session_id.to_owned(),
            backend: "test",
            evidence: IsolationEvidence {
                contract_version: CONTRACT_VERSION.to_owned(),
                backend: "test".to_owned(),
                backend_version: "1".to_owned(),
                kernel: KernelPrerequisites {
                    user_namespaces: true,
                    pid_namespaces: true,
                    network_namespaces: true,
                    cgroup_v2: true,
                    details: Vec::new(),
                },
                dimensions: Vec::new(),
            },
            child: Command::new("/bin/sleep")
                .arg("30")
                .spawn()
                .expect("child starts"),
            cgroup: Some(Cgroup {
                path: cgroup_path.to_owned(),
            }),
            sandbox_leader_pid: None,
            _status_guard: status_guard,
        }
    }

    fn lifecycle_evidence(cgroup: Option<&Cgroup>) -> DimensionEvidence {
        BubblewrapBackend::new()
            .evidence(NetworkPolicy::Denied, cgroup, None)
            .dimensions
            .into_iter()
            .find(|evidence| evidence.dimension == Dimension::Lifecycle)
            .expect("Lifecycle evidence is present")
    }

    #[test]
    fn launcher_backend_rejects_an_untrusted_pinned_cgroup_parent_without_fallback() {
        let fixture = tempfile::tempdir().expect("fixture opens");
        fs::write(fixture.path().join("cgroup.procs"), "")
            .expect("membership control fixture writes");
        fs::set_permissions(fixture.path(), fs::Permissions::from_mode(0o700))
            .expect("fixture mode is exact");
        let backend = BubblewrapBackend::for_launcher(
            Path::new("/usr/bin/bwrap"),
            fixture.path(),
            "bubblewrap 1.0".to_owned(),
        );

        let error = backend
            .cgroup_parent()
            .expect_err("a caller-owned parent must not be trusted or replaced");
        assert!(
            matches!(error, SandboxError::NoCgroup(ref reason) if reason.contains(&fixture.path().display().to_string())),
            "unexpected error: {error}",
        );

        let link = fixture.path().with_extension("link");
        std::os::unix::fs::symlink(fixture.path(), &link).expect("parent symlink creates");
        let backend = BubblewrapBackend::for_launcher(
            Path::new("/usr/bin/bwrap"),
            &link,
            "bubblewrap 1.0".to_owned(),
        );
        assert!(
            matches!(backend.cgroup_parent(), Err(SandboxError::NoCgroup(_))),
            "a pinned symlink must not be followed or replaced",
        );
        fs::remove_file(link).expect("parent symlink removes");
    }

    #[test]
    fn launcher_evidence_uses_the_cached_backend_version_without_spawning() {
        let fixture = tempfile::tempdir().expect("fixture opens");
        fs::write(fixture.path().join("cgroup.procs"), "")
            .expect("membership control fixture writes");
        let marker = fixture.path().join("backend-ran");
        let program = fixture.path().join("bwrap");
        fs::write(
            &program,
            format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
        )
        .expect("backend fixture writes");
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755))
            .expect("backend fixture becomes executable");
        let backend = BubblewrapBackend::for_launcher(
            &program,
            fixture.path(),
            "bubblewrap measured-before-authorization".to_owned(),
        );

        let evidence = backend.evidence(NetworkPolicy::Denied, None, None);

        assert_eq!(
            evidence.backend_version,
            "bubblewrap measured-before-authorization",
        );
        assert!(!marker.exists(), "evidence must not spawn Bubblewrap");
    }

    #[test]
    fn pinned_root_cgroup_prevents_operator_migration_of_a_session_process() {
        if std::env::var_os("LOUISELM_TEST_PINNED_CGROUP_ESCAPE").is_none() {
            return;
        }
        assert!(
            rustix::process::geteuid().is_root(),
            "the pinned-cgroup acceptance must run as root",
        );
        let parse_id = |name: &str| {
            std::env::var(name)
                .unwrap_or_else(|_| panic!("{name} is required"))
                .parse::<u32>()
                .unwrap_or_else(|error| panic!("{name} must be numeric: {error}"))
        };
        let operator_uid = parse_id("LOUISELM_TEST_OPERATOR_UID");
        let operator_gid = parse_id("LOUISELM_TEST_OPERATOR_GID");
        let session_identity = parse_id("LOUISELM_TEST_HOST_ID");
        assert_ne!(operator_uid, 0, "the operator must be non-root");
        assert_ne!(operator_gid, 0, "the operator group must be non-root");
        assert_ne!(session_identity, 0, "the Session identity must be non-root");

        struct CgroupFixture(PathBuf);

        impl Drop for CgroupFixture {
            fn drop(&mut self) {
                let _ = fs::remove_dir(&self.0);
            }
        }

        let cgroup_root = Path::new("/sys/fs/cgroup");
        assert!(
            cgroup_root.join("cgroup.controllers").is_file(),
            "the pinned-cgroup acceptance requires cgroup v2",
        );
        let suffix = std::process::id();
        let launcher_parent = cgroup_root.join(format!("louiselm-launch-test-{suffix}"));
        fs::create_dir(&launcher_parent).expect("the root-only launcher cgroup parent creates");
        let _launcher_parent_guard = CgroupFixture(launcher_parent.clone());
        fs::set_permissions(&launcher_parent, fs::Permissions::from_mode(0o700))
            .expect("the launcher cgroup parent becomes root-only");

        let operator_cgroup = cgroup_root.join(format!("louiselm-operator-test-{suffix}"));
        fs::create_dir(&operator_cgroup).expect("the operator cgroup creates");
        let _operator_cgroup_guard = CgroupFixture(operator_cgroup.clone());
        chown(&operator_cgroup, Some(operator_uid), Some(operator_gid))
            .expect("the operator owns its cgroup");
        fs::set_permissions(&operator_cgroup, fs::Permissions::from_mode(0o700))
            .expect("the operator cgroup becomes private");
        let operator_procs = operator_cgroup.join("cgroup.procs");
        chown(&operator_procs, Some(operator_uid), Some(operator_gid))
            .expect("the operator can manage its own cgroup membership");

        let fixture = tempfile::tempdir().expect("Session fixture opens");
        fs::set_permissions(fixture.path(), fs::Permissions::from_mode(0o755))
            .expect("the assigned identity can traverse the fixture");
        let sessions_root = fixture.path().join("sessions");
        fs::DirBuilder::new()
            .mode(0o711)
            .create(&sessions_root)
            .expect("the root-owned Sessions root creates");
        fs::set_permissions(&sessions_root, fs::Permissions::from_mode(0o711))
            .expect("the root-owned Sessions root has its fixed mode");
        let runtime_root = fixture.path().join("runtime");
        fs::create_dir(&runtime_root).expect("the runtime root creates");
        fs::set_permissions(&runtime_root, fs::Permissions::from_mode(0o755))
            .expect("the runtime root is traversable");
        let executable = runtime_root.join("agent");
        fs::write(&executable, "#!/bin/sh\nexec /bin/sleep 30\n")
            .expect("the Agent fixture writes");
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))
            .expect("the Agent fixture becomes executable");
        let session_id = format!("pinned-cgroup-{suffix}");
        let plan = ConfinementPlan {
            session_id: session_id.clone(),
            runtime_root,
            executable,
            arguments: Vec::new(),
            environment: BTreeMap::new(),
            home: sessions_root.join(&session_id).join("home"),
            workspace: sessions_root.join(&session_id).join("workspace"),
            system_roots: default_system_roots(),
            network: NetworkPolicy::Denied,
            identity: IdentityPlan::HostIdentity {
                uid: session_identity,
                gid: session_identity,
            },
            channels: vec![Channel::AcpStdio {
                id: "acp".to_owned(),
            }],
        };
        let backend = BubblewrapBackend::for_launcher(
            Path::new("/usr/bin/bwrap"),
            &launcher_parent,
            "bubblewrap measured before authorization".to_owned(),
        );
        let mut prepared = backend
            .prepare(&plan)
            .expect("the pinned backend prepares the Session");
        let sandbox_leader = prepared
            .sandbox_leader_pid()
            .expect("Bubblewrap reports its sandbox leader");
        let session_cgroup = launcher_parent.join(format!("louiselm-session-{session_id}"));
        assert!(session_cgroup.is_dir(), "the pinned parent owns the child");

        let mut helper = Command::new("/bin/sh");
        helper
            .arg("-c")
            .arg(
                "IFS= read -r pid; printf '0\\n' > \"$DESTINATION\" || exit 10; printf '%s\\n' \"$pid\" > \"$DESTINATION\"",
            )
            .env_clear()
            .env("DESTINATION", &operator_procs)
            .uid(operator_uid)
            .gid(operator_gid)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut helper = helper.spawn().expect("the operator helper starts");
        fs::write(&operator_procs, format!("{}\n", helper.id()))
            .expect("root places the helper in its delegated cgroup");
        writeln!(
            helper.stdin.as_mut().expect("helper stdin is piped"),
            "{sandbox_leader}",
        )
        .expect("the migration target reaches the helper");
        drop(helper.stdin.take());
        let attack = helper
            .wait_with_output()
            .expect("the operator migration attempt exits");
        let still_enclosed = prepared
            .process_tree()
            .expect("the prepared Session has a process tree")
            .contains(sandbox_leader)
            .expect("Session membership remains readable");
        if !still_enclosed {
            fs::write(
                session_cgroup.join("cgroup.procs"),
                format!("{sandbox_leader}\n"),
            )
            .expect("root restores an escaped leader before cleanup");
        }
        let disposal = prepared.dispose();
        let session_cgroup_removed = !session_cgroup.exists();

        assert_ne!(
            attack.status.code(),
            Some(10),
            "the operator helper must be able to write its own cgroup.procs: {}",
            String::from_utf8_lossy(&attack.stderr),
        );
        assert!(
            !attack.status.success(),
            "the operator escaped the Session leader from the pinned cgroup",
        );
        assert!(
            still_enclosed,
            "the failed migration must leave the leader enclosed"
        );
        let disposal = disposal.expect("the prepared Session cleans up");
        assert_eq!(disposal.survivors, 0);
        assert!(
            session_cgroup_removed,
            "cleanup removes the Session child cgroup",
        );
    }

    #[test]
    fn namespace_only_omits_lifecycle_when_kill_control_is_missing() {
        let fixture = tempfile::tempdir().expect("fixture opens");
        fs::write(fixture.path().join("cgroup.procs"), "").expect("membership fixture writes");
        fs::write(fixture.path().join("cgroup.freeze"), "0")
            .expect("freeze control fixture writes");
        let kill = fixture.path().join("cgroup.kill");
        std::os::unix::fs::symlink(fixture.path().join("missing/cgroup.kill"), &kill)
            .expect("missing kill control fixture links");
        let cgroup = Cgroup {
            path: fixture.path().to_owned(),
        };

        let checked = preflight_cgroup(Some(cgroup), IdentityPlan::NamespaceOnly)
            .expect("namespace-only startup degrades without lifecycle controls");
        assert!(checked.is_none());
        let evidence = lifecycle_evidence(checked.as_ref());
        assert!(!evidence.satisfied);
        assert_eq!(evidence.mechanism, "none");

        fs::remove_file(&kill).expect("missing kill control fixture unlinks");
        fs::write(&kill, "").expect("kill control fixture writes");
        let cgroup = Cgroup {
            path: fixture.path().to_owned(),
        };
        let checked = preflight_cgroup(Some(cgroup), IdentityPlan::NamespaceOnly)
            .expect("complete lifecycle controls pass preflight");
        let evidence = lifecycle_evidence(checked.as_ref());
        assert!(evidence.satisfied);
        assert_eq!(evidence.mechanism, "cgroup v2 freeze and kill");
        assert_eq!(
            fs::read_to_string(fixture.path().join("cgroup.freeze")).expect("freeze control reads"),
            "0",
        );
        assert_eq!(fs::read_to_string(&kill).expect("kill control reads"), "1",);
    }

    #[test]
    fn host_identity_refuses_kill_control_write() {
        let identity = IdentityPlan::HostIdentity { uid: 1, gid: 1 };
        assert!(matches!(
            preflight_cgroup(None, identity),
            Err(SandboxError::Refused(_)),
        ));

        let fixture = tempfile::tempdir().expect("fixture opens");
        fs::write(fixture.path().join("cgroup.procs"), "").expect("membership fixture writes");
        fs::write(fixture.path().join("cgroup.freeze"), "0")
            .expect("freeze control fixture writes");
        std::os::unix::fs::symlink("/dev/full", fixture.path().join("cgroup.kill"))
            .expect("refusing kill control fixture links");
        let cgroup = Cgroup {
            path: fixture.path().to_owned(),
        };

        let error = preflight_cgroup(Some(cgroup), identity)
            .expect_err("HostIdentity fails closed when kill is refused");
        assert!(matches!(
            error,
            SandboxError::Io { ref path, .. }
                if Path::new(path) == fixture.path().join("cgroup.kill")
        ));
        assert_eq!(
            fs::read_to_string(fixture.path().join("cgroup.freeze")).expect("freeze control reads"),
            "0",
            "failed kill preflight leaves the empty cgroup thawed",
        );
    }

    #[test]
    fn host_identity_refuses_unreadable_membership_control() {
        let fixture = tempfile::tempdir().expect("fixture opens");
        fs::create_dir(fixture.path().join("cgroup.procs"))
            .expect("unreadable membership fixture creates");
        fs::write(fixture.path().join("cgroup.freeze"), "0")
            .expect("freeze control fixture writes");
        fs::write(fixture.path().join("cgroup.kill"), "").expect("kill control fixture writes");
        let cgroup = Cgroup {
            path: fixture.path().to_owned(),
        };

        let error = preflight_cgroup(Some(cgroup), IdentityPlan::HostIdentity { uid: 1, gid: 1 })
            .expect_err("HostIdentity fails closed when membership cannot be read");
        assert!(matches!(
            error,
            SandboxError::Io { ref path, .. }
                if Path::new(path) == fixture.path().join("cgroup.procs")
        ));
    }

    #[test]
    fn lifecycle_preflight_refuses_a_nonempty_cgroup() {
        let fixture = tempfile::tempdir().expect("fixture opens");
        fs::write(fixture.path().join("cgroup.procs"), "123\n").expect("membership fixture writes");
        fs::write(fixture.path().join("cgroup.freeze"), "0")
            .expect("freeze control fixture writes");
        fs::write(fixture.path().join("cgroup.kill"), "").expect("kill control fixture writes");
        let cgroup = Cgroup {
            path: fixture.path().to_owned(),
        };

        let error = preflight_cgroup(Some(cgroup), IdentityPlan::NamespaceOnly)
            .expect_err("a lifecycle probe never kills an existing process tree");
        assert!(matches!(error, SandboxError::NoCgroup(_)));
        assert_eq!(
            fs::read_to_string(fixture.path().join("cgroup.kill")).expect("kill control reads"),
            "",
        );
    }

    #[test]
    fn disposal_still_terminates_after_initial_membership_failure() {
        let fixture = tempfile::tempdir().expect("fixture opens");
        let (status_guard, _status_peer) = UnixStream::pair().expect("status pair opens");
        let mut session = test_session(
            "initial-membership-error",
            fixture.path(),
            Some(status_guard),
        );
        let membership = fixture.path().join("cgroup.procs");

        let error = session
            .dispose()
            .expect_err("missing initial membership fails the disposal");
        match error {
            SandboxError::Io { path, source } => {
                assert_eq!(Path::new(&path), membership);
                assert_eq!(source.kind(), io::ErrorKind::NotFound);
            }
            other => panic!("unexpected error: {other}"),
        }
        let terminated = session
            .child
            .try_wait()
            .expect("child status reads")
            .is_some();
        if !terminated {
            session
                .child
                .kill()
                .expect("failed assertion cleans up child");
            session.child.wait().expect("failed assertion reaps child");
        }
        assert!(session.cgroup.is_some(), "the cgroup handle stays owned");
        assert!(
            session._status_guard.is_some(),
            "the identity-lifetime handle stays owned",
        );

        fs::write(&membership, "").expect("membership becomes readably empty");
        session.dispose().expect("empty membership can be retried");
        assert!(terminated, "Disposal must still terminate the child");
    }

    #[test]
    fn disposal_does_not_treat_lost_membership_as_zero_survivors() {
        let fixture = tempfile::tempdir().expect("fixture opens");
        let (status_guard, _status_peer) = UnixStream::pair().expect("status pair opens");
        let mut session = test_session("membership-error", fixture.path(), Some(status_guard));
        let membership = fixture.path().join("cgroup.procs");
        std::os::unix::fs::symlink(
            format!("/proc/{}/oom_score", session.child.id()),
            &membership,
        )
        .expect("membership follows a file that disappears with the child");

        let error = session
            .dispose()
            .expect_err("an unreadable final membership is not zero survivors");
        match error {
            SandboxError::Io { path, source } => {
                assert_eq!(Path::new(&path), membership);
                assert_eq!(source.kind(), io::ErrorKind::NotFound);
            }
            other => panic!("unexpected error: {other}"),
        }
        assert!(session.cgroup.is_some(), "the cgroup handle stays owned");
        assert!(
            session._status_guard.is_some(),
            "the identity-lifetime handle stays owned",
        );

        fs::remove_file(&membership).expect("dangling membership link is removed");
        fs::write(&membership, "").expect("membership becomes readably empty");
        let disposal = session.dispose().expect("empty membership can be retried");
        assert_eq!(disposal.survivors, 0);
        assert!(disposal.identity_released);
        assert!(session.cgroup.is_none(), "the empty cgroup is released");
        assert!(
            session._status_guard.is_none(),
            "the identity-lifetime handle is released",
        );
    }

    #[test]
    fn interrupt_thaws_after_membership_read_failure() {
        let fixture = tempfile::tempdir().expect("fixture opens");
        let freeze = fixture.path().join("cgroup.freeze");
        fs::write(&freeze, "0").expect("freeze fixture writes");
        replace_freeze_event(fixture.path(), false);
        let mut session = test_session("interrupt-error", fixture.path(), None);
        let kernel_path = fixture.path().to_owned();
        let kernel_freeze = freeze.clone();
        let kernel = thread::spawn(move || {
            wait_for_freeze_request(&kernel_freeze, "1");
            replace_freeze_event(&kernel_path, true);
            wait_for_freeze_request(&kernel_freeze, "0");
            replace_freeze_event(&kernel_path, false);
        });

        let error = session
            .interrupt_with_timeout(Duration::from_secs(1))
            .expect_err("unreadable membership fails the interrupt");
        kernel.join().expect("kernel fixture completes");
        match error {
            SandboxError::Io { path, source } => {
                assert_eq!(Path::new(&path), fixture.path().join("cgroup.procs"));
                assert_eq!(source.kind(), io::ErrorKind::NotFound);
            }
            other => panic!("unexpected error: {other}"),
        }
        assert_eq!(
            fs::read_to_string(freeze).expect("freeze state reads"),
            "0",
            "membership failure must not leave the Session frozen",
        );

        session.child.kill().expect("child can be cleaned up");
        session.child.wait().expect("child is reaped");
    }

    #[test]
    fn membership_distinguishes_missing_malformed_and_empty() {
        let fixture = tempfile::tempdir().expect("fixture opens");
        let cgroup = Cgroup {
            path: fixture.path().to_owned(),
        };
        let membership = fixture.path().join("cgroup.procs");

        for (contents, kind) in [
            (None, io::ErrorKind::NotFound),
            (Some("not-a-pid\n"), io::ErrorKind::InvalidData),
        ] {
            if let Some(contents) = contents {
                fs::write(&membership, contents).expect("membership fixture writes");
            }
            let error = cgroup
                .processes()
                .expect_err("missing or malformed membership fails closed");
            match error {
                SandboxError::Io { path, source } => {
                    assert_eq!(Path::new(&path), membership);
                    assert_eq!(source.kind(), kind);
                }
                other => panic!("unexpected error: {other}"),
            }
        }

        fs::write(&membership, "").expect("empty membership fixture writes");
        assert_eq!(
            cgroup.processes().expect("empty membership is readable"),
            Vec::<u32>::new(),
        );
    }

    #[test]
    fn read_only_cgroup_files_are_not_delegation() {
        if rustix::process::geteuid().is_root() {
            return;
        }
        let fixture = tempfile::tempdir().expect("fixture opens");
        let procs = fixture.path().join("cgroup.procs");
        fs::write(&procs, "").expect("membership file is created");
        assert!(usable_delegated_parent(fixture.path()));

        fs::set_permissions(fixture.path(), fs::Permissions::from_mode(0o555))
            .expect("fixture becomes read-only");
        fs::set_permissions(&procs, fs::Permissions::from_mode(0o444))
            .expect("membership becomes read-only");

        assert!(!usable_delegated_parent(fixture.path()));

        fs::set_permissions(fixture.path(), fs::Permissions::from_mode(0o700))
            .expect("fixture is restored for cleanup");
    }
}
