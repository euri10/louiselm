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
    env, fs,
    io::{BufRead, BufReader, Read, Write},
    os::unix::fs::PermissionsExt,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
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
    session
        .processes()
        .expect("cgroup membership is readable")
        .iter()
        .any(|&pid| is_alive(pid))
}

fn executable_script(path: &Path, script: &str) {
    write_file(path, script);
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("script is chmod +x");
}

/// Builds a minimal, valid confinement plan running `script` as the Session.
fn plan(fixture: &Fixture, id: &str, script: &str) -> ConfinementPlan {
    let runtimes_root = fixture.path("runtimes");
    let sessions_root = fixture.path("sessions");
    fs::create_dir_all(&sessions_root).expect("Sessions root is creatable");
    fs::set_permissions(&sessions_root, fs::Permissions::from_mode(0o711))
        .expect("Sessions root has its fixed mode");
    let runtime_root = runtimes_root.join(id);
    let agent = runtime_root.join("bin/agent");
    executable_script(&agent, script);
    for path in [&runtimes_root, &runtime_root, &runtime_root.join("bin")] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))
            .expect("the runtime fixture is traversable");
    }

    ConfinementPlan {
        session_id: id.to_owned(),
        runtime_root,
        executable: agent,
        arguments: Vec::new(),
        environment: BTreeMap::new(),
        home: sessions_root.join(id).join("home"),
        workspace: sessions_root.join(id).join("workspace"),
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
    let Some(parent) = Cgroup::delegated_parent() else {
        eprintln!("skipping: no delegated cgroup v2 hierarchy in this environment");
        return false;
    };
    static NEXT_PROBE: AtomicU64 = AtomicU64::new(0);
    let id = format!(
        "lifecycle-probe-{}-{}",
        std::process::id(),
        NEXT_PROBE.fetch_add(1, Ordering::Relaxed),
    );
    let cgroup = match Cgroup::create(&parent, &id) {
        Ok(cgroup) => cgroup,
        Err(error) => {
            eprintln!("skipping: cannot create a lifecycle probe cgroup: {error}");
            return false;
        }
    };
    let result = (|| {
        if !cgroup.processes()?.is_empty() {
            return Err(SandboxError::NoCgroup(
                "lifecycle probe cgroup is not empty".to_owned(),
            ));
        }
        cgroup.freeze()?;
        cgroup.thaw()?;
        cgroup.kill_all()
    })();
    let _ = cgroup.thaw();
    cgroup.remove();
    if let Err(error) = result {
        eprintln!("skipping: cgroup lifecycle controls are unusable: {error}");
        return false;
    }
    true
}

fn expect_spawn_error(
    backend: &BubblewrapBackend,
    confinement: &ConfinementPlan,
    message: &str,
) -> SandboxError {
    match backend.spawn(confinement) {
        Err(error) => error,
        Ok(mut session) => {
            let _ = session.dispose();
            panic!("{message}");
        }
    }
}

fn process_status_values(pid: u32, field: &str) -> Vec<u32> {
    fs::read_to_string(format!("/proc/{pid}/status"))
        .unwrap_or_else(|error| panic!("process {pid} status is readable: {error}"))
        .lines()
        .find_map(|line| line.strip_prefix(field))
        .unwrap_or_else(|| panic!("process {pid} status contains {field}"))
        .split_whitespace()
        .map(|value| {
            value
                .parse()
                .unwrap_or_else(|error| panic!("{field} value {value:?} is numeric: {error}"))
        })
        .collect()
}

fn effective_uid() -> u32 {
    process_status_values(std::process::id(), "Uid:")[1]
}

fn is_mapped_root() -> bool {
    effective_uid() == 0
        && id_map(std::process::id(), "uid_map")
            .iter()
            .any(|[namespace_id, outer_id, _]| *namespace_id == 0 && *outer_id != 0)
}

fn is_initial_user_namespace() -> bool {
    id_map(std::process::id(), "uid_map") == [[0, 0, u32::MAX]]
}

fn host_identity_test_id() -> Option<u32> {
    if effective_uid() != 0 {
        return None;
    }
    match env::var("LOUISELM_TEST_HOST_ID") {
        Ok(value) => Some(
            value
                .parse()
                .expect("LOUISELM_TEST_HOST_ID is a numeric non-root uid/gid"),
        ),
        Err(_) if is_mapped_root() => Some(12_345),
        Err(_) => None,
    }
}

fn unique_id(prefix: &str) -> String {
    format!("{prefix}-{}", std::process::id())
}

fn id_map(pid: u32, name: &str) -> Vec<[u32; 3]> {
    fs::read_to_string(format!("/proc/{pid}/{name}"))
        .unwrap_or_else(|error| panic!("process {pid} {name} is readable: {error}"))
        .lines()
        .map(|line| {
            let values = line
                .split_whitespace()
                .map(|value| value.parse().expect("id-map values are numeric"))
                .collect::<Vec<_>>();
            values
                .try_into()
                .unwrap_or_else(|values: Vec<u32>| panic!("id-map row has 3 values: {values:?}"))
        })
        .collect()
}

fn assert_maps_assigned_identity(pid: u32, name: &str, host_id: u32) {
    let map = id_map(pid, name);
    assert_eq!(
        map,
        vec![[host_id, host_id, 1]],
        "process {pid} must map its assigned namespace identity to the same host identity in {name}",
    );
}

#[test]
fn spawn_runs_the_planned_executable_and_reports_matching_evidence() {
    let lifecycle_available = cgroup_available();
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
        lifecycle_available,
        "Lifecycle evidence must match usable cgroup freeze-and-kill controls",
    );

    session.dispose().expect("disposal succeeds");
}

#[test]
fn prepare_blocks_the_workload_until_start() {
    let fixture = Fixture::new();
    let marker = fixture.path("sessions/prepared/workspace/started");
    let mut confinement = plan(
        &fixture,
        "prepared",
        "#!/bin/sh\ntouch \"$STARTED\"\nsleep 30\n",
    );
    confinement
        .environment
        .insert("STARTED".to_owned(), marker.display().to_string());
    let backend = BubblewrapBackend::new();

    let prepared = backend
        .prepare(&confinement)
        .expect("bwrap prepares the session");
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        !marker.exists(),
        "the workload must remain blocked before start",
    );

    let mut session = prepared.start().expect("the prepared session starts");
    assert!(
        wait_for(Duration::from_secs(2), || marker.exists()),
        "the workload never ran after start",
    );
    assert_eq!(
        session.try_wait().expect("running status is readable"),
        None,
        "a nonblocking status check leaves the running Session alone",
    );
    session.dispose().expect("disposal succeeds");
}

#[test]
fn prepared_and_running_sessions_expose_their_dynamic_process_tree() {
    if !cgroup_available() {
        return;
    }
    let fixture = Fixture::new();
    let confinement = plan(
        &fixture,
        "process-tree-handle",
        "#!/bin/sh\nexec sleep 30\n",
    );
    let backend = BubblewrapBackend::new();

    let prepared = backend
        .prepare(&confinement)
        .expect("bwrap prepares the session");
    let member_pid = *prepared
        .processes()
        .expect("prepared membership is readable")
        .first()
        .expect("the prepared Session has a cgroup member");
    let process_tree = prepared
        .process_tree()
        .expect("a lifecycle-backed Session exposes its process tree");
    let cloned_tree = process_tree.clone();
    assert!(
        process_tree
            .contains(member_pid)
            .expect("process-tree membership is readable"),
        "the prepared process belongs to the Session tree",
    );
    assert_eq!(
        process_tree
            .processes()
            .expect("process-tree membership is readable"),
        cloned_tree
            .processes()
            .expect("a cloned handle reads the same live tree"),
    );

    let mut session = prepared.start().expect("the prepared session starts");
    assert!(
        session
            .process_tree()
            .expect("the running Session retains its process tree")
            .contains(member_pid)
            .expect("running membership is readable"),
    );
    session.dispose().expect("disposal succeeds");
}

#[test]
fn disposing_a_prepared_session_never_runs_its_workload() {
    let fixture = Fixture::new();
    let marker = fixture.path("sessions/dispose-prepared/workspace/started");
    let mut confinement = plan(
        &fixture,
        "dispose-prepared",
        "#!/bin/sh\ntouch \"$STARTED\"\nsleep 30\n",
    );
    confinement
        .environment
        .insert("STARTED".to_owned(), marker.display().to_string());
    let backend = BubblewrapBackend::new();

    let mut prepared = backend
        .prepare(&confinement)
        .expect("bwrap prepares the session");
    let monitor_pid = prepared.monitor_pid();
    let disposal = prepared.dispose().expect("prepared disposal succeeds");

    assert_eq!(disposal.survivors, 0);
    assert!(
        !Path::new(&format!("/proc/{monitor_pid}")).exists(),
        "prepared disposal reaps the Bubblewrap monitor",
    );
    assert!(!marker.exists(), "disposed workload must never run");
}

#[test]
fn dropping_a_prepared_session_kills_it_without_releasing_the_gate() {
    let fixture = Fixture::new();
    let marker = fixture.path("sessions/drop-prepared/workspace/started");
    let mut confinement = plan(
        &fixture,
        "drop-prepared",
        "#!/bin/sh\ntouch \"$STARTED\"\nsleep 30\n",
    );
    confinement
        .environment
        .insert("STARTED".to_owned(), marker.display().to_string());
    let backend = BubblewrapBackend::new();

    let prepared = backend
        .prepare(&confinement)
        .expect("bwrap prepares the session");
    let monitor_pid = prepared.monitor_pid();
    let enclosed = prepared
        .processes()
        .expect("prepared cgroup membership is readable");
    drop(prepared);

    assert!(
        !Path::new(&format!("/proc/{monitor_pid}")).exists(),
        "dropping a prepared Session reaps the Bubblewrap monitor",
    );
    for pid in enclosed {
        assert!(
            !Path::new(&format!("/proc/{pid}")).exists(),
            "dropping kills enclosed process {pid}",
        );
    }
    assert!(!marker.exists(), "dropping must not release the workload");
}

#[test]
fn sandboxed_stdin_can_be_taken_for_an_owned_relay() {
    let fixture = Fixture::new();
    let confinement = plan(
        &fixture,
        "take-stdin",
        "#!/bin/sh\nIFS= read -r line\nprintf '%s\\n' \"$line\"\n",
    );
    let backend = BubblewrapBackend::new();
    let mut session = backend
        .spawn(&confinement)
        .expect("bwrap starts the session");

    session
        .take_stdin()
        .expect("stdin is piped")
        .write_all(b"relayed input\n")
        .expect("stdin accepts relayed input");
    let mut stdout = String::new();
    session
        .take_stdout()
        .expect("stdout is piped")
        .read_to_string(&mut stdout)
        .expect("stdout reads to EOF");

    assert_eq!(session.wait().expect("the session exits"), 0);
    assert_eq!(stdout, "relayed input\n");
    session.dispose().expect("disposal succeeds");
}

#[test]
fn host_identity_is_never_inferred_from_an_unprivileged_plan() {
    if effective_uid() == 0 {
        eprintln!("skipping: this regression exercises an unprivileged launcher");
        return;
    }
    let fixture = Fixture::new();
    let mut confinement = plan(&fixture, "unprivileged-identity", "#!/bin/sh\ntrue\n");
    confinement.identity = IdentityPlan::HostIdentity {
        uid: effective_uid(),
        gid: process_status_values(std::process::id(), "Gid:")[1],
    };
    let backend = BubblewrapBackend::new();

    let error = expect_spawn_error(
        &backend,
        &confinement,
        "a non-root launcher cannot establish a distinct host identity",
    );
    assert!(matches!(error, SandboxError::Refused(_)));
}

#[test]
fn host_identity_refuses_a_root_uid_or_gid() {
    if effective_uid() != 0 {
        eprintln!("skipping: this regression requires a root launcher");
        return;
    }
    let session_identity = host_identity_test_id().unwrap_or(12_345);
    let fixture = Fixture::new();
    let mut confinement = plan(&fixture, "root-identity", "#!/bin/sh\ntrue\n");
    let backend = BubblewrapBackend::new();

    for identity in [
        IdentityPlan::HostIdentity {
            uid: 0,
            gid: session_identity,
        },
        IdentityPlan::HostIdentity {
            uid: session_identity,
            gid: 0,
        },
    ] {
        confinement.identity = identity;
        let error = expect_spawn_error(
            &backend,
            &confinement,
            "a root Session identity is inadmissible",
        );
        assert!(matches!(error, SandboxError::Refused(_)));
    }
}

#[test]
fn host_identity_changes_outer_credentials_and_owns_private_directories() {
    let Some(session_identity) = host_identity_test_id() else {
        eprintln!("skipping: run in a mapped root namespace or set LOUISELM_TEST_HOST_ID as root");
        return;
    };
    assert_ne!(session_identity, 0, "the test identity must be non-root");
    let require_initial_host = env::var_os("LOUISELM_REQUIRE_INITIAL_HOST_IDENTITY").is_some();
    if require_initial_host {
        assert!(
            is_initial_user_namespace(),
            "the privileged acceptance must run in the initial user namespace",
        );
    }
    let fixture = Fixture::new();
    fs::set_permissions(fixture.path(""), fs::Permissions::from_mode(0o755))
        .expect("the assigned identity can traverse the fixture");
    let session_id = unique_id("host-identity");
    let home_probe = fixture.path(&format!("sessions/{session_id}/home/created"));
    let workspace_probe = fixture.path(&format!("sessions/{session_id}/workspace/created"));
    let mut confinement = plan(
        &fixture,
        &session_id,
        "#!/bin/sh\ntouch \"$HOME_PROBE\" \"$WORKSPACE_PROBE\"\nid -u\nid -g\nid -G\nsleep 30\n",
    );
    confinement.identity = IdentityPlan::HostIdentity {
        uid: session_identity,
        gid: session_identity,
    };
    confinement
        .environment
        .insert("HOME_PROBE".to_owned(), home_probe.display().to_string());
    confinement.environment.insert(
        "WORKSPACE_PROBE".to_owned(),
        workspace_probe.display().to_string(),
    );
    let backend = BubblewrapBackend::new();

    let mut session = backend
        .spawn(&confinement)
        .expect("a privileged launcher applies the assigned identity");
    let mut stdout = BufReader::new(session.take_stdout().expect("stdout is piped")).lines();
    let expected_identity = session_identity.to_string();
    assert_eq!(
        stdout.next().transpose().expect("uid reads").as_deref(),
        Some(expected_identity.as_str())
    );
    assert_eq!(
        stdout.next().transpose().expect("gid reads").as_deref(),
        Some(expected_identity.as_str())
    );
    assert_eq!(
        stdout.next().transpose().expect("groups read").as_deref(),
        Some(expected_identity.as_str()),
        "the Session must inherit no supplementary groups",
    );

    let outer_uids = process_status_values(session.monitor_pid(), "Uid:");
    let outer_gids = process_status_values(session.monitor_pid(), "Gid:");
    assert!(outer_uids.iter().all(|&uid| uid == session_identity));
    assert!(outer_gids.iter().all(|&gid| gid == session_identity));
    assert!(process_status_values(session.monitor_pid(), "Groups:").is_empty());
    let sandbox_leader_pid = session
        .sandbox_leader_pid()
        .expect("Bubblewrap reports its host-view sandbox leader");
    assert_ne!(
        sandbox_leader_pid,
        session.monitor_pid(),
        "the reported sandbox leader is distinct from the launcher-side bwrap process",
    );
    assert!(
        process_status_values(sandbox_leader_pid, "Uid:")
            .iter()
            .all(|&uid| uid == session_identity),
    );
    assert!(
        process_status_values(sandbox_leader_pid, "Gid:")
            .iter()
            .all(|&gid| gid == session_identity),
    );
    assert!(process_status_values(sandbox_leader_pid, "Groups:").is_empty());
    assert_maps_assigned_identity(sandbox_leader_pid, "uid_map", session_identity);
    assert_maps_assigned_identity(sandbox_leader_pid, "gid_map", session_identity);
    assert!(
        session
            .processes()
            .expect("cgroup membership is readable")
            .contains(&sandbox_leader_pid),
    );
    for private in [&confinement.home, &confinement.workspace] {
        assert_eq!(
            fs::metadata(private)
                .expect("private directory exists")
                .permissions()
                .mode()
                & 0o777,
            0o700,
            "private directories have explicit modes independent of umask",
        );
    }
    assert_eq!(
        fs::metadata(confinement.home.parent().expect("home has a Session root"),)
            .expect("Session root exists")
            .permissions()
            .mode()
            & 0o777,
        0o711,
        "the Session root is traversable but not listable",
    );
    assert!(home_probe.is_file(), "the assigned identity owns its home");
    assert!(
        workspace_probe.is_file(),
        "the assigned identity owns its workspace",
    );

    let identity = session
        .evidence
        .dimensions
        .iter()
        .find(|evidence| evidence.dimension == Dimension::Identity)
        .expect("identity is covered");
    if require_initial_host {
        assert!(
            identity.satisfied,
            "initial-host observations establish identity evidence",
        );
    }
    assert_eq!(
        identity.satisfied,
        is_initial_user_namespace(),
        "mapped-root observations prove mechanics, not initial-host identity",
    );

    session.dispose().expect("disposal succeeds");
}

#[test]
fn host_identity_refuses_an_existing_session_root_with_the_wrong_mode() {
    let Some(session_identity) = host_identity_test_id() else {
        eprintln!("skipping: run in a mapped root namespace or set LOUISELM_TEST_HOST_ID as root");
        return;
    };
    if !cgroup_available() {
        eprintln!("skipping: this regression requires a writable cgroup");
        return;
    }
    let fixture = Fixture::new();
    fs::set_permissions(fixture.path(""), fs::Permissions::from_mode(0o755))
        .expect("the assigned identity can traverse the fixture");
    let session_id = unique_id("wrong-session-root");
    let mut confinement = plan(&fixture, &session_id, "#!/bin/sh\ntrue\n");
    let session_root = confinement.home.parent().expect("home has a Session root");
    fs::create_dir(session_root).expect("the Session root is creatable");
    fs::set_permissions(session_root, fs::Permissions::from_mode(0o700))
        .expect("the wrong mode is installed");
    confinement.identity = IdentityPlan::HostIdentity {
        uid: session_identity,
        gid: session_identity,
    };
    let backend = BubblewrapBackend::new();

    let error = expect_spawn_error(
        &backend,
        &confinement,
        "an existing wrong-mode Session root must not be rewritten",
    );

    assert!(
        error.to_string().contains("existing Session root"),
        "unexpected refusal: {error}",
    );
    assert_eq!(
        fs::metadata(session_root)
            .expect("Session root remains")
            .permissions()
            .mode()
            & 0o777,
        0o700,
        "refusal does not chmod a caller-owned existing directory",
    );
}

#[test]
fn plausible_but_unverified_host_identity_status_fails_closed() {
    let Some(session_identity) = host_identity_test_id() else {
        eprintln!("skipping: run in a mapped root namespace or set LOUISELM_TEST_HOST_ID as root");
        return;
    };
    if !cgroup_available() {
        eprintln!("skipping: this regression requires a writable cgroup");
        return;
    }
    let fixture = Fixture::new();
    fs::set_permissions(fixture.path(""), fs::Permissions::from_mode(0o755))
        .expect("the assigned identity can traverse the fixture");
    let fake_bwrap = fixture.path("fake-bwrap");
    executable_script(
        &fake_bwrap,
        r#"#!/bin/bash
status_fd=
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--json-status-fd" ]; then
    status_fd="$2"
    break
  fi
  shift
done
eval "exec 9>&$status_fd"
printf '{"child-pid":%s}\n' "$$" >&9
sleep 30
"#,
    );
    let session_id = unique_id("bad-identity-status");
    let mut confinement = plan(&fixture, &session_id, "#!/bin/sh\ntrue\n");
    confinement.identity = IdentityPlan::HostIdentity {
        uid: session_identity,
        gid: session_identity,
    };
    let cgroup_path = Cgroup::delegated_parent()
        .expect("a delegated cgroup exists")
        .join(format!("louiselm-session-{session_id}"));
    let backend = BubblewrapBackend::at(&fake_bwrap);

    let error = expect_spawn_error(
        &backend,
        &confinement,
        "a parseable status claim is not identity evidence by itself",
    );

    assert!(
        matches!(error, SandboxError::SpawnFailed { .. }),
        "unexpected error: {error}",
    );
    assert!(
        error.to_string().contains("unexpected parent"),
        "independent process verification remains visible: {error}",
    );
    assert!(
        !cgroup_path.exists(),
        "failed startup must kill its process and remove its cgroup",
    );
}

#[test]
fn rewritten_host_identity_flags_cannot_pass_transitional_map_evidence() {
    let Some(session_identity) = host_identity_test_id() else {
        eprintln!("skipping: run in a mapped root namespace or set LOUISELM_TEST_HOST_ID as root");
        return;
    };
    if !cgroup_available() {
        eprintln!("skipping: this regression requires a writable cgroup");
        return;
    }
    let fixture = Fixture::new();
    fs::set_permissions(fixture.path(""), fs::Permissions::from_mode(0o755))
        .expect("the assigned identity can traverse the fixture");
    let wrapper = fixture.path("rewrite-identity-flags");
    executable_script(
        &wrapper,
        r#"#!/bin/bash
args=()
while (($#)); do
  case "$1" in
    --uid|--gid) args+=("$1" "0"); shift 2 ;;
    *) args+=("$1"); shift ;;
  esac
done
exec /usr/bin/bwrap "${args[@]}"
"#,
    );
    let session_id = unique_id("rewritten-identity");
    let workload_marker =
        fixture.path(&format!("sessions/{session_id}/workspace/workload-started"));
    let mut confinement = plan(
        &fixture,
        &session_id,
        "#!/bin/sh\ntouch \"$WORKLOAD_MARKER\"\nsleep 30\n",
    );
    confinement.identity = IdentityPlan::HostIdentity {
        uid: session_identity,
        gid: session_identity,
    };
    confinement.environment.insert(
        "WORKLOAD_MARKER".to_owned(),
        workload_marker.display().to_string(),
    );
    let cgroup_path = Cgroup::delegated_parent()
        .expect("a delegated cgroup exists")
        .join(format!("louiselm-session-{session_id}"));
    let backend = BubblewrapBackend::at(&wrapper);

    let error = expect_spawn_error(
        &backend,
        &confinement,
        "namespace-local root is not the assigned inside identity",
    );

    assert!(
        error
            .to_string()
            .contains("did not reach the assigned namespace-to-host mapping"),
        "the final id-map failure remains visible: {error}",
    );
    assert!(
        !cgroup_path.exists(),
        "failed startup must kill its process and remove its cgroup",
    );
    assert!(
        !workload_marker.exists(),
        "the workload stays blocked until final identity evidence passes",
    );
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
    let mut session = backend
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
    let heartbeat = fixture.path("sessions/interrupt/workspace/heartbeat");
    let mut confinement = plan(
        &fixture,
        "interrupt",
        "#!/bin/sh\ni=0\nwhile true; do\n  i=$((i + 1))\n  echo \"$i\" > \"$HEARTBEAT\"\n  sleep 0.05\ndone\n",
    );
    confinement
        .environment
        .insert("HEARTBEAT".to_owned(), heartbeat.display().to_string());
    let backend = BubblewrapBackend::new();
    let mut session = backend
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
    let mut session = backend
        .spawn(&confinement)
        .expect("bwrap starts the session");

    assert!(
        wait_for(Duration::from_secs(2), || {
            session
                .processes()
                .expect("cgroup membership is readable")
                .len()
                >= 2
        }),
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
