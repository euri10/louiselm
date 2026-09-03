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

use std::{
    collections::BTreeMap,
    fs, io,
    io::Write,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    isolation::{
        CONTRACT_VERSION, Dimension, DimensionEvidence, IsolationEvidence, KernelPrerequisites,
    },
    registry::NetworkPolicy,
};

/// How long disposal waits for a process tree to actually be gone.
pub const DISPOSAL_TIMEOUT: Duration = Duration::from_secs(5);

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
        path.join("cgroup.procs").is_file().then_some(path)
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
    pub fn processes(&self) -> Vec<u32> {
        fs::read_to_string(self.path.join("cgroup.procs"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| line.trim().parse().ok())
            .collect()
    }

    /// Reports whether the cgroup supports freezing.
    pub fn supports_freeze(&self) -> bool {
        self.path.join("cgroup.freeze").is_file()
    }

    /// Reports whether every process in the cgroup is currently frozen.
    pub fn is_frozen(&self) -> bool {
        fs::read_to_string(self.path.join("cgroup.events"))
            .unwrap_or_default()
            .lines()
            .find_map(|line| line.strip_prefix("frozen "))
            .is_some_and(|value| value.trim() == "1")
    }

    /// Freezes every process in the cgroup, including ones forked since.
    pub fn freeze(&self) -> Result<(), SandboxError> {
        self.write("cgroup.freeze", "1")
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
    /// Returns the launcher-side process id of the sandbox.
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Returns every process in the Session's tree.
    pub fn processes(&self) -> Vec<u32> {
        self.cgroup
            .as_ref()
            .map(Cgroup::processes)
            .unwrap_or_default()
    }

    /// Borrows the Session's stdin, when the ACP channel is stdio.
    pub fn stdin(&mut self) -> Option<&mut std::process::ChildStdin> {
        self.child.stdin.as_mut()
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

    /// Freezes the whole tree, preserving in-flight work.
    ///
    /// Freezing rather than stopping the direct child is the point: a Park that
    /// only stopped the process the launcher knows about would leave every
    /// descendant running.
    pub fn park(&self) -> Result<(), SandboxError> {
        self.cgroup
            .as_ref()
            .ok_or_else(|| SandboxError::NoCgroup("this Session has no cgroup".to_owned()))?
            .freeze()
    }

    /// Thaws a parked Session.
    pub fn resume(&self) -> Result<(), SandboxError> {
        self.cgroup
            .as_ref()
            .ok_or_else(|| SandboxError::NoCgroup("this Session has no cgroup".to_owned()))?
            .thaw()
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
    pub fn interrupt(&self) -> Result<usize, SandboxError> {
        let cgroup = self.cgroup.as_ref();
        if let Some(cgroup) = cgroup.filter(|cgroup| cgroup.supports_freeze()) {
            cgroup.freeze()?;
            wait_for_stable_membership(cgroup);
        }

        let outcome = signal(&self.processes(), "-INT", self.backend);

        if let Some(cgroup) = cgroup {
            let _ = cgroup.thaw();
        }
        outcome
    }

    /// Terminates the whole tree and releases the Session's identity.
    pub fn dispose(mut self) -> Result<DisposalReport, SandboxError> {
        let processes_before = self.processes().len();
        if let Some(cgroup) = &self.cgroup {
            // Thaw first: a frozen cgroup cannot process the kill.
            let _ = cgroup.thaw();
            cgroup.kill_all()?;
        }
        let _ = self.child.kill();
        let _ = self.child.wait();

        let deadline = Instant::now() + DISPOSAL_TIMEOUT;
        let mut survivors = self.processes().len();
        while survivors > 0 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
            survivors = self.processes().len();
        }
        if let Some(cgroup) = &self.cgroup {
            cgroup.remove();
        }
        if survivors > 0 {
            return Err(SandboxError::Survivors {
                session_id: self.session_id.clone(),
                survivors,
            });
        }
        Ok(DisposalReport {
            session_id: self.session_id.clone(),
            processes_before,
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
}

/// Waits for `cgroup`'s membership to stop changing, or gives up.
///
/// A frozen cgroup still admits newly forked tasks — they are simply born
/// frozen — so this only closes the startup-fork race before one enumeration;
/// it says nothing about descendants forked later in the Session's life.
fn wait_for_stable_membership(cgroup: &Cgroup) {
    let deadline = Instant::now() + SIGNAL_SETTLE_TIMEOUT;
    let mut previous = cgroup.processes();
    loop {
        thread::sleep(Duration::from_millis(20));
        let current = cgroup.processes();
        if current == previous || Instant::now() >= deadline {
            return;
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
        }
    }

    /// Uses a specific `bwrap` binary.
    pub fn at(program: &Path) -> Self {
        Self {
            program: program.to_path_buf(),
        }
    }

    fn arguments(&self, plan: &ConfinementPlan) -> Vec<String> {
        let mut arguments = vec![
            "--unshare-all".to_owned(),
            "--die-with-parent".to_owned(),
            "--new-session".to_owned(),
            "--clearenv".to_owned(),
        ];
        if let IdentityPlan::HostIdentity { uid, gid } = plan.identity {
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

    fn evidence(&self, plan: &ConfinementPlan, cgroup: Option<&Cgroup>) -> IsolationEvidence {
        let identity_satisfied = matches!(plan.identity, IdentityPlan::HostIdentity { .. });
        let lifecycle_satisfied = cgroup.is_some_and(Cgroup::supports_freeze);
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
                satisfied: plan.network == NetworkPolicy::Denied,
                mechanism: "network namespace".to_owned(),
                detail: "The Session has an empty network namespace with no route out.".to_owned(),
            },
            DimensionEvidence {
                dimension: Dimension::Identity,
                satisfied: identity_satisfied,
                mechanism: if identity_satisfied {
                    "distinct host uid".to_owned()
                } else {
                    "namespace only".to_owned()
                },
                detail: if identity_satisfied {
                    "The Session runs under a host identity of its own.".to_owned()
                } else {
                    "The launcher is not root, so the Session shares the operator's host identity.".to_owned()
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
                    "No writable cgroup v2 hierarchy, so the tree cannot be frozen or proven gone.".to_owned()
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
        let cgroup_v2 = Cgroup::delegated_parent().is_some();
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
        if plan.network != NetworkPolicy::Denied {
            return Err(SandboxError::Refused(
                "this build confines only Sessions with no network; brokered egress belongs to the control service".to_owned(),
            ));
        }
        for writable in [&plan.home, &plan.workspace] {
            fs::create_dir_all(writable).map_err(|source| SandboxError::Io {
                path: writable.display().to_string(),
                source,
            })?;
        }

        let cgroup = Cgroup::delegated_parent()
            .map(|parent| Cgroup::create(&parent, &plan.session_id))
            .transpose()?;

        let mut command = Command::new(&self.program);
        command
            .args(self.arguments(plan))
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        if let Some(cgroup) = &cgroup {
            let procs = cgroup.path().join("cgroup.procs");
            let file = fs::OpenOptions::new()
                .write(true)
                .open(&procs)
                .map_err(|source| SandboxError::Io {
                    path: procs.display().to_string(),
                    source,
                })?;
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

        let child = command.spawn().map_err(|error| SandboxError::SpawnFailed {
            backend: self.name(),
            reason: error.to_string(),
        })?;

        Ok(SandboxedSession {
            session_id: plan.session_id.clone(),
            backend: self.name(),
            evidence: self.evidence(plan, cgroup.as_ref()),
            child,
            cgroup,
        })
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
