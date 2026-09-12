//! The same deterministic attack implementation runs outside and inside containment.

pub use louiselm_skills::conformance::{Observation, Outcome};
use serde::{Deserialize, Serialize};
use std::{
    env, fs,
    io::{self, BufRead, Read, Write},
    net::{SocketAddr, TcpStream},
    os::{
        linux::net::SocketAddrExt,
        unix::net::{SocketAddr as UnixAddress, UnixStream},
    },
    path::PathBuf,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum Attack {
    Read(PathBuf),
    Write(PathBuf),
    Environment(String),
    FileDescriptor(i32),
    SocketDescriptor(i32),
    Ptrace(u32),
    Signal(u32),
    Unix(Endpoint, String),
    Tcp(SocketAddr),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum Endpoint {
    Path(PathBuf),
    Abstract(String),
}

impl Endpoint {
    pub fn address(&self) -> io::Result<UnixAddress> {
        match self {
            Self::Path(path) => UnixAddress::from_pathname(path),
            Self::Abstract(name) => UnixAddress::from_abstract_name(name),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Probe {
    pub name: String,
    pub attack: Attack,
}

impl Probe {
    pub fn new(name: &str, attack: Attack) -> Self {
        Self {
            name: name.into(),
            attack,
        }
    }
}

fn observe_io(result: io::Result<()>, denied: &[i32]) -> Outcome {
    match result {
        Ok(()) => Outcome::Allowed,
        Err(error)
            if error
                .raw_os_error()
                .is_some_and(|code| denied.contains(&code)) =>
        {
            Outcome::Denied(error.to_string())
        }
        Err(error) => Outcome::Error(error.to_string()),
    }
}

fn attempt(attack: &Attack) -> Outcome {
    use rustix::io::Errno;
    let read_denial = [
        Errno::NOENT.raw_os_error(),
        Errno::ACCESS.raw_os_error(),
        Errno::PERM.raw_os_error(),
    ];
    match attack {
        Attack::Read(path) => observe_io(fs::read(path).map(|_| ()), &read_denial),
        Attack::Write(path) => observe_io(
            fs::OpenOptions::new()
                .write(true)
                .open(path)
                .and_then(|mut file| file.write_all(b"hostile-write")),
            &[
                Errno::ROFS.raw_os_error(),
                Errno::ACCESS.raw_os_error(),
                Errno::PERM.raw_os_error(),
            ],
        ),
        Attack::Environment(name) => match env::var(name) {
            Ok(value) if value == "hostile-ambient-sentinel" => Outcome::Allowed,
            Err(env::VarError::NotPresent) => Outcome::Denied("absent environment".into()),
            other => Outcome::Error(format!("unexpected ambient value: {other:?}")),
        },
        Attack::FileDescriptor(fd) => match fs::read(format!("/proc/self/fd/{fd}")) {
            Ok(bytes) if bytes == b"hostile-fd-sentinel" => Outcome::Allowed,
            Ok(_) => Outcome::Error("descriptor was reused, not proven absent".into()),
            Err(error) => observe_io(Err(error), &[Errno::NOENT.raw_os_error()]),
        },
        Attack::SocketDescriptor(fd) => {
            let output = Command::new("/bin/bash")
                .args(["-c", &format!("printf hostile-socket-sentinel >&{fd}")])
                .output()
                .expect("bash probe starts");
            if output.status.success() {
                Outcome::Allowed
            } else if String::from_utf8_lossy(&output.stderr).contains("Bad file descriptor") {
                Outcome::Denied("EBADF".into())
            } else {
                Outcome::Error(format!("socket fd probe: {output:?}"))
            }
        }
        Attack::Ptrace(pid) => trace(*pid),
        Attack::Signal(pid) => {
            let pid = rustix::process::Pid::from_raw(i32::try_from(*pid).unwrap()).unwrap();
            // SIGCONT makes a real harmless delivery; unlike signal 0 this
            // actually exercises the signal syscall's delivery path.
            observe_io(
                rustix::process::kill_process(pid, rustix::process::Signal::CONT)
                    .map_err(Into::into),
                &[Errno::SRCH.raw_os_error(), Errno::PERM.raw_os_error()],
            )
        }
        Attack::Unix(endpoint, request) => {
            let address = endpoint.address().expect("valid owned endpoint");
            match UnixStream::connect_addr(&address) {
                Ok(mut stream) => {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(2)))
                        .unwrap();
                    stream
                        .set_write_timeout(Some(Duration::from_secs(2)))
                        .unwrap();
                    exchange(&mut stream, request)
                }
                Err(error) => observe_io(
                    Err(error),
                    &[
                        Errno::NOENT.raw_os_error(),
                        Errno::ACCESS.raw_os_error(),
                        Errno::CONNREFUSED.raw_os_error(),
                    ],
                ),
            }
        }
        Attack::Tcp(address) => match TcpStream::connect_timeout(address, Duration::from_secs(2)) {
            Ok(mut stream) => {
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                exchange(&mut stream, "local-network-sentinel\n")
            }
            Err(error) => observe_io(
                Err(error),
                &[
                    Errno::NETUNREACH.raw_os_error(),
                    Errno::HOSTUNREACH.raw_os_error(),
                    Errno::CONNREFUSED.raw_os_error(),
                    Errno::ACCESS.raw_os_error(),
                    Errno::PERM.raw_os_error(),
                ],
            ),
        },
    }
}

fn exchange(stream: &mut (impl Read + Write), request: &str) -> Outcome {
    let mut reply = [0; 9];
    if let Err(error) = stream
        .write_all(request.as_bytes())
        .and_then(|()| stream.read_exact(&mut reply))
    {
        return Outcome::Error(format!("connected but sentinel exchange failed: {error}"));
    }
    if &reply == b"executed\n" {
        Outcome::Allowed
    } else {
        Outcome::Error("sentinel reply mismatch".into())
    }
}

fn trace(pid: u32) -> Outcome {
    let mut tracer = Command::new("/usr/bin/strace")
        .args([
            "-e",
            "trace=none",
            "-o",
            "/dev/null",
            "-p",
            &pid.to_string(),
        ])
        .env("LC_ALL", "C")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("existing strace is required for actual ptrace");
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if tracer.try_wait().unwrap().is_some() {
            let output = tracer.wait_with_output().unwrap();
            let detail = String::from_utf8_lossy(&output.stderr);
            return if detail.contains("ptrace")
                && (detail.contains("Operation not permitted")
                    || detail.contains("No such process"))
            {
                Outcome::Denied(detail.into())
            } else {
                Outcome::Error(format!("unexpected strace exit: {output:?}"))
            };
        }
        let attached = fs::read_to_string(format!("/proc/{pid}/status")).is_ok_and(|status| {
            status.lines().any(|line| {
                line.strip_prefix("TracerPid:")
                    .is_some_and(|value| value.trim() == tracer.id().to_string())
            })
        });
        if attached || Instant::now() >= deadline {
            tracer.kill().unwrap();
            tracer.wait().unwrap();
            return if attached {
                Outcome::Allowed
            } else {
                Outcome::Error("ptrace attachment not observed before deadline".into())
            };
        }
        thread::sleep(Duration::from_millis(5));
    }
}

pub fn serve() {
    for line in io::stdin().lock().lines() {
        let probes: Vec<Probe> =
            serde_json::from_str(&line.expect("request reads")).expect("request decodes");
        let rows: Vec<_> = probes
            .into_iter()
            .map(|probe| Observation {
                name: probe.name,
                outcome: attempt(&probe.attack),
            })
            .collect();
        println!(
            "HOSTILE_JSON:{}",
            serde_json::to_string(&rows).expect("observations encode")
        );
        io::stdout().flush().expect("observations flush");
    }
}
