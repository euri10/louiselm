//! Filesystem, process and lifecycle checks using the fixed hostile inventory.

use super::{
    Attack, CertificateStore, CertificationError, Cleanup, Duration, File, Fixture, Instant,
    Outcome, Probe, Result, fs, mode, thread,
};
use std::{
    io::Read,
    os::{
        fd::{AsFd, AsRawFd},
        unix::{fs::symlink, net::UnixStream},
    },
};

pub(super) fn filesystem(fixture: &mut Fixture, store: &mut CertificateStore) -> Result<()> {
    let plan = fixture.plan("filesystem", 0)?;
    let mut prepared = fixture.backend.prepare(&plan)?;
    let setup = (|| {
        let mut probes = Vec::new();
        for name in [
            "operator-home",
            "operator-checkout",
            "operator-credentials",
            "native-provider-config",
            "native-mcp-config",
        ] {
            let path = fixture.path(name);
            fs::write(&path, b"harmless-sentinel")?;
            mode(&path, 0o644)?;
            probes.push(Probe::new(name, Attack::Read(path.clone())));
            let link = plan.workspace.join(format!("{name}-link"));
            symlink(path, &link)?;
            probes.push(Probe::new(&format!("symlink-{name}"), Attack::Read(link)));
        }
        for name in ["provider-config.json", "mcp.json", "adapter.js"] {
            let path = plan.runtime_root.join(name);
            fs::write(&path, b"immutable-config")?;
            mode(&path, 0o666)?;
            probes.push(Probe::new(
                &format!("runtime-write-{name}"),
                Attack::Write(path),
            ));
        }
        probes.push(Probe::new("ambient-environment", Attack::Environment));
        let identity = fixture.identity(0)?;
        let outside = fixture.outside(identity.uid, identity.gid)?;
        let allowed = fixture.request(outside, &probes)?;
        fixture.dispose(outside)?;
        Ok::<_, CertificationError>((probes, allowed))
    })();
    // Prepared resources are not ordinary Agents; explicitly own their cleanup
    // even when constructing a sentinel or outside control failed.
    if prepared.dispose().is_err() {
        fixture.report.cleanup = Cleanup::Unconfirmed;
        store.observe(&fixture.report)?;
        return Err(CertificationError::Invalid);
    }
    let (probes, allowed) = setup?;
    let inside = fixture.inside(&plan)?;
    let denied = fixture.request(inside, &probes)?;
    fixture.paired(store, &probes, &allowed, &denied)?;
    fixture.dispose(inside)?;
    inherited_descriptors(fixture, store)
}

fn inherited_descriptors(fixture: &mut Fixture, store: &mut CertificateStore) -> Result<()> {
    let fd_path = fixture.path("ambient-fd");
    fs::write(&fd_path, b"hostile-fd-sentinel")?;
    let file = File::open(&fd_path)?;
    let (socket, mut peer) = UnixStream::pair()?;
    peer.set_read_timeout(Some(Duration::from_secs(2)))?;
    for (name, fd) in [("file", file.as_fd()), ("socket", socket.as_fd())] {
        let inherited = rustix::io::fcntl_dupfd_cloexec(fd, 100).map_err(std::io::Error::from)?;
        rustix::io::fcntl_setfd(&inherited, rustix::io::FdFlags::empty())
            .map_err(std::io::Error::from)?;
        let attack = if name == "file" {
            Attack::FileDescriptor(inherited.as_raw_fd())
        } else {
            Attack::SocketDescriptor(inherited.as_raw_fd())
        };
        let probe_name = format!("inherited-{name}-fd");
        let probes = [Probe::new(&probe_name, attack)];
        let outside = fixture.outside(0, 0)?;
        let rows = fixture.request(outside, &probes)?;
        fixture.dispose(outside)?;
        let mut control = rows
            .first()
            .ok_or(CertificationError::Invalid)?
            .outcome
            .clone();
        if name == "socket" && control == Outcome::Allowed {
            let mut sent = [0; b"hostile-socket-sentinel".len()];
            if peer.read_exact(&mut sent).is_err() || &sent != b"hostile-socket-sentinel" {
                control = Outcome::Error("socket sentinel unavailable".into());
            }
        }
        let plan = fixture.plan(&format!("fd-{name}"), 0)?;
        let confined = match fixture.backend.prepare(&plan) {
            Ok(mut prepared) => {
                fixture.record(store, &probe_name, control, Outcome::Allowed)?;
                if prepared.dispose().is_err() {
                    fixture.report.cleanup = Cleanup::Unconfirmed;
                    store.observe(&fixture.report)?;
                }
                return Err(CertificationError::Invalid);
            }
            Err(error) if error.to_string().contains("Invalid argument") => {
                Outcome::Denied("bootstrap EINVAL".into())
            }
            Err(_) => Outcome::Error("bootstrap unavailable".into()),
        };
        drop(inherited);
        let clean = fixture.inside(&plan)?;
        if !fixture.request(clean, &[])?.is_empty() {
            return Err(CertificationError::Invalid);
        }
        fixture.dispose(clean)?;
        fixture.record(store, &probe_name, control, confined)?;
    }
    Ok(())
}

pub(super) fn processes(fixture: &mut Fixture, store: &mut CertificateStore) -> Result<()> {
    let operator = fixture.outside(fixture.config.operator_uid, fixture.config.operator_uid)?;
    let launcher = fixture.outside(0, 0)?;
    let identity = fixture.identity(2)?;
    let broker = fixture.outside(identity.uid, identity.gid)?;
    let first_plan = fixture.plan("first", 0)?;
    let second_plan = fixture.plan("second", 1)?;
    let first = fixture.inside(&first_plan)?;
    let second = fixture.inside(&second_plan)?;
    for index in [operator, launcher, broker, first, second] {
        fixture.request(index, &[])?;
    }
    let mut probes = Vec::new();
    for (name, index) in [
        ("operator", operator),
        ("launcher", launcher),
        ("broker", broker),
        ("second-session", second),
    ] {
        let pid = fixture.agents[index].pid()?;
        if pid <= 20 {
            return Err(CertificationError::Unsupported);
        }
        probes.extend(process_probes(name, pid));
    }
    let second_file = second_plan.home.join("private-sentinel");
    fs::write(&second_file, b"second-session-private")?;
    probes.push(Probe::new("second-session-file", Attack::Read(second_file)));
    let control = fixture.outside(0, 0)?;
    let allowed = fixture.request(control, &probes)?;
    let denied = fixture.request(first, &probes)?;
    fixture.paired(store, &probes, &allowed, &denied)?;
    let first_file = first_plan.home.join("private-sentinel");
    fs::write(&first_file, b"first-session-private")?;
    let mut reverse = process_probes("first-session", fixture.agents[first].pid()?);
    reverse.push(Probe::new("first-session-file", Attack::Read(first_file)));
    let allowed = fixture.request(control, &reverse)?;
    let denied = fixture.request(second, &reverse)?;
    fixture.paired(store, &reverse, &allowed, &denied)?;
    for index in [operator, launcher, broker, first, second, control] {
        fixture.request(index, &[])?;
        fixture.dispose(index)?;
    }
    lifecycle(fixture, store)
}

fn process_probes(name: &str, pid: u32) -> Vec<Probe> {
    vec![
        Probe::new(
            &format!("proc-{name}"),
            Attack::Read(format!("/proc/{pid}/status").into()),
        ),
        Probe::new(&format!("ptrace-{name}"), Attack::Ptrace(pid)),
        Probe::new(&format!("signal-{name}"), Attack::Signal(pid)),
    ]
}

fn wait_for(deadline: Instant, mut predicate: impl FnMut() -> bool) -> Result<()> {
    let deadline = deadline.min(Instant::now() + Duration::from_secs(3));
    while !predicate() {
        if Instant::now() >= deadline {
            return Err(CertificationError::Unsupported);
        }
        thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

fn lifecycle(fixture: &mut Fixture, store: &mut CertificateStore) -> Result<()> {
    let mut plan = fixture.plan("forking", 0)?;
    let heartbeat = plan.workspace.join("heartbeat");
    let foreground = plan.workspace.join("foreground");
    plan.executable = plan.runtime_root.join("fork-agent");
    fs::copy("/usr/bin/bash", &plan.executable)?;
    mode(&plan.executable, 0o755)?;
    plan.arguments = vec!["-c".into(), "( trap '' INT; while :; do printf x >> \"$HEARTBEAT\"; sleep .01; done ) &\nwhile :; do printf x >> \"$FOREGROUND\"; sleep .01; done\n".into()];
    plan.environment
        .insert("HEARTBEAT".into(), heartbeat.display().to_string());
    plan.environment
        .insert("FOREGROUND".into(), foreground.display().to_string());
    let agent = fixture.inside(&plan)?;
    wait_for(fixture.deadline, || {
        fs::metadata(&heartbeat).is_ok_and(|metadata| metadata.len() > 2)
    })?;
    wait_for(fixture.deadline, || {
        fs::metadata(&foreground).is_ok_and(|metadata| metadata.len() > 2)
    })?;
    if fixture.agents[agent].session()?.processes()?.len() < 4 {
        return Err(CertificationError::Unsupported);
    }
    fixture.agents[agent].session()?.park()?;
    let frozen = fs::read(&heartbeat)?;
    thread::sleep(Duration::from_millis(100));
    if fs::read(&heartbeat)? != frozen {
        fixture.record(store, "lifecycle", Outcome::Allowed, Outcome::Allowed)?;
        return Err(CertificationError::Invalid);
    }
    fixture.agents[agent].session()?.resume()?;
    wait_for(fixture.deadline, || {
        fs::read(&heartbeat).is_ok_and(|bytes| bytes.len() > frozen.len())
    })?;
    if fixture.agents[agent].session()?.interrupt()? == 0 {
        return Err(CertificationError::Unsupported);
    }
    thread::sleep(Duration::from_millis(150));
    let interrupted = fs::read(&foreground)?;
    thread::sleep(Duration::from_millis(100));
    if fs::read(&foreground)? != interrupted {
        fixture.record(store, "lifecycle", Outcome::Allowed, Outcome::Allowed)?;
        return Err(CertificationError::Invalid);
    }
    fixture.dispose(agent)?;
    fixture.record(
        store,
        "lifecycle",
        Outcome::Allowed,
        Outcome::Denied("observed Park/Resume/Interrupt and zero-survivor Disposal".into()),
    )
}
