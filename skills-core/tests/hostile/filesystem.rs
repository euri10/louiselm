//! Filesystem, native configuration and inherited-authority sentinels.

use super::{
    fixture::{Fixture, mode},
    probe::{Attack, Probe},
};
use std::{
    fs,
    io::Read,
    os::{
        fd::{AsFd, AsRawFd},
        unix::{fs::symlink, net::UnixStream},
    },
    time::Duration,
};

pub fn run(fixture: &Fixture) {
    let plan = fixture.plan("filesystem", 60000);
    // Preparation creates the real private roots; these links are controlled by
    // the Session, but their harmless targets live outside its mounts.
    let mut prepared = fixture.backend.prepare(&plan).unwrap();
    let mut probes = Vec::new();
    for name in [
        "operator-home",
        "operator-checkout",
        "operator-credentials",
        "native-provider-config",
        "native-mcp-config",
    ] {
        let path = fixture.path(name);
        fs::write(&path, b"harmless-sentinel").unwrap();
        mode(&path, 0o644);
        probes.push(Probe::new(name, Attack::Read(path.clone())));
        let link = plan.workspace.join(format!("{name}-link"));
        symlink(path, &link).unwrap();
        probes.push(Probe::new(&format!("symlink-{name}"), Attack::Read(link)));
    }
    for name in ["provider-config.json", "mcp.json", "adapter.js"] {
        let path = plan.runtime_root.join(name);
        fs::write(&path, b"immutable-config").unwrap();
        // Deliberately writable outside: the denial must come from the
        // read-only mount, not merely a root-owned file's DAC permissions.
        // Registry rejection of this mode is independently gated by ln30.
        mode(&path, 0o666);
        probes.push(Probe::new(
            &format!("runtime-write-{name}"),
            Attack::Write(path),
        ));
    }
    probes.push(Probe::new("ambient-environment", Attack::Environment));
    let mut outside = fixture.outside(60000);
    let allowed = outside.request(&probes);
    outside.dispose();
    let runtime_after_control: Vec<_> = ["provider-config.json", "mcp.json", "adapter.js"]
        .iter()
        .map(|name| fs::read(plan.runtime_root.join(name)).unwrap())
        .collect();
    prepared.dispose().unwrap();
    let mut inside = fixture.inside(&plan);
    fixture.check(&probes, &allowed, &inside.request(&probes));
    for (name, expected) in ["provider-config.json", "mcp.json", "adapter.js"]
        .iter()
        .zip(runtime_after_control)
    {
        assert_eq!(fs::read(plan.runtime_root.join(name)).unwrap(), expected);
    }
    inside.dispose();
    inherited_descriptors(fixture);
}

fn inherited_descriptors(fixture: &Fixture) {
    let fd_path = fixture.path("ambient-fd");
    fs::write(&fd_path, b"hostile-fd-sentinel").unwrap();
    let file = fs::File::open(&fd_path).unwrap();
    // Deliberately non-CLOEXEC: merely testing Rust's default close-on-exec
    // descriptors would not exercise the backend's undeclared-FD boundary.
    let (socket, mut peer) = UnixStream::pair().unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    for (name, fd) in [("file", file.as_fd()), ("socket", socket.as_fd())] {
        let inherited = rustix::io::fcntl_dupfd_cloexec(fd, 100).unwrap();
        rustix::io::fcntl_setfd(&inherited, rustix::io::FdFlags::empty()).unwrap();
        let attack = if name == "file" {
            Attack::FileDescriptor(inherited.as_raw_fd())
        } else {
            Attack::SocketDescriptor(inherited.as_raw_fd())
        };
        let probes = [Probe::new(&format!("inherited-{name}-fd"), attack)];
        let mut outside = fixture.outside(0);
        let rows = outside.request(&probes);
        assert!(
            matches!(rows[0].outcome, super::probe::Outcome::Allowed),
            "outside inherited {name}: {rows:?}"
        );
        outside.dispose();
        if name == "socket" {
            let mut sent = [0; b"hostile-socket-sentinel".len()];
            peer.read_exact(&mut sent).unwrap();
            assert_eq!(&sent, b"hostile-socket-sentinel");
        }
        let plan = fixture.plan(&format!("fd-{name}"), 60000);
        match fixture.backend.prepare(&plan) {
            Ok(mut prepared) => {
                prepared.dispose().unwrap();
                panic!("undeclared {name} FD admitted");
            }
            Err(error) => assert!(
                error.to_string().contains("Invalid argument"),
                "unexpected refusal: {error}"
            ),
        }
        drop(inherited);
        let mut clean = fixture.inside(&plan);
        assert!(
            clean.request(&[]).is_empty(),
            "same plan launches without ambient FD"
        );
        clean.dispose();
        fixture.record(
            &format!("inherited-{name}-fd"),
            "bootstrap EINVAL; clean launch passed",
        );
        println!(
            "inherited-{name}-fd: outside Allowed; bootstrap refused before exec; clean launch passed"
        );
    }
}
