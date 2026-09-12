//! Owned socket services execute only a fixed harmless printf, never client commands.

use super::super::probes::Endpoint;
use super::{
    Attack, CertificateStore, CertificationError, Channel, Command, CommandExt, Duration, Fixture,
    Instant, JoinHandle, Outcome, Probe, Read, Result, Stdio, Write, mode, thread,
};
use std::{
    io,
    net::{IpAddr, TcpListener},
    os::unix::net::UnixListener,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

pub(super) struct Service {
    stop: Arc<AtomicBool>,
    executions: Arc<AtomicUsize>,
    worker: Option<JoinHandle<io::Result<()>>>,
}

impl Service {
    fn unix(endpoint: &Endpoint, uid: u32) -> Result<Self> {
        let listener = UnixListener::bind_addr(&endpoint.address()?)?;
        if let Endpoint::Path(path) = endpoint {
            mode(path, 0o666)?;
        }
        listener.set_nonblocking(true)?;
        Self::start(
            move || {
                let (stream, _) = listener.accept()?;
                stream.set_read_timeout(Some(Duration::from_secs(2)))?;
                stream.set_write_timeout(Some(Duration::from_secs(2)))?;
                Ok(stream)
            },
            uid,
        )
    }

    fn tcp(address: IpAddr, uid: u32) -> Result<(Self, Attack)> {
        let listener = TcpListener::bind((address, 0))?;
        let address = listener.local_addr()?;
        listener.set_nonblocking(true)?;
        Ok((
            Self::start(
                move || {
                    let (stream, _) = listener.accept()?;
                    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
                    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
                    Ok(stream)
                },
                uid,
            )?,
            Attack::Tcp(address),
        ))
    }

    fn start<S: Read + Write>(
        accept: impl Fn() -> io::Result<S> + Send + 'static,
        uid: u32,
    ) -> Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let executions = Arc::new(AtomicUsize::new(0));
        let stopping = Arc::clone(&stop);
        let count = Arc::clone(&executions);
        let worker = thread::Builder::new()
            .name("conformance-sentinel".into())
            .spawn(move || {
                while !stopping.load(Ordering::Acquire) {
                    match accept() {
                        Ok(mut stream) => {
                            let mut bytes = [0; 9];
                            stream.read_exact(&mut bytes)?;
                            if &bytes != b"sentinel\n" {
                                return Err(io::ErrorKind::InvalidInput.into());
                            }
                            // The endpoint models service-mediated execution, not a
                            // D-Bus/Docker parser. No caller-controlled argv or file.
                            let mut child = Command::new("/usr/bin/bash")
                                .args(["-c", "printf 'executed\\n'"])
                                .env_clear()
                                .uid(uid)
                                .gid(uid)
                                .stdin(Stdio::null())
                                .stdout(Stdio::piped())
                                .stderr(Stdio::null())
                                .spawn()?;
                            let deadline = Instant::now() + Duration::from_secs(2);
                            loop {
                                if child.try_wait()?.is_some() {
                                    break;
                                }
                                if Instant::now() >= deadline {
                                    child.kill()?;
                                    child.wait()?;
                                    return Err(io::ErrorKind::TimedOut.into());
                                }
                                thread::sleep(Duration::from_millis(5));
                            }
                            let output = child.wait_with_output()?;
                            if !output.status.success() || output.stdout != b"executed\n" {
                                return Err(io::ErrorKind::InvalidData.into());
                            }
                            count.fetch_add(1, Ordering::Release);
                            stream.write_all(&output.stdout)?;
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => return Err(error),
                    }
                }
                Ok(())
            })?;
        Ok(Self {
            stop,
            executions,
            worker: Some(worker),
        })
    }

    fn count(&self) -> usize {
        self.executions.load(Ordering::Acquire)
    }

    pub(super) fn finish(&mut self) -> Result<()> {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.join().map_err(|_| CertificationError::Invalid)??;
        }
        Ok(())
    }
}

pub(super) fn run(fixture: &mut Fixture, store: &mut CertificateStore) -> Result<()> {
    let mut probes = Vec::new();
    let first_service = fixture.services.len();
    for name in ["pathname-unix", "dbus-systemd", "docker", "abstract-unix"] {
        let endpoint = if name == "abstract-unix" {
            Endpoint::Abstract(format!("louiselm-cert-{}", std::process::id()))
        } else {
            Endpoint::Path(fixture.path(&format!("{name}.sock")))
        };
        fixture
            .services
            .push(Service::unix(&endpoint, fixture.config.operator_uid)?);
        probes.push(Probe::new(name, Attack::Unix(endpoint)));
    }
    for (name, address) in [
        ("ipv4", "198.18.0.1"),
        ("ipv6", "fd00:10::1"),
        ("ipv4-loopback", "127.0.0.1"),
        ("ipv6-loopback", "::1"),
    ] {
        let (service, attack) = Service::tcp(
            address.parse().map_err(|_| CertificationError::Invalid)?,
            fixture.config.operator_uid,
        )?;
        fixture.services.push(service);
        probes.push(Probe::new(name, attack));
    }
    let identity = fixture.identity(0)?;
    let outside = fixture.outside(identity.uid, identity.gid)?;
    let inside = fixture.inside(&fixture.plan("sockets", 0)?)?;
    let allowed = fixture.request(outside, &probes)?;
    if fixture.services[first_service..]
        .iter()
        .any(|service| service.count() != 1)
    {
        return Err(CertificationError::Unsupported);
    }
    let denied = fixture.request(inside, &probes)?;
    fixture.paired(store, &probes, &allowed, &denied)?;
    // The report already retains any successful forbidden exchange. An
    // unexpected endpoint client can only make this run incomplete.
    if fixture.services[first_service..]
        .iter()
        .any(|service| service.count() != 1)
    {
        return Err(CertificationError::Unsupported);
    }
    if fixture
        .request(outside, &probes)?
        .iter()
        .any(|row| row.outcome != Outcome::Allowed)
    {
        return Err(CertificationError::Unsupported);
    }
    fixture.dispose(outside)?;
    fixture.dispose(inside)?;
    channels(fixture, store)
}

fn channels(fixture: &mut Fixture, store: &mut CertificateStore) -> Result<()> {
    let mut first_plan = fixture.plan("channel-first", 0)?;
    let mut second_plan = fixture.plan("channel-second", 1)?;
    let mut attacks = Vec::new();
    for (name, plan) in [("first", &mut first_plan), ("second", &mut second_plan)] {
        let host_path = fixture.path(&format!("channel-{name}.sock"));
        let guest_path = plan.home.join("capability.sock");
        fixture.services.push(Service::unix(
            &Endpoint::Path(host_path.clone()),
            fixture.config.operator_uid,
        )?);
        plan.channels.push(Channel::UnixSocket {
            id: format!("capability-{name}"),
            host_path: host_path.clone(),
            guest_path: guest_path.clone(),
        });
        attacks.push((
            Probe::new(
                &format!("own-channel-{name}"),
                Attack::Unix(Endpoint::Path(guest_path)),
            ),
            Probe::new(
                &format!("foreign-channel-{name}"),
                Attack::Unix(Endpoint::Path(host_path)),
            ),
        ));
    }
    let first = fixture.inside(&first_plan)?;
    let second = fixture.inside(&second_plan)?;
    let control = fixture.outside(0, 0)?;
    for (index, (own, host)) in attacks.iter().enumerate() {
        let (owner, foreign) = if index == 0 {
            (first, second)
        } else {
            (second, first)
        };
        let allowed = fixture.request(owner, std::slice::from_ref(own))?;
        let denied = fixture.request(foreign, std::slice::from_ref(own))?;
        fixture.paired(store, std::slice::from_ref(own), &allowed, &denied)?;
        let allowed = fixture.request(control, std::slice::from_ref(host))?;
        let denied = fixture.request(foreign, std::slice::from_ref(host))?;
        fixture.paired(store, std::slice::from_ref(host), &allowed, &denied)?;
    }
    fixture.dispose(first)?;
    fixture.dispose(second)?;
    fixture.dispose(control)
}
