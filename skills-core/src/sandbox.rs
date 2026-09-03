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
        fs::read_to_string(self.path.join("cgroup.events"))
            .unwrap_or_default()
            .lines()
            .find_map(|line| line.strip_prefix("frozen "))
            .is_some_and(|value| value.trim() == "1")
    }

    fn supports_kill(&self) -> bool {
        self.path.join("cgroup.kill").is_file()
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
        let outcome = (|| {
            let processes = if let Some(cgroup) = cgroup.filter(|cgroup| cgroup.supports_freeze()) {
                cgroup.freeze()?;
                wait_for_stable_membership(cgroup)?
            } else {
                self.processes()?
            };
            signal(&processes, "-INT", self.backend)
        })();

        if let Some(cgroup) = cgroup {
            let _ = cgroup.thaw();
        }
        outcome
    }

    /// Terminates the whole tree and releases the Session's identity.
    pub fn dispose(&mut self) -> Result<DisposalReport, SandboxError> {
        let processes_before = self.processes().map(|processes| processes.len());
        let cgroup_kill = if let Some(cgroup) = &self.cgroup {
            // Thaw first: a frozen cgroup cannot process the kill.
            let _ = cgroup.thaw();
            cgroup.kill_all()
        } else {
            Ok(())
        };
        let _ = self.child.kill();
        let _ = self.child.wait();
        let processes_before = processes_before?;
        cgroup_kill?;

        let deadline = Instant::now() + DISPOSAL_TIMEOUT;
        let mut survivors = self.processes()?.len();
        while survivors > 0 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
            survivors = self.processes()?.len();
        }
        if survivors > 0 {
            return Err(SandboxError::Survivors {
                session_id: self.session_id.clone(),
                survivors,
            });
        }
        if let Some(cgroup) = self.cgroup.take() {
            cgroup.remove();
        }
        self._status_guard = None;
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
    SandboxError::SpawnFailed {
        backend,
        reason: format!(
            "host identity verification failed: {source}; cleanup of session {session_id} was not proven ({killing}; {membership}; {reaping})",
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

    fn evidence(
        &self,
        network: NetworkPolicy,
        cgroup: Option<&Cgroup>,
        identity: Option<HostIdentityObservation>,
    ) -> IsolationEvidence {
        let identity_satisfied = identity.is_some_and(|observed| observed.initial_user_namespace);
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
        let cgroup_parent = Cgroup::delegated_parent();
        if host_identity.is_some() && cgroup_parent.is_none() {
            return Err(SandboxError::Refused(
                "a distinct host identity requires a writable cgroup for fail-closed startup"
                    .to_owned(),
            ));
        }
        if host_identity.is_some() {
            materialize_session_root(&plan.home, &plan.workspace)?;
        }
        for writable in [&plan.home, &plan.workspace] {
            materialize_writable(writable, host_identity)?;
        }

        let mut identity_gate = host_identity
            .map(|_| HostIdentityGate::new())
            .transpose()
            .map_err(|source| SandboxError::Io {
                path: "host identity startup gate".to_owned(),
                source,
            })?;
        let cgroup = cgroup_parent
            .map(|parent| Cgroup::create(&parent, &plan.session_id))
            .transpose()?;
        if host_identity.is_some() && !cgroup.as_ref().is_some_and(|cgroup| cgroup.supports_kill())
        {
            if let Some(cgroup) = &cgroup {
                cgroup.remove();
            }
            return Err(SandboxError::Refused(
                "a distinct host identity requires cgroup v2 unconditional kill support".to_owned(),
            ));
        }

        let mut command = Command::new(&self.program);
        command
            .args(self.arguments(plan, identity_gate.as_ref()))
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some((uid, gid)) = host_identity {
            command.gid(gid).uid(uid);
        }
        if let Some(gate) = &identity_gate {
            gate.inherit_fds(&mut command);
        }

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
        if let Some(gate) = &mut identity_gate {
            gate.child_spawned();
        }

        let (identity, sandbox_leader_pid, status) = if let Some((uid, gid)) = host_identity {
            let cgroup = cgroup
                .as_ref()
                .expect("HostIdentity requires a cgroup before spawning");
            let gate = identity_gate
                .as_mut()
                .expect("HostIdentity creates a startup gate");
            let observation = match gate.verify(child.id(), cgroup, uid, gid) {
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
            if let Err(error) = gate.release() {
                return Err(identity_start_failed(
                    self.name(),
                    &plan.session_id,
                    &mut child,
                    cgroup,
                    error,
                ));
            }
            let gate = identity_gate
                .take()
                .expect("the verified identity gate is present");
            (
                Some(observation),
                Some(observation.sandbox_leader_pid),
                Some(gate.into_status()),
            )
        } else {
            (None, None, None)
        };

        Ok(SandboxedSession {
            session_id: plan.session_id.clone(),
            backend: self.name(),
            evidence: self.evidence(plan.network, cgroup.as_ref(), identity),
            child,
            cgroup,
            sandbox_leader_pid,
            _status_guard: status,
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

#[cfg(test)]
mod tests {
    use super::*;

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
        fs::write(&freeze, "").expect("freeze fixture writes");
        let mut session = test_session("interrupt-error", fixture.path(), None);

        let error = session
            .interrupt()
            .expect_err("unreadable membership fails the interrupt");
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
