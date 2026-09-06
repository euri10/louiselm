//! Real endpoints with harmless service-execution doubles, not desktop daemons.

use super::{
    fixture::{Fixture, check, mode},
    probe::{Attack, Endpoint, Outcome, Probe},
};
use louiselm_skills::sandbox::Channel;
use std::{
    fs,
    io::{self, Read, Write},
    net::{IpAddr, TcpListener},
    os::unix::{net::UnixListener, process::CommandExt},
    path::PathBuf,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

struct Service {
    stop: Arc<AtomicBool>,
    executions: Arc<AtomicUsize>,
    worker: Option<JoinHandle<()>>,
}

impl Service {
    fn unix(endpoint: &Endpoint, request: &str, marker: PathBuf, uid: u32) -> Self {
        let listener = UnixListener::bind_addr(&endpoint.address().unwrap()).unwrap();
        if let Endpoint::Path(path) = endpoint {
            mode(path, 0o666);
        }
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let executions = Arc::new(AtomicUsize::new(0));
        let stopping = stop.clone();
        let count = executions.clone();
        let request = request.to_owned();
        let worker = thread::spawn(move || {
            while !stopping.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        stream
                            .set_write_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        let mut bytes = vec![0; request.len()];
                        stream.read_exact(&mut bytes).unwrap();
                        assert_eq!(
                            bytes,
                            request.as_bytes(),
                            "only the fixed service request is accepted"
                        );
                        // Model the confused-deputy consequence: the service
                        // executes OUTSIDE containment under its own identity.
                        // No supplied executable or command is evaluated.
                        let status = Command::new("/bin/sh")
                            .args(["-c", "printf service-executed > \"$1\"", "sentinel"])
                            .arg(&marker)
                            .uid(uid)
                            .gid(uid)
                            .status()
                            .unwrap();
                        assert!(status.success());
                        assert_eq!(fs::read(&marker).unwrap(), b"service-executed");
                        count.fetch_add(1, Ordering::Release);
                        stream.write_all(b"executed\n").unwrap();
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("service accept failed: {error}"),
                }
            }
        });
        Self {
            stop,
            executions,
            worker: Some(worker),
        }
    }

    fn tcp(address: IpAddr) -> (Self, Attack) {
        let listener =
            TcpListener::bind((address, 0)).expect("real guest-local IPv4/IPv6 endpoint required");
        let attack = Attack::Tcp(listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let executions = Arc::new(AtomicUsize::new(0));
        let stopping = stop.clone();
        let count = executions.clone();
        let worker = thread::spawn(move || {
            while !stopping.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        stream
                            .set_write_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        let mut bytes = [0; b"local-network-sentinel\n".len()];
                        stream.read_exact(&mut bytes).unwrap();
                        assert_eq!(&bytes, b"local-network-sentinel\n");
                        count.fetch_add(1, Ordering::Release);
                        stream.write_all(b"executed\n").unwrap();
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("network sentinel accept failed: {error}"),
                }
            }
        });
        (
            Self {
                stop,
                executions,
                worker: Some(worker),
            },
            attack,
        )
    }

    fn count(&self) -> usize {
        self.executions.load(Ordering::Acquire)
    }

    fn finish(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.worker
            .take()
            .unwrap()
            .join()
            .expect("sentinel worker completed without failure");
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub fn run(fixture: &Fixture) {
    let markers = fixture.path("service-markers");
    fs::create_dir(&markers).unwrap();
    mode(&markers, 0o777);
    let mut services = Vec::new();
    let mut probes = Vec::new();
    // These are service-shaped doubles, not implementations of the D-Bus or
    // Docker protocols. The production assertion is socket denial BEFORE any
    // such service can execute for the attacker.
    for (name, request) in [
        ("pathname-unix", "execute-fixed-sentinel\n"),
        (
            "dbus-systemd",
            "org.freedesktop.systemd1.Manager.StartTransientUnit:sentinel\n",
        ),
        (
            "docker",
            "POST /containers/create HTTP/1.1\r\nContent-Length: 0\r\n\r\n",
        ),
    ] {
        let endpoint = Endpoint::Path(fixture.path(&format!("{name}.sock")));
        services.push(Service::unix(
            &endpoint,
            request,
            markers.join(name),
            fixture.operator,
        ));
        probes.push(Probe::new(name, Attack::Unix(endpoint, request.into())));
    }
    let endpoint = Endpoint::Abstract(format!("louiselm-hostile-{}", std::process::id()));
    services.push(Service::unix(
        &endpoint,
        "execute-fixed-sentinel\n",
        markers.join("abstract"),
        fixture.operator,
    ));
    probes.push(Probe::new(
        "abstract-unix",
        Attack::Unix(endpoint, "execute-fixed-sentinel\n".into()),
    ));
    // The gate creates these private local addresses in a throwaway NET
    // namespace. No dependence on the guest NIC or external network reachability.
    let ipv4: IpAddr = "198.18.0.1".parse().unwrap();
    let ipv6: IpAddr = "fd00:10::1".parse().unwrap();
    for (name, address) in [
        ("ipv4", ipv4),
        ("ipv6", ipv6),
        ("ipv4-loopback", "127.0.0.1".parse().unwrap()),
        ("ipv6-loopback", "::1".parse().unwrap()),
    ] {
        let (service, attack) = Service::tcp(address);
        services.push(service);
        probes.push(Probe::new(name, attack));
    }
    let mut outside = fixture.outside(60000);
    let mut inside = fixture.inside(&fixture.plan("sockets", 60000));
    let allowed = outside.request(&probes);
    assert!(
        services.iter().all(|service| service.count() == 1),
        "every positive exchange reached its live service"
    );
    let denied = inside.request(&probes);
    check(&probes, &allowed, &denied);
    assert!(
        services.iter().all(|service| service.count() == 1),
        "no confined service-mediated execution"
    );
    let after = outside.request(&probes);
    assert!(
        after
            .iter()
            .all(|row| matches!(row.outcome, Outcome::Allowed)),
        "targets remain live after denied attempts"
    );
    assert!(services.iter().all(|service| service.count() == 2));
    outside.dispose();
    inside.dispose();
    for service in &mut services {
        service.finish();
    }
    cross_session_channels(fixture, &markers);
}

fn cross_session_channels(fixture: &Fixture, markers: &std::path::Path) {
    let mut first_plan = fixture.plan("channel-first", 60000);
    let mut second_plan = fixture.plan("channel-second", 60001);
    let mut services = Vec::new();
    let request = "execute-fixed-sentinel\n";
    let mut attacks = Vec::new();
    for (name, plan) in [("first", &mut first_plan), ("second", &mut second_plan)] {
        let host_path = fixture.path(&format!("channel-{name}.sock"));
        let guest_path = plan.home.join("capability.sock");
        services.push(Service::unix(
            &Endpoint::Path(host_path.clone()),
            request,
            markers.join(format!("channel-{name}")),
            fixture.operator,
        ));
        plan.channels.push(Channel::UnixSocket {
            id: format!("capability-{name}"),
            host_path: host_path.clone(),
            guest_path: guest_path.clone(),
        });
        attacks.push((
            Probe::new(
                &format!("own-channel-{name}"),
                Attack::Unix(Endpoint::Path(guest_path), request.into()),
            ),
            Probe::new(
                &format!("foreign-channel-{name}"),
                Attack::Unix(Endpoint::Path(host_path), request.into()),
            ),
        ));
    }
    let mut first = fixture.inside(&first_plan);
    let mut second = fixture.inside(&second_plan);
    let mut control = fixture.outside(0);
    for (index, (own, host)) in attacks.iter().enumerate() {
        let (owner, foreign) = if index == 0 {
            (&mut first, &mut second)
        } else {
            (&mut second, &mut first)
        };
        let allowed = owner.request(std::slice::from_ref(own));
        assert!(
            matches!(allowed[0].outcome, Outcome::Allowed),
            "declared owner channel works"
        );
        check(
            std::slice::from_ref(own),
            &allowed,
            &foreign.request(std::slice::from_ref(own)),
        );
        check(
            std::slice::from_ref(host),
            &control.request(std::slice::from_ref(host)),
            &foreign.request(std::slice::from_ref(host)),
        );
    }
    first.dispose();
    second.dispose();
    control.dispose();
    for service in &mut services {
        service.finish();
    }
}
