//! The bubblewrap backend: real confinement, not a mock of one.
//!
//! These tests spawn real sandboxed processes through the system `bwrap` and
//! verify the kernel actually enforces what the backend claims — a file
//! outside the plan is unreachable, a frozen cgroup reports frozen, a
//! backgrounded grandchild the launcher never directly forked still dies on
//! disposal. Where a guarantee depends on a delegated cgroup v2 hierarchy
//! that not every environment has, the test skips with a printed reason
//! rather than asserting a fact the environment cannot back up.

mod support;

use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    os::unix::fs::PermissionsExt,
    path::Path,
    time::{Duration, Instant},
};

use louiselm_skills::{
    isolation::{CONTRACT_VERSION, Dimension},
    registry::NetworkPolicy,
    sandbox::{
        Backend, BubblewrapBackend, Cgroup, Channel, ConfinementPlan, IdentityPlan, SandboxError,
        SandboxedSession, default_system_roots,
    },
};
use support::{Fixture, write_file};

/// Reports whether `pid` is a real, scheduled process rather than a zombie.
///
/// A zombie stays in `cgroup.procs` until its parent reaps it, so `processes()`
/// alone cannot tell "the planned command is actually running" from "something
/// died the instant it started and nobody has called `wait` yet." Distinguishing
/// those is the whole point of this check: a broken launch that spawns nothing
/// must not be able to satisfy a test by leaving a corpse behind.
fn is_alive(pid: u32) -> bool {
    let stat = match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(text) => text,
        Err(_) => return false,
    };
    match stat.rsplit_once(')') {
        Some((_, rest)) => rest.trim_start().split(' ').next() != Some("Z"),
        None => false,
    }
}

fn any_alive(session: &SandboxedSession) -> bool {
    session.processes().iter().any(|&pid| is_alive(pid))
}

fn executable_script(path: &Path, script: &str) {
    write_file(path, script);
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("script is chmod +x");
}

/// Builds a minimal, valid confinement plan running `script` as the Session.
fn plan(fixture: &Fixture, id: &str, script: &str) -> ConfinementPlan {
    let runtime_root = fixture.path(&format!("{id}/runtime"));
    let agent = runtime_root.join("bin/agent");
    executable_script(&agent, script);

    ConfinementPlan {
        session_id: id.to_owned(),
        runtime_root,
        executable: agent,
        arguments: Vec::new(),
        environment: BTreeMap::new(),
        home: fixture.path(&format!("{id}/home")),
        workspace: fixture.path(&format!("{id}/workspace")),
        system_roots: default_system_roots(),
        network: NetworkPolicy::Denied,
        identity: IdentityPlan::NamespaceOnly,
        channels: vec![Channel::AcpStdio {
            id: "acp".to_owned(),
        }],
    }
}

/// Polls `condition` until it holds or `timeout` elapses.
fn wait_for(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if condition() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Every test that depends on cgroup lifecycle control calls this first.
///
/// A delegated cgroup v2 hierarchy is common on a systemd user session but
/// not guaranteed in every CI container. Skipping with a reason is honest;
/// asserting a fact the environment cannot back up is not.
fn cgroup_available() -> bool {
    if Cgroup::delegated_parent().is_some() {
        return true;
    }
    eprintln!("skipping: no delegated cgroup v2 hierarchy in this environment");
    false
}

#[test]
fn spawn_runs_the_planned_executable_and_reports_matching_evidence() {
    let fixture = Fixture::new();
    let confinement = plan(&fixture, "echo", "#!/bin/sh\necho sandboxed-marker\n");
    let backend = BubblewrapBackend::new();

    let mut session = backend
        .spawn(&confinement)
        .expect("bwrap starts the session");

    let mut stdout = String::new();
    session
        .take_stdout()
        .expect("stdout is piped")
        .read_to_string(&mut stdout)
        .expect("stdout reads to EOF");
    let exit_code = session.wait().expect("the session exits");
    assert_eq!(exit_code, 0);
    assert_eq!(stdout.trim(), "sandboxed-marker");

    let evidence = &session.evidence;
    assert_eq!(evidence.contract_version, CONTRACT_VERSION);
    assert_eq!(evidence.backend, "bubblewrap");
    let dimension = |wanted: Dimension| {
        evidence
            .dimensions
            .iter()
            .find(|found| found.dimension == wanted)
            .unwrap_or_else(|| panic!("{wanted:?} is covered"))
            .satisfied
    };
    assert!(dimension(Dimension::NetworkDenial));
    assert!(dimension(Dimension::ProcessSeparation));
    assert!(
        !dimension(Dimension::Identity),
        "an unprivileged launcher cannot grant a distinct host identity",
    );
    assert_eq!(
        dimension(Dimension::Lifecycle),
        Cgroup::delegated_parent().is_some(),
        "Lifecycle evidence must reflect whether a cgroup was actually available, not assume one",
    );

    session.dispose().expect("disposal succeeds");
}

#[test]
fn a_confined_session_cannot_reach_paths_outside_its_plan() {
    let fixture = Fixture::new();
    let secret = fixture.path("secret/outside.txt");
    write_file(&secret, "top-secret-marker\n");

    let mut confinement = plan(&fixture, "containment", "#!/bin/sh\ncat \"$SECRET\"\n");
    confinement
        .environment
        .insert("SECRET".to_owned(), secret.display().to_string());

    let backend = BubblewrapBackend::new();
    let mut session = backend
        .spawn(&confinement)
        .expect("bwrap starts the session");

    let mut stdout = String::new();
    session
        .take_stdout()
        .expect("stdout is piped")
        .read_to_string(&mut stdout)
        .expect("stdout reads to EOF");
    let mut stderr = String::new();
    session
        .take_stderr()
        .expect("stderr is piped")
        .read_to_string(&mut stderr)
        .expect("stderr reads to EOF");
    let exit_code = session.wait().expect("the session exits");

    assert_ne!(
        exit_code, 0,
        "cat must fail: the secret path was never bound into the sandbox",
    );
    assert!(
        !stdout.contains("top-secret-marker"),
        "the sandbox leaked a host file it was never given: {stdout:?}",
    );
    assert!(!stderr.is_empty(), "cat should explain why it failed");

    session.dispose().expect("disposal succeeds");
}

#[test]
fn park_freezes_the_whole_tree_and_resume_thaws_it() {
    if !cgroup_available() {
        return;
    }
    let fixture = Fixture::new();
    let confinement = plan(&fixture, "park", "#!/bin/sh\nexec sleep 30\n");
    let backend = BubblewrapBackend::new();
    let session = backend
        .spawn(&confinement)
        .expect("bwrap starts the session");

    assert!(
        wait_for(Duration::from_secs(2), || any_alive(&session)),
        "the planned process never actually started running",
    );
    assert!(!session.is_parked());
    session.park().expect("park freezes the tree");
    assert!(
        wait_for(Duration::from_secs(2), || session.is_parked()),
        "the cgroup never reported frozen",
    );

    session.resume().expect("resume thaws the tree");
    assert!(
        wait_for(Duration::from_secs(2), || !session.is_parked()),
        "the cgroup never reported thawed",
    );

    session.dispose().expect("disposal succeeds");
}

#[test]
fn interrupt_reaches_the_workload_and_dispose_still_reaches_zero_survivors() {
    if !cgroup_available() {
        return;
    }
    // A heartbeat file, not a pid count, is the oracle here: bwrap's own
    // process topology is an implementation detail (see the doc comment on
    // `interrupt`), but "the workload is still doing work" versus "it has
    // stopped" is exactly the observable behavior `interrupt` exists to
    // change, independent of how many supporting processes bwrap keeps
    // around underneath it.
    let fixture = Fixture::new();
    let heartbeat = fixture.path("interrupt/workspace/heartbeat");
    let mut confinement = plan(
        &fixture,
        "interrupt",
        "#!/bin/sh\ni=0\nwhile true; do\n  i=$((i + 1))\n  echo \"$i\" > \"$HEARTBEAT\"\n  sleep 0.05\ndone\n",
    );
    confinement
        .environment
        .insert("HEARTBEAT".to_owned(), heartbeat.display().to_string());
    let backend = BubblewrapBackend::new();
    let session = backend
        .spawn(&confinement)
        .expect("bwrap starts the session");

    let read_heartbeat = || -> u64 {
        fs::read_to_string(&heartbeat)
            .ok()
            .and_then(|text| text.trim().parse().ok())
            .unwrap_or(0)
    };

    assert!(
        wait_for(Duration::from_secs(2), || read_heartbeat() >= 2),
        "the workload never started counting",
    );

    let signaled = session.interrupt().expect("interrupt signals the tree");
    assert!(signaled > 0);

    std::thread::sleep(Duration::from_millis(400));
    let after_interrupt = read_heartbeat();
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(
        read_heartbeat(),
        after_interrupt,
        "the workload kept counting after SIGINT",
    );

    // `interrupt` does not promise zero survivors: bwrap's own namespace-init
    // helper is, by the kernel's own pid-namespace rules, immune to a
    // default-action signal it never explicitly handled; only
    // SIGKILL/SIGSTOP reach it, regardless of what runs inside the Session.
    // `dispose`, which uses the cgroup's unconditional kill, is what actually
    // guarantees zero survivors.
    let disposal = session.dispose().expect("disposal succeeds");
    assert_eq!(
        disposal.survivors, 0,
        "dispose, unlike interrupt, must guarantee zero survivors",
    );
}

#[test]
fn dispose_kills_a_grandchild_the_launcher_never_directly_forked() {
    if !cgroup_available() {
        return;
    }
    let fixture = Fixture::new();
    let confinement = plan(&fixture, "grandchild", "#!/bin/sh\nsleep 30 &\nwait\n");
    let backend = BubblewrapBackend::new();
    let session = backend
        .spawn(&confinement)
        .expect("bwrap starts the session");

    assert!(
        wait_for(Duration::from_secs(2), || session.processes().len() >= 2),
        "the backgrounded grandchild never joined the Session's cgroup",
    );

    let disposal = session.dispose().expect("disposal succeeds");
    assert!(
        disposal.processes_before >= 2,
        "expected the shell and its backgrounded child, got {}",
        disposal.processes_before,
    );
    assert_eq!(disposal.survivors, 0);
    assert!(disposal.identity_released);
}

#[test]
fn spawn_refuses_a_plan_with_anything_but_denied_network() {
    let fixture = Fixture::new();
    let mut confinement = plan(&fixture, "network", "#!/bin/sh\ntrue\n");
    confinement.network = NetworkPolicy::Brokered;
    let backend = BubblewrapBackend::new();

    let error = backend
        .spawn(&confinement)
        .expect_err("brokered egress belongs to the control service, not this backend");
    assert!(
        matches!(error, SandboxError::Refused(_)),
        "unexpected error: {error}",
    );
}

#[test]
fn a_missing_backend_program_is_reported_clearly() {
    let backend = BubblewrapBackend::at(Path::new("/definitely/not/a/real/bwrap"));

    let error = backend.version().expect_err("the program does not exist");
    assert!(
        matches!(
            error,
            SandboxError::BackendMissing {
                backend: "bubblewrap",
                ..
            }
        ),
        "unexpected error: {error}",
    );
}

#[test]
fn default_system_roots_are_fixed_and_do_not_come_from_the_plan() {
    let roots = default_system_roots();
    for expected in [
        "/usr",
        "/bin",
        "/sbin",
        "/lib",
        "/lib64",
        "/etc/alternatives",
    ] {
        assert!(
            roots.iter().any(|root| root == Path::new(expected)),
            "{expected} missing from default system roots",
        );
    }
}
