//! Owned guest resources and bounded request/response exchange with the fake Agent.

use super::probe::{Observation, Probe};
use louiselm_skills::{
    conformance::{Check, Cleanup, Outcome, REPORT_SCHEMA, Report, ReportResult, Scope},
    registry::NetworkPolicy,
    sandbox::{
        Backend, BubblewrapBackend, Channel, ConfinementPlan, IdentityPlan, SandboxedSession,
        default_system_roots,
    },
};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    env, fs,
    io::{BufRead, BufReader, Read, Write},
    os::{
        fd::OwnedFd,
        unix::{fs::PermissionsExt, process::CommandExt},
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread::{self, JoinHandle},
    time::Duration,
};

pub struct Fixture {
    directory: tempfile::TempDir,
    checks: RefCell<Vec<Check>>,
    pub backend: BubblewrapBackend,
    pub operator: u32,
}

pub fn mode(path: &Path, value: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(value)).expect("fixture chmod succeeds");
}

impl Fixture {
    pub fn new() -> Self {
        assert!(
            rustix::process::geteuid().is_root(),
            "required conformance needs guest root"
        );
        assert_eq!(
            fs::read_to_string("/proc/self/uid_map")
                .unwrap()
                .split_whitespace()
                .collect::<Vec<_>>(),
            ["0", "0", "4294967295"],
            "required conformance needs the initial guest user namespace"
        );
        let operator: u32 = env::var("LOUISELM_TEST_OPERATOR_UID")
            .expect("operator identity required")
            .parse()
            .unwrap();
        assert!(operator > 0 && ![60000, 60001, 60002].contains(&operator));
        for id in [60000, 60001, 60002] {
            for database in ["passwd", "group"] {
                assert_eq!(
                    Command::new("getent")
                        .args([database, &id.to_string()])
                        .status()
                        .unwrap()
                        .code(),
                    Some(2),
                    "fixture uid/gid must be unallocated"
                );
            }
            for entry in fs::read_dir("/proc").unwrap().flatten() {
                let status = fs::read_to_string(entry.path().join("status")).unwrap_or_default();
                assert!(
                    !status.lines().any(|line| line.starts_with("Uid:")
                        && line
                            .split_whitespace()
                            .skip(1)
                            .any(|uid| uid == id.to_string())),
                    "fixture uid {id} already has a live process"
                );
            }
        }
        let backend = BubblewrapBackend::new()
            .with_bootstrap(Path::new(env!("CARGO_BIN_EXE_louiselm-launch")));
        assert!(
            backend.prerequisites().missing().is_empty(),
            "all kernel prerequisites required"
        );
        assert_eq!(
            env::var("LOUISELM_AMBIENT_SENTINEL").unwrap(),
            "hostile-ambient-sentinel"
        );
        let directory = tempfile::tempdir().unwrap();
        mode(directory.path(), 0o755);
        let runtime = directory.path().join("runtime");
        fs::create_dir(&runtime).unwrap();
        mode(&runtime, 0o755);
        fs::copy(env::current_exe().unwrap(), runtime.join("agent")).unwrap();
        mode(&runtime.join("agent"), 0o755);
        // An explicit private copy avoids depending on cargo output modes or
        // traversing an operator-private target directory after the UID drop.
        let bootstrap = directory.path().join("bootstrap");
        fs::copy(env!("CARGO_BIN_EXE_louiselm-launch"), &bootstrap).unwrap();
        mode(&bootstrap, 0o755);
        let backend = backend.with_bootstrap(&bootstrap);
        let sessions = directory.path().join("sessions");
        fs::create_dir(&sessions).unwrap();
        mode(&sessions, 0o711);
        Self {
            directory,
            checks: RefCell::new(Vec::new()),
            backend,
            operator,
        }
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.directory.path().join(name)
    }

    pub fn plan(&self, name: &str, uid: u32) -> ConfinementPlan {
        ConfinementPlan {
            session_id: format!("hostile-{name}-{}", std::process::id()),
            runtime_root: self.path("runtime"),
            executable: self.path("runtime/agent"),
            arguments: vec!["--exact".into(), "probe_child".into(), "--nocapture".into()],
            environment: BTreeMap::from([("LOUISELM_HOSTILE_CHILD".into(), "1".into())]),
            home: self.path(&format!("sessions/{name}/home")),
            workspace: self.path(&format!("sessions/{name}/workspace")),
            cache: None,
            beads_replica: None,
            system_roots: default_system_roots(),
            network: NetworkPolicy::Denied,
            identity: IdentityPlan::HostIdentity { uid, gid: uid },
            channels: vec![Channel::AcpStdio { id: "acp".into() }],
        }
    }

    pub fn outside(&self, uid: u32) -> Agent {
        let child = Command::new(self.path("runtime/agent"))
            .args(["--exact", "probe_child", "--nocapture"])
            .env("LOUISELM_HOSTILE_CHILD", "1")
            .uid(uid)
            .gid(uid)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        Agent::plain(child)
    }

    pub fn inside(&self, plan: &ConfinementPlan) -> Agent {
        Agent::confined(
            self.backend
                .spawn(plan)
                .expect("required HostIdentity launch succeeds"),
        )
    }

    pub fn check(&self, probes: &[Probe], outside: &[Observation], inside: &[Observation]) {
        for (context, rows) in [("outside", outside), ("inside", inside)] {
            for row in rows {
                println!("{context}: {} {:?}", row.name, row.outcome);
            }
        }
        let names: Vec<_> = probes.iter().map(|probe| probe.name.as_str()).collect();
        let checks = louiselm_skills::conformance::pair(&names, outside, inside).unwrap();
        assert!(
            checks.iter().all(|check| check.control == Outcome::Allowed
                && matches!(check.confined, Outcome::Denied(_))),
            "every named attack requires a positive control and an actual denial"
        );
        self.checks.borrow_mut().extend(checks);
    }

    pub fn record(&self, name: &str, detail: &str) {
        self.checks.borrow_mut().push(Check {
            name: name.into(),
            control: Outcome::Allowed,
            confined: Outcome::Denied(detail.into()),
        });
    }

    pub fn finish(self) {
        // All process/service owners have explicitly completed before this call.
        // Include private fixture removal in the success boundary too.
        self.directory.close().expect("owned fixture files removed");
        let report = Report {
            schema: REPORT_SCHEMA.into(),
            scope: Scope::DisposableGuest,
            checks: self.checks.into_inner(),
            completed: true,
            cleanup: Cleanup::Confirmed,
        };
        // This component matrix retains all 46 original attacks. The installed
        // certifier additionally runs the production Sender guard; this fixture
        // must not invent that observation to claim a complete host report.
        let mut actual: Vec<_> = report
            .checks
            .iter()
            .map(|check| check.name.as_str())
            .collect();
        let mut expected: Vec<_> = louiselm_skills::conformance::REQUIRED_CHECKS
            .iter()
            .copied()
            .filter(|name| *name != louiselm_skills::conformance::SENDER_GUARD_CHECK)
            .collect();
        actual.sort_unstable();
        expected.sort_unstable();
        assert_eq!(actual, expected);
        assert_eq!(report.result().unwrap(), ReportResult::Incomplete);
        println!(
            "HOSTILE_REPORT:{}",
            String::from_utf8(report.canonical_bytes().unwrap()).unwrap()
        );
    }
}

enum Process {
    Plain(Child),
    Confined(Box<SandboxedSession>),
}

pub struct Agent {
    process: Process,
    input: Option<fs::File>,
    output: Receiver<String>,
    reader: Option<JoinHandle<()>>,
}

impl Agent {
    fn new(process: Process, input: OwnedFd, output: impl Read + Send + 'static) -> Self {
        let (sender, receiver) = mpsc::channel();
        let reader = thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                let line = line.expect("Agent output reads");
                if let Some(json) = line.strip_prefix("HOSTILE_JSON:")
                    && sender.send(json.into()).is_err()
                {
                    break;
                }
            }
        });
        Self {
            process,
            input: Some(input.into()),
            output: receiver,
            reader: Some(reader),
        }
    }

    fn plain(mut child: Child) -> Self {
        let input = child.stdin.take().unwrap().into();
        let output = child.stdout.take().unwrap();
        Self::new(Process::Plain(child), input, output)
    }

    fn confined(mut session: SandboxedSession) -> Self {
        let input = session.take_stdin().unwrap().into();
        let output = session.take_stdout().unwrap();
        Self::new(Process::Confined(Box::new(session)), input, output)
    }

    pub fn request(&mut self, probes: &[Probe]) -> Vec<Observation> {
        let input = self.input.as_mut().unwrap();
        writeln!(input, "{}", serde_json::to_string(probes).unwrap()).unwrap();
        input.flush().unwrap();
        let json = self
            .output
            .recv_timeout(Duration::from_secs(10))
            .expect("Agent must answer within 10s; silence is not denial");
        serde_json::from_str(&json).expect("observations decode")
    }

    pub fn session(&mut self) -> &mut SandboxedSession {
        let Process::Confined(session) = &mut self.process else {
            panic!("confined fixture required")
        };
        session
    }

    pub fn pid(&mut self) -> u32 {
        match &mut self.process {
            Process::Plain(child) => child.id(),
            Process::Confined(session) => {
                // The fake Agent is the only process running this test executable.
                session
                    .processes()
                    .unwrap()
                    .into_iter()
                    .find(|pid| {
                        fs::read_link(format!("/proc/{pid}/exe"))
                            .is_ok_and(|path| path.file_name().is_some_and(|name| name == "agent"))
                    })
                    .expect("live workload PID is independently observed")
            }
        }
    }

    pub fn dispose(&mut self) {
        self.input.take();
        match &mut self.process {
            Process::Confined(session) => {
                let report = session
                    .dispose()
                    .expect("all confined descendants must be gone");
                assert_eq!(report.survivors, 0);
                assert!(report.identity_released);
            }
            Process::Plain(child) => {
                child.kill().unwrap();
                child.wait().unwrap();
            }
        }
        if let Some(reader) = self.reader.take() {
            reader.join().unwrap();
        }
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        if self.reader.is_some() {
            // During assertion unwinding the test already fails; still own cleanup.
            self.input.take();
            match &mut self.process {
                Process::Confined(session) => {
                    if let Err(error) = session.dispose() {
                        eprintln!("FAILED fixture cleanup: {error}");
                    }
                }
                Process::Plain(child) => {
                    let _ = child.kill();
                    if let Err(error) = child.wait() {
                        eprintln!("FAILED fixture reap: {error}");
                    }
                }
            }
            if let Some(reader) = self.reader.take() {
                let _ = reader.join();
            }
        }
    }
}
