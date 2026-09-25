//! One explicitly invoked certifier owns every probe, lease and temporary resource.

use super::{
    Certificate, CertificateStore, CertificationError, measure,
    probes::{Attack, Probe},
};
use crate::{
    conformance::{Check, Cleanup, Observation, Outcome, REPORT_SCHEMA, Report, Scope, pair},
    launcher_install::{
        self, CommandInvocation, Identity, IdentityLease, LauncherConfig, LauncherPaths,
    },
    sandbox::{
        Backend, BubblewrapBackend, Channel, ConfinementPlan, IdentityPlan, SandboxedSession,
        default_system_roots,
    },
};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{BufRead, BufReader, Read, Write},
    os::{
        fd::OwnedFd,
        unix::{fs::PermissionsExt, process::CommandExt},
    },
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

mod checks;
mod endpoints;

type Result<T> = std::result::Result<T, CertificationError>;

#[cfg(test)]
std::thread_local! {
    pub(crate) static CANCEL_AFTER_FIRST_GROUP: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    pub(crate) static FORCE_UNCONFIRMED_CLEANUP: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run the fixed certifier under an owned, empty network namespace and deadline.
/// Caller must first authenticate the installed operator and running release.
/// No arbitrary worker path, arguments or environment can be supplied.
///
/// # Errors
/// Rejects unmeasurable authority, failed worker/cleanup and invalid evidence.
pub fn certify_isolated(
    paths: &LauncherPaths,
    config: &LauncherConfig,
    deadline: Instant,
) -> Result<Certificate> {
    measure(paths, config, deadline)?;
    let output = launcher_install::run_signing_command(
        &CommandInvocation {
            program: "/usr/bin/unshare".into(),
            arguments: vec![
                "--net".into(),
                "--".into(),
                config.launcher_path.as_os_str().into(),
                "__conformance-worker".into(),
            ],
            stdin: std::process::id().to_string().into_bytes(),
            current_dir: Some("/".into()),
        },
        deadline,
        None,
    )?;
    if !matches!(output.exit_code, Some(0 | 1)) || !output.stderr.is_empty() {
        return Err(CertificationError::Unsupported);
    }
    Certificate::parse_canonical(&output.stdout)
}

/// Execute the fixed installed host-safe suite in a newly owned network namespace.
/// The fixed launcher entrypoint supplies that namespace and an outer deadline.
/// `expected_parent` is the authorizing process ID sent over its owned input pipe;
/// a pidfd binds cancellation to that process. No commands, endpoints, leased
/// identities or probe lists can be selected by callers.
///
/// # Errors
/// Refuses invalid authority/profile, unavailable leases, malformed evidence or
/// unproven cleanup. Interrupted attempts and poisoned leases are never retried.
pub fn certify(
    paths: &LauncherPaths,
    deadline: Instant,
    expected_parent: u32,
) -> Result<Certificate> {
    if !rustix::process::geteuid().is_root() {
        return Err(CertificationError::Invalid);
    }
    let owner = rustix::process::getppid()
        .filter(|pid| {
            pid.as_raw_nonzero().get() > 1
                && pid.as_raw_nonzero().get().cast_unsigned() == expected_parent
        })
        .ok_or(CertificationError::Invalid)?;
    let owner = rustix::process::pidfd_open(owner, rustix::process::PidfdFlags::empty())
        .map_err(std::io::Error::from)?;
    if rustix::process::getppid().map(|pid| pid.as_raw_nonzero().get().cast_unsigned())
        != Some(expected_parent)
    {
        return Err(CertificationError::Pending);
    }
    // Never reconfigure the caller's/host's network namespace.
    if fs::read_link("/proc/self/ns/net")? == fs::read_link("/proc/1/ns/net")? {
        return Err(CertificationError::Invalid);
    }
    let config = launcher_install::runtime_config_with_deadline(paths, deadline)?;
    let before = measure(paths, &config, deadline)?;
    let mut store = CertificateStore::open(&paths.state_root.join("conformance"))?;
    store.begin(&before)?;
    let mut fixture = Fixture::new(config, deadline, owner)?;
    let tested = fixture
        .initialize(paths, &store)
        .and_then(|()| fixture.run(&mut store));
    fixture.report.completed = tested.is_ok();
    fixture.report.cleanup = if fixture.cleanup().is_ok() {
        Cleanup::Confirmed
    } else {
        Cleanup::Unconfirmed
    };
    // Incomplete measurement cannot erase observed failures. Retain partial
    // observations first, then decide whether old inputs still describe the run.
    let mut partial = fixture.report.clone();
    partial.completed = false;
    store.observe(&partial)?;
    let current = measure(paths, &fixture.config, deadline);
    if current.as_ref().ok() != Some(&before) || fixture.check_deadline().is_err() {
        fixture.report.completed = false;
        if let Some(check) = fixture
            .report
            .checks
            .iter_mut()
            .find(|check| check.name == crate::conformance::SENDER_GUARD_CHECK)
        {
            check.confined =
                Outcome::Error("guard measurements changed or certification interrupted".into());
        }
    }
    let certificate = Certificate::new(before, fixture.report.clone())?;
    store.finish(&certificate)?;
    Ok(certificate)
}

struct Fixture {
    directory: Option<tempfile::TempDir>,
    root: PathBuf,
    backend: BubblewrapBackend,
    config: LauncherConfig,
    leases: Vec<IdentityLease>,
    agents: Vec<Agent>,
    services: Vec<endpoints::Service>,
    report: Report,
    deadline: Instant,
    owner: OwnedFd,
}

impl Fixture {
    fn new(config: LauncherConfig, deadline: Instant, owner: OwnedFd) -> Result<Self> {
        let directory = tempfile::Builder::new()
            .prefix("louiselm-cert-")
            .tempdir_in("/var/tmp")?;
        let root = directory.path().to_owned();
        mode(&root, 0o755)?;
        let backend =
            BubblewrapBackend::at(&config.bwrap_path).with_bootstrap(&config.launcher_path);
        Ok(Self {
            directory: Some(directory),
            root,
            backend,
            config,
            leases: Vec::new(),
            agents: Vec::new(),
            services: Vec::new(),
            report: Report {
                schema: REPORT_SCHEMA.into(),
                scope: Scope::InstalledHost,
                checks: Vec::new(),
                completed: false,
                cleanup: Cleanup::Pending,
            },
            deadline,
            owner,
        })
    }

    fn initialize(&mut self, paths: &LauncherPaths, store: &CertificateStore) -> Result<()> {
        store.remember_resources(&self.root, &[])?;
        for slot in 0..self.config.pool.slots {
            self.check_deadline()?;
            match launcher_install::acquire_identity_with_deadline(paths, slot, self.deadline) {
                Ok(lease) => {
                    self.leases.push(lease);
                    store.remember_resources(
                        &self.root,
                        &self
                            .leases
                            .iter()
                            .map(IdentityLease::identity)
                            .collect::<Vec<_>>(),
                    )?;
                }
                Err(
                    launcher_install::LauncherError::Occupied { .. }
                    | launcher_install::LauncherError::Poisoned { .. },
                ) => continue,
                Err(error) => return Err(error.into()),
            }
            if self.leases.len() == 3 {
                break;
            }
        }
        if self.leases.len() != 3 {
            return Err(CertificationError::Pending);
        }
        fs::create_dir(self.path("runtime"))?;
        mode(&self.path("runtime"), 0o755)?;
        fs::copy(&self.config.launcher_path, self.path("runtime/probe"))?;
        mode(&self.path("runtime/probe"), 0o755)?;
        fs::create_dir(self.path("sessions"))?;
        mode(&self.path("sessions"), 0o711)?;
        for args in [
            vec!["link", "set", "lo", "up"],
            vec!["addr", "replace", "198.18.0.1/32", "dev", "lo"],
            vec!["-6", "addr", "replace", "fd00:10::1/128", "dev", "lo"],
        ] {
            let output = launcher_install::run_signing_command(
                &CommandInvocation {
                    program: "/usr/sbin/ip".into(),
                    arguments: args.into_iter().map(Into::into).collect(),
                    stdin: Vec::new(),
                    current_dir: Some("/".into()),
                },
                self.deadline,
                None,
            )?;
            if !output.success {
                return Err(CertificationError::Unsupported);
            }
        }
        Ok(())
    }

    fn run(&mut self, store: &mut CertificateStore) -> Result<()> {
        let (check, cleanup) = super::guard_probe::run(
            self.path("runtime/probe"),
            self.root.clone(),
            self.identity(0)?.uid,
            self.identity(2)?.uid,
            self.deadline,
        );
        self.report.checks.push(check);
        if cleanup == Cleanup::Unconfirmed {
            self.report.cleanup = cleanup;
        }
        store.observe(&self.report)?;
        if cleanup == Cleanup::Unconfirmed {
            return Err(CertificationError::Invalid);
        }
        checks::filesystem(self, store)?;
        #[cfg(test)]
        if CANCEL_AFTER_FIRST_GROUP.with(|cancel| cancel.replace(false)) {
            let live = self.inside(&self.plan("cancellation", 0)?)?;
            self.request(live, &[])?;
            self.deadline = Instant::now();
        }
        checks::processes(self, store)?;
        endpoints::run(self, store)?;
        Ok(())
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }
    fn identity(&self, index: usize) -> Result<Identity> {
        self.leases
            .get(index)
            .map(IdentityLease::identity)
            .ok_or(CertificationError::Invalid)
    }
    fn plan(&self, name: &str, index: usize) -> Result<ConfinementPlan> {
        let identity = self.identity(index)?;
        Ok(ConfinementPlan {
            session_id: format!("cert-{name}-{}", std::process::id()),
            runtime_root: self.path("runtime"),
            executable: self.path("runtime/probe"),
            arguments: vec!["__conformance-probe".into()],
            environment: BTreeMap::new(),
            home: self.path(&format!("sessions/{name}/home")),
            workspace: self.path(&format!("sessions/{name}/workspace")),
            cache: None,
            beads_replica: None,
            system_roots: default_system_roots(),
            network: crate::registry::NetworkPolicy::Denied,
            identity: IdentityPlan::HostIdentity {
                uid: identity.uid,
                gid: identity.gid,
            },
            channels: vec![Channel::AcpStdio { id: "acp".into() }],
        })
    }
    fn outside(&mut self, uid: u32, gid: u32) -> Result<usize> {
        self.check_deadline()?;
        let child = Command::new(self.path("runtime/probe"))
            .arg("__conformance-probe")
            .env_clear()
            .env("LOUISELM_AMBIENT_SENTINEL", "hostile-ambient-sentinel")
            .uid(uid)
            .gid(gid)
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let index = self.agents.len();
        match Agent::plain(child) {
            Ok(agent) => self.agents.push(agent),
            Err(error) => {
                self.report.cleanup = Cleanup::Unconfirmed;
                return Err(error);
            }
        }
        Ok(index)
    }
    fn inside(&mut self, plan: &ConfinementPlan) -> Result<usize> {
        self.check_deadline()?;
        let session = self.backend.spawn(plan)?;
        let index = self.agents.len();
        match Agent::confined(session) {
            Ok(agent) => self.agents.push(agent),
            Err(error) => {
                self.report.cleanup = Cleanup::Unconfirmed;
                return Err(error);
            }
        }
        Ok(index)
    }
    fn request(&mut self, index: usize, probes: &[Probe]) -> Result<Vec<Observation>> {
        self.check_deadline()?;
        self.agents
            .get_mut(index)
            .ok_or(CertificationError::Invalid)?
            .request(probes, self.deadline)
    }
    fn paired(
        &mut self,
        store: &mut CertificateStore,
        probes: &[Probe],
        control: &[Observation],
        confined: &[Observation],
    ) -> Result<()> {
        let names: Vec<_> = probes.iter().map(|probe| probe.name.as_str()).collect();
        self.report.checks.extend(pair(&names, control, confined)?);
        store.observe(&self.report)?;
        Ok(())
    }
    fn record(
        &mut self,
        store: &mut CertificateStore,
        name: &str,
        control: Outcome,
        confined: Outcome,
    ) -> Result<()> {
        self.report.checks.push(Check {
            name: name.into(),
            control,
            confined,
        });
        store.observe(&self.report)
    }
    fn check_deadline(&self) -> Result<()> {
        if Instant::now() >= self.deadline {
            return Err(CertificationError::Io(std::io::ErrorKind::TimedOut.into()));
        }
        owner_live(&self.owner)
    }
    fn dispose(&mut self, index: usize) -> Result<()> {
        self.agents
            .get_mut(index)
            .ok_or(CertificationError::Invalid)?
            .dispose()
    }
    fn cleanup(&mut self) -> Result<()> {
        let mut clean = self.report.cleanup != Cleanup::Unconfirmed;
        #[cfg(test)]
        if FORCE_UNCONFIRMED_CLEANUP.with(|fail| fail.replace(false)) {
            clean = false;
        }
        for agent in &mut self.agents {
            clean &= agent.dispose().is_ok();
        }
        for service in &mut self.services {
            clean &= service.finish().is_ok();
        }
        if clean {
            if let Some(directory) = self.directory.take() {
                clean &= directory.close().is_ok();
            }
        } else if let Some(directory) = self.directory.take() {
            // Retain owned resources for inspection when disposal is unproven.
            let _retained_path = directory.keep();
        }
        for lease in self.leases.drain(..) {
            clean &= if clean {
                lease.release().is_ok()
            } else {
                lease.poison().is_ok()
            };
        }
        if clean {
            Ok(())
        } else {
            Err(CertificationError::Invalid)
        }
    }
}

fn owner_live(owner: &OwnedFd) -> Result<()> {
    use rustix::event::{PollFd, PollFlags, Timespec, poll};
    let mut descriptors = [PollFd::new(owner, PollFlags::IN)];
    poll(&mut descriptors, Some(&Timespec::default())).map_err(std::io::Error::from)?;
    if !descriptors[0].revents().is_empty() {
        return Err(CertificationError::Pending);
    }
    Ok(())
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Normal flow explicitly checks cleanup and records its result. This
        // fallback never clears the store's pending marker after an early error.
        let _ = self.cleanup();
    }
}

enum Process {
    Plain(Child),
    Confined(Box<SandboxedSession>),
}
struct Agent {
    process: Process,
    input: Option<ChildStdin>,
    output: Receiver<Vec<u8>>,
    reader: Option<JoinHandle<()>>,
    disposed: bool,
}

impl Agent {
    fn new(
        process: Process,
        input: ChildStdin,
        output: impl Read + Send + 'static,
    ) -> Result<Self> {
        let (sender, receiver) = mpsc::sync_channel(1);
        let mut agent = Self {
            process,
            input: Some(input),
            output: receiver,
            reader: None,
            disposed: false,
        };
        let reader = thread::Builder::new()
            .name("conformance-probe".into())
            .spawn(move || {
                let mut output = BufReader::new(output);
                loop {
                    let mut line = Vec::new();
                    if output
                        .by_ref()
                        .take(64 * 1024 + 1)
                        .read_until(b'\n', &mut line)
                        .is_err()
                        || line.is_empty()
                        || line.len() > 64 * 1024
                    {
                        break;
                    }
                    let Some(line) = line.strip_prefix(b"HOSTILE_JSON:") else {
                        break;
                    };
                    if sender.send(line.to_vec()).is_err() {
                        break;
                    }
                }
            })?;
        agent.reader = Some(reader);
        Ok(agent)
    }
    fn plain(mut child: Child) -> Result<Self> {
        let input = child.stdin.take().ok_or(CertificationError::Invalid)?;
        let output = child.stdout.take().ok_or(CertificationError::Invalid)?;
        Self::new(Process::Plain(child), input, output)
    }
    fn confined(mut session: SandboxedSession) -> Result<Self> {
        let input = session.take_stdin().ok_or(CertificationError::Invalid)?;
        let output = session.take_stdout().ok_or(CertificationError::Invalid)?;
        Self::new(Process::Confined(Box::new(session)), input, output)
    }
    fn request(&mut self, probes: &[Probe], deadline: Instant) -> Result<Vec<Observation>> {
        let input = self.input.as_mut().ok_or(CertificationError::Invalid)?;
        serde_json::to_writer(&mut *input, probes).map_err(|_| CertificationError::Invalid)?;
        input.write_all(b"\n")?;
        input.flush()?;
        let bytes = self
            .output
            .recv_timeout(
                deadline
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_secs(10)),
            )
            .map_err(|_| CertificationError::Unsupported)?;
        serde_json::from_slice(&bytes).map_err(|_| CertificationError::Invalid)
    }
    fn pid(&mut self) -> Result<u32> {
        match &mut self.process {
            Process::Plain(child) => Ok(child.id()),
            Process::Confined(session) => session
                .processes()?
                .into_iter()
                .find(|pid| {
                    fs::read_link(format!("/proc/{pid}/exe"))
                        .is_ok_and(|path| path.file_name().is_some_and(|name| name == "probe"))
                })
                .ok_or(CertificationError::Invalid),
        }
    }
    fn session(&mut self) -> Result<&mut SandboxedSession> {
        if let Process::Confined(session) = &mut self.process {
            Ok(session)
        } else {
            Err(CertificationError::Invalid)
        }
    }
    fn dispose(&mut self) -> Result<()> {
        if self.disposed {
            return Ok(());
        }
        self.input.take();
        match &mut self.process {
            Process::Plain(child) => {
                let group = rustix::process::Pid::from_child(child);
                match rustix::process::kill_process_group(group, rustix::process::Signal::KILL) {
                    Ok(()) | Err(rustix::io::Errno::SRCH) => (),
                    Err(error) => return Err(std::io::Error::from(error).into()),
                }
                child.wait()?;
                let deadline = Instant::now() + Duration::from_secs(3);
                loop {
                    match rustix::process::test_kill_process_group(group) {
                        Err(rustix::io::Errno::SRCH) => break,
                        Err(error) => return Err(std::io::Error::from(error).into()),
                        Ok(()) => (),
                    }
                    if Instant::now() >= deadline {
                        return Err(CertificationError::Invalid);
                    }
                    thread::sleep(Duration::from_millis(5));
                }
            }
            Process::Confined(session) => {
                let report = session.dispose()?;
                if report.survivors != 0 || !report.identity_released {
                    return Err(CertificationError::Invalid);
                }
            }
        }
        if let Some(reader) = self.reader.take() {
            reader.join().map_err(|_| CertificationError::Invalid)?;
        }
        self.disposed = true;
        Ok(())
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        // The Fixture explicitly checks normal cleanup. Construction/early
        // failure fallback never clears the durable store/lease poison markers.
        let _ = self.dispose();
    }
}

fn mode(path: &Path, value: u32) -> Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(value))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    #[allow(
        clippy::unwrap_used,
        reason = "Owned waitable process establishes pidfd liveness."
    )]
    fn lost_authorizing_process_is_not_live() {
        let mut child = std::process::Command::new("/bin/cat")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let owner = rustix::process::pidfd_open(
            rustix::process::Pid::from_child(&child),
            rustix::process::PidfdFlags::empty(),
        )
        .unwrap();
        assert!(super::owner_live(&owner).is_ok());
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(super::owner_live(&owner).is_err());
    }
}
