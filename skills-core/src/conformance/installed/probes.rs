//! Fixed harmless probe implementation; only the trusted certifier supplies requests.

use crate::conformance::{Observation, Outcome};
use serde::{Deserialize, Serialize};
use std::{
    fs,
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

/// Closed harmless operations shared by installed and disposable probe runners.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Attack {
    /// Read an owned harmless target.
    Read(PathBuf),
    /// Overwrite a deliberately writable owned sentinel.
    Write(PathBuf),
    /// Inspect the fixed ambient sentinel variable.
    Environment,
    /// Inspect an explicitly injected sentinel descriptor.
    FileDescriptor(i32),
    /// Write to an explicitly injected socket descriptor.
    SocketDescriptor(i32),
    /// Attach to a certifier-owned target process.
    Ptrace(u32),
    /// Deliver harmless SIGCONT to a certifier-owned target.
    Signal(u32),
    /// Exchange a fixed sentinel with an owned Unix endpoint.
    Unix(Endpoint),
    /// Exchange a fixed sentinel in the owned network namespace.
    Tcp(SocketAddr),
}

/// An endpoint created and owned by the trusted test runner.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Endpoint {
    /// Owned pathname socket.
    Path(PathBuf),
    /// Owned abstract socket in the isolated network namespace.
    Abstract(String),
}

impl Endpoint {
    /// Resolve this endpoint without connecting.
    ///
    /// # Errors
    /// Rejects invalid or overlong Unix socket addresses.
    pub fn address(&self) -> io::Result<UnixAddress> {
        match self {
            Self::Path(path) => UnixAddress::from_pathname(path),
            Self::Abstract(name) => UnixAddress::from_abstract_name(name),
        }
    }
}

/// One fixed inventory entry and its owned sentinel operation.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Probe {
    /// Fixed name in the required inventory.
    pub name: String,
    /// Harmless operation against an owned resource.
    pub attack: Attack,
}

impl Probe {
    /// Pair a fixed check name with its owned operation.
    #[must_use]
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
            Outcome::Denied(format!("errno:{}", error.raw_os_error().unwrap_or(0)))
        }
        Err(_) => Outcome::Error("operation unavailable".into()),
    }
}

fn attempt(attack: &Attack) -> io::Result<Outcome> {
    use rustix::io::Errno;
    let read_denial = [
        Errno::NOENT.raw_os_error(),
        Errno::ACCESS.raw_os_error(),
        Errno::PERM.raw_os_error(),
    ];
    Ok(match attack {
        Attack::Read(path) => observe_io(fs::read(path).map(|_| ()), &read_denial),
        Attack::Write(path) => observe_io(
            fs::OpenOptions::new()
                .write(true)
                .open(path)
                .and_then(|mut file| file.write_all(b"harmless-write")),
            &[
                Errno::ROFS.raw_os_error(),
                Errno::ACCESS.raw_os_error(),
                Errno::PERM.raw_os_error(),
            ],
        ),
        Attack::Environment => match std::env::var("LOUISELM_AMBIENT_SENTINEL") {
            Ok(value) if value == "hostile-ambient-sentinel" => Outcome::Allowed,
            Err(std::env::VarError::NotPresent) => Outcome::Denied("absent environment".into()),
            _ => Outcome::Error("unexpected environment".into()),
        },
        Attack::FileDescriptor(fd) => match fs::read(format!("/proc/self/fd/{fd}")) {
            Ok(bytes) if bytes == b"hostile-fd-sentinel" => Outcome::Allowed,
            Ok(_) => Outcome::Error("descriptor reused".into()),
            Err(error) => observe_io(Err(error), &[Errno::NOENT.raw_os_error()]),
        },
        Attack::SocketDescriptor(fd) => {
            // Only a small decimal descriptor from the trusted parent enters this fixed script.
            if !(100..=1024).contains(fd) {
                return Err(io::ErrorKind::InvalidInput.into());
            }
            let output = Command::new("/bin/bash")
                .args(["-c", &format!("printf hostile-socket-sentinel >&{fd}")])
                .output()?;
            if output.status.success() {
                Outcome::Allowed
            } else if String::from_utf8_lossy(&output.stderr).contains("Bad file descriptor") {
                Outcome::Denied("EBADF".into())
            } else {
                Outcome::Error("descriptor probe unavailable".into())
            }
        }
        Attack::Ptrace(pid) => trace(*pid)?,
        Attack::Signal(pid) => {
            let pid = rustix::process::Pid::from_raw(
                i32::try_from(*pid).map_err(|_| io::ErrorKind::InvalidInput)?,
            )
            .ok_or(io::ErrorKind::InvalidInput)?;
            observe_io(
                rustix::process::kill_process(pid, rustix::process::Signal::CONT)
                    .map_err(Into::into),
                &[Errno::SRCH.raw_os_error(), Errno::PERM.raw_os_error()],
            )
        }
        Attack::Unix(endpoint) => match UnixStream::connect_addr(&endpoint.address()?) {
            Ok(mut stream) => {
                stream.set_read_timeout(Some(Duration::from_secs(2)))?;
                stream.set_write_timeout(Some(Duration::from_secs(2)))?;
                exchange(&mut stream)
            }
            Err(error) => observe_io(
                Err(error),
                &[
                    Errno::NOENT.raw_os_error(),
                    Errno::ACCESS.raw_os_error(),
                    Errno::CONNREFUSED.raw_os_error(),
                ],
            ),
        },
        Attack::Tcp(address) => match TcpStream::connect_timeout(address, Duration::from_secs(2)) {
            Ok(mut stream) => {
                stream.set_read_timeout(Some(Duration::from_secs(2)))?;
                stream.set_write_timeout(Some(Duration::from_secs(2)))?;
                exchange(&mut stream)
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
    })
}

fn exchange(stream: &mut (impl Read + Write)) -> Outcome {
    let mut reply = [0; 9];
    if stream
        .write_all(b"sentinel\n")
        .and_then(|()| stream.read_exact(&mut reply))
        .is_err()
    {
        Outcome::Error("sentinel exchange unavailable".into())
    } else if &reply == b"executed\n" {
        Outcome::Allowed
    } else {
        Outcome::Error("sentinel reply mismatch".into())
    }
}

fn trace(pid: u32) -> io::Result<Outcome> {
    let mut tracer = Command::new("/usr/bin/strace")
        .args([
            "-e",
            "trace=none",
            "-o",
            "/dev/null",
            "-p",
            &pid.to_string(),
        ])
        .env_clear()
        .env("LC_ALL", "C")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if tracer.try_wait()?.is_some() {
            let output = tracer.wait_with_output()?;
            let detail = String::from_utf8_lossy(&output.stderr);
            return Ok(
                if detail.contains("ptrace")
                    && (detail.contains("Operation not permitted")
                        || detail.contains("No such process"))
                {
                    Outcome::Denied("ptrace refused".into())
                } else {
                    Outcome::Error("ptrace unavailable".into())
                },
            );
        }
        let attached = fs::read_to_string(format!("/proc/{pid}/status")).is_ok_and(|status| {
            status.lines().any(|line| {
                line.strip_prefix("TracerPid:")
                    .is_some_and(|value| value.trim() == tracer.id().to_string())
            })
        });
        if attached || Instant::now() >= deadline {
            tracer.kill()?;
            tracer.wait()?;
            return Ok(if attached {
                Outcome::Allowed
            } else {
                Outcome::Error("ptrace deadline".into())
            });
        }
        thread::sleep(Duration::from_millis(5));
    }
}

/// Internal probe stdio loop, without installed authority or automatic execution.
/// The launcher exposes this only as an unprivileged internal verb; privileged
/// callers must own the request pipe and every named target.
///
/// # Errors
/// Rejects unbounded/invalid frames and propagates pipe failures.
#[doc(hidden)]
pub fn serve_probe() -> io::Result<()> {
    let mut input = io::stdin().lock();
    loop {
        let mut line = Vec::new();
        input
            .by_ref()
            .take(32 * 1024 + 1)
            .read_until(b'\n', &mut line)?;
        if line.is_empty() {
            return Ok(());
        }
        if line.len() > 32 * 1024 {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let probes: Vec<Probe> = serde_json::from_slice(&line)?;
        if probes.len() > 46
            || probes
                .iter()
                .any(|probe| !crate::conformance::REQUIRED_CHECKS.contains(&probe.name.as_str()))
        {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let observations: Vec<_> = probes
            .into_iter()
            .map(|probe| Observation {
                name: probe.name,
                outcome: attempt(&probe.attack)
                    .unwrap_or_else(|_| Outcome::Error("probe unavailable".into())),
            })
            .collect();
        let mut output = io::stdout().lock();
        output.write_all(b"HOSTILE_JSON:")?;
        serde_json::to_writer(&mut output, &observations)?;
        output.write_all(b"\n")?;
        output.flush()?;
    }
}
